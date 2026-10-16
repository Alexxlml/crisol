use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use super::fake::FakeNrpt;
use super::*;
use crate::hosts::{DirectHosts, MemoryHosts};

const LOOPBACK: IpAddr = IpAddr::V4(DEFAULT_DNS_ADDR);

fn query(name: &str, qtype: u16, class: u16) -> Vec<u8> {
    let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        q.push(u8::try_from(label.len()).unwrap());
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&class.to_be_bytes());
    q
}

fn zone() -> Zone {
    Zone::new("test").unwrap()
}

fn rcode(reply: &[u8]) -> u8 {
    reply[3] & 0x0F
}

fn answers(reply: &[u8]) -> u16 {
    u16::from_be_bytes([reply[6], reply[7]])
}

#[test]
fn a_and_aaaa_in_the_zone_point_at_this_machine() {
    let a = answer(&query("shop.test", 1, 1), &zone()).unwrap();
    assert_eq!(&a[..2], &[0x12, 0x34], "same id");
    assert_eq!(a[2] & 0x80, 0x80, "a response");
    assert_eq!(a[2] & 0x04, 0x04, "authoritative");
    assert_eq!(a[2] & 0x01, 0x01, "recursion desired is copied");
    assert_eq!(rcode(&a), 0);
    assert_eq!(answers(&a), 1);
    assert_eq!(&a[a.len() - 4..], &[127, 0, 0, 1]);

    let aaaa = answer(&query("api.shop.test", 28, 1), &zone()).unwrap();
    assert_eq!(answers(&aaaa), 1);
    let mut loopback = [0_u8; 16];
    loopback[15] = 1;
    assert_eq!(&aaaa[aaaa.len() - 16..], &loopback);
}

#[test]
fn names_are_matched_without_regard_to_case_or_a_trailing_dot() {
    for name in ["SHOP.Test", "test", "a.b.c.test"] {
        let reply = answer(&query(name, 1, 1), &zone()).unwrap();
        assert_eq!(answers(&reply), 1, "{name}");
    }
}

#[test]
fn other_record_types_get_an_empty_successful_answer() {
    for qtype in [15, 16, 2, 255] {
        let reply = answer(&query("shop.test", qtype, 1), &zone()).unwrap();
        assert_eq!(rcode(&reply), 0, "type {qtype}");
        assert_eq!(answers(&reply), 0, "type {qtype}");
    }
}

#[test]
fn nothing_outside_the_zone_is_answered_or_forwarded() {
    for name in [
        "example.com",
        "testing",
        "shop.test.com",
        "nottest",
        "localhost",
    ] {
        let reply = answer(&query(name, 1, 1), &zone()).unwrap();
        assert_eq!(rcode(&reply), RCODE_REFUSED, "{name}");
        assert_eq!(answers(&reply), 0, "{name}");
    }
    // A class other than IN is refused even inside the zone.
    let chaos = answer(&query("shop.test", 1, 3), &zone()).unwrap();
    assert_eq!(rcode(&chaos), RCODE_REFUSED);
}

#[test]
fn malformed_datagrams_are_dropped_or_rejected_and_never_panic() {
    assert!(answer(&[], &zone()).is_none());
    assert!(answer(&[1, 2, 3], &zone()).is_none());
    // A response is not answered (no loops between two responders).
    let mut response = query("shop.test", 1, 1);
    response[2] |= 0x80;
    assert!(answer(&response, &zone()).is_none());
    // A question cut short, a pointer in the question, a label that claims to be too long.
    let mut cut = query("shop.test", 1, 1);
    cut.truncate(cut.len() - 3);
    assert_eq!(rcode(&answer(&cut, &zone()).unwrap()), RCODE_FORMERR);
    let mut pointer = vec![0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xC0, 0x0C, 0, 1, 0, 1];
    assert_eq!(rcode(&answer(&pointer, &zone()).unwrap()), RCODE_FORMERR);
    pointer[12] = 200;
    assert_eq!(rcode(&answer(&pointer, &zone()).unwrap()), RCODE_FORMERR);
    // No question at all.
    let mut none = query("shop.test", 1, 1);
    none[5] = 0;
    assert_eq!(rcode(&answer(&none, &zone()).unwrap()), RCODE_FORMERR);
    // An opcode that is not a query.
    let mut status = query("shop.test", 1, 1);
    status[2] |= 2 << 3;
    assert_eq!(rcode(&answer(&status, &zone()).unwrap()), RCODE_REFUSED);
    // Every prefix of a good query is survivable.
    let good = query("shop.test", 1, 1);
    for n in 0..good.len() {
        let _ = answer(&good[..n], &zone());
    }
}

#[test]
fn only_special_use_suffixes_make_a_zone() {
    for ok in ["test", ".test", "LOCALHOST", "dev.test", "home.arpa"] {
        assert!(Zone::new(ok).is_ok(), "{ok}");
    }
    for bad in ["com", "dev", "app", "io"] {
        assert!(
            matches!(Zone::new(bad), Err(DnsError::NotSpecialUse(_))),
            "{bad}"
        );
    }
    for bad in ["", ".", "a b", "te..st", "-test", "tést", "x;y"] {
        assert!(
            matches!(Zone::new(bad), Err(DnsError::InvalidSuffix(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn the_server_answers_over_a_real_loopback_socket_and_stops() {
    let server = DnsServer::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), zone()).unwrap();
    let addr = server.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let handle = std::thread::spawn(move || server.run(&flag));

    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client.send_to(&query("shop.test", 1, 1), addr).unwrap();
    let mut buf = [0_u8; 512];
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(answers(&buf[..n]), 1);
    client.send_to(&query("example.com", 1, 1), addr).unwrap();
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(rcode(&buf[..n]), RCODE_REFUSED);

    stop.store(true, Ordering::Relaxed);
    assert_eq!(handle.join().unwrap(), 2);
}

#[test]
fn the_server_refuses_to_listen_off_loopback() {
    let any = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
    assert!(matches!(
        DnsServer::bind(any, zone()),
        Err(DnsError::NotLoopback(_))
    ));
    let lan = SocketAddr::from((Ipv4Addr::new(192, 168, 1, 5), 0));
    assert!(matches!(
        DnsServer::bind(lan, zone()),
        Err(DnsError::NotLoopback(_))
    ));
}

// ---- the rule ---------------------------------------------------------------------------

fn foreign_rule() -> NrptRule {
    NrptRule {
        namespace: ".test".to_owned(),
        name_server: "10.0.0.1".parse().unwrap(),
        comment: "corporate VPN".to_owned(),
    }
}

#[test]
fn enabling_adds_one_marked_rule_and_is_idempotent() {
    let table = FakeNrpt::new();
    assert_eq!(
        enable(&table, "test", LOOPBACK).unwrap(),
        EnableOutcome::Added
    );
    let rules = table.current();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].namespace, ".test");
    assert_eq!(rules[0].name_server, LOOPBACK);
    assert!(rules[0].is_ours());

    assert_eq!(
        enable(&table, ".TEST", LOOPBACK).unwrap(),
        EnableOutcome::AlreadyThere
    );
    assert_eq!(table.writes(), 1, "the second call wrote nothing");
}

#[test]
fn our_rule_pointing_elsewhere_is_updated_in_place() {
    let table = FakeNrpt::new();
    enable(&table, "test", LOOPBACK).unwrap();
    let other: IpAddr = "127.77.0.54".parse().unwrap();
    assert_eq!(
        enable(&table, "test", other).unwrap(),
        EnableOutcome::Updated
    );
    let rules = table.current();
    assert_eq!(rules.len(), 1, "no duplicate");
    assert_eq!(rules[0].name_server, other);
}

#[test]
fn a_rule_made_by_another_tool_is_reported_and_never_touched() {
    let table = FakeNrpt::with(vec![foreign_rule()]);
    let error = enable(&table, "test", LOOPBACK).unwrap_err();
    assert!(
        matches!(&error, DnsError::Foreign { namespace, comment, .. } if namespace == ".test" && comment == "corporate VPN"),
        "{error}"
    );
    assert!(error.to_string().contains("leaves it alone"));
    assert!(matches!(
        disable(&table, "test"),
        Err(DnsError::Foreign { .. })
    ));
    assert_eq!(table.current(), vec![foreign_rule()]);
    assert_eq!(table.writes(), 0);
}

#[test]
fn disabling_removes_only_our_rule_and_leaves_the_rest_as_it_was() {
    let other = NrptRule {
        namespace: ".corp.example".to_owned(),
        name_server: "10.0.0.1".parse().unwrap(),
        comment: String::new(),
    };
    let table = FakeNrpt::with(vec![other.clone()]);
    enable(&table, "test", LOOPBACK).unwrap();
    assert!(disable(&table, "test").unwrap());
    assert_eq!(table.current(), vec![other], "back to how it was");
    assert!(!disable(&table, "test").unwrap(), "nothing left to remove");
}

#[test]
fn a_server_that_is_not_loopback_or_a_public_suffix_is_refused_before_anything_is_written() {
    let table = FakeNrpt::new();
    for server in ["8.8.8.8", "192.168.1.1", "0.0.0.0", "2001:db8::1"] {
        let ip: IpAddr = server.parse().unwrap();
        assert!(
            matches!(enable(&table, "test", ip), Err(DnsError::NotLoopback(_))),
            "{server}"
        );
    }
    assert!(matches!(
        enable(&table, "com", LOOPBACK),
        Err(DnsError::NotSpecialUse(_))
    ));
    assert!(enable(&table, "test", IpAddr::V6(Ipv6Addr::LOCALHOST)).is_ok());
    assert_eq!(table.writes(), 1);
}

#[test]
fn a_failing_table_is_an_error_not_a_panic() {
    let table = FakeNrpt::new();
    table.fail_with("access denied");
    let error = enable(&table, "test", LOOPBACK).unwrap_err();
    assert!(matches!(error, DnsError::Policy(_)));
    assert!(error.to_string().contains("access denied"));
}

#[test]
fn error_messages_have_no_double_spaces() {
    let errors = [
        DnsError::InvalidSuffix("x".to_owned()),
        DnsError::NotSpecialUse("com".to_owned()),
        DnsError::NotLoopback("8.8.8.8".parse().unwrap()),
        DnsError::Foreign {
            namespace: ".test".to_owned(),
            server: LOOPBACK,
            comment: "x".to_owned(),
        },
    ];
    for error in errors {
        let text = error.to_string();
        assert!(!text.contains("  "), "{text}");
    }
}

// ---- the helper's arguments ---------------------------------------------------------------

#[test]
fn the_helper_accepts_exactly_two_operations() {
    let add = ElevatedOp::parse(&["nrpt-add", "test", "127.77.0.53"]).unwrap();
    assert_eq!(
        add,
        ElevatedOp::NrptAdd {
            suffix: "test".to_owned(),
            server: LOOPBACK
        }
    );
    assert_eq!(ElevatedOp::parse(&add.to_args()).unwrap(), add);
    let remove = ElevatedOp::parse(&["nrpt-remove", ".Test"]).unwrap();
    assert_eq!(
        remove,
        ElevatedOp::NrptRemove {
            suffix: "test".to_owned()
        }
    );
    assert_eq!(ElevatedOp::parse(&remove.to_args()).unwrap(), remove);

    for bad in [
        vec![],
        vec!["nrpt-add"],
        vec!["nrpt-add", "test"],
        vec!["nrpt-add", "com", "127.77.0.53"],
        vec!["nrpt-add", "test", "8.8.8.8"],
        vec!["nrpt-add", "test", "127.0.0.1; calc"],
        vec!["nrpt-add", "test'; calc; '", "127.77.0.53"],
        vec!["nrpt-remove", "test", "extra"],
        vec!["nrpt-remove", "$(calc)"],
        vec!["run", "cmd.exe"],
    ] {
        assert!(ElevatedOp::parse(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn applying_an_operation_follows_the_same_rules_as_enable_and_disable() {
    let table = FakeNrpt::with(vec![foreign_rule()]);
    let add = ElevatedOp::parse(&["nrpt-add", "test", "127.77.0.53"]).unwrap();
    let file = MemoryHosts::new("");
    let hosts = DirectHosts::new(&file);
    assert!(
        add.apply(&table, &hosts).is_err(),
        "the other tool's rule stands"
    );
    let free = FakeNrpt::new();
    add.apply(&free, &hosts).unwrap();
    ElevatedOp::parse(&["nrpt-remove", "test"])
        .unwrap()
        .apply(&free, &hosts)
        .unwrap();
    assert_eq!(free.current(), Vec::new());
}

// ---- reading what PowerShell prints ---------------------------------------------------------

#[test]
fn powershell_json_is_read_as_an_object_an_array_or_nothing() {
    assert_eq!(parse_nrpt_json("").unwrap(), Vec::new());
    assert_eq!(
        parse_nrpt_json(
            "  
"
        )
        .unwrap(),
        Vec::new()
    );

    let one = r#"{"Namespace":[".test"],"NameServers":"127.77.0.53","Comment":"Crisol"}"#;
    let rules = parse_nrpt_json(one).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].is_ours());
    assert_eq!(rules[0].name_server, LOOPBACK);

    let many = r#"[
        {"Namespace":[".a.example",".b.example"],"NameServers":["10.0.0.1"],"Comment":null},
        {"Namespace":".test","NameServers":"10.0.0.1,10.0.0.2","Comment":"x"},
        {"Namespace":".dns.example","NameServers":"dns.example.com"}
    ]"#;
    let rules = parse_nrpt_json(many).unwrap();
    assert_eq!(rules.len(), 4);
    assert_eq!(rules[1].namespace, ".b.example");
    assert_eq!(
        rules[2].name_server,
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        "two servers: not ours"
    );
    assert!(rules.iter().all(|r| !r.is_ours()));
    assert!(parse_nrpt_json("not json").is_err());

    // What Windows really printed for a rule made by Crisol: the server is an `IPAddress`
    // object (found in the hand test of 2026-10-16).
    let real = r#"{
        "Namespace":  [ ".test" ],
        "NameServers":  {
            "Address":  889212287,
            "AddressFamily":  2,
            "IsIPv6Multicast":  false,
            "IPAddressToString":  "127.77.0.53"
        },
        "Comment":  "Crisol"
    }"#;
    let rules = parse_nrpt_json(real).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].name_server, LOOPBACK);
    assert!(rules[0].is_ours());
    // The same inside a list of objects, and two servers are still "not ours alone".
    let two = r#"[{"Namespace":[".test"],"NameServers":[
        {"IPAddressToString":"10.0.0.1"},{"IPAddressToString":"10.0.0.2"}],"Comment":"x"}]"#;
    assert_eq!(
        parse_nrpt_json(two).unwrap()[0].name_server,
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    );
}

#[test]
fn the_helper_also_edits_the_hosts_block_and_only_that() {
    let set = ElevatedOp::parse(&["hosts-set", "b.test", "A.test", "a.test"]).unwrap();
    assert_eq!(
        set,
        ElevatedOp::HostsSet {
            names: vec!["a.test".to_owned(), "b.test".to_owned()]
        }
    );
    assert_eq!(ElevatedOp::parse(&set.to_args()).unwrap(), set);
    assert_eq!(
        ElevatedOp::parse(&["hosts-clear"]).unwrap(),
        ElevatedOp::HostsClear
    );
    for bad in [
        vec!["hosts-set"],
        vec!["hosts-set", "example.com"],
        vec!["hosts-set", "a.test", "evil.com"],
        vec!["hosts-set", "a.test;calc"],
        vec!["hosts-clear", "a.test"],
    ] {
        assert!(ElevatedOp::parse(&bad).is_err(), "{bad:?}");
    }

    let file = MemoryHosts::new(
        "127.0.0.1 localhost
",
    );
    let hosts = DirectHosts::new(&file);
    let table = FakeNrpt::new();
    set.apply(&table, &hosts).unwrap();
    assert!(file.content().contains("127.0.0.1 a.test"));
    assert_eq!(table.writes(), 0);
    ElevatedOp::HostsClear.apply(&table, &hosts).unwrap();
    assert_eq!(
        file.content(),
        "127.0.0.1 localhost
"
    );

    // A damaged file is an error from the helper, and stays as it was.
    let damaged = MemoryHosts::new(
        "# BEGIN CRISOL
",
    );
    let error = set.apply(&table, &DirectHosts::new(&damaged)).unwrap_err();
    assert!(matches!(error, DnsError::Hosts(_)), "{error}");
    assert_eq!(
        damaged.content(),
        "# BEGIN CRISOL
"
    );
}

// ---- review of phase 7, findings 5 to 7 --------------------------------------------------------

fn authority_count(reply: &[u8]) -> u16 {
    u16::from_be_bytes([reply[8], reply[9]])
}

#[test]
fn a_name_longer_than_253_characters_is_not_a_question() {
    let label = "a".repeat(60);
    let long = format!("{label}.{label}.{label}.{label}.{label}.test"); // 5 * 61 + 4 = 309
    assert_eq!(
        rcode(&answer(&query(&long, 1, 1), &zone()).unwrap()),
        RCODE_FORMERR
    );
    // The longest one that is still a name is answered.
    let fits = format!(
        "{}.{}.{}.{}.test",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(56)
    );
    assert_eq!(fits.len(), 253);
    assert_eq!(answers(&answer(&query(&fits, 1, 1), &zone()).unwrap()), 1);
    let one_more = format!(
        "{}.{}.{}.{}.test",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(57)
    );
    assert_eq!(
        rcode(&answer(&query(&one_more, 1, 1), &zone()).unwrap()),
        RCODE_FORMERR
    );
}

/// The authority section of a reply with one question: name, type, class, ttl and the fields
/// of the SOA, found by walking the bytes.
fn soa_of(reply: &[u8]) -> (String, u32) {
    // Skip the header and the question (name, type, class).
    let mut i = 12;
    while reply[i] != 0 {
        i += 1 + usize::from(reply[i]);
    }
    i += 1 + 4;
    let mut name = Vec::new();
    while reply[i] != 0 {
        let len = usize::from(reply[i]);
        name.push(String::from_utf8(reply[i + 1..=i + len].to_vec()).unwrap());
        i += 1 + len;
    }
    i += 1;
    assert_eq!(u16::from_be_bytes([reply[i], reply[i + 1]]), 6, "type SOA");
    assert_eq!(
        u16::from_be_bytes([reply[i + 2], reply[i + 3]]),
        1,
        "class IN"
    );
    let ttl = u32::from_be_bytes([reply[i + 4], reply[i + 5], reply[i + 6], reply[i + 7]]);
    let rdlength = usize::from(u16::from_be_bytes([reply[i + 8], reply[i + 9]]));
    let rdata = &reply[i + 10..];
    assert_eq!(rdata.len(), rdlength, "the record ends the message");
    // Two names, then five 32-bit numbers; the last one is the negative-caching time.
    let minimum = u32::from_be_bytes(rdata[rdata.len() - 4..].try_into().unwrap());
    assert_eq!(
        ttl, minimum,
        "the record is not kept longer than the negative time"
    );
    (name.join("."), minimum)
}

#[test]
fn an_answer_with_no_data_carries_the_zones_soa_and_the_others_do_not() {
    // HTTPS (65), as Chrome and Edge ask for every name; and MX.
    for qtype in [65, 15, 16] {
        let reply = answer(&query("api.shop.test", qtype, 1), &zone()).unwrap();
        assert_eq!(rcode(&reply), 0);
        assert_eq!(answers(&reply), 0);
        assert_eq!(authority_count(&reply), 1, "type {qtype}");
        let (apex, minimum) = soa_of(&reply);
        assert_eq!(apex, "test");
        assert!(minimum <= 60, "short, so removing a site is noticed");
    }
    // A and AAAA are real answers; refusals and malformed queries have no SOA either.
    for (reply, why) in [
        (answer(&query("shop.test", 1, 1), &zone()).unwrap(), "A"),
        (answer(&query("shop.test", 28, 1), &zone()).unwrap(), "AAAA"),
        (
            answer(&query("example.com", 15, 1), &zone()).unwrap(),
            "outside",
        ),
    ] {
        assert_eq!(authority_count(&reply), 0, "{why}");
    }
    // The zone never says a name does not exist (it answers for all of them), so no
    // NXDOMAIN is produced anywhere.
    for name in ["a.test", "deep.er.sub.test", "test"] {
        for qtype in [1, 28, 65, 255] {
            let reply = answer(&query(name, qtype, 1), &zone()).unwrap();
            assert_ne!(rcode(&reply), 3, "{name} {qtype}");
        }
    }
}

#[test]
fn the_helpers_arguments_fail_with_the_message_that_matches_the_problem() {
    let message = |args: &[&str]| ElevatedOp::parse(args).unwrap_err().to_string();
    assert!(message(&["nrpt-add", "test", "not-an-ip"]).contains("not an IP address"));
    assert!(message(&["nrpt-add", "test", "8.8.8.8"]).contains("not a loopback"));
    assert!(message(&["nrpt-add", "com", "127.77.0.53"]).contains("special-use"));
    assert!(message(&["hosts-set", "example.com"]).contains("cannot go in the hosts file"));
    let unknown = message(&["run", "cmd.exe"]);
    assert!(
        unknown.contains("only accepts") && unknown.contains("run cmd.exe"),
        "{unknown}"
    );
    for text in [&unknown, &message(&[]), &message(&["hosts-clear", "x"])] {
        assert!(
            !text.contains("suffix") || text.contains("nrpt-add <suffix>"),
            "{text}"
        );
        assert!(!text.contains("  "), "{text}");
    }
}
