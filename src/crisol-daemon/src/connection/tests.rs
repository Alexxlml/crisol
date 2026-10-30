// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]

use std::sync::mpsc;
use std::time::Duration;

use crisol_protocol::{DoneInfo, Event, Reply};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::*;

/// A connection over an in-memory stream with a very small buffer, an actor that answers every
/// request at once, and the handles to drive it.
struct World {
    client: tokio::io::DuplexStream,
    serving: tokio::task::JoinHandle<()>,
    events: Bus,
    closing: watch::Sender<bool>,
}

fn world(buffer: usize) -> World {
    let (client, server) = tokio::io::duplex(buffer);
    let (actor, inbox) = mpsc::channel::<Input>();
    std::thread::spawn(move || {
        for input in inbox {
            if let Input::Request(msg) = input {
                let _ = msg.reply.send(Ok(Reply::Done(DoneInfo {
                    message: "ok".to_owned(),
                    changed: false,
                    warnings: Vec::new(),
                })));
            }
        }
    });
    let events = Bus::new(256);
    let (closing_tx, closing) = watch::channel(false);
    let shared = Shared {
        actor,
        events: events.clone(),
        closing,
    };
    World {
        client,
        serving: tokio::spawn(serve(server, shared)),
        events,
        closing: closing_tx,
    }
}

fn request(id: u64, method: &str) -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\"}}\n")
}

#[tokio::test]
async fn a_client_that_never_reads_its_answers_loses_its_connection_not_the_daemons_memory() {
    let World {
        client, serving, ..
    } = world(64);
    let (_read, mut write) = tokio::io::split(client);
    // Far more requests than the queue holds, and nobody reads a single answer.
    let feeder = tokio::spawn(async move {
        for id in 0..(OUT_CAPACITY as u64 * 4) {
            if write
                .write_all(request(id, "sites").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("the connection was never closed")
        .unwrap();
    feeder.abort();
}

#[tokio::test]
async fn a_subscriber_that_cannot_keep_up_loses_events_and_keeps_its_connection() {
    let World {
        client,
        serving,
        events,
        closing,
    } = world(64);
    let (read, mut write) = tokio::io::split(client);
    let mut reader = BufReader::new(read);
    write
        .write_all(request(1, "subscribe").as_bytes())
        .await
        .unwrap();
    let mut first = String::new();
    reader.read_line(&mut first).await.unwrap();
    assert!(first.contains("\"id\":1"), "{first}");

    // Many more events than the queue holds, while nobody reads.
    let flood = OUT_CAPACITY * 5;
    for _ in 0..flood {
        events.send(Event::Reloaded);
        tokio::task::yield_now().await;
    }
    // Still there: a request is answered, behind however many events were kept.
    write
        .write_all(request(7, "sites").as_bytes())
        .await
        .unwrap();
    let mut kept = 0_usize;
    loop {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
            .await
            .expect("no answer")
            .unwrap();
        assert!(read > 0, "the connection was closed");
        if line.contains("\"id\":7") {
            break;
        }
        kept += 1;
    }
    assert!(kept > 0, "some events arrive");
    assert!(
        kept <= OUT_CAPACITY + 64,
        "{kept} events were kept of {flood}: the queue is bounded"
    );
    let _ = closing.send(true);
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn leaving_delivers_the_answer_and_the_last_event_before_the_connection_ends() {
    let World {
        client,
        serving,
        events,
        closing,
    } = world(8192);
    let (read, mut write) = tokio::io::split(client);
    let mut reader = BufReader::new(read);
    write
        .write_all(request(1, "subscribe").as_bytes())
        .await
        .unwrap();
    write
        .write_all(request(2, "shutdown").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.contains("\"id\":2"), "the answer to shutdown: {line}");

    // The last thing the daemon says, and then it leaves.
    events.send(Event::ShuttingDown);
    let _ = closing.send(true);
    let mut rest = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut next = String::new();
            if reader.read_line(&mut next).await.unwrap() == 0 {
                break;
            }
            rest.push_str(&next);
        }
    })
    .await
    .expect("the connection never ended");
    assert_eq!(rest.lines().count(), 1, "{rest}");
    assert!(rest.to_lowercase().contains("shutting"), "{rest}");
    serving.await.unwrap();
}
