use std::net::TcpListener;
use std::sync::mpsc;
use std::thread::JoinHandle;

use super::*;

// ---- SCRAM-SHA-256: the test vector of RFC 7677 ------------------------------------------

#[test]
fn the_scram_exchange_of_rfc_7677_comes_out_as_in_the_rfc() {
    let mut client = ScramClient::new("user", "pencil", "rOprNGfwEbeRWgbNEkqO");
    assert_eq!(client.first(), "n,,n=user,r=rOprNGfwEbeRWgbNEkqO");
    let server_first =
        "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    assert_eq!(
        client.final_message(server_first).unwrap(),
        "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,\
         p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
    );
    client
        .verify_server("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=")
        .unwrap();
    assert!(matches!(
        client.verify_server("v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        Err(MongoError::Auth(_))
    ));
}

#[test]
fn a_server_that_does_not_keep_our_nonce_or_asks_for_too_little_work_is_refused() {
    let mut client = ScramClient::new("user", "pencil", "abc");
    assert!(client.final_message("r=zzz,s=AAAA,i=4096").is_err());
    assert!(client.final_message("r=abcdef,s=AAAA,i=2").is_err());
    assert!(client.final_message("r=abcdef,s=%%%,i=4096").is_err());
    assert!(client.final_message("garbage").is_err());
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[test]
fn hmac_and_pbkdf2_match_their_standard_vectors() {
    // RFC 4231, test case 2.
    let mac = hmac(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
        hex(&mac),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // RFC 7914, PBKDF2-HMAC-SHA256, P = "passwd", S = "salt", c = 1.
    let key = pbkdf2(b"passwd", b"salt", 1);
    assert_eq!(
        hex(&key),
        "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
    );
}

// ---- BSON and OP_MSG ---------------------------------------------------------------------

#[test]
fn a_document_survives_being_written_and_read() {
    let bytes = encode_document(&[
        ("shutdown", Value::Int32(1)),
        ("name", Value::Str("root")),
        ("force", Value::Bool(true)),
        ("ratio", Value::Double(1.0)),
        ("blob", Value::Binary(b"abc")),
        (
            "roles",
            Value::Array(vec![Value::Doc(vec![("role", Value::Str("root"))])]),
        ),
    ]);
    let back = decode_document(&bytes).unwrap();
    assert_eq!(field(&back, "shutdown"), Some(&Bson::Int32(1)));
    assert_eq!(field(&back, "name"), Some(&Bson::Str("root".to_owned())));
    assert_eq!(field(&back, "force"), Some(&Bson::Bool(true)));
    assert_eq!(field(&back, "ratio"), Some(&Bson::Double(1.0)));
    assert_eq!(field(&back, "blob"), Some(&Bson::Binary(b"abc".to_vec())));
    assert_eq!(
        field(&back, "roles"),
        Some(&Bson::Array(vec![Bson::Doc(vec![(
            "role".to_owned(),
            Bson::Str("root".to_owned())
        )])]))
    );
    // The empty document is five bytes.
    assert_eq!(encode_document(&[]), [5, 0, 0, 0, 0]);
}

#[test]
fn a_truncated_or_lying_document_is_an_error_not_a_panic() {
    let bytes = encode_document(&[("a", Value::Str("hello"))]);
    for cut in 0..bytes.len() {
        assert!(decode_document(&bytes[..cut]).is_err(), "cut at {cut}");
    }
    let mut lying = bytes.clone();
    lying[0] = 0xff;
    assert!(decode_document(&lying).is_err());
}

#[test]
fn an_op_msg_has_the_header_the_server_expects() {
    let document = encode_document(&[("hello", Value::Int32(1))]);
    let message = op_msg(7, &document);
    let int = |at: usize| i32::from_le_bytes(message[at..at + 4].try_into().unwrap());
    assert_eq!(int(0), i32::try_from(message.len()).unwrap());
    assert_eq!((int(4), int(8), int(12)), (7, 0, 2013));
    assert_eq!(&message[16..20], &[0, 0, 0, 0], "no flags");
    assert_eq!(message[20], 0, "a section of one document");
    assert_eq!(&message[21..], &document[..]);
    // And it reads back as the document.
    let back = read_op_msg(&mut &message[..]).unwrap();
    assert_eq!(field(&back, "hello"), Some(&Bson::Int32(1)));
    // A message of another operation is refused.
    let mut other = message.clone();
    other[12] = 1;
    assert!(read_op_msg(&mut &other[..]).is_err());
}

// ---- a MongoDB that only knows what is needed --------------------------------------------

/// What the fake server saw.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Command(String),
    UserCreated { name: String, pwd: String },
    Shutdown,
}

#[allow(clippy::too_many_lines)] // one fake server, read top to bottom
fn fake_server(password: &'static str) -> (u16, mpsc::Receiver<Seen>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let salt = b"0123456789abcdef";
        let salted = pbkdf2(password.as_bytes(), salt, 4096);
        let stored = Sha256::digest(hmac(&salted, b"Client Key"));
        let mut combined = String::new();
        let mut first_bare = String::new();
        let mut server_first = String::new();
        let mut authenticated = false;
        let mut id = 100;
        let mut reply = |stream: &mut TcpStream, fields: &[(&str, Value<'_>)]| {
            id += 1;
            stream
                .write_all(&op_msg(id, &encode_document(fields)))
                .unwrap();
        };
        while let Ok(request) = read_op_msg(&mut stream) {
            let name = request[0].0.clone();
            let _ = tx.send(Seen::Command(name.clone()));
            match name.as_str() {
                "saslStart" => {
                    let Some(Bson::Binary(payload)) = field(&request, "payload") else {
                        return;
                    };
                    let client_first = String::from_utf8_lossy(payload).into_owned();
                    first_bare = client_first.trim_start_matches("n,,").to_owned();
                    let nonce = first_bare.split("r=").nth(1).unwrap().to_owned();
                    combined = format!("{nonce}SERVERPART");
                    server_first = format!("r={combined},s={},i=4096", STANDARD.encode(salt));
                    reply(
                        &mut stream,
                        &[
                            ("conversationId", Value::Int32(1)),
                            ("done", Value::Bool(false)),
                            ("payload", Value::Binary(server_first.as_bytes())),
                            ("ok", Value::Double(1.0)),
                        ],
                    );
                }
                "saslContinue" => {
                    let Some(Bson::Binary(payload)) = field(&request, "payload") else {
                        return;
                    };
                    let client_final = String::from_utf8_lossy(payload).into_owned();
                    let proof = client_final.split(",p=").nth(1).unwrap_or_default();
                    let without_proof = client_final.split(",p=").next().unwrap().to_owned();
                    let auth_message = format!("{first_bare},{server_first},{without_proof}");
                    let signature = hmac(&stored, auth_message.as_bytes());
                    let proof = STANDARD.decode(proof).unwrap_or_default();
                    let client_key: Vec<u8> =
                        proof.iter().zip(signature).map(|(a, b)| a ^ b).collect();
                    if Sha256::digest(&client_key)[..] == stored[..]
                        && without_proof.contains(&combined)
                    {
                        authenticated = true;
                        let server_key = hmac(&salted, b"Server Key");
                        let v = format!(
                            "v={}",
                            STANDARD.encode(hmac(&server_key, auth_message.as_bytes()))
                        );
                        reply(
                            &mut stream,
                            &[
                                ("conversationId", Value::Int32(1)),
                                ("done", Value::Bool(true)),
                                ("payload", Value::Binary(v.as_bytes())),
                                ("ok", Value::Double(1.0)),
                            ],
                        );
                    } else {
                        reply(
                            &mut stream,
                            &[
                                ("ok", Value::Double(0.0)),
                                ("errmsg", Value::Str("Authentication failed.")),
                                ("code", Value::Int32(18)),
                            ],
                        );
                    }
                }
                "createUser" => {
                    let user = match field(&request, "createUser") {
                        Some(Bson::Str(s)) => s.clone(),
                        _ => String::new(),
                    };
                    let pwd = match field(&request, "pwd") {
                        Some(Bson::Str(s)) => s.clone(),
                        _ => String::new(),
                    };
                    let _ = tx.send(Seen::UserCreated { name: user, pwd });
                    reply(&mut stream, &[("ok", Value::Double(1.0))]);
                }
                "shutdown" if authenticated => {
                    let _ = tx.send(Seen::Shutdown);
                    return; // the server goes without an answer
                }
                _ => reply(
                    &mut stream,
                    &[
                        ("ok", Value::Double(0.0)),
                        ("errmsg", Value::Str("command requires authentication")),
                        ("code", Value::Int32(13)),
                    ],
                ),
            }
        }
    });
    (port, rx, handle)
}

#[test]
fn the_first_user_is_made_with_the_password_given() {
    let (port, seen, server) = fake_server("unused");
    create_root_user(port, "s3cretPassw0rd").unwrap();
    server.join().unwrap();
    let all: Vec<Seen> = seen.try_iter().collect();
    assert!(all.contains(&Seen::UserCreated {
        name: "root".to_owned(),
        pwd: "s3cretPassw0rd".to_owned()
    }));
}

#[test]
fn shutdown_logs_in_first_and_a_server_that_goes_without_answering_is_a_success() {
    let (port, seen, server) = fake_server("the-password");
    shutdown(port, "the-password").unwrap();
    server.join().unwrap();
    let commands: Vec<String> = seen
        .try_iter()
        .filter_map(|s| match s {
            Seen::Command(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(commands, ["saslStart", "saslContinue", "shutdown"]);
}

#[test]
fn a_wrong_password_never_reaches_shutdown() {
    let (port, seen, server) = fake_server("the-password");
    let error = shutdown(port, "not-the-password").unwrap_err();
    assert!(matches!(error, MongoError::Auth(_)), "{error}");
    drop(server);
    assert!(!seen.try_iter().any(|s| s == Seen::Shutdown));
}

#[test]
fn a_refused_command_says_which_and_why() {
    let (port, _seen, _server) = fake_server("x");
    let mut wire = Wire::new(connect(port, Duration::from_secs(5)).unwrap());
    match wire.command(&[
        ("listDatabases", Value::Int32(1)),
        ("$db", Value::Str("admin")),
    ]) {
        Err(MongoError::Server {
            command,
            code,
            message,
        }) => {
            assert_eq!((command.as_str(), code), ("listDatabases", 13));
            assert!(message.contains("authentication"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}
