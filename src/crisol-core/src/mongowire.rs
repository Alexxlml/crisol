//! Just enough of MongoDB's wire protocol to make its first user and to shut it down cleanly
//! (review of phase 9, finding 3; ADR-0016), with no `mongosh` (a 100 MB download) and no new
//! dependency.
//!
//! Three things, all over TCP to `127.0.0.1`:
//!
//! - **`OP_MSG`**: a message is a 16-byte header, four bytes of flags, one section of kind 0 and
//!   a BSON document ([`Wire::command`]);
//! - **BSON** for the few commands and replies involved ([`Value`], [`Bson`]);
//! - **SCRAM-SHA-256**, the login MongoDB uses, written from RFC 5802/7677 with the `sha2`
//!   crate already in the build ([`ScramClient`]; checked against the RFC 7677 test vector).
//!
//! What it is used for: [`create_root_user`] makes the administrator (with the password Crisol
//! generated) through the *localhost exception* the first time the server runs, and [`shutdown`]
//! logs in as that user and sends the `shutdown` command, so a stop is an ordered one and not a
//! crash of the process.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

/// `OP_MSG`'s operation code.
const OP_MSG: i32 = 2013;

/// The biggest message accepted from the server: the replies read here are a few hundred bytes.
const MAX_MESSAGE: usize = 1 << 20;

/// What went wrong talking to MongoDB.
#[derive(Debug, thiserror::Error)]
pub enum MongoError {
    /// The connection failed or was cut.
    #[error("connection to MongoDB: {0}")]
    Io(#[from] io::Error),
    /// The server said something that is not the protocol.
    #[error("unexpected MongoDB message: {0}")]
    Protocol(String),
    /// The login failed.
    #[error("MongoDB login failed: {0}")]
    Auth(String),
    /// A command was refused.
    #[error("MongoDB refused `{command}` (code {code}): {message}")]
    Server {
        /// The command.
        command: String,
        /// MongoDB's error code.
        code: i32,
        /// Its message.
        message: String,
    },
}

/// A BSON value to send.
#[derive(Debug, Clone)]
pub enum Value<'a> {
    /// A double.
    Double(f64),
    /// A string.
    Str(&'a str),
    /// A 32-bit integer.
    Int32(i32),
    /// A boolean.
    Bool(bool),
    /// Bytes.
    Binary(&'a [u8]),
    /// A document.
    Doc(Vec<(&'a str, Value<'a>)>),
    /// An array.
    Array(Vec<Value<'a>>),
}

fn put_cstring(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(text.as_bytes());
    out.push(0);
}

fn put_string(out: &mut Vec<u8>, text: &str) {
    let length = i32::try_from(text.len() + 1).unwrap_or(i32::MAX);
    out.extend_from_slice(&length.to_le_bytes());
    put_cstring(out, text);
}

fn encode_value(out: &mut Vec<u8>, name: &str, value: &Value<'_>) {
    let tag = match value {
        Value::Double(_) => 0x01,
        Value::Str(_) => 0x02,
        Value::Doc(_) => 0x03,
        Value::Array(_) => 0x04,
        Value::Binary(_) => 0x05,
        Value::Bool(_) => 0x08,
        Value::Int32(_) => 0x10,
    };
    out.push(tag);
    put_cstring(out, name);
    match value {
        Value::Double(number) => out.extend_from_slice(&number.to_le_bytes()),
        Value::Str(text) => put_string(out, text),
        Value::Doc(fields) => out.extend(encode_document(fields)),
        Value::Array(items) => {
            let numbered: Vec<(String, &Value<'_>)> = items
                .iter()
                .enumerate()
                .map(|(i, item)| (i.to_string(), item))
                .collect();
            let mut body = Vec::new();
            for (key, item) in &numbered {
                encode_value(&mut body, key, item);
            }
            body.push(0);
            let length = i32::try_from(body.len() + 4).unwrap_or(i32::MAX);
            out.extend_from_slice(&length.to_le_bytes());
            out.extend(body);
        }
        Value::Binary(bytes) => {
            out.extend_from_slice(&i32::try_from(bytes.len()).unwrap_or(i32::MAX).to_le_bytes());
            out.push(0); // generic subtype
            out.extend_from_slice(bytes);
        }
        Value::Bool(flag) => out.push(u8::from(*flag)),
        Value::Int32(number) => out.extend_from_slice(&number.to_le_bytes()),
    }
}

/// A BSON document from its fields, in order.
#[must_use]
pub fn encode_document(fields: &[(&str, Value<'_>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        encode_value(&mut body, name, value);
    }
    body.push(0);
    let length = i32::try_from(body.len() + 4).unwrap_or(i32::MAX);
    let mut out = length.to_le_bytes().to_vec();
    out.extend(body);
    out
}

/// A BSON value received.
#[derive(Debug, Clone, PartialEq)]
pub enum Bson {
    /// A double (MongoDB's `ok`).
    Double(f64),
    /// A string.
    Str(String),
    /// A document.
    Doc(Vec<(String, Bson)>),
    /// An array.
    Array(Vec<Bson>),
    /// Bytes.
    Binary(Vec<u8>),
    /// A boolean.
    Bool(bool),
    /// A 32-bit integer.
    Int32(i32),
    /// A 64-bit integer.
    Int64(i64),
    /// Anything else, skipped.
    Other,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], MongoError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| MongoError::Protocol("a document ends too soon".to_owned()))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn i32(&mut self) -> Result<i32, MongoError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn cstring(&mut self) -> Result<String, MongoError> {
        let rest = &self.bytes[self.at..];
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| MongoError::Protocol("a name has no end".to_owned()))?;
        let text = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.at += end + 1;
        Ok(text)
    }

    fn string(&mut self) -> Result<String, MongoError> {
        let length = usize::try_from(self.i32()?)
            .map_err(|_| MongoError::Protocol("a negative string length".to_owned()))?;
        let raw = self.take(length)?;
        Ok(String::from_utf8_lossy(&raw[..raw.len().saturating_sub(1)]).into_owned())
    }

    fn document(&mut self) -> Result<Vec<(String, Bson)>, MongoError> {
        let start = self.at;
        let length = usize::try_from(self.i32()?)
            .map_err(|_| MongoError::Protocol("a negative document length".to_owned()))?;
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len() && length >= 5)
            .ok_or_else(|| MongoError::Protocol("a document has a bad length".to_owned()))?;
        let mut fields = Vec::new();
        while self.at < end - 1 {
            let tag = self.take(1)?[0];
            let name = self.cstring()?;
            let value = match tag {
                0x01 => {
                    let b = self.take(8)?;
                    Bson::Double(f64::from_le_bytes([
                        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                    ]))
                }
                0x02 => Bson::Str(self.string()?),
                0x03 => Bson::Doc(self.document()?),
                0x04 => Bson::Array(self.document()?.into_iter().map(|(_, v)| v).collect()),
                0x05 => {
                    let length = usize::try_from(self.i32()?)
                        .map_err(|_| MongoError::Protocol("a negative binary".to_owned()))?;
                    let _subtype = self.take(1)?;
                    Bson::Binary(self.take(length)?.to_vec())
                }
                0x08 => Bson::Bool(self.take(1)?[0] != 0),
                0x0a => Bson::Other,
                0x10 => Bson::Int32(self.i32()?),
                0x12 => {
                    let b = self.take(8)?;
                    Bson::Int64(i64::from_le_bytes([
                        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                    ]))
                }
                0x09 => {
                    self.take(8)?;
                    Bson::Other
                }
                other => {
                    return Err(MongoError::Protocol(format!(
                        "a BSON type this client does not read ({other:#x}) in `{name}`"
                    )))
                }
            };
            fields.push((name, value));
        }
        self.at = end;
        Ok(fields)
    }
}

/// Reads a BSON document.
pub fn decode_document(bytes: &[u8]) -> Result<Vec<(String, Bson)>, MongoError> {
    Cursor { bytes, at: 0 }.document()
}

/// A field of a decoded document.
#[must_use]
pub fn field<'a>(document: &'a [(String, Bson)], name: &str) -> Option<&'a Bson> {
    document.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

/// An `OP_MSG` carrying `document`.
#[must_use]
pub fn op_msg(request_id: i32, document: &[u8]) -> Vec<u8> {
    let length = i32::try_from(16 + 4 + 1 + document.len()).unwrap_or(i32::MAX);
    let mut out = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&request_id.to_le_bytes());
    out.extend_from_slice(&0_i32.to_le_bytes()); // responseTo
    out.extend_from_slice(&OP_MSG.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes()); // flagBits
    out.push(0); // section kind 0: one document
    out.extend_from_slice(document);
    out
}

/// Reads one `OP_MSG` and returns the document of its first section.
pub fn read_op_msg(stream: &mut impl Read) -> Result<Vec<(String, Bson)>, MongoError> {
    let mut header = [0_u8; 16];
    stream.read_exact(&mut header)?;
    let length = usize::try_from(i32::from_le_bytes([
        header[0], header[1], header[2], header[3],
    ]))
    .map_err(|_| MongoError::Protocol("a negative message length".to_owned()))?;
    let code = i32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    if code != OP_MSG {
        return Err(MongoError::Protocol(format!(
            "operation code {code}, not OP_MSG"
        )));
    }
    if !(16 + 4 + 1 + 5..=MAX_MESSAGE).contains(&length) {
        return Err(MongoError::Protocol(format!("a message of {length} bytes")));
    }
    let mut body = vec![0_u8; length - 16];
    stream.read_exact(&mut body)?;
    let flags = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
    if body[4] != 0 {
        return Err(MongoError::Protocol(
            "a section that is not a document".to_owned(),
        ));
    }
    // Bit 0 says a checksum follows the sections.
    let end = if flags & 1 == 1 {
        body.len() - 4
    } else {
        body.len()
    };
    decode_document(&body[5..end])
}

// ---- SCRAM-SHA-256 ---------------------------------------------------------------------

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0_u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut first = salt.to_vec();
    first.extend_from_slice(&1_u32.to_be_bytes());
    let mut u = hmac(password, &first);
    let mut result = u;
    for _ in 1..iterations {
        u = hmac(password, &u);
        for (r, b) in result.iter_mut().zip(u) {
            *r ^= b;
        }
    }
    result
}

/// The client side of a SCRAM-SHA-256 conversation.
#[derive(Debug)]
pub struct ScramClient {
    user: String,
    password: String,
    nonce: String,
    first_bare: String,
    expected_server_signature: Option<[u8; 32]>,
}

impl ScramClient {
    /// A conversation for `user`, with a nonce chosen by the caller.
    #[must_use]
    pub fn new(user: &str, password: &str, nonce: &str) -> Self {
        let escaped = user.replace('=', "=3D").replace(',', "=2C");
        Self {
            user: user.to_owned(),
            password: password.to_owned(),
            nonce: nonce.to_owned(),
            first_bare: format!("n={escaped},r={nonce}"),
            expected_server_signature: None,
        }
    }

    /// The first message.
    #[must_use]
    pub fn first(&self) -> String {
        format!("n,,{}", self.first_bare)
    }

    /// The answer to the server's first message (`r=…,s=…,i=…`): the final message with the
    /// proof.
    pub fn final_message(&mut self, server_first: &str) -> Result<String, MongoError> {
        let mut combined = None;
        let mut salt = None;
        let mut iterations = None;
        for part in server_first.split(',') {
            match part.split_once('=') {
                Some(("r", v)) => combined = Some(v),
                Some(("s", v)) => salt = Some(v),
                Some(("i", v)) => iterations = v.parse::<u32>().ok(),
                _ => {}
            }
        }
        let (Some(combined), Some(salt), Some(iterations)) = (combined, salt, iterations) else {
            return Err(MongoError::Auth(
                "the server's first message is malformed".to_owned(),
            ));
        };
        if !combined.starts_with(&self.nonce) {
            return Err(MongoError::Auth(
                "the server did not keep our nonce".to_owned(),
            ));
        }
        if !(4096..=1_000_000).contains(&iterations) {
            return Err(MongoError::Auth(format!(
                "a strange iteration count ({iterations})"
            )));
        }
        let salt = STANDARD
            .decode(salt)
            .map_err(|_| MongoError::Auth("the salt is not base64".to_owned()))?;
        let salted = pbkdf2(self.password.as_bytes(), &salt, iterations);
        let client_key = hmac(&salted, b"Client Key");
        let stored_key = Sha256::digest(client_key);
        let without_proof = format!("c=biws,r={combined}");
        let auth_message = format!("{},{server_first},{without_proof}", self.first_bare);
        let signature = hmac(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(signature)
            .map(|(a, b)| a ^ b)
            .collect();
        let server_key = hmac(&salted, b"Server Key");
        self.expected_server_signature = Some(hmac(&server_key, auth_message.as_bytes()));
        Ok(format!("{without_proof},p={}", STANDARD.encode(proof)))
    }

    /// Checks the server's last message (`v=…`): the server knows the password too.
    pub fn verify_server(&self, server_final: &str) -> Result<(), MongoError> {
        let expected = self
            .expected_server_signature
            .ok_or_else(|| MongoError::Auth("the proof was never sent".to_owned()))?;
        match server_final.split(',').find_map(|p| p.strip_prefix("v=")) {
            Some(v) if STANDARD.decode(v).ok().as_deref() == Some(&expected[..]) => Ok(()),
            Some(_) => Err(MongoError::Auth(
                "the server's signature is wrong".to_owned(),
            )),
            None => Err(MongoError::Auth(format!(
                "the server answered {server_final:?} instead of its signature"
            ))),
        }
    }

    /// The user this conversation is for.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }
}

// ---- a connection ------------------------------------------------------------------------

/// A connection to a MongoDB server.
#[derive(Debug)]
pub struct Wire<S> {
    stream: S,
    next_id: i32,
}

impl<S: Read + Write> Wire<S> {
    /// Talks over `stream`.
    #[must_use]
    pub fn new(stream: S) -> Self {
        Self { stream, next_id: 1 }
    }

    /// Sends one command (`fields` in order, the command first, `$db` included) and returns the
    /// reply document. A refusal (`ok: 0`) is an error.
    pub fn command(
        &mut self,
        fields: &[(&str, Value<'_>)],
    ) -> Result<Vec<(String, Bson)>, MongoError> {
        let name = fields.first().map_or("", |(n, _)| *n).to_owned();
        self.stream
            .write_all(&op_msg(self.next_id, &encode_document(fields)))?;
        self.next_id += 1;
        self.stream.flush()?;
        let reply = read_op_msg(&mut self.stream)?;
        let ok = match field(&reply, "ok") {
            Some(Bson::Double(v)) => *v >= 1.0,
            Some(Bson::Int32(v)) => *v == 1,
            Some(Bson::Int64(v)) => *v == 1,
            _ => false,
        };
        if ok {
            return Ok(reply);
        }
        let code = match field(&reply, "code") {
            Some(Bson::Int32(c)) => *c,
            _ => 0,
        };
        let message = match field(&reply, "errmsg") {
            Some(Bson::Str(m)) => m.clone(),
            _ => "no message".to_owned(),
        };
        Err(MongoError::Server {
            command: name,
            code,
            message,
        })
    }

    /// Logs in to `admin` as `user` with SCRAM-SHA-256.
    pub fn authenticate(
        &mut self,
        user: &str,
        password: &str,
        nonce: &str,
    ) -> Result<(), MongoError> {
        let mut scram = ScramClient::new(user, password, nonce);
        let first = scram.first();
        let started = self
            .command(&[
                ("saslStart", Value::Int32(1)),
                ("mechanism", Value::Str("SCRAM-SHA-256")),
                ("payload", Value::Binary(first.as_bytes())),
                ("autoAuthorize", Value::Int32(1)),
                (
                    "options",
                    Value::Doc(vec![("skipEmptyExchange", Value::Bool(true))]),
                ),
                ("$db", Value::Str("admin")),
            ])
            .map_err(auth_error)?;
        let conversation = match field(&started, "conversationId") {
            Some(Bson::Int32(id)) => *id,
            _ => return Err(MongoError::Auth("no conversation id".to_owned())),
        };
        let payload = |reply: &[(String, Bson)]| match field(reply, "payload") {
            Some(Bson::Binary(bytes)) => Ok(String::from_utf8_lossy(bytes).into_owned()),
            _ => Err(MongoError::Auth("no payload".to_owned())),
        };
        let proof = scram.final_message(&payload(&started)?)?;
        let answered = self
            .command(&[
                ("saslContinue", Value::Int32(1)),
                ("conversationId", Value::Int32(conversation)),
                ("payload", Value::Binary(proof.as_bytes())),
                ("$db", Value::Str("admin")),
            ])
            .map_err(auth_error)?;
        scram.verify_server(&payload(&answered)?)?;
        if field(&answered, "done") != Some(&Bson::Bool(true)) {
            // Without the shortcut there is one empty exchange more.
            self.command(&[
                ("saslContinue", Value::Int32(1)),
                ("conversationId", Value::Int32(conversation)),
                ("payload", Value::Binary(&[])),
                ("$db", Value::Str("admin")),
            ])
            .map_err(auth_error)?;
        }
        Ok(())
    }
}

fn auth_error(error: MongoError) -> MongoError {
    match error {
        MongoError::Server { message, .. } => MongoError::Auth(message),
        other => other,
    }
}

/// A random nonce for a login.
fn fresh_nonce() -> Result<String, MongoError> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| MongoError::Protocol(e.to_string()))?;
    Ok(STANDARD.encode(bytes))
}

fn connect(port: u16, timeout: Duration) -> Result<TcpStream, MongoError> {
    let stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_secs(2),
    )?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}

/// The name of the administrator Crisol makes.
pub const ADMIN_USER: &str = "root";

/// Makes the administrator through the *localhost exception*: MongoDB lets a connection from
/// this computer make the first user of an empty server, and refuses once there is one (so a
/// second call is an error, and nothing is overwritten).
pub fn create_root_user(port: u16, password: &str) -> Result<(), MongoError> {
    let mut wire = Wire::new(connect(port, Duration::from_secs(10))?);
    wire.command(&[
        ("createUser", Value::Str(ADMIN_USER)),
        ("pwd", Value::Str(password)),
        (
            "roles",
            Value::Array(vec![Value::Doc(vec![
                ("role", Value::Str("root")),
                ("db", Value::Str("admin")),
            ])]),
        ),
        ("$db", Value::Str("admin")),
    ])
    .map(drop)
}

/// Logs in as the administrator and sends `shutdown`. The server closes the connection as it
/// goes, which is the answer.
pub fn shutdown(port: u16, password: &str) -> Result<(), MongoError> {
    let mut wire = Wire::new(connect(port, Duration::from_secs(30))?);
    wire.authenticate(ADMIN_USER, password, &fresh_nonce()?)?;
    send_shutdown(&mut wire)
}

/// Sends `shutdown` to a server that asks for no login (one made before Crisol generated
/// passwords).
pub fn shutdown_without_login(port: u16) -> Result<(), MongoError> {
    let mut wire = Wire::new(connect(port, Duration::from_secs(30))?);
    send_shutdown(&mut wire)
}

fn send_shutdown(wire: &mut Wire<TcpStream>) -> Result<(), MongoError> {
    match wire.command(&[
        ("shutdown", Value::Int32(1)),
        ("timeoutSecs", Value::Int32(10)),
        ("$db", Value::Str("admin")),
    ]) {
        // A server that is going down does not always answer.
        Ok(_) => Ok(()),
        Err(MongoError::Io(e))
            if matches!(
                e.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
            ) =>
        {
            Ok(())
        }
        Err(other) => Err(other),
    }
}

/// Asks for the list of databases with no login: what any program on this computer can try. A
/// server with its login on answers with the error "requires authentication" (code 13).
pub fn list_databases_without_login(port: u16) -> Result<(), MongoError> {
    let mut wire = Wire::new(connect(port, Duration::from_secs(10))?);
    wire.command(&[
        ("listDatabases", Value::Int32(1)),
        ("$db", Value::Str("admin")),
    ])
    .map(drop)
}

#[cfg(test)]
mod tests;
