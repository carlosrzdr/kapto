// SPDX-License-Identifier: Apache-2.0

//! The capture process's control protocol, over a real Unix socket.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use kapto_capture::control::{
    Announcement, ControlClient, ControlServer, NewEvent, PROTOCOL_VERSION, Request, Response,
    Status, StreamStatus,
};
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{Event, Phase};
use kapto_testkit::scratch;

const TIMEOUT: Duration = Duration::from_secs(5);

/// Enough announcements queued that a watcher connects before they are all
/// told.
const QUEUED: usize = 200_000;

/// Sends raw lines, says it has nothing more to send, and returns each
/// answer line until the server closes.
fn raw(socket: &PathBuf, lines: &[&str]) -> Vec<String> {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    for line in lines {
        stream.write_all(line.as_bytes()).unwrap();
    }
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    BufReader::new(stream).lines().map(Result::unwrap).collect()
}

/// Answers every request the server receives with `answer` until `done`.
fn answer_until(
    server: &ControlServer,
    done: &dyn Fn() -> bool,
    answer: &dyn Fn(&Request) -> Response,
) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() && Instant::now() < deadline {
        for pending in server.pending() {
            let response = answer(&pending.request);
            pending.answer(response);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_event_its_answer_and_its_announcement_are_json_as_documented() {
    let request: Request = serde_json::from_str(
        r#"{"command":"event","kind":"grasp","phase":"begin","id":"g1","source":"cli","text":"second try"}"#,
    )
    .unwrap();
    let Request::Event(asked) = request else {
        panic!("not an event")
    };
    assert_eq!(asked.at_utc_ns, None, "now, unless given");
    let event = NewEvent::clone(&asked).at(1_772_700_400_000_000_000);
    assert_eq!(
        (
            event.kind.as_str(),
            event.phase,
            event.id.as_deref(),
            event.at_utc_ns
        ),
        ("grasp", Phase::Begin, Some("g1"), 1_772_700_400_000_000_000)
    );
    // A moment given is kept; the phase left out is a moment.
    let Request::Event(given) =
        serde_json::from_str(r#"{"command":"event","kind":"note","source":"api","at_utc_ns":5}"#)
            .unwrap()
    else {
        panic!("not an event")
    };
    assert_eq!(
        given.clone().at(9),
        Event {
            at_utc_ns: 5,
            ..given.at(9)
        }
    );
    assert_eq!(
        serde_json::to_string(&Response::Noted {
            event: Box::new(event.clone()),
            episode: Some("e".to_owned()),
        })
        .unwrap(),
        r#"{"noted":{"event":{"kind":"grasp","at_utc_ns":1772700400000000000,"phase":"begin","id":"g1","source":"cli","text":"second try"},"episode":"e"}}"#
    );
    assert_eq!(
        serde_json::to_string(&Announcement::EventNoted {
            happened: Box::new(event),
            episode: None,
        })
        .unwrap(),
        r#"{"event":"event_noted","happened":{"kind":"grasp","at_utc_ns":1772700400000000000,"phase":"begin","id":"g1","source":"cli","text":"second try"},"episode":null}"#
    );
}

#[test]
fn requests_and_answers_are_one_line_of_json_each_as_documented() {
    let json = |request: &Request| serde_json::to_string(request).unwrap();
    assert_eq!(
        json(&Request::Hello { version: 1 }),
        r#"{"command":"hello","version":1}"#
    );
    assert_eq!(
        json(&Request::Start {
            duration_s: None,
            profile: None,
            task: None,
            waive: Vec::new(),
        }),
        r#"{"command":"start"}"#
    );
    assert_eq!(
        json(&Request::Start {
            duration_s: Some(30),
            profile: None,
            task: None,
            waive: Vec::new(),
        }),
        r#"{"command":"start","duration_s":30}"#
    );
    assert_eq!(json(&Request::Stop), r#"{"command":"stop"}"#);
    assert_eq!(
        serde_json::to_string(&Response::Started {
            episode: "e".into()
        })
        .unwrap(),
        r#"{"started":{"episode":"e"}}"#
    );
    let status = Response::Status(Status {
        episode: None,
        seconds: 3,
        streams: vec![StreamStatus {
            stream: "arm".into(),
            received: 5,
            ..StreamStatus::default()
        }],
        preflight: None,
    });
    let text = serde_json::to_string(&status).unwrap();
    assert_eq!(
        text,
        r#"{"status":{"episode":null,"seconds":3,"streams":[{"stream":"arm","received":5,"written":0,"dropped_by_us":0,"lost_upstream":0}]}}"#
    );
    assert_eq!(serde_json::from_str::<Response>(&text).unwrap(), status);
}

#[test]
fn hello_is_answered_by_the_connection_itself_with_both_versions() {
    let folder = scratch("hello");
    let socket = folder.join("c.sock");
    let _server = ControlServer::listen(&socket).unwrap();
    let answers = raw(&socket, &["{\"command\":\"hello\",\"version\":1}\n"]);
    assert_eq!(
        answers,
        [format!(
            r#"{{"hello":{{"version":{PROTOCOL_VERSION},"kapto":"{}"}}}}"#,
            env!("CARGO_PKG_VERSION")
        )]
    );
}

#[test]
fn a_client_of_another_protocol_version_is_refused_and_disconnected() {
    let folder = scratch("version");
    let socket = folder.join("c.sock");
    let _server = ControlServer::listen(&socket).unwrap();
    let answers = raw(
        &socket,
        &[
            "{\"command\":\"hello\",\"version\":2}\n",
            "{\"command\":\"status\"}\n",
        ],
    );
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert!(
        answers[0].starts_with(r#"{"error":{"code":"capture.control_version_mismatch","#),
        "{answers:?}"
    );
    assert!(answers[0].contains("version 2"), "{answers:?}");
}

#[test]
fn a_refused_client_reads_the_refusal_then_the_end_whatever_else_it_sent() {
    let folder = scratch("refused-end");
    let socket = folder.join("c.sock");
    let _server = ControlServer::listen(&socket).unwrap();
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    // More than the server reads before refusing, so some is never read.
    let mut sent = b"{\"command\":\"hello\",\"version\":2}\n".to_vec();
    sent.extend(std::iter::repeat_n(b'x', 64 * 1024));
    let _ = stream.write_all(&sent);
    let mut reader = BufReader::new(stream);
    let mut answer = String::new();
    reader.read_line(&mut answer).unwrap();
    assert!(
        answer.contains("capture.control_version_mismatch"),
        "{answer}"
    );
    let mut rest = Vec::new();
    // A clean end, not a reset.
    assert_eq!(
        reader.read_to_end(&mut rest).map_err(|error| error.kind()),
        Ok(0)
    );
}

#[test]
fn requests_before_hello_and_requests_that_arent_json_are_refused() {
    let folder = scratch("refused");
    let socket = folder.join("c.sock");
    let _server = ControlServer::listen(&socket).unwrap();
    for lines in [
        vec!["{\"command\":\"start\"}\n"],
        vec!["{\"command\":\"hello\",\"version\":1}\n", "start please\n"],
        vec![
            "{\"command\":\"hello\",\"version\":1}\n",
            "{\"command\":\"launch\"}\n",
        ],
    ] {
        let answers = raw(&socket, &lines);
        let last = answers.last().unwrap();
        assert!(
            last.starts_with(r#"{"error":{"code":"capture.control_invalid_request","#),
            "{lines:?} → {answers:?}"
        );
    }
}

#[test]
fn a_request_longer_than_the_limit_is_refused() {
    let folder = scratch("long");
    let socket = folder.join("c.sock");
    let _server = ControlServer::listen(&socket).unwrap();
    // One byte over: the server stops reading there, so nothing is left
    // unread to reset the connection before the answer is read.
    let long = "x".repeat(4097);
    let answers = raw(&socket, &["{\"command\":\"hello\",\"version\":1}\n", &long]);
    assert!(answers[1].contains("longer than 4096 bytes"), "{answers:?}");
}

#[test]
fn a_clients_requests_reach_the_owner_and_its_answers_and_refusals_come_back() {
    let folder = scratch("ask");
    let socket = folder.join("c.sock");
    let server = ControlServer::listen(&socket).unwrap();
    let client = thread::spawn({
        let socket = socket.clone();
        move || {
            let mut client = ControlClient::connect(&socket, TIMEOUT).unwrap();
            let status = client.ask(&Request::Status).unwrap();
            let refused = client
                .ask(&Request::Start {
                    duration_s: None,
                    profile: None,
                    task: None,
                    waive: Vec::new(),
                })
                .unwrap_err();
            (status, refused)
        }
    });
    answer_until(
        &server,
        &|| client.is_finished(),
        &|request| match request {
            Request::Status => Response::Status(Status::default()),
            _ => Response::error(
                &KaptoError::new(ErrorCode::CaptureAlreadyRecording).detail("episode e"),
            ),
        },
    );
    let (status, refused) = client.join().unwrap();
    assert_eq!(status, Response::Status(Status::default()));
    assert_eq!(refused.code(), ErrorCode::CaptureAlreadyRecording);
    assert_eq!(
        refused.message(),
        "An episode is already being recorded: episode e"
    );
}

#[test]
fn an_error_this_side_doesnt_know_is_an_unexpected_answer_not_capture_gone() {
    let folder = scratch("unknown-error");
    let socket = folder.join("c.sock");
    let server = ControlServer::listen(&socket).unwrap();
    let client = thread::spawn({
        let socket = socket.clone();
        move || {
            let mut client = ControlClient::connect(&socket, TIMEOUT).unwrap();
            client.ask(&Request::Status).unwrap_err()
        }
    });
    answer_until(&server, &|| client.is_finished(), &|_| Response::Error {
        code: "capture.from_a_later_release".to_owned(),
        message: "Something new".to_owned(),
    });
    let error = client.join().unwrap();
    assert_eq!(error.code(), ErrorCode::CaptureControlUnexpectedAnswer);
    assert!(
        error
            .message()
            .contains("capture.from_a_later_release: Something new"),
        "{}",
        error.message()
    );
}

/// Waits until `check` holds.
fn eventually(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !check() {
        assert!(Instant::now() < deadline, "never {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_supervisor_is_missed_only_once_its_last_connection_has_closed() {
    let folder = scratch("supervise");
    let socket = folder.join("c.sock");
    let server = ControlServer::listen(&socket).unwrap();
    // Run by hand, nobody supervises and nobody is missed.
    let mut client = ControlClient::connect(&socket, TIMEOUT).unwrap();
    assert_eq!(server.unsupervised_for(), None);
    drop(client);

    // Answered by the connection itself: nothing polls the server here.
    let mut first = ControlClient::connect(&socket, TIMEOUT).unwrap();
    assert_eq!(
        first.ask(&Request::Supervise).unwrap(),
        Response::Supervised
    );
    let mut second = ControlClient::connect(&socket, TIMEOUT).unwrap();
    assert_eq!(
        second.ask(&Request::Supervise).unwrap(),
        Response::Supervised
    );
    assert_eq!(
        second.ask(&Request::Supervise).unwrap(),
        Response::Supervised
    );
    drop(first);
    thread::sleep(Duration::from_millis(50));
    assert_eq!(server.unsupervised_for(), None, "one is still open");

    drop(second);
    eventually("missed", || server.unsupervised_for().is_some());
    let gone = server.unsupervised_for().unwrap();
    thread::sleep(Duration::from_millis(20));
    assert!(server.unsupervised_for().unwrap() > gone, "the time grows");

    client = ControlClient::connect(&socket, TIMEOUT).unwrap();
    assert_eq!(
        client.ask(&Request::Supervise).unwrap(),
        Response::Supervised
    );
    assert_eq!(server.unsupervised_for(), None, "back again");
}

#[test]
fn every_watcher_is_told_each_episode_starting_and_ending_as_it_happens() {
    let folder = scratch("watch");
    let socket = folder.join("c.sock");
    let server = ControlServer::listen(&socket).unwrap();
    // Nobody watching yet: told to nobody, and nothing waits. So many that
    // the announcing thread is still telling them when the watchers come,
    // which must still hear only what is announced after they watch.
    for _ in 0..QUEUED {
        server.announce(Announcement::EpisodeStarted {
            episode: "before".into(),
        });
    }
    let mut first = ControlClient::connect(&socket, TIMEOUT)
        .unwrap()
        .watch()
        .unwrap();
    let mut second = ControlClient::connect(&socket, TIMEOUT)
        .unwrap()
        .watch()
        .unwrap();
    // One that watched and went away is dropped, never holding up the rest.
    drop(
        ControlClient::connect(&socket, TIMEOUT)
            .unwrap()
            .watch()
            .unwrap(),
    );

    let started = Announcement::EpisodeStarted {
        episode: "e1".into(),
    };
    let ended = Announcement::EpisodeEnded {
        episode: "e1".into(),
        finalised: true,
    };
    server.announce(started.clone());
    server.announce(ended.clone());
    for watcher in [&mut first, &mut second] {
        assert_eq!(watcher.next(), Some(started.clone()));
        assert_eq!(watcher.next(), Some(ended.clone()));
    }
    // Answered by the connection itself: nothing polls the server here.
    assert_eq!(server.pending().count(), 0);
}

#[test]
fn an_episode_recovered_is_told_to_every_watcher_those_connecting_later_first() {
    let folder = scratch("watch-recovered");
    let socket = folder.join("c.sock");
    let server = ControlServer::listen(&socket).unwrap();
    let watch = || {
        ControlClient::connect(&socket, TIMEOUT)
            .unwrap()
            .watch()
            .unwrap()
    };
    let mut early = watch();
    let recovered = Announcement::EpisodeRecovered {
        episode: "r1".into(),
    };
    // Announced from the thread that recovers it.
    let announcer = server.announcer();
    thread::spawn({
        let recovered = recovered.clone();
        move || announcer.announce(recovered)
    })
    .join()
    .unwrap();
    let started = Announcement::EpisodeStarted {
        episode: "e1".into(),
    };
    server.announce(started.clone());
    assert_eq!(early.next(), Some(recovered.clone()));
    assert_eq!(early.next(), Some(started));

    // Told it first, then only what happens from then on.
    let mut late = watch();
    let ended = Announcement::EpisodeEnded {
        episode: "e1".into(),
        finalised: true,
    };
    server.announce(ended.clone());
    assert_eq!(late.next(), Some(recovered));
    assert_eq!(late.next(), Some(ended.clone()));
    // Told once to one already watching.
    assert_eq!(early.next(), Some(ended));
}

#[test]
fn an_episode_recovered_still_being_told_reaches_a_watcher_connecting_meanwhile_once() {
    let folder = scratch("watch-recovered-queued");
    let socket = folder.join("c.sock");
    let server = ControlServer::listen(&socket).unwrap();
    for _ in 0..QUEUED {
        server.announce(Announcement::EpisodeStarted {
            episode: "before".into(),
        });
    }
    let recovered = Announcement::EpisodeRecovered {
        episode: "r1".into(),
    };
    server.announce(recovered.clone());
    let mut watcher = ControlClient::connect(&socket, TIMEOUT)
        .unwrap()
        .watch()
        .unwrap();
    let started = Announcement::EpisodeStarted {
        episode: "e1".into(),
    };
    server.announce(started.clone());
    assert_eq!(watcher.next(), Some(recovered));
    assert_eq!(watcher.next(), Some(started));
}

#[test]
fn a_watch_is_one_line_answer_then_one_line_per_announcement_as_documented() {
    let folder = scratch("watch-raw");
    let socket = folder.join("c.sock");
    let server = ControlServer::listen(&socket).unwrap();
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream
        .write_all(b"{\"command\":\"hello\",\"version\":1}\n{\"command\":\"watch\"}\n")
        .unwrap();
    let mut lines = BufReader::new(stream).lines();
    assert!(lines.next().unwrap().unwrap().starts_with(r#"{"hello":"#));
    assert_eq!(lines.next().unwrap().unwrap(), r#""watching""#);
    server.announce(Announcement::EpisodeEnded {
        episode: "e1".into(),
        finalised: false,
    });
    assert_eq!(
        lines.next().unwrap().unwrap(),
        r#"{"announced":{"event":"episode_ended","episode":"e1","finalised":false}}"#
    );
    server.announce(Announcement::EpisodeRecovered {
        episode: "r1".into(),
    });
    assert_eq!(
        lines.next().unwrap().unwrap(),
        r#"{"announced":{"event":"episode_recovered","episode":"r1"}}"#
    );
}

#[test]
fn nothing_listening_is_reported_as_unreachable_naming_the_socket() {
    let folder = scratch("nobody");
    let socket = folder.join("c.sock");
    let Err(error) = ControlClient::connect(&socket, TIMEOUT) else {
        panic!("connected to nothing");
    };
    assert_eq!(error.code(), ErrorCode::CaptureControlUnreachable);
    assert!(error.report().contains("c.sock"), "{}", error.report());
}

#[test]
fn the_socket_replaces_a_stale_one_but_not_another_file_and_is_removed_at_the_end() {
    let folder = scratch("files");
    let socket = folder.join("c.sock");
    drop(ControlServer::listen(&socket).unwrap());
    assert!(!socket.exists(), "removed when the server stops");
    std::mem::forget(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(
        ControlServer::listen(&socket).is_ok(),
        "a stale socket is replaced"
    );

    let notes = folder.join("notes.txt");
    std::fs::write(&notes, "keep").unwrap();
    let Err(error) = ControlServer::listen(&notes) else {
        panic!("replaced a file");
    };
    assert_eq!(error.code(), ErrorCode::CaptureControlSocketFailed);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "keep");
}
