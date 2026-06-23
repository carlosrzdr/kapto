// SPDX-License-Identifier: Apache-2.0

//! `kapto capture` as the server will run it: a long-running process that
//! receives all the time and records episodes on command.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::io::{BufRead, BufReader};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::free_udp_port;
use kapto_capture::control::{Announcement, ControlClient, NewEvent, Request, Response, Status};
use kapto_capture::reader::read_events;
use kapto_core::ErrorCode;
use kapto_store::{Episode, Event, Phase, verify_episode};
use kapto_testkit::scratch;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The capture process under test, and a robot sending it numbered
/// messages.
struct Capture {
    /// Taken by [`Capture::stop`]; killed if a test ends without it.
    child: Option<Child>,
    folder: PathBuf,
    robot: UdpSocket,
    port: String,
    next: u32,
}

impl Capture {
    /// Starts `kapto capture` on one stream and waits until it receives.
    fn start(folder: &Path) -> Self {
        Self::start_with(folder, &[])
    }

    /// The same, with more flags. Pre-flight's failed checks don't refuse
    /// a start unless the flags say `--preflight refuse`: the robot is
    /// silent until a test sends.
    fn start_with(folder: &Path, flags: &[&str]) -> Self {
        let warn: &[&str] = if flags.contains(&"--preflight") {
            &[]
        } else {
            &["--preflight", "warn"]
        };
        let port = free_udp_port();
        let streams = format!(
            "[streams.joints]\nrate_hz = 1000\nudp = {{ bind = \"127.0.0.1:{port}\" }}\n\
             sequence = {{ offset = 0, type = \"u32\" }}\n\
             [streams.joints.decoder]\nkind = \"struct\"\n\
             [[streams.joints.decoder.fields]]\nname = \"sequence\"\ntype = \"u32\"\noffset = 0\n"
        );
        Self::declaring(folder, &streams, &port, &[warn, flags].concat())
    }

    /// Starts `kapto capture` on the streams `declared`, the robot sending
    /// to UDP `port`, and waits until it receives.
    fn declaring(folder: &Path, declared: &str, port: &str, flags: &[&str]) -> Self {
        let streams = folder.join("streams.toml");
        std::fs::write(&streams, declared).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_kapto"))
            .args(["capture", "--streams"])
            .arg(&streams)
            .arg("--data-dir")
            .arg(folder.join("data"))
            .args(flags)
            .env_clear()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        loop {
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("kapto capture exited early"));
            if line.unwrap().contains(" streams, not recording.") {
                break;
            }
        }
        // Keep reading stderr so the process never blocks on a full pipe.
        std::thread::spawn(move || lines.for_each(drop));
        Self {
            child: Some(child),
            folder: folder.to_path_buf(),
            robot: UdpSocket::bind("127.0.0.1:0").unwrap(),
            port: port.to_owned(),
            next: 0,
        }
    }

    fn client(&self) -> ControlClient {
        let socket = self.folder.join("data/capture.sock");
        ControlClient::connect(&socket, TIMEOUT).unwrap()
    }

    /// Sends `count` more numbered messages.
    fn send(&mut self, count: u32) {
        for _ in 0..count {
            let mut message = [0_u8; 32];
            message[..4].copy_from_slice(&self.next.to_le_bytes());
            self.robot
                .send_to(&message, format!("127.0.0.1:{}", self.port))
                .unwrap();
            self.next += 1;
            // Paced like a robot, so no socket buffer overflows.
            if self.next.is_multiple_of(50) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// Leaves `count` numbers out, as messages lost on the way.
    fn skip(&mut self, count: u32) {
        self.next += count;
    }

    /// Waits until `status` holds for the capture process's status.
    fn wait_for(&self, what: &str, holds: impl Fn(&Status) -> bool) -> Status {
        let mut client = self.client();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let Response::Status(status) = client.ask(&Request::Status).unwrap() else {
                panic!("not a status");
            };
            if holds(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{what}: {status:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Sends SIGTERM, as a supervisor or service manager would, and waits
    /// for the process to exit.
    fn stop(mut self) -> Output {
        let child = self.child.take().unwrap();
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The finalised episode in `folder`, checked against its manifest.
fn finalised(folder: &str) -> Episode {
    let folder = Path::new(folder);
    verify_episode(folder).unwrap();
    serde_json::from_str(&std::fs::read_to_string(folder.join("episode.json")).unwrap()).unwrap()
}

fn start(client: &mut ControlClient) -> String {
    match client
        .ask(&Request::Start {
            duration_s: None,
            profile: None,
            task: None,
            waive: Vec::new(),
        })
        .unwrap()
    {
        Response::Started { episode } => episode,
        other => panic!("{other:?}"),
    }
}

fn stop(client: &mut ControlClient) -> String {
    match client.ask(&Request::Stop).unwrap() {
        Response::Stopped { folder, .. } => folder,
        other => panic!("{other:?}"),
    }
}

#[test]
fn each_episode_holds_exactly_the_messages_received_while_it_was_recorded() {
    let folder = scratch("episodes");
    let mut capture = Capture::start(&folder);
    let mut client = capture.client();

    capture.send(100);
    let idle = capture.wait_for("idle receiving", |s| s.streams[0].received == 100);
    assert_eq!((idle.episode, idle.streams[0].written), (None, 0));

    let first = start(&mut client);
    capture.send(500);
    capture.wait_for("first episode", |s| s.streams[0].written == 500);
    let first_folder = stop(&mut client);
    assert!(first_folder.ends_with(&first), "{first_folder}");

    capture.send(50);
    capture.wait_for("idle again", |s| {
        s.episode.is_none() && s.streams[0].received == 50
    });
    let second = start(&mut client);
    assert_ne!(first, second);
    capture.send(300);
    capture.wait_for("second episode", |s| s.streams[0].written == 300);
    let second_folder = stop(&mut client);

    for (folder, messages) in [(&first_folder, 500), (&second_folder, 300)] {
        let stream = &finalised(folder).streams[0];
        assert_eq!(
            (
                stream.received,
                stream.written,
                stream.dropped_by_us,
                stream.lost_upstream
            ),
            (messages, messages, 0, 0),
            "{folder}"
        );
    }
    assert!(capture.stop().status.success());
}

#[test]
fn an_episode_starts_with_the_last_second_before_it_was_started() {
    let folder = scratch("pre-roll");
    let port = free_udp_port();
    let streams = format!(
        "[streams.joints]\nrate_hz = 1000\npre_roll_s = 1\nudp = {{ bind = \"127.0.0.1:{port}\" }}\n\
         sequence = {{ offset = 0, type = \"u32\" }}\n"
    );
    let mut capture = Capture::declaring(&folder, &streams, &port, &["--preflight", "warn"]);
    let mut client = capture.client();

    // Numbers 0 to 299 more than a second before the start, 300 to 399
    // just before it.
    capture.send(300);
    capture.wait_for("the first messages", |s| s.streams[0].received == 300);
    std::thread::sleep(Duration::from_millis(1500));
    capture.send(100);
    capture.wait_for("the last ones before the start", |s| {
        s.streams[0].received == 400
    });
    start(&mut client);
    capture.send(200);
    capture.wait_for("the episode", |s| s.streams[0].written == 300);
    let folder = stop(&mut client);

    let bytes = std::fs::read(Path::new(&folder).join("recording.mcap")).unwrap();
    let messages: Vec<(u32, u64)> = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .filter(|message| message.channel.topic == "joints")
        .map(|message| {
            (
                u32::from_le_bytes(message.data[..4].try_into().unwrap()),
                message.log_time,
            )
        })
        .collect();
    let numbers: Vec<u32> = messages.iter().map(|(number, _)| *number).collect();
    assert_eq!(numbers, (300..600).collect::<Vec<_>>());
    // Each at the time it was received, in order; the episode starts with
    // its first.
    assert!(messages.windows(2).all(|pair| pair[0].1 <= pair[1].1));
    let episode = finalised(&folder);
    assert_eq!(
        episode.started_utc_ns,
        i64::try_from(messages[0].1).unwrap()
    );
    let stream = &episode.streams[0];
    assert_eq!(
        (
            stream.received,
            stream.written,
            stream.dropped_by_us,
            stream.lost_upstream
        ),
        (300, 300, 0, 0)
    );
    // 300 to 399 came before Start was asked, which is kept beside.
    assert_eq!(stream.pre_roll, Some(100));
    assert_eq!(stream.post_roll, None);
    let commanded = episode.commanded_start_utc_ns.unwrap();
    assert!(i64::try_from(messages[99].1).unwrap() < commanded);
    assert!(commanded <= i64::try_from(messages[100].1).unwrap());
    let said = capture.stop();
    assert!(said.status.success());
}

#[test]
fn loss_in_the_seconds_before_the_start_counts_in_the_episode_and_earlier_loss_does_not() {
    let folder = scratch("pre-roll-loss");
    let port = free_udp_port();
    let streams = format!(
        "[streams.joints]\nrate_hz = 1000\npre_roll_s = 1\nudp = {{ bind = \"127.0.0.1:{port}\" }}\n\
         sequence = {{ offset = 0, type = \"u32\" }}\n"
    );
    let mut capture = Capture::declaring(&folder, &streams, &port, &["--preflight", "warn"]);
    let mut client = capture.client();

    // 5 lost more than a second before the start, then 10 just before it.
    capture.send(100);
    capture.skip(5);
    capture.send(100);
    capture.wait_for("the first messages", |s| s.streams[0].received == 200);
    std::thread::sleep(Duration::from_millis(1500));
    capture.send(50);
    capture.skip(10);
    capture.send(50);
    capture.wait_for("the last ones before the start", |s| {
        s.streams[0].received == 300
    });
    start(&mut client);
    capture.send(100);
    capture.wait_for("the episode", |s| s.streams[0].written == 200);
    let folder = stop(&mut client);

    let stream = &finalised(&folder).streams[0];
    assert_eq!(
        (stream.received, stream.written, stream.lost_upstream),
        (200, 200, 10)
    );
    assert!(capture.stop().status.success());
}

#[test]
fn with_a_pre_roll_every_stream_starts_when_the_start_was_asked_however_long_it_waits() {
    let folder = scratch("start-waits");
    let (joints, wrist) = (free_udp_port(), free_udp_port());
    let streams = format!(
        "[streams.joints]\nrate_hz = 1000\npre_roll_s = 1\nudp = {{ bind = \"127.0.0.1:{joints}\" }}\n\
         [streams.wrist]\nrate_hz = 1000\nudp = {{ bind = \"127.0.0.1:{wrist}\" }}\n\
         sequence = {{ offset = 0, type = \"u32\" }}\n"
    );
    // Pre-flight watches the streams for 3 s once capture starts, and a
    // start asked meanwhile waits for it.
    let flags = ["--preflight", "warn", "--preflight-window", "3"];
    let capture = Capture::declaring(&folder, &streams, &joints, &flags);
    let mut client = capture.client();
    let starting = std::thread::spawn(move || start(&mut client));
    // Once the start is noted, at the next tenth of a second, the wrist
    // sends while it waits; it has no pre-roll of its own.
    std::thread::sleep(Duration::from_millis(500));
    let robot = UdpSocket::bind("127.0.0.1:0").unwrap();
    let send = |numbers: std::ops::Range<u32>| {
        for number in numbers {
            let mut message = [0_u8; 32];
            message[..4].copy_from_slice(&number.to_le_bytes());
            robot
                .send_to(&message, format!("127.0.0.1:{wrist}"))
                .unwrap();
        }
    };
    send(0..100);
    capture.wait_for("the wrist while the start waits", |s| {
        s.streams[1].received == 100
    });
    assert!(
        !starting.is_finished(),
        "the start didn't wait for pre-flight"
    );
    starting.join().unwrap();
    send(100..200);
    capture.wait_for("the episode", |s| s.streams[1].written == 200);
    let folder = stop(&mut capture.client());

    let bytes = std::fs::read(Path::new(&folder).join("recording.mcap")).unwrap();
    let numbers: Vec<u32> = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .filter(|message| message.channel.topic == "wrist")
        .map(|message| u32::from_le_bytes(message.data[..4].try_into().unwrap()))
        .collect();
    assert_eq!(numbers, (0..200).collect::<Vec<_>>());
    let stream = &finalised(&folder).streams[1];
    assert_eq!(
        (stream.received, stream.written, stream.lost_upstream),
        (200, 200, 0)
    );
    assert!(capture.stop().status.success());
}

#[test]
fn after_stop_only_a_stream_with_a_post_roll_goes_on_being_recorded_for_it() {
    let folder = scratch("post-roll");
    let (joints, voice) = (free_udp_port(), free_udp_port());
    let streams = format!(
        "[streams.joints]\nrate_hz = 1000\nudp = {{ bind = \"127.0.0.1:{joints}\" }}\n\
         sequence = {{ offset = 0, type = \"u32\" }}\n\
         [streams.voice]\nrate_hz = 1000\npost_roll_s = 2\nudp = {{ bind = \"127.0.0.1:{voice}\" }}\n\
         sequence = {{ offset = 0, type = \"u32\" }}\n"
    );
    let capture = Capture::declaring(&folder, &streams, &joints, &["--preflight", "warn"]);
    let robot = UdpSocket::bind("127.0.0.1:0").unwrap();
    let send = |port: &str, numbers: std::ops::Range<u32>| {
        for number in numbers {
            let mut message = [0_u8; 32];
            message[..4].copy_from_slice(&number.to_le_bytes());
            robot
                .send_to(&message, format!("127.0.0.1:{port}"))
                .unwrap();
        }
    };
    let mut client = capture.client();
    start(&mut client);
    send(&joints, 0..100);
    send(&voice, 0..100);
    capture.wait_for("both streams", |s| {
        s.streams[0].written == 100 && s.streams[1].written == 100
    });
    let stopping = std::thread::spawn(move || stop(&mut client));
    // Once Stop is noted, at the next tenth of a second, both send more:
    // only the voice is still being recorded.
    std::thread::sleep(Duration::from_millis(500));
    send(&joints, 100..200);
    send(&voice, 100..200);
    let folder = stopping.join().unwrap();

    let episode = finalised(&folder);
    let counts: Vec<(u64, u64, u64, Option<u64>)> = episode
        .streams
        .iter()
        .map(|stream| {
            (
                stream.received,
                stream.written,
                stream.lost_upstream,
                stream.post_roll,
            )
        })
        .collect();
    assert_eq!(counts, [(100, 100, 0, None), (200, 200, 0, Some(100))]);
    let stopped = episode.commanded_stop_utc_ns.unwrap();
    assert!(episode.commanded_start_utc_ns.unwrap() < stopped);
    assert!(stopped < episode.ended_utc_ns);
    assert!(capture.stop().status.success());
}

#[test]
fn the_status_carries_each_streams_newest_decoded_values() {
    let folder = scratch("values");
    let mut capture = Capture::start(&folder);
    // Nothing decoded yet.
    assert_eq!(
        capture.wait_for("no values", |_| true).streams[0].values,
        None
    );
    capture.send(100);
    let status = capture.wait_for("the newest message's values", |status| {
        status.streams[0].values.as_deref() == Some(&[99.0])
    });
    assert!(
        status.streams[0]
            .values_age_ms
            .is_some_and(|age| age < 10_000)
    );
    assert!(capture.stop().status.success());
}

/// A limit in MiB `above` MiB more than the disk holding `folder` has
/// free, so the disk counts as below it whatever else runs on the machine.
fn above_free(folder: &Path, above: u64) -> String {
    let free = kapto_capture::disk::free_bytes(folder).unwrap() / (1024 * 1024);
    (free + above).to_string()
}

#[test]
fn no_episode_starts_on_a_disk_below_the_stopping_limit_and_a_low_one_still_records() {
    let folder = scratch("disk");
    let limit = above_free(&folder, 1024);
    let capture = Capture::start_with(
        &folder,
        &["--disk-stop-below", &limit, "--disk-warn-below", &limit],
    );
    let error = capture
        .client()
        .ask(&Request::Start {
            duration_s: None,
            profile: None,
            task: None,
            waive: Vec::new(),
        })
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureDiskNearlyFull);
    assert!(
        error.message().contains("capture.disk_stop_below"),
        "{}",
        error.message()
    );
    assert!(capture.stop().status.success());

    // Only low: warned, and recorded as usual.
    let folder = scratch("disk-low");
    let limit = above_free(&folder, 1024);
    let mut capture = Capture::start_with(&folder, &["--disk-warn-below", &limit]);
    let mut client = capture.client();
    start(&mut client);
    capture.send(100);
    capture.wait_for("recording", |s| s.streams[0].written == 100);
    assert_eq!(finalised(&stop(&mut client)).streams[0].written, 100);
    assert!(capture.stop().status.success());
}

#[test]
fn orders_that_dont_fit_what_capture_is_doing_are_refused_with_their_cause() {
    let folder = scratch("refused");
    let capture = Capture::start(&folder);
    let mut client = capture.client();

    let error = client.ask(&Request::Stop).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureNotRecording);
    start(&mut client);
    let error = client
        .ask(&Request::Start {
            duration_s: None,
            profile: None,
            task: None,
            waive: Vec::new(),
        })
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureAlreadyRecording);
    stop(&mut client);
    assert!(capture.stop().status.success());
}

#[test]
fn an_aborted_episode_leaves_nothing_and_the_next_one_records_normally() {
    let folder = scratch("abort");
    let mut capture = Capture::start(&folder);
    let mut client = capture.client();

    let aborted = start(&mut client);
    capture.send(200);
    capture.wait_for("recording", |s| s.streams[0].written == 200);
    assert_eq!(
        client.ask(&Request::Abort).unwrap(),
        Response::Aborted {
            episode: aborted.clone()
        }
    );
    let data = folder.join("data");
    let leftovers: Vec<_> = std::fs::read_dir(&data)
        .unwrap()
        .chain(std::fs::read_dir(data.join(".recording")).unwrap())
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(&aborted))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let error = client.ask(&Request::Abort).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureNotRecording);
    start(&mut client);
    capture.send(100);
    capture.wait_for("recording again", |s| s.streams[0].written == 100);
    assert_eq!(finalised(&stop(&mut client)).streams[0].written, 100);
    assert!(capture.stop().status.success());
}

#[test]
fn a_watcher_is_told_of_every_episode_however_short_and_whether_it_was_kept() {
    let folder = scratch("watch");
    let capture = Capture::start(&folder);
    let mut watcher = capture.client().watch().unwrap();
    let mut client = capture.client();

    // Shorter than anything polling the status would see.
    let kept = start(&mut client);
    stop(&mut client);
    let aborted = start(&mut client);
    client.ask(&Request::Abort).unwrap();

    // Episodes only: Kapto's own events are told too.
    let told: Vec<Announcement> = watcher
        .by_ref()
        .filter(|told| !matches!(told, Announcement::EventNoted { .. }))
        .take(4)
        .collect();
    assert_eq!(
        told,
        [
            Announcement::EpisodeStarted {
                episode: kept.clone()
            },
            Announcement::EpisodeEnded {
                episode: kept,
                finalised: true
            },
            Announcement::EpisodeStarted {
                episode: aborted.clone()
            },
            Announcement::EpisodeEnded {
                episode: aborted,
                finalised: false
            },
        ]
    );
    assert!(capture.stop().status.success());
}

/// An event of `kind` from the API, at the moment capture takes it.
fn event(kind: &str, phase: Phase) -> Request {
    Request::Event(Box::new(NewEvent {
        kind: kind.to_owned(),
        at_utc_ns: None,
        until_utc_ns: None,
        phase,
        id: (phase != Phase::Moment).then(|| "g1".to_owned()),
        source: "api".to_owned(),
        text: None,
        attributes: std::collections::BTreeMap::new(),
    }))
}

/// Sends `request`, an event, and returns what capture noted.
fn noted(client: &mut ControlClient, request: &Request) -> (Event, Option<String>) {
    match client.ask(request).unwrap() {
        Response::Noted { event, episode } => (*event, episode),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_event_is_recorded_in_the_episode_being_recorded_and_told_to_every_watcher_either_way() {
    let folder = scratch("events");
    let mut capture = Capture::start(&folder);
    let mut watcher = capture.client().watch().unwrap();
    let mut client = capture.client();

    // Between episodes: there is no recording to keep it in.
    let (before, none) = noted(&mut client, &event("note", Phase::Moment));
    assert_eq!(none, None);
    let episode = start(&mut client);
    capture.send(100);
    let (begin, recorded) = noted(&mut client, &event("grasp", Phase::Begin));
    assert_eq!(recorded.as_deref(), Some(episode.as_str()));
    capture.send(100);
    let (end, _) = noted(&mut client, &event("grasp", Phase::End));
    // Kapto's own kinds are its own.
    let refused = client
        .ask(&event("kapto.episode_started", Phase::Moment))
        .unwrap_err();
    assert_eq!(refused.code(), ErrorCode::CaptureEventInvalid);
    capture.wait_for("all sent written", |status| {
        status.streams[0].written >= 200
    });
    let stopped = stop(&mut client);

    let recording = Path::new(&stopped).join("recording.mcap");
    let sent: Vec<Event> = read_events(&recording)
        .unwrap()
        .into_iter()
        .filter(|event| event.source == "api")
        .collect();
    assert_eq!(sent, [begin.clone(), end.clone()]);
    let described = finalised(&stopped);
    for kept in [&begin, &end] {
        assert!(
            (described.started_utc_ns..=described.ended_utc_ns).contains(&kept.at_utc_ns),
            "{kept:?} within the episode"
        );
    }
    // The events sent, among Kapto's own.
    let told: Vec<Announcement> = watcher
        .by_ref()
        .filter(|told| {
            matches!(told, Announcement::EventNoted { happened, .. } if happened.source == "api")
        })
        .take(3)
        .collect();
    let noted_told = |happened: &Event, episode: Option<&str>| Announcement::EventNoted {
        happened: Box::new(happened.clone()),
        episode: episode.map(str::to_owned),
    };
    assert_eq!(
        told,
        [
            noted_told(&before, None),
            noted_told(&begin, Some(&episode)),
            noted_told(&end, Some(&episode)),
        ]
    );
    assert!(capture.stop().status.success());
}

#[test]
fn kapto_records_its_own_events_around_what_clients_send() {
    let folder = scratch("own-events");
    let mut capture = Capture::start(&folder);
    let mut client = capture.client();
    start(&mut client);
    capture.send(100);
    noted(&mut client, &event("grasp", Phase::Begin));
    // Five messages lost on the way.
    capture.skip(5);
    capture.send(100);
    std::thread::sleep(Duration::from_millis(1_500));
    // Silent for longer than it may be, then back.
    std::thread::sleep(Duration::from_secs(3));
    capture.send(100);
    std::thread::sleep(Duration::from_millis(1_500));
    let stopped = stop(&mut client);

    let events = read_events(&Path::new(&stopped).join("recording.mcap")).unwrap();
    let told: Vec<String> = events
        .iter()
        .map(|event| {
            let about = ["stream", "lost_upstream"]
                .iter()
                .filter_map(|name| {
                    event
                        .attributes
                        .get(*name)
                        .map(|value| format!(" {name}={value}"))
                })
                .collect::<String>();
            format!("{}{about}", event.kind)
        })
        .collect();
    assert_eq!(
        told,
        [
            "grasp",
            "kapto.stream_lost stream=joints lost_upstream=5",
            "kapto.stream_silent stream=joints",
            "kapto.stream_back stream=joints",
        ]
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].at_utc_ns <= pair[1].at_utc_ns)
    );
    assert!(capture.stop().status.success());
}

#[test]
fn an_episode_is_finalised_once_its_supervisor_has_been_gone_for_the_control_timeout() {
    let folder = scratch("timeout");
    let mut capture = Capture::start_with(&folder, &["--control-timeout", "1"]);
    let mut supervisor = capture.client();
    assert_eq!(
        supervisor.ask(&Request::Supervise).unwrap(),
        Response::Supervised
    );
    let mut client = capture.client();
    let episode = start(&mut client);
    capture.send(100);
    capture.wait_for("recording", |s| s.streams[0].written == 100);
    std::thread::sleep(Duration::from_millis(1500));
    let status = capture.wait_for("supervised", |_| true);
    assert_eq!(
        status.episode.as_ref(),
        Some(&episode),
        "ended while supervised"
    );

    drop(supervisor);
    let gone = Instant::now();
    capture.wait_for("finalised", |s| s.episode.is_none());
    assert!(gone.elapsed() >= Duration::from_secs(1), "ended too early");
    let folder_of = folder.join("data").join(&episode);
    assert_eq!(
        finalised(folder_of.to_str().unwrap()).streams[0].written,
        100
    );

    // Started after the supervisor left: the operator's, so it goes on.
    let next = start(&mut client);
    std::thread::sleep(Duration::from_millis(1500));
    let status = capture.wait_for("recording by hand", |_| true);
    assert_eq!(status.episode, Some(next));
    stop(&mut client);
    let output = capture.stop();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
}

#[test]
fn an_episode_started_with_a_duration_ends_by_itself() {
    let folder = scratch("duration");
    let mut capture = Capture::start(&folder);
    let mut client = capture.client();
    client
        .ask(&Request::Start {
            duration_s: Some(1),
            profile: None,
            task: None,
            waive: Vec::new(),
        })
        .unwrap();
    capture.send(200);
    capture.wait_for("back to idle", |s| s.episode.is_none());
    let error = client.ask(&Request::Stop).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureNotRecording);
    let output = capture.stop();
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Finalised "));
}

#[test]
fn sigterm_while_recording_finalises_the_episode_before_exiting() {
    let folder = scratch("sigterm");
    let mut capture = Capture::start(&folder);
    let mut client = capture.client();
    start(&mut client);
    capture.send(200);
    capture.wait_for("recording", |s| s.streams[0].written == 200);
    let output = capture.stop();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let episode_folder = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Finalised "))
        .unwrap();
    assert_eq!(finalised(episode_folder).streams[0].written, 200);
}

#[test]
fn an_episode_killed_mid_recording_is_recovered_when_capture_starts_again_and_watchers_hear_of_it()
{
    let folder = scratch("kill");
    let mut capture = Capture::start(&folder);
    let mut client = capture.client();
    let episode = start(&mut client);
    capture.send(200);
    capture.wait_for("recording", |s| s.streams[0].written == 200);
    // Longer than the writer takes to put what it wrote on disk.
    std::thread::sleep(Duration::from_millis(1500));
    // SIGKILL: nothing of it gets to finish the episode.
    let mut child = capture.child.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let data = folder.join("data");
    assert!(data.join(".recording").join(&episode).exists());

    let capture = Capture::start(&folder);
    let recovered = data.join(&episode);
    let deadline = Instant::now() + TIMEOUT;
    while !recovered.join("_FINALISED").exists() {
        assert!(Instant::now() < deadline, "not recovered");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Watching once it was recovered, as a serve started later would.
    let mut watcher = capture.client().watch().unwrap();
    assert_eq!(
        watcher.next(),
        Some(Announcement::EpisodeRecovered {
            episode: episode.clone()
        })
    );
    let described = finalised(recovered.to_str().unwrap());
    assert!(described.recovered.is_some());
    assert_eq!(described.streams[0].written, 200);
    assert!(capture.stop().status.success());
}

#[test]
fn the_episode_command_starts_shows_stops_and_aborts_episodes() {
    let folder = scratch("cli");
    let mut capture = Capture::start(&folder);
    let kapto = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_kapto"))
            .args(args)
            .arg("--data-dir")
            .arg(folder.join("data"))
            .env_clear()
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    let (code, stdout, _) = kapto(&["episode", "start"]);
    assert_eq!(code, Some(0));
    assert!(stdout.starts_with("Recording episode "), "{stdout}");
    capture.send(10);
    capture.wait_for("recording", |s| s.streams[0].written == 10);
    let (_, stdout, _) = kapto(&["episode", "status"]);
    assert!(stdout.contains("joints"), "{stdout}");
    assert!(stdout.lines().nth(2).unwrap().contains(" 10 "), "{stdout}");
    let (code, stdout, _) = kapto(&["episode", "stop"]);
    assert_eq!(code, Some(0));
    assert!(stdout.starts_with("Finalised "), "{stdout}");
    kapto(&["episode", "start"]);
    let (code, stdout, _) = kapto(&["episode", "abort"]);
    assert_eq!(code, Some(0));
    assert!(stdout.starts_with("Aborted episode "), "{stdout}");
    let (code, _, stderr) = kapto(&["episode", "stop"]);
    assert_eq!(code, Some(1));
    assert!(
        stderr.starts_with("error[capture.not_recording]"),
        "{stderr}"
    );
    assert!(capture.stop().status.success());

    let (code, _, stderr) = kapto(&["episode", "status"]);
    assert_eq!(code, Some(1));
    assert!(
        stderr.starts_with("error[capture.control_unreachable]"),
        "{stderr}"
    );
}

#[test]
fn the_episode_event_command_sends_events_of_the_vocabulary_into_the_episode_being_recorded() {
    let folder = scratch("cli-events");
    let config = folder.join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("kapto.toml"), "").unwrap();
    std::fs::write(
        config.join("events.toml"),
        "[kinds.grasp]\nstretch = true\n",
    )
    .unwrap();
    let mut capture = Capture::start(&folder);
    let kapto = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_kapto"))
            .args(["episode", "event"])
            .args(args)
            .arg("--config")
            .arg(config.join("kapto.toml"))
            .arg("--data-dir")
            .arg(folder.join("data"))
            .env_clear()
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };

    let (code, stdout, _) = kapto(&["--kind", "grasp"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        stdout,
        "Noted grasp; no episode is being recorded, so it isn't kept.\n"
    );
    let (code, _, stderr) = kapto(&["--kind", "note"]);
    assert_eq!(code, Some(1));
    assert!(
        stderr.starts_with("error[events.unknown_kind]") && stderr.contains("the kinds are grasp"),
        "{stderr}"
    );
    let (code, _, stderr) = kapto(&["--kind", "grasp", "--begin", "g1", "--end", "g1"]);
    assert_eq!(code, Some(2), "{stderr}");

    let mut client = capture.client();
    let episode = start(&mut client);
    capture.send(10);
    let (code, stdout, _) = kapto(&["--kind", "grasp", "--begin", "g1", "--text", "left hand"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        stdout,
        format!("Recorded the beginning of grasp g1 in episode {episode}.\n")
    );
    kapto(&["--kind", "grasp", "--end", "g1"]);
    let (_, stdout, _) = kapto(&["--kind", "grasp", "--last", "5"]);
    assert!(
        stdout.starts_with("Recorded a stretch of grasp"),
        "{stdout}"
    );
    let stopped = stop(&mut client);

    let sent: Vec<(Phase, Option<String>, Option<String>)> =
        read_events(&Path::new(&stopped).join("recording.mcap"))
            .unwrap()
            .into_iter()
            .filter(|event| event.source == "cli")
            .map(|event| {
                if event.phase == Phase::Stretch {
                    let lasted = event.until_utc_ns.unwrap() - event.at_utc_ns;
                    assert_eq!(lasted, 5_000_000_000);
                }
                (event.phase, event.id, event.text)
            })
            .collect();
    assert_eq!(
        sent,
        [
            (
                Phase::Begin,
                Some("g1".to_owned()),
                Some("left hand".to_owned())
            ),
            (Phase::End, Some("g1".to_owned()), None),
            (Phase::Stretch, None, None),
        ]
    );
    assert!(capture.stop().status.success());
}

#[test]
fn a_start_is_refused_while_pre_flight_failed_and_records_the_check_once_waived() {
    let folder = scratch("preflight-refused");
    let capture = Capture::start_with(
        &folder,
        &["--preflight", "refuse", "--preflight-window", "1"],
    );
    let episode = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_kapto"))
            .arg("episode")
            .args(args)
            .arg("--data-dir")
            .arg(folder.join("data"))
            .env_clear()
            .output()
            .unwrap()
    };

    // Nothing sent: the start waits for the first run, which finds the
    // stream silent.
    let refused = episode(&["start"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{stderr}");
    assert!(stderr.contains("capture.preflight_failed"), "{stderr}");
    assert!(
        stderr.contains("preflight.stream_silent (joints)"),
        "{stderr}"
    );
    // Its status says what failed, as a page shows it before starting.
    let idle = capture.wait_for("nothing recorded", |status| status.episode.is_none());
    let failed = |status: &Status| {
        let run = status.preflight.as_ref().unwrap();
        let check = run.checks.iter().find(|check| !check.passed).unwrap();
        (check.code.clone(), check.waived)
    };
    assert_eq!(failed(&idle), ("preflight.stream_silent".to_owned(), false));

    let started = episode(&["start", "--waive", "preflight.stream_silent"]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    // While recording, the run the episode keeps.
    let recording = capture.wait_for("recording", |status| status.episode.is_some());
    assert_eq!(
        failed(&recording),
        ("preflight.stream_silent".to_owned(), true)
    );
    let stopped = String::from_utf8(episode(&["stop"]).stdout).unwrap();
    let recorded = finalised(stopped.trim().strip_prefix("Finalised ").unwrap());
    let checks: Vec<(String, bool, bool)> = recorded
        .preflight
        .unwrap()
        .checks
        .into_iter()
        .map(|check| (check.code, check.passed, check.waived))
        .collect();
    assert_eq!(
        checks,
        [
            ("preflight.disk_too_small".to_owned(), true, false),
            ("preflight.disk_too_slow".to_owned(), true, false),
            ("preflight.stream_silent".to_owned(), false, true),
        ]
    );
    assert!(capture.stop().status.success());
}

#[test]
fn pre_flight_runs_at_start_and_on_demand_and_never_while_recording() {
    let folder = scratch("preflight");
    let mut capture = Capture::start_with(&folder, &["--preflight-window", "1"]);
    let failed = |response: Response| {
        let Response::Preflight(report) = response else {
            panic!("not a pre-flight: {response:?}");
        };
        assert!(report.window_ms >= 1000, "{report:?}");
        let failed: Vec<(String, Option<String>)> = report
            .checks
            .into_iter()
            .filter(|check| !check.passed)
            .map(|check| (check.code, check.stream))
            .collect();
        failed
    };

    // Asked while the first run goes on, the answer is that run's: nothing
    // has been sent, so the stream is silent; the disk is fine.
    let answer = capture.client().ask(&Request::Preflight).unwrap();
    assert_eq!(
        failed(answer),
        [(
            ErrorCode::PreflightStreamSilent.id().to_owned(),
            Some("joints".to_owned())
        )]
    );

    // On demand, with the robot sending at its declared rate.
    let robot = UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = format!("127.0.0.1:{}", capture.port);
    // Twelve every 10 ms, a little above the declared 1 kHz, so a busy
    // machine's late wake-ups still leave it above 90 %.
    let sending = std::thread::spawn(move || {
        for number in 0..2_400_u32 {
            let mut message = [0_u8; 32];
            message[..4].copy_from_slice(&number.to_le_bytes());
            robot.send_to(&message, &target).unwrap();
            if number % 12 == 11 {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    });
    std::thread::sleep(Duration::from_millis(200));
    let answer = capture.client().ask(&Request::Preflight).unwrap();
    assert_eq!(failed(answer), []);
    sending.join().unwrap();

    // The command line says the same, and fails when a check does.
    let output = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["episode", "preflight", "--data-dir"])
        .arg(folder.join("data"))
        .env_clear()
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("FAILED preflight.stream_silent (joints): 0 messages in "),
        "{stdout}"
    );
    assert!(
        stdout.contains("ok     preflight.disk_too_slow: "),
        "{stdout}"
    );

    // Never while recording: its test file would compete with the episode's.
    let mut client = capture.client();
    assert!(matches!(
        client
            .ask(&Request::Start {
                duration_s: None,
                profile: None,
                task: None,
                waive: Vec::new(),
            })
            .unwrap(),
        Response::Started { .. }
    ));
    let Err(error) = client.ask(&Request::Preflight) else {
        panic!("pre-flight ran while recording");
    };
    assert_eq!(error.code(), ErrorCode::CaptureAlreadyRecording);
    assert!(
        error.report().contains("pre-flight runs between episodes"),
        "{}",
        error.report()
    );
    capture.send(1);
    capture.stop();
}

#[test]
fn a_cameras_frames_on_their_way_to_the_live_view_are_counted_in_the_status() {
    use std::io::Write;
    use std::net::TcpStream;

    let folder = scratch("camera-status");
    let [port] = common::free_tcp_ports();
    let declared = format!(
        "[streams.wrist_camera]\nrate_hz = 30\nmax_message_bytes = 65536\n\
         tcp = {{ listen = \"127.0.0.1:{port}\", framing = {{ kind = \"length_prefix\", length_bytes = 4 }} }}\n\
         camera = {{ format = \"jpeg\" }}\n"
    );
    // No viewer bridge listens on the frame socket.
    let frames = folder.join("frames.sock");
    let frame_socket = frames.to_str().unwrap();
    let capture = Capture::declaring(
        &folder,
        &declared,
        &port,
        &[
            "--preflight",
            "warn",
            "--frame-socket",
            frame_socket,
            "--camera-max-fps",
            "100",
        ],
    );
    let status = capture.wait_for("no frames yet", |_| true);
    assert_eq!(
        status.streams[0].live_view.map(|camera| camera.offered),
        Some(0)
    );

    let mut camera = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    for _ in 0..10 {
        let jpeg = [0xff, 0xd8, 0xff, 0xe0];
        camera.write_all(&4_u32.to_le_bytes()).unwrap();
        camera.write_all(&jpeg).unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }
    let status = capture.wait_for("each frame offered and not sent", |status| {
        status.streams[0]
            .live_view
            .is_some_and(|camera| camera.offered >= 5 && camera.not_sent == camera.offered)
    });
    let camera = status.streams[0].live_view.unwrap();
    assert_eq!((camera.sent, camera.skipped), (0, 0), "{camera:?}");
    assert!(capture.stop().status.success());
}
