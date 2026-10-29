// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]

use super::*;

fn round_trip<T>(value: &T) -> T
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    let line = to_line(value).unwrap();
    assert!(
        line.ends_with('\n') && line.matches('\n').count() == 1,
        "{line:?}"
    );
    from_line(&line).unwrap()
}

#[test]
fn a_request_is_json_rpc_with_method_and_params() {
    let request = Request::new(
        7,
        Call::Install {
            service: "php".to_owned(),
            version: "8.4".to_owned(),
            from: None,
            sha256: None,
        },
    );
    assert_eq!(
        to_line(&request).unwrap(),
        "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"install\",\"params\":{\"service\":\"php\",\"version\":\"8.4\"}}\n"
    );
    assert_eq!(round_trip(&request), request);

    // A call with no parameters has no `params`.
    let shutdown = Request::new(1, Call::Shutdown);
    assert_eq!(
        to_line(&shutdown).unwrap(),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\"}\n"
    );
    assert_eq!(round_trip(&shutdown), shutdown);
}

#[test]
fn every_call_survives_a_round_trip_and_names_itself() {
    let calls = [
        Call::Hello {
            client: "test".to_owned(),
            protocol: PROTOCOL_VERSION,
        },
        Call::Status { service: None },
        Call::Status {
            service: Some("nginx".to_owned()),
        },
        Call::Start { service: None },
        Call::Stop {
            service: Some("php-8.4.26".to_owned()),
        },
        Call::Restart { service: None },
        Call::Shutdown,
        Call::Install {
            service: "node".to_owned(),
            version: "latest".to_owned(),
            from: None,
            sha256: None,
        },
        Call::Uninstall {
            service: "php".to_owned(),
            version: "7.4.33".to_owned(),
        },
        Call::List { available: true },
        Call::Use {
            tool: Tool::Php,
            version: "8.4".to_owned(),
            dir: Some("D:/shop".to_owned()),
        },
        Call::Use {
            tool: Tool::Node,
            version: "22".to_owned(),
            dir: None,
        },
        Call::Park {
            path: "D:/Proyectos".to_owned(),
        },
        Call::Unpark {
            path: "D:/Proyectos".to_owned(),
        },
        Call::Link {
            name: "shop".to_owned(),
            path: "D:/shop".to_owned(),
            docroot: Some("public".to_owned()),
        },
        Call::Unlink {
            name: "shop".to_owned(),
        },
        Call::Sites,
        Call::Secure {
            name: "shop".to_owned(),
        },
        Call::Unsecure {
            name: "shop".to_owned(),
        },
        Call::Logs {
            service: "nginx".to_owned(),
            lines: 20,
            from: Some(1234),
        },
        Call::Doctor,
        Call::Credentials {
            service: "mysql".to_owned(),
        },
        Call::Enable {
            service: "redis".to_owned(),
        },
        Call::Disable {
            service: "redis".to_owned(),
        },
        Call::ResetPassword {
            service: "mysql".to_owned(),
        },
        Call::Subscribe,
    ];
    let mut names = std::collections::BTreeSet::new();
    for (i, call) in calls.into_iter().enumerate() {
        let request = Request::new(i as u64, call.clone());
        assert_eq!(round_trip(&request).call, call);
        let json: serde_json::Value = serde_json::from_str(&to_line(&request).unwrap()).unwrap();
        assert_eq!(json["method"], call.method());
        names.insert(call.method());
    }
    assert_eq!(
        names.len(),
        24,
        "every operation has its own name: {names:?}"
    );
}

#[test]
fn optional_parameters_may_be_left_out() {
    let status: Request =
        from_line(r#"{"jsonrpc":"2.0","id":3,"method":"status","params":{}}"#).unwrap();
    assert_eq!(status.call, Call::Status { service: None });
    let logs: Request =
        from_line(r#"{"jsonrpc":"2.0","id":4,"method":"logs","params":{"service":"nginx"}}"#)
            .unwrap();
    assert_eq!(
        logs.call,
        Call::Logs {
            service: "nginx".to_owned(),
            lines: 50,
            from: None
        }
    );
    // A required one may not.
    assert!(from_line::<Request>(
        r#"{"jsonrpc":"2.0","id":5,"method":"install","params":{"service":"php"}}"#
    )
    .is_err());
}

#[test]
fn an_unknown_method_or_junk_is_an_error_not_a_panic() {
    for junk in [
        "",
        "not json",
        "{}",
        r#"{"jsonrpc":"2.0","id":1,"method":"format_disk"}"#,
        r#"{"jsonrpc":"2.0","id":"one","method":"sites"}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"use","params":{"tool":"ruby","version":"3"}}"#,
        "\u{0}\u{1}",
    ] {
        assert!(from_line::<Request>(junk).is_err(), "{junk:?}");
    }
}

#[test]
fn a_line_longer_than_the_limit_is_refused_before_it_is_read() {
    let line = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"logs\",\"params\":{{\"service\":\"{}\"}}}}",
        "a".repeat(MAX_LINE)
    );
    assert!(matches!(
        from_line::<Request>(&line),
        Err(LineError::TooLong)
    ));
}

#[test]
fn a_response_carries_a_result_or_an_error_never_both() {
    let ok = Response::ok(
        2,
        Reply::Done(DoneInfo {
            message: "nginx started".to_owned(),
            changed: true,
            warnings: vec![],
        }),
    );
    let json = to_line(&ok).unwrap();
    assert!(json.contains("\"result\":{\"type\":\"done\""), "{json}");
    assert!(!json.contains("\"error\""), "{json}");
    assert_eq!(round_trip(&ok), ok);
    assert!(matches!(ok.into_result(), Ok(Reply::Done(_))));

    let failed = Response::failed(
        Some(2),
        RpcError::new(ErrorCode::HomeExposed, "C:/Crisol is open").with_fix("icacls ..."),
    );
    let json = to_line(&failed).unwrap();
    assert!(json.contains("\"error\":{\"code\":4,"), "{json}");
    assert!(!json.contains("\"result\""), "{json}");
    let back: Response = round_trip(&failed);
    let error = back.into_result().unwrap_err();
    assert_eq!(error.code, ErrorCode::HomeExposed);
    assert_eq!(error.fix.as_deref(), Some("icacls ..."));
    assert_eq!(error.to_string(), "C:/Crisol is open");

    let empty = Response {
        jsonrpc: JSONRPC.to_owned(),
        id: Some(1),
        result: None,
        error: None,
    };
    assert_eq!(
        empty.into_result().unwrap_err().code,
        ErrorCode::InvalidRequest
    );
}

#[test]
fn the_error_codes_are_stable_numbers_and_unknown_ones_are_refused() {
    let all = [
        (ErrorCode::ParseError, -32700),
        (ErrorCode::InvalidRequest, -32600),
        (ErrorCode::MethodNotFound, -32601),
        (ErrorCode::InvalidParams, -32602),
        (ErrorCode::Internal, -32603),
        (ErrorCode::UnsupportedProtocol, 1),
        (ErrorCode::NotFound, 2),
        (ErrorCode::Conflict, 3),
        (ErrorCode::HomeExposed, 4),
        (ErrorCode::Io, 5),
        (ErrorCode::NotAvailable, 6),
    ];
    for (code, number) in all {
        assert_eq!(code.number(), number);
        assert_eq!(ErrorCode::try_from(number).unwrap(), code);
        assert_eq!(serde_json::to_string(&code).unwrap(), number.to_string());
    }
    assert!(ErrorCode::try_from(99).is_err());
    assert!(serde_json::from_str::<ErrorCode>("99").is_err());
}

#[test]
fn replies_ignore_fields_they_do_not_know_so_the_daemon_can_add_some() {
    let newer = r#"{"type":"status","services":[{"name":"nginx","state":"running","pid":1234}],"warnings":[],"uptime":9}"#;
    let reply: Reply = serde_json::from_str(newer).unwrap();
    assert_eq!(
        reply,
        Reply::Status(StatusInfo {
            services: vec![ServiceInfo {
                name: "nginx".to_owned(),
                state: "running".to_owned(),
                detail: None
            }],
            warnings: vec![]
        })
    );
}

#[test]
fn events_are_notifications_with_no_id_and_every_kind_round_trips() {
    let events = [
        Event::ServiceState {
            service: "nginx".to_owned(),
            state: "failed".to_owned(),
            detail: Some("port 80 is already in use".to_owned()),
        },
        Event::ReloadStarted,
        Event::Reloaded,
        Event::ReloadFailed {
            reason: "nginx rejected it".to_owned(),
        },
        Event::Blocked {
            reason: "open".to_owned(),
        },
        Event::Log {
            service: "nginx".to_owned(),
            line: "started".to_owned(),
        },
        Event::Progress {
            what: "php 8.4.26".to_owned(),
            done: 10,
            total: Some(20),
        },
        Event::ShuttingDown,
    ];
    for event in events {
        let note = Notification::new(event.clone());
        let line = to_line(&note).unwrap();
        assert!(
            line.starts_with("{\"jsonrpc\":\"2.0\",\"method\":\"event\",\"params\":{\"type\":"),
            "{line}"
        );
        assert!(!line.contains("\"id\""), "{line}");
        assert_eq!(round_trip(&note).params, event);
        // On a connection a reader does not know in advance which of the two it will get.
        match from_line::<Message>(&line).unwrap() {
            Message::Notification(n) => assert_eq!(n.params, event),
            Message::Response(r) => panic!("{r:?}"),
        }
    }
    let response = to_line(&Response::ok(1, Reply::Doctor { checks: vec![] })).unwrap();
    assert!(matches!(
        from_line::<Message>(&response).unwrap(),
        Message::Response(_)
    ));
}

#[test]
fn only_the_calls_about_running_services_need_the_daemon() {
    let needs: Vec<&str> = [
        Call::Status { service: None },
        Call::Start { service: None },
        Call::Stop { service: None },
        Call::Restart { service: None },
        Call::Shutdown,
        Call::Subscribe,
        Call::Hello {
            client: String::new(),
            protocol: 1,
        },
    ]
    .iter()
    .filter(|c| c.needs_daemon())
    .map(Call::method)
    .collect();
    assert_eq!(needs.len(), 7);
    for call in [
        Call::Sites,
        Call::Doctor,
        Call::List { available: false },
        Call::Link {
            name: "a".to_owned(),
            path: "D:/a".to_owned(),
            docroot: None,
        },
        Call::Logs {
            service: "nginx".to_owned(),
            lines: 1,
            from: None,
        },
    ] {
        assert!(!call.needs_daemon(), "{}", call.method());
    }
}

#[test]
fn version_is_not_empty() {
    assert_ne!(VERSION, "");
    assert_eq!(PROTOCOL_VERSION, 3);
}

// ---- the client over plain buffers -------------------------------------------------------

mod client {
    use std::io::Cursor;

    use super::*;

    /// A client whose "daemon" has already written `incoming`; what it sends is kept.
    fn over(incoming: &str) -> Client<Cursor<Vec<u8>>, Vec<u8>> {
        Client::new(Cursor::new(incoming.as_bytes().to_vec()), Vec::new())
    }

    fn line<T: Serialize>(value: &T) -> String {
        to_line(value).unwrap()
    }

    fn done(message: &str) -> Reply {
        Reply::Done(DoneInfo {
            message: message.to_owned(),
            changed: true,
            warnings: vec![],
        })
    }

    #[test]
    fn a_call_sends_one_request_and_returns_the_reply_with_its_id() {
        let mut client = over(&line(&Response::ok(1, done("ok"))));
        let reply = client.call(&Call::Sites).unwrap();
        assert_eq!(reply, done("ok"));
        let sent = String::from_utf8(client.writer().clone()).unwrap();
        assert_eq!(
            sent,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"sites\"}\n"
        );
    }

    #[test]
    fn ids_go_up_and_events_in_between_go_to_the_callback() {
        let incoming = format!(
            "{}{}{}{}",
            line(&Response::ok(1, done("first"))),
            line(&Notification::new(Event::Reloaded)),
            line(&Notification::new(Event::ReloadStarted)),
            line(&Response::ok(2, done("second"))),
        );
        let mut client = over(&incoming);
        assert_eq!(client.call(&Call::Sites).unwrap(), done("first"));
        let mut seen = Vec::new();
        let reply = client
            .call_with_events(&Call::Doctor, &mut |e| seen.push(e))
            .unwrap();
        assert_eq!(reply, done("second"));
        assert_eq!(seen, [Event::Reloaded, Event::ReloadStarted]);
        assert!(String::from_utf8(client.writer().clone())
            .unwrap()
            .contains("\"id\":2"));
    }

    #[test]
    fn an_error_response_is_an_rpc_error_with_its_code_and_fix() {
        let failed = Response::failed(
            Some(1),
            RpcError::new(ErrorCode::HomeExposed, "open").with_fix("icacls ..."),
        );
        let mut client = over(&line(&failed));
        match client.call(&Call::Sites).unwrap_err() {
            ClientError::Rpc(error) => {
                assert_eq!(error.code, ErrorCode::HomeExposed);
                assert_eq!(error.fix.as_deref(), Some("icacls ..."));
            }
            other => panic!("{other}"),
        }
        // A daemon that could not read the request answers with no id.
        let unreadable = Response::failed(None, RpcError::new(ErrorCode::ParseError, "bad"));
        let mut client = over(&line(&unreadable));
        assert!(matches!(
            client.call(&Call::Sites),
            Err(ClientError::Rpc(e)) if e.code == ErrorCode::ParseError
        ));
    }

    #[test]
    fn a_closed_stream_a_wrong_id_and_junk_are_errors_not_hangs() {
        assert!(matches!(
            over("").call(&Call::Sites),
            Err(ClientError::Closed)
        ));
        assert!(matches!(
            over(&line(&Response::ok(9, done("x")))).call(&Call::Sites),
            Err(ClientError::Protocol(_))
        ));
        assert!(matches!(
            over("not json\n").call(&Call::Sites),
            Err(ClientError::Protocol(_))
        ));
        // Half a message and then silence.
        assert!(matches!(
            over("{\"jsonrpc\":").call(&Call::Sites),
            Err(ClientError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn subscribers_read_events_until_the_daemon_hangs_up() {
        let incoming = format!(
            "{}{}",
            line(&Notification::new(Event::Reloaded)),
            line(&Notification::new(Event::ShuttingDown)),
        );
        let mut client = over(&incoming);
        assert_eq!(client.next_event().unwrap(), Some(Event::Reloaded));
        assert_eq!(client.next_event().unwrap(), Some(Event::ShuttingDown));
        assert_eq!(client.next_event().unwrap(), None);
    }

    #[test]
    fn a_line_without_end_is_never_read_past_the_limit() {
        let huge = "a".repeat(MAX_LINE + 10);
        let mut reader = Cursor::new(huge.into_bytes());
        let error = read_bounded_line(&mut reader).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let mut ok = Cursor::new(b"one\ntwo\n".to_vec());
        assert_eq!(read_bounded_line(&mut ok).unwrap().as_deref(), Some("one"));
        assert_eq!(read_bounded_line(&mut ok).unwrap().as_deref(), Some("two"));
        assert_eq!(read_bounded_line(&mut ok).unwrap(), None);
    }
}
