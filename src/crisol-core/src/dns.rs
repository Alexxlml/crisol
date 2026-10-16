//! Resolving `.test` to this machine (F7.2, ADR-0014).
//!
//! Two independent pieces, both free of any operating system:
//!
//! - **The answers** ([`Zone`], [`answer`], [`DnsServer`]): a tiny DNS responder that says
//!   `127.0.0.1` / `::1` for every name under the configured suffix and refuses everything else.
//!   It never forwards a query, so it cannot be used as an open resolver. It listens on a
//!   dedicated loopback address ([`DEFAULT_DNS_ADDR`]) at port 53, which an ordinary user can
//!   bind, because the NRPT can name a server by IP only, with no port.
//! - **The rule** ([`NrptPolicy`], [`enable`], [`disable`], [`status`]): the Name Resolution
//!   Policy Table entry that tells Windows "ask that address about `.test`". Reading and writing
//!   it is the platform's job (`crisol-platform`); writing needs an administrator, so it is done
//!   by a minimal helper ([`ElevatedOp`]) that only knows these two operations. The logic here
//!   decides what may be done: only the special-use suffixes of the CA ([`crate::ca`]), only a
//!   loopback server, and **only rules that Crisol itself made** (marked with [`RULE_COMMENT`]);
//!   a rule another tool made for the same suffix is reported, never replaced or removed
//!   (ADR-0007).

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::ca::is_special_use;
use crate::hosts::{clean_names, HostsError, HostsManager, HostsManagerError};

pub mod fake;

/// The address the responder listens on: loopback, but not `127.0.0.1`, whose port 53 other
/// programs (WSL, connection sharing) like to take.
pub const DEFAULT_DNS_ADDR: Ipv4Addr = Ipv4Addr::new(127, 77, 0, 53);

/// What marks an NRPT rule as ours.
pub const RULE_COMMENT: &str = "Crisol";

/// Time to live of every answer, in seconds. Short, so removing a site is noticed quickly.
const TTL: u32 = 60;

// ---- the answers -----------------------------------------------------------------------

/// The names the responder answers for: the suffix itself and everything under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    suffix: String,
}

impl Zone {
    /// A zone for `test` (or `.test`). Only special-use suffixes are accepted.
    pub fn new(suffix: &str) -> Result<Self, DnsError> {
        Ok(Self {
            suffix: clean_suffix(suffix)?,
        })
    }

    /// The suffix without dots: `test`.
    #[must_use]
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// The synthetic SOA record of the zone, ready to go in the authority section. Its last
    /// field, the negative-caching time, is [`TTL`].
    fn soa_record(&self) -> Vec<u8> {
        let apex = encode_name(&self.suffix);
        let mut rdata = encode_name(&format!("crisol.{}", self.suffix)); // primary server
        rdata.extend_from_slice(&encode_name(&format!("hostmaster.{}", self.suffix)));
        for field in [1_u32, 3600, 600, 86_400, TTL] {
            rdata.extend_from_slice(&field.to_be_bytes());
        }
        let mut record = apex;
        record.extend_from_slice(&TYPE_SOA.to_be_bytes());
        record.extend_from_slice(&CLASS_IN.to_be_bytes());
        record.extend_from_slice(&TTL.to_be_bytes());
        record.extend_from_slice(&u16::try_from(rdata.len()).unwrap_or(0).to_be_bytes());
        record.extend_from_slice(&rdata);
        record
    }

    /// Whether `name` is the suffix or under it.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        name == self.suffix || name.ends_with(&format!(".{}", self.suffix))
    }
}

fn clean_suffix(suffix: &str) -> Result<String, DnsError> {
    let clean = suffix.trim_matches('.').to_ascii_lowercase();
    let valid = !clean.is_empty()
        && clean.len() <= 63
        && clean
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && !clean.contains("..")
        && !clean.starts_with('-');
    if !valid {
        return Err(DnsError::InvalidSuffix(suffix.to_owned()));
    }
    if !is_special_use(&clean) {
        return Err(DnsError::NotSpecialUse(clean));
    }
    Ok(clean)
}

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const TYPE_SOA: u16 = 6;
/// Longest domain name, in text, without the final dot (RFC 1035: 255 bytes on the wire).
const MAX_NAME: usize = 253;
const CLASS_IN: u16 = 1;
const RCODE_FORMERR: u8 = 1;
const RCODE_REFUSED: u8 = 5;

/// The reply to one query datagram, or `None` when it must be dropped (too short to carry an
/// id, or not a query at all).
///
/// - A name in the zone: `A` gives `127.0.0.1`, `AAAA` gives `::1`, any other type gets an
///   empty successful answer (the name exists, with no data of that type).
/// - A name outside the zone, a class other than `IN`, or an opcode other than a query:
///   `REFUSED`. Nothing is ever forwarded.
/// - A question that cannot be read: `FORMERR`.
#[must_use]
pub fn answer(query: &[u8], zone: &Zone) -> Option<Vec<u8>> {
    if query.len() < 12 || query[2] & 0x80 != 0 {
        return None; // too short, or a response: never answer an answer
    }
    let id = [query[0], query[1]];
    let recursion_desired = query[2] & 0x01;
    let opcode = (query[2] >> 3) & 0x0F;
    let questions = u16::from_be_bytes([query[4], query[5]]);

    let reply = |rcode: u8, question: &[u8], data: Option<(u16, Vec<u8>)>, soa: bool| {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(&id);
        // QR, authoritative, opcode 0, RD copied; no recursion available.
        out.push(0x84 | recursion_desired);
        out.push(rcode);
        out.extend_from_slice(&u16::from(!question.is_empty()).to_be_bytes());
        out.extend_from_slice(&u16::from(data.is_some()).to_be_bytes());
        out.extend_from_slice(&u16::from(soa).to_be_bytes()); // authority records
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(question);
        if let Some((qtype, rdata)) = data {
            out.extend_from_slice(&[0xC0, 0x0C]); // the name, pointing at the question
            out.extend_from_slice(&qtype.to_be_bytes());
            out.extend_from_slice(&CLASS_IN.to_be_bytes());
            out.extend_from_slice(&TTL.to_be_bytes());
            out.extend_from_slice(&u16::try_from(rdata.len()).unwrap_or(0).to_be_bytes());
            out.extend_from_slice(&rdata);
        }
        if soa {
            out.extend_from_slice(&zone.soa_record());
        }
        out
    };

    if opcode != 0 {
        return Some(reply(RCODE_REFUSED, &[], None, false));
    }
    let Some((name, qtype, qclass, question)) = read_question(&query[12..]) else {
        return Some(reply(RCODE_FORMERR, &[], None, false));
    };
    if questions != 1 {
        return Some(reply(RCODE_FORMERR, &[], None, false));
    }
    if qclass != CLASS_IN || !zone.contains(&name) {
        return Some(reply(RCODE_REFUSED, question, None, false));
    }
    let data = match qtype {
        TYPE_A => Some((TYPE_A, Ipv4Addr::LOCALHOST.octets().to_vec())),
        TYPE_AAAA => Some((TYPE_AAAA, Ipv6Addr::LOCALHOST.octets().to_vec())),
        _ => None,
    };
    // A name with no data of the asked type (HTTPS/SVCB, MX, ...) says so with the zone's SOA,
    // so the client keeps "there is none" for a while instead of asking again.
    let soa = data.is_none();
    Some(reply(0, question, data, soa))
}

/// A name as DNS labels, without compression: `crisol.test` becomes `6crisol4test0`.
fn encode_name(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.split('.') {
        out.push(u8::try_from(label.len()).unwrap_or(0));
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

/// Reads the first question: its name, type, class and its raw bytes. No compression is
/// allowed in a question, so a pointer means a malformed query.
fn read_question(bytes: &[u8]) -> Option<(String, u16, u16, &[u8])> {
    let mut i = 0;
    let mut name = String::new();
    loop {
        let len = usize::from(*bytes.get(i)?);
        if len == 0 {
            i += 1;
            break;
        }
        if len > 63 {
            return None;
        }
        let label = bytes.get(i + 1..i + 1 + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(std::str::from_utf8(label).ok()?);
        if name.len() > MAX_NAME {
            return None; // not a domain name: the question is malformed
        }
        i += 1 + len;
    }
    let tail = bytes.get(i..i + 4)?;
    Some((
        name,
        u16::from_be_bytes([tail[0], tail[1]]),
        u16::from_be_bytes([tail[2], tail[3]]),
        &bytes[..i + 4],
    ))
}

/// The responder bound to a UDP socket.
#[derive(Debug)]
pub struct DnsServer {
    socket: UdpSocket,
    zone: Zone,
}

impl DnsServer {
    /// Binds `addr`. The address must be loopback: this server is never reachable from the
    /// network.
    pub fn bind(addr: SocketAddr, zone: Zone) -> Result<Self, DnsError> {
        if !addr.ip().is_loopback() {
            return Err(DnsError::NotLoopback(addr.ip()));
        }
        let socket = UdpSocket::bind(addr).map_err(DnsError::Bind)?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(DnsError::Bind)?;
        Ok(Self { socket, zone })
    }

    /// The address it is actually listening on.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Answers queries until `stop` is set. Returns how many it answered.
    pub fn run(&self, stop: &AtomicBool) -> usize {
        let mut buf = [0_u8; 512];
        let mut served = 0;
        while !stop.load(Ordering::Relaxed) {
            let Ok((n, from)) = self.socket.recv_from(&mut buf) else {
                continue; // timeout, or a reset from a client that went away
            };
            if let Some(reply) = answer(&buf[..n], &self.zone) {
                if self.socket.send_to(&reply, from).is_ok() {
                    served += 1;
                }
            }
        }
        served
    }
}

// ---- the rule --------------------------------------------------------------------------

/// One NRPT rule, as far as Crisol cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NrptRule {
    /// The namespace, with its leading dot: `.test`.
    pub namespace: String,
    /// The one server it points at.
    pub name_server: IpAddr,
    /// The rule's comment; [`RULE_COMMENT`] on ours.
    pub comment: String,
}

impl NrptRule {
    /// Whether Crisol made it.
    #[must_use]
    pub fn is_ours(&self) -> bool {
        self.comment == RULE_COMMENT
    }
}

/// The Name Resolution Policy Table of the machine.
pub trait NrptPolicy: Send + Sync {
    /// The rules with a single namespace each (a rule with several is reported once per
    /// namespace). Read only.
    fn rules(&self) -> io::Result<Vec<NrptRule>>;

    /// Adds a rule. Needs an administrator on the real table.
    fn add(&self, rule: &NrptRule) -> io::Result<()>;

    /// Removes the rule for this namespace. Returns whether there was one.
    fn remove(&self, namespace: &str) -> io::Result<bool>;
}

/// What is in the table for a suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NrptStatus {
    /// No rule for it.
    Absent,
    /// Our rule.
    Ours(NrptRule),
    /// A rule some other tool (or a group policy) made.
    Foreign(NrptRule),
}

/// What [`enable`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnableOutcome {
    /// The rule was added.
    Added,
    /// Our rule was already exactly this; nothing changed.
    AlreadyThere,
    /// Our rule pointed somewhere else; it now points at the new server.
    Updated,
}

/// Why the DNS pieces refuse or fail.
#[derive(Debug, thiserror::Error)]
pub enum DnsError {
    /// The suffix is not a plain domain name.
    #[error("`{0}` is not a valid domain suffix")]
    InvalidSuffix(String),
    /// The server of a rule is not an IP address.
    #[error("`{0}` is not an IP address")]
    InvalidAddress(String),
    /// The helper was given something other than one of its operations.
    #[error("the helper does not do `{0}`; it only accepts nrpt-add <suffix> <ip>, nrpt-remove <suffix>, hosts-set <name>... and hosts-clear")]
    InvalidArguments(String),
    /// Only the reserved names may be pointed at the local responder.
    #[error(
        "`{0}` could be a real public domain; Crisol only resolves local, special-use suffixes \
         (test, localhost, example, invalid, local, internal, home.arpa)"
    )]
    NotSpecialUse(String),
    /// The server for the rule or the responder must be loopback.
    #[error(
        "{0} is not a loopback address; the local DNS server is never reachable from the network"
    )]
    NotLoopback(IpAddr),
    /// Another tool already has a rule for this suffix.
    #[error(
        "the rule for `{namespace}` was made by something else (server {server}, comment \
         `{comment}`); Crisol leaves it alone. Remove it yourself or use another suffix"
    )]
    Foreign {
        /// The namespace.
        namespace: String,
        /// Where the rule points.
        server: IpAddr,
        /// Its comment.
        comment: String,
    },
    /// The socket could not be bound.
    #[error("could not listen for DNS queries: {0}")]
    Bind(io::Error),
    /// The `hosts` file or the names for it are refused.
    #[error(transparent)]
    Hosts(HostsError),
    /// The table could not be read or changed.
    #[error("name resolution policy: {0}")]
    Policy(#[from] io::Error),
}

/// The rule Crisol would create for this suffix and server, after validating both.
pub fn rule_for(suffix: &str, server: IpAddr) -> Result<NrptRule, DnsError> {
    let suffix = clean_suffix(suffix)?;
    if !server.is_loopback() {
        return Err(DnsError::NotLoopback(server));
    }
    Ok(NrptRule {
        namespace: format!(".{suffix}"),
        name_server: server,
        comment: RULE_COMMENT.to_owned(),
    })
}

/// What the table holds for this suffix.
pub fn status(policy: &dyn NrptPolicy, suffix: &str) -> Result<NrptStatus, DnsError> {
    let namespace = format!(".{}", clean_suffix(suffix)?);
    let found = policy
        .rules()?
        .into_iter()
        .find(|r| r.namespace.eq_ignore_ascii_case(&namespace));
    Ok(match found {
        None => NrptStatus::Absent,
        Some(rule) if rule.is_ours() => NrptStatus::Ours(rule),
        Some(rule) => NrptStatus::Foreign(rule),
    })
}

fn foreign(rule: NrptRule) -> DnsError {
    DnsError::Foreign {
        namespace: rule.namespace,
        server: rule.name_server,
        comment: rule.comment,
    }
}

/// Makes sure the table sends `suffix` to `server`. Idempotent. A rule made by someone else
/// for the same suffix is an error and is left untouched.
pub fn enable(
    policy: &dyn NrptPolicy,
    suffix: &str,
    server: IpAddr,
) -> Result<EnableOutcome, DnsError> {
    let wanted = rule_for(suffix, server)?;
    match status(policy, suffix)? {
        NrptStatus::Foreign(rule) => Err(foreign(rule)),
        NrptStatus::Ours(rule) if rule == wanted => Ok(EnableOutcome::AlreadyThere),
        NrptStatus::Ours(_) => {
            policy.remove(&wanted.namespace)?;
            policy.add(&wanted)?;
            Ok(EnableOutcome::Updated)
        }
        NrptStatus::Absent => {
            policy.add(&wanted)?;
            Ok(EnableOutcome::Added)
        }
    }
}

/// Removes our rule for `suffix`. Returns whether there was one. Someone else's rule is an
/// error and is left untouched.
pub fn disable(policy: &dyn NrptPolicy, suffix: &str) -> Result<bool, DnsError> {
    match status(policy, suffix)? {
        NrptStatus::Absent => Ok(false),
        NrptStatus::Foreign(rule) => Err(foreign(rule)),
        NrptStatus::Ours(rule) => Ok(policy.remove(&rule.namespace)?),
    }
}

// ---- the elevated helper ---------------------------------------------------------------

/// The only four things the elevated helper does. It is given them as command-line arguments,
/// so what it accepts is what [`ElevatedOp::parse`] accepts, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevatedOp {
    /// `nrpt-add <suffix> <loopback ip>`
    NrptAdd {
        /// The suffix, without dots.
        suffix: String,
        /// The loopback server.
        server: IpAddr,
    },
    /// `nrpt-remove <suffix>`
    NrptRemove {
        /// The suffix, without dots.
        suffix: String,
    },
    /// `hosts-set <name>...`: the managed block of `hosts` holds exactly these names.
    HostsSet {
        /// Clean, sorted names (see [`crate::hosts::clean_names`]).
        names: Vec<String>,
    },
    /// `hosts-clear`: the managed block of `hosts` is taken out.
    HostsClear,
}

impl ElevatedOp {
    /// Parses and validates the helper's arguments (without the program name).
    pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Self, DnsError> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        match args.as_slice() {
            ["nrpt-add", suffix, server] => {
                let server: IpAddr = server
                    .parse()
                    .map_err(|_| DnsError::InvalidAddress((*server).to_owned()))?;
                let rule = rule_for(suffix, server)?;
                Ok(Self::NrptAdd {
                    suffix: rule.namespace.trim_start_matches('.').to_owned(),
                    server,
                })
            }
            ["nrpt-remove", suffix] => Ok(Self::NrptRemove {
                suffix: clean_suffix(suffix)?,
            }),
            ["hosts-set", names @ ..] if !names.is_empty() => Ok(Self::HostsSet {
                names: clean_names(names).map_err(DnsError::Hosts)?,
            }),
            ["hosts-clear"] => Ok(Self::HostsClear),
            _ => Err(DnsError::InvalidArguments(args.join(" "))),
        }
    }

    /// The arguments that [`parse`](Self::parse) turns back into this operation.
    #[must_use]
    pub fn to_args(&self) -> Vec<String> {
        match self {
            Self::NrptAdd { suffix, server } => {
                vec!["nrpt-add".to_owned(), suffix.clone(), server.to_string()]
            }
            Self::NrptRemove { suffix } => vec!["nrpt-remove".to_owned(), suffix.clone()],
            Self::HostsSet { names } => std::iter::once("hosts-set".to_owned())
                .chain(names.iter().cloned())
                .collect(),
            Self::HostsClear => vec!["hosts-clear".to_owned()],
        }
    }

    /// Does the operation against `policy` or `hosts`, with the same rules as
    /// [`enable`]/[`disable`] and [`HostsManager`].
    pub fn apply(&self, policy: &dyn NrptPolicy, hosts: &dyn HostsManager) -> Result<(), DnsError> {
        match self {
            Self::NrptAdd { suffix, server } => enable(policy, suffix, *server).map(|_| ()),
            Self::NrptRemove { suffix } => disable(policy, suffix).map(|_| ()),
            Self::HostsSet { names } => hosts.set(names).map(|_| ()).map_err(hosts_error),
            Self::HostsClear => hosts.clear().map_err(hosts_error),
        }
    }
}

fn hosts_error(error: HostsManagerError) -> DnsError {
    match error {
        HostsManagerError::Io(e) => DnsError::Policy(e),
        HostsManagerError::Hosts(e) => DnsError::Hosts(e),
    }
}

/// Reads the JSON that `Get-DnsClientNrptRule | ConvertTo-Json` prints (an object, an array
/// or nothing) into rules with one namespace each. Rules without exactly one IP server, or
/// with a server that is not an IP, are kept with an unspecified address (`0.0.0.0`) so that
/// they still show up as somebody else's.
pub fn parse_nrpt_json(json: &str) -> Result<Vec<NrptRule>, serde_json::Error> {
    let json = json.trim();
    if json.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(json)?;
    let items = match value {
        serde_json::Value::Array(items) => items,
        other => vec![other],
    };
    let mut rules = Vec::new();
    for item in items {
        let text = |key: &str| {
            item.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned()
        };
        let namespaces: Vec<String> = match item.get("Namespace") {
            Some(serde_json::Value::Array(list)) => list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            Some(serde_json::Value::String(one)) => vec![one.clone()],
            _ => Vec::new(),
        };
        let servers = item.get("NameServers").map_or_else(Vec::new, |v| match v {
            serde_json::Value::Array(list) => list
                .iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect(),
            serde_json::Value::String(s) => s.split(',').map(|p| p.trim().to_owned()).collect(),
            _ => Vec::new(),
        });
        let server = match servers.as_slice() {
            [one] => one.parse().unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            _ => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        };
        for namespace in namespaces {
            rules.push(NrptRule {
                namespace,
                name_server: server,
                comment: text("Comment"),
            });
        }
    }
    Ok(rules)
}

#[cfg(test)]
mod tests;
