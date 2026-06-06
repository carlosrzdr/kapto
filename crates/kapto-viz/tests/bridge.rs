// SPDX-License-Identifier: Apache-2.0

//! The bridge's outside edges: the tap socket it reads, the stream it
//! serves the web UI's pages, and what it reports when it can't start.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::sync::Arc;

use kapto_capture::control::{Announcement, ControlServer};
use kapto_capture::tap::wire::{Summary, encode};
use kapto_capture::tap::{TapSocket, TapStats};
use kapto_config::{ConfigFile, parse_mapping, parse_streams};
use kapto_core::clock::monotonic_now;
use kapto_core::{ClockSample, ErrorCode};
use kapto_robot::{JointMap, Kinematics, RobotModel, load_urdf};
use kapto_store::{Event, Phase};
use kapto_testkit::scratch;
use kapto_viz::history::{HISTORY_MAGIC, History, Record};
use kapto_viz::marks::Marks;
use kapto_viz::stream::{FRAME_MAGIC, Frame, FrameStream, Opening, STREAM_VERSION};
use kapto_viz::{
    Bridge, BridgeSettings, LiveRobot, Plots, StreamFields, TapReader, TapReaderStats,
};
use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

fn free_port() -> u16 {
    TcpListener::bind((LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn waiting_samples_are_read_oldest_first_and_datagrams_that_arent_samples_are_counted() {
    let folder = scratch("reader");
    let path = folder.join("tap.sock");
    let mut reader = TapReader::bind(&path).unwrap();
    let mut sender = TapSocket::new(&path, Arc::new(TapStats::default())).unwrap();
    sender.send(&Summary::single(0, 10, &[1.0]));
    sender.send(&Summary::single(2, 20, &[2.0, 3.0]));
    let raw = UnixDatagram::unbound().unwrap();
    raw.send_to(b"hello", &path).unwrap();
    let mut other_version = Vec::new();
    encode(&Summary::single(0, 30, &[4.0]), &mut other_version);
    other_version[4] = 1;
    raw.send_to(&other_version, &path).unwrap();
    sender.send(&Summary::single(0, 40, &[5.0]));

    let mut seen = Vec::new();
    let count = reader
        .drain(|sample| seen.push((sample.stream, sample.recv_time_ns, sample.values.clone())))
        .unwrap();
    assert_eq!(count, 3);
    assert_eq!(
        seen,
        [
            (0, 10, vec![1.0]),
            (2, 20, vec![2.0, 3.0]),
            (0, 40, vec![5.0])
        ]
    );
    assert_eq!(
        reader.stats(),
        TapReaderStats {
            samples: 3,
            other_version: 1,
            unreadable: 1
        }
    );
    assert_eq!(reader.drain(|_| {}).unwrap(), 0, "nothing more is waiting");
    drop(reader);
    assert!(
        !path.exists(),
        "the socket file is removed when the reader stops"
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_socket_left_by_an_earlier_run_is_replaced_but_any_other_file_is_kept() {
    let folder = scratch("stale");
    let stale = folder.join("stale.sock");
    std::mem::forget(UnixDatagram::bind(&stale).unwrap());
    assert!(TapReader::bind(&stale).is_ok());

    let precious = folder.join("notes.txt");
    std::fs::write(&precious, "keep me").unwrap();
    let error = TapReader::bind(&precious).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(&precious).unwrap(), "keep me");
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_socket_path_too_long_for_unix_sockets_is_refused_naming_the_setting() {
    let path = Path::new("/tmp").join("x".repeat(120));
    let error = TapReader::bind(&path).unwrap_err();
    assert!(error.to_string().contains("viewer.tap_socket"), "{error}");
}

fn arm7() -> (RobotModel, LiveRobot) {
    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7");
    let robot = load_urdf(&package.join("urdf/arm7.urdf"), &[]).unwrap();
    let kinematics = Kinematics::new(&robot).unwrap();
    let read = |name: &str| ConfigFile {
        contents: std::fs::read_to_string(package.join(name)).unwrap(),
        path: package.join(name),
    };
    let streams = parse_streams(&read("streams.toml")).unwrap();
    let mapping = parse_mapping(&read("mapping.toml")).unwrap();
    let map = JointMap::new(&robot, &kinematics, &streams, &mapping, Path::new("m")).unwrap();
    let live = LiveRobot::new(&robot, kinematics, &map, streams.len(), ClockSample::now());
    (robot, live)
}

fn no_plots() -> Plots {
    Plots::new(&[])
}

fn settings(folder: &Path) -> BridgeSettings {
    BridgeSettings::new(LOCALHOST, (free_port(), 7700), folder.join("tap.sock"))
}

#[test]
fn the_bridge_moves_the_robot_with_the_samples_waiting_on_its_socket() {
    let folder = scratch("bridge");
    let settings = settings(&folder);
    let (robot, live) = arm7();
    let mut bridge = Bridge::start(&settings, &robot, live, &no_plots()).unwrap();
    assert_eq!(bridge.stream_address().port(), settings.stream_port);

    let mut sender = TapSocket::new(&settings.tap_socket, Arc::new(TapStats::default())).unwrap();
    for time in 0..5 {
        sender.send(&Summary::single(
            0,
            time,
            &[0.0, 0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
        ));
    }
    assert_eq!(bridge.step().unwrap(), 5);
    assert_eq!(bridge.tap_stats().samples, 5);
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_port_already_in_use_stops_the_bridge_with_its_cause_and_address() {
    let folder = scratch("busy");
    let (robot, _) = arm7();
    let mut settings = settings(&folder);
    let taken = TcpListener::bind((LOCALHOST, 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    settings.stream_port = port;
    let Err(error) = Bridge::start(&settings, &robot, arm7().1, &no_plots()) else {
        panic!("started on a port in use")
    };
    assert_eq!(error.code(), ErrorCode::ViewerListenFailed);
    assert!(
        error
            .report()
            .contains(&format!("address: 127.0.0.1:{port}")),
        "{}",
        error.report()
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_tap_socket_that_cannot_be_listened_on_stops_the_bridge_with_its_cause() {
    let folder = scratch("no-socket");
    let mut settings = settings(&folder);
    settings.tap_socket = folder.join("in-the-way");
    std::fs::write(&settings.tap_socket, "not a socket").unwrap();
    let (robot, live) = arm7();
    let Err(error) = Bridge::start(&settings, &robot, live, &no_plots()) else {
        panic!("started without its socket")
    };
    assert_eq!(error.code(), ErrorCode::ViewerTapSocketFailed);
    assert!(error.report().contains("in-the-way"), "{}", error.report());
    std::fs::remove_dir_all(folder).unwrap();
}

/// A frame read as the specification says, independently of how the
/// bridge writes one.
#[derive(Debug, PartialEq)]
struct ReadFrame {
    received_utc_ns: i64,
    sent_utc_ns: i64,
    poses: Vec<[f32; 7]>,
    samples: Vec<ReadSample>,
}

/// One sample of a frame: its stream, message count, first and newest
/// receive times, and each value's minimum and maximum.
#[derive(Debug, PartialEq)]
struct ReadSample(u16, u32, i64, i64, Vec<(f64, f64)>);

fn read_frame(bytes: &[u8]) -> ReadFrame {
    let mut at = 0;
    let mut take = |n: usize| {
        let part = &bytes[at..at + n];
        at += n;
        part
    };
    assert_eq!(take(4), FRAME_MAGIC);
    assert_eq!(
        u16::from_le_bytes(take(2).try_into().unwrap()),
        STREAM_VERSION
    );
    let has_poses = u16::from_le_bytes(take(2).try_into().unwrap()) & 1 == 1;
    let received_utc_ns = i64::from_le_bytes(take(8).try_into().unwrap());
    let sent_utc_ns = i64::from_le_bytes(take(8).try_into().unwrap());
    let links = u32::from_le_bytes(take(4).try_into().unwrap());
    let samples = u32::from_le_bytes(take(4).try_into().unwrap());
    assert_eq!(has_poses, links > 0);
    let poses = (0..links)
        .map(|_| std::array::from_fn(|_| f32::from_le_bytes(take(4).try_into().unwrap())))
        .collect();
    let samples = (0..samples)
        .map(|_| {
            let stream = u16::from_le_bytes(take(2).try_into().unwrap());
            take(2);
            let messages = u32::from_le_bytes(take(4).try_into().unwrap());
            let values = u32::from_le_bytes(take(4).try_into().unwrap());
            let first = i64::from_le_bytes(take(8).try_into().unwrap());
            let newest = i64::from_le_bytes(take(8).try_into().unwrap());
            let extremes = (0..values)
                .map(|_| {
                    let minimum = f64::from_le_bytes(take(8).try_into().unwrap());
                    (minimum, f64::from_le_bytes(take(8).try_into().unwrap()))
                })
                .collect();
            ReadSample(stream, messages, first, newest, extremes)
        })
        .collect();
    assert_eq!(at, bytes.len(), "nothing after the last sample");
    ReadFrame {
        received_utc_ns,
        sent_utc_ns,
        poses,
        samples,
    }
}

/// Connects to the stream at `port` as a page from `origin`, if given.
fn watch(
    port: u16,
    origin: Option<&str>,
) -> Result<WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>, tungstenite::Error> {
    let mut request = format!("ws://127.0.0.1:{port}/")
        .into_client_request()
        .unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", origin.parse().unwrap());
    }
    tungstenite::connect(request).map(|(socket, _)| socket)
}

/// The next frame the page is sent, past any marks; `None` if the next
/// message is something else.
fn next_binary(
    socket: &mut WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
) -> Option<ReadFrame> {
    loop {
        match socket.read().unwrap() {
            Message::Binary(bytes) => return Some(read_frame(&bytes)),
            Message::Text(text) if text.starts_with(r#"{"marks":"#) => {}
            Message::Ping(_) | Message::Pong(_) => {}
            _ => return None,
        }
    }
}

/// The next marks the page is told, as JSON, past any frames.
fn next_marks(
    socket: &mut WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
) -> Vec<Value> {
    loop {
        if let Message::Text(text) = socket.read().unwrap()
            && let Ok(said) = serde_json::from_str::<Value>(&text)
            && let Some(marks) = said["marks"].as_array()
        {
            return marks.clone();
        }
    }
}

fn noted(kind: &str, phase: Phase, id: Option<&str>) -> Announcement {
    Announcement::EventNoted {
        happened: Box::new(Event {
            kind: kind.to_owned(),
            at_utc_ns: 5,
            until_utc_ns: None,
            phase,
            id: id.map(str::to_owned),
            source: "api".to_owned(),
            text: None,
            attributes: std::collections::BTreeMap::new(),
        }),
        episode: Some("e1".to_owned()),
    }
}

#[test]
fn what_capture_announces_is_told_to_every_page_and_kept_for_pages_connecting_later() {
    let folder = scratch("marks");
    let mut settings = settings(&folder);
    let capture = ControlServer::listen(&folder.join("capture.sock")).unwrap();
    settings.control_socket = Some(folder.join("capture.sock"));
    let (robot, live) = arm7();
    let _bridge = Bridge::start(&settings, &robot, live, &no_plots()).unwrap();
    let mut page = watch(settings.stream_port, Some("http://127.0.0.1:7700")).unwrap();
    page.read().unwrap();
    assert_eq!(
        next_marks(&mut page),
        Vec::<Value>::new(),
        "nothing kept yet"
    );

    // Told again until the bridge is watching: what comes before is missed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let tungstenite::stream::MaybeTlsStream::Plain(connection) = page.get_ref() else {
        panic!("a plain connection")
    };
    connection
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the bridge never watched capture"
        );
        capture.announce(noted("note", Phase::Moment, None));
        if let Ok(Message::Text(text)) = page.read()
            && text.starts_with(r#"{"marks":"#)
        {
            break;
        }
    }
    let tungstenite::stream::MaybeTlsStream::Plain(connection) = page.get_ref() else {
        panic!("a plain connection")
    };
    connection.set_read_timeout(None).unwrap();

    capture.announce(Announcement::EpisodeStarted {
        episode: "e2".to_owned(),
    });
    capture.announce(noted("grasp", Phase::Begin, Some("g1")));
    capture.announce(Announcement::EpisodeEnded {
        episode: "e2".to_owned(),
        finalised: true,
    });
    let mut told = Vec::new();
    while told.len() < 3 {
        told.extend(
            next_marks(&mut page)
                .into_iter()
                .filter(|mark| mark["noted"]["event"]["kind"] != "note"),
        );
    }
    assert_eq!(told[0]["episode_started"]["episode"], "e2");
    assert!(told[0]["episode_started"]["at_utc_ns"].as_i64().unwrap() > 0);
    assert_eq!(
        told[1],
        serde_json::json!({"noted": {"event": {"kind": "grasp", "at_utc_ns": 5, "phase": "begin", "id": "g1", "source": "api"}, "episode": "e1"}})
    );
    assert_eq!(told[2]["episode_ended"]["finalised"], true);

    // A page connecting now is told all of them at once.
    let mut later = watch(settings.stream_port, None).unwrap();
    later.read().unwrap();
    let kept: Vec<Value> = next_marks(&mut later)
        .into_iter()
        .filter(|mark| mark["noted"]["event"]["kind"] != "note")
        .collect();
    assert_eq!(kept, told);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_frame_reads_back_as_the_specification_says() {
    let (robot, mut live) = arm7();
    live.advance().unwrap();
    let mut frame = Frame::default();
    assert!(frame.is_empty());
    frame.poses(live.poses());
    let mut sample = kapto_capture::tap::wire::TapSample::single(1, 2_000, vec![0.5, -1.0]);
    sample.first_recv_time_ns = 1_000;
    sample.messages = 3;
    sample.minimums = vec![0.25, -2.0];
    sample.maximums = vec![0.75, -1.0];
    frame.sample(&sample, 1_000_000);
    let (bytes, record) = frame.finish(i64::MIN, 9_000_000);
    let read = read_frame(&bytes);
    assert_eq!(record.received_utc_ns, 1_002_000, "kept as it was sent");
    assert_eq!(read.poses.len(), robot.links.len());
    // The root link sits at the origin, unturned.
    assert_eq!(read.poses[robot.root], [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
    assert_eq!(
        read.samples,
        [ReadSample(
            1,
            3,
            1_001_000,
            1_002_000,
            vec![(0.25, 0.75), (-2.0, -1.0)]
        )]
    );
    assert_eq!(
        (read.received_utc_ns, read.sent_utc_ns),
        (1_002_000, 9_000_000)
    );
}

#[test]
fn receive_times_follow_the_wall_clock_again_after_it_moved_against_the_monotonic_one() {
    let folder = scratch("clock-moved");
    let settings = settings(&folder);
    let (robot, mut live) = arm7();
    // Read before the machine slept for an hour: the wall clock has since
    // moved an hour further than the monotonic one.
    let now = ClockSample::now();
    let hour: i64 = 3_600_000_000_000;
    live.set_clock(ClockSample {
        utc_ns: now.utc_ns - hour,
        ..now
    });
    let mut bridge = Bridge::start(&settings, &robot, live, &no_plots()).unwrap();
    let mut page = watch(settings.stream_port, Some("http://127.0.0.1:7700")).unwrap();
    let _opening = page.read().unwrap();
    let _at_rest = next_binary(&mut page).unwrap();
    let mut sender = TapSocket::new(&settings.tap_socket, Arc::new(TapStats::default())).unwrap();
    let wall = |frame: &ReadFrame| (frame.received_utc_ns - ClockSample::now().utc_ns).abs();

    // Read again within a second: a sample received then is on the wall
    // clock, not an hour behind it.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let received = monotonic_now().as_nanos();
    sender.send(&Summary::single(
        0,
        received,
        &[1.0, 0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
    ));
    assert_eq!(bridge.step().unwrap(), 1);
    let moved = next_binary(&mut page).unwrap();
    assert!(wall(&moved) < 5_000_000_000, "{} ns off", wall(&moved));
    drop(page);
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_page_is_told_the_links_and_values_then_the_robot_then_each_sample_as_it_arrives() {
    let folder = scratch("stream");
    let settings = settings(&folder);
    let (robot, live) = arm7();
    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7");
    let streams = parse_streams(&ConfigFile {
        contents: std::fs::read_to_string(package.join("streams.toml")).unwrap(),
        path: package.join("streams.toml"),
    })
    .unwrap();
    let plots = Plots::new(&StreamFields::of(&streams));
    let mut bridge = Bridge::start(&settings, &robot, live, &plots).unwrap();
    assert_eq!(bridge.stream_address().port(), settings.stream_port);

    let mut page = watch(settings.stream_port, Some("http://127.0.0.1:7700")).unwrap();
    let Message::Text(opening) = page.read().unwrap() else {
        panic!("the opening message comes first, as text")
    };
    let opening: Value = serde_json::from_str(&opening).unwrap();
    assert_eq!(opening["version"], STREAM_VERSION);
    let links: Vec<&str> = opening["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link.as_str().unwrap())
        .collect();
    let names: Vec<&str> = robot.links.iter().map(|link| link.name.as_str()).collect();
    assert_eq!(links, names);
    let arm = &opening["streams"][0];
    assert_eq!(arm["name"], "arm_joints");
    // The sequence number and the robot's clock aren't plotted.
    assert_eq!(
        arm["values"][0],
        serde_json::json!({"name": "sequence", "plotted": false})
    );
    assert_eq!(
        arm["values"][2],
        serde_json::json!({"name": "joint_positions[0]", "plotted": true})
    );

    // The robot at rest, as worked out when the bridge started.
    let at_rest = next_binary(&mut page).unwrap();
    assert_eq!(at_rest.poses.len(), robot.links.len());
    assert_eq!(at_rest.samples, []);

    let mut sender = TapSocket::new(&settings.tap_socket, Arc::new(TapStats::default())).unwrap();
    sender.send(&Summary::single(
        0,
        5_000,
        &[7.0, 0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
    ));
    assert_eq!(bridge.step().unwrap(), 1);
    let moved = next_binary(&mut page).unwrap();
    assert_eq!(moved.samples.len(), 1);
    assert_eq!(moved.samples[0].0, 0);
    assert_eq!(moved.samples[0].4[2], (0.1, 0.1));
    assert_ne!(moved.poses, at_rest.poses, "the joints moved the arm");
    assert!(moved.sent_utc_ns >= moved.received_utc_ns);
    assert_eq!(bridge.stream_stats().pages, 1);
    drop(page);
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn another_site_s_page_is_refused_and_one_that_never_reads_costs_only_the_frames_it_misses() {
    let opening = Opening {
        version: STREAM_VERSION,
        links: Vec::new(),
        streams: Vec::new(),
        history_s: 600,
    };
    let history = History::new(i64::MAX, usize::MAX);
    let stream = FrameStream::start(
        (LOCALHOST, 0, 7700),
        &opening,
        history,
        Marks::new(i64::MAX),
    )
    .unwrap();
    let port = stream.address().port();
    let refused = watch(port, Some("http://evil.example:7700")).unwrap_err();
    assert!(
        matches!(&refused, tungstenite::Error::Http(response) if response.status() == 403),
        "{refused:?}"
    );

    // Connected, but never reading past the opening message.
    let stalled = watch(port, None).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while stream.stats().pages == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the page never registered"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // A megabyte a frame: the connection's buffers fill after a few.
    let frame = tungstenite::Bytes::from(vec![0_u8; 1 << 20]);
    let started = std::time::Instant::now();
    for _ in 0..200 {
        stream.send(&frame, Record::default());
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "sending never waits on a page"
    );
    let stats = stream.stats();
    assert_eq!(stats.sent + stats.dropped, 200);
    assert!(stats.dropped > 0, "{stats:?}");
    assert_eq!(stats.refused, 1);
    drop(stalled);
}

#[test]
fn waiting_on_the_tap_returns_as_soon_as_a_sample_arrives() {
    let folder = scratch("wait");
    let path = folder.join("tap.sock");
    let mut reader = TapReader::bind(&path).unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        reader
            .drain_waiting(std::time::Duration::from_millis(50), |_| {})
            .unwrap(),
        0
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(40),
        "it waited"
    );
    let sender = std::thread::spawn({
        let path = path.clone();
        move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut sender = TapSocket::new(&path, Arc::new(TapStats::default())).unwrap();
            sender.send(&Summary::single(0, 10, &[1.0]));
        }
    });
    let started = std::time::Instant::now();
    let mut seen = 0;
    let read = reader
        .drain_waiting(std::time::Duration::from_secs(30), |_| seen += 1)
        .unwrap();
    assert_eq!((read, seen), (1, 1));
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    sender.join().unwrap();
    // Reading goes back to never waiting.
    assert_eq!(reader.drain(|_| {}).unwrap(), 0);
    drop(reader);
    std::fs::remove_dir_all(folder).unwrap();
}

/// A reply to a question about the history, read as the specification
/// says: its ID, the span kept, the poses with their times, and each
/// stream's parts as (first, newest, messages, extremes).
type ReadReply = (
    u32,
    (i64, i64),
    Vec<(i64, Vec<[f32; 7]>)>,
    Vec<(u16, Vec<ReadPart>)>,
);

#[derive(Debug, PartialEq)]
struct ReadPart(i64, i64, u32, Vec<(f64, f64)>);

fn read_reply(bytes: &[u8]) -> ReadReply {
    let mut at = 0;
    let mut take = |n: usize| {
        let part = &bytes[at..at + n];
        at += n;
        part
    };
    assert_eq!(take(4), HISTORY_MAGIC);
    assert_eq!(
        u16::from_le_bytes(take(2).try_into().unwrap()),
        STREAM_VERSION
    );
    take(2);
    let id = u32::from_le_bytes(take(4).try_into().unwrap());
    let oldest = i64::from_le_bytes(take(8).try_into().unwrap());
    let newest = i64::from_le_bytes(take(8).try_into().unwrap());
    let links = u32::from_le_bytes(take(4).try_into().unwrap());
    let poses = (0..u32::from_le_bytes(take(4).try_into().unwrap()))
        .map(|_| {
            let time = i64::from_le_bytes(take(8).try_into().unwrap());
            let pose = (0..links)
                .map(|_| std::array::from_fn(|_| f32::from_le_bytes(take(4).try_into().unwrap())))
                .collect();
            (time, pose)
        })
        .collect();
    let streams = (0..u32::from_le_bytes(take(4).try_into().unwrap()))
        .map(|_| {
            let stream = u16::from_le_bytes(take(2).try_into().unwrap());
            take(2);
            let values = u32::from_le_bytes(take(4).try_into().unwrap());
            let parts = (0..u32::from_le_bytes(take(4).try_into().unwrap()))
                .map(|_| {
                    let first = i64::from_le_bytes(take(8).try_into().unwrap());
                    let last = i64::from_le_bytes(take(8).try_into().unwrap());
                    let messages = u32::from_le_bytes(take(4).try_into().unwrap());
                    take(4);
                    let extremes = (0..values)
                        .map(|_| {
                            let minimum = f64::from_le_bytes(take(8).try_into().unwrap());
                            (minimum, f64::from_le_bytes(take(8).try_into().unwrap()))
                        })
                        .collect();
                    ReadPart(first, last, messages, extremes)
                })
                .collect();
            (stream, parts)
        })
        .collect();
    assert_eq!(at, bytes.len(), "nothing after the last part");
    (id, (oldest, newest), poses, streams)
}

#[test]
fn a_page_asks_for_a_stretch_of_the_last_minutes_and_gets_it_merged_to_its_points() {
    let folder = scratch("history");
    let settings = settings(&folder);
    let (robot, live) = arm7();
    let clock = live.clock();
    let mut bridge = Bridge::start(&settings, &robot, live, &no_plots()).unwrap();
    let mut page = watch(settings.stream_port, Some("http://127.0.0.1:7700")).unwrap();
    let Message::Text(opening) = page.read().unwrap() else {
        panic!("the opening message comes first")
    };
    let opening: Value = serde_json::from_str(&opening).unwrap();
    assert_eq!(opening["history_s"], 600);
    next_binary(&mut page).unwrap();

    // Four samples of the arm, a millisecond apart: joint 1 at 0.1, 0.4,
    // -0.2 and 0.3.
    let mut sender = TapSocket::new(&settings.tap_socket, Arc::new(TapStats::default())).unwrap();
    let start = clock.mono.as_nanos();
    for (index, joint) in [0.1, 0.4, -0.2, 0.3].into_iter().enumerate() {
        let time = start + 1_000_000 * (index as u64 + 1);
        sender.send(&Summary::single(
            0,
            time,
            &[0.0, 0.0, joint, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ));
        assert_eq!(bridge.step().unwrap(), 1);
        next_binary(&mut page).unwrap();
    }
    let utc = |index: i64| clock.utc_ns + 1_000_000 * index;
    // Two parts: the first two samples, then the last two.
    let question = format!(
        r#"{{"history":{{"id":7,"from":{},"to":{},"points":2}}}}"#,
        utc(1),
        utc(5) - 1
    );
    page.send(Message::text(question)).unwrap();
    let Message::Binary(reply) = page.read().unwrap() else {
        panic!("a stretch is binary")
    };
    let (id, span, poses, streams) = read_reply(&reply);
    assert_eq!(id, 7);
    assert_eq!(span.1, utc(4), "the newest kept");
    assert_eq!(poses.len(), 2, "the newest pose of each part");
    assert_eq!(poses[1].0, utc(4));
    let parts = &streams[0].1;
    assert_eq!(parts.len(), 2);
    assert_eq!(
        (parts[0].0, parts[0].1, parts[0].2, parts[0].3[2]),
        (utc(1), utc(2), 2, (0.1, 0.4))
    );
    assert_eq!(parts[1].3[2], (-0.2, 0.3));

    // A question that can't be answered says why, as text.
    page.send(Message::text(
        r#"{"history":{"id":8,"from":5,"to":1,"points":2}}"#,
    ))
    .unwrap();
    let Message::Text(refusal) = page.read().unwrap() else {
        panic!("a refusal is text")
    };
    let refusal: Value = serde_json::from_str(&refusal).unwrap();
    assert_eq!(refusal["error"]["id"], 8);
    page.send(Message::text("what happened?")).unwrap();
    let Message::Text(refusal) = page.read().unwrap() else {
        panic!("a refusal is text")
    };
    assert!(refusal.contains("\"error\""), "{refusal}");
    drop(page);
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_page_streamed_to_without_pause_still_has_its_questions_answered() {
    let opening = Opening {
        version: STREAM_VERSION,
        links: Vec::new(),
        streams: Vec::new(),
        history_s: 600,
    };
    let stream = Arc::new(
        FrameStream::start(
            (LOCALHOST, 0, 7700),
            &opening,
            History::new(i64::MAX, usize::MAX),
            Marks::new(i64::MAX),
        )
        .unwrap(),
    );
    let mut page = watch(stream.address().port(), None).unwrap();
    let Message::Text(_) = page.read().unwrap() else {
        panic!("the opening message comes first")
    };
    // A frame every 5 ms, more often than the page's thread would otherwise
    // stop to look for a question.
    let streaming = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let sender = std::thread::spawn({
        let (stream, streaming) = (Arc::clone(&stream), Arc::clone(&streaming));
        move || {
            let mut time = 0;
            while streaming.load(std::sync::atomic::Ordering::Relaxed) {
                time += 5_000_000;
                let record = Record {
                    received_utc_ns: time,
                    ..Record::default()
                };
                stream.send(&tungstenite::Bytes::from_static(b"frame"), record);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    page.send(Message::text(
        r#"{"history":{"id":3,"from":0,"to":9000000000000000000,"points":1}}"#,
    ))
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let answered = loop {
        assert!(std::time::Instant::now() < deadline, "never answered");
        if let Message::Binary(bytes) = page.read().unwrap()
            && bytes.starts_with(HISTORY_MAGIC)
        {
            break read_reply(&bytes);
        }
    };
    assert_eq!(answered.0, 3);
    streaming.store(false, std::sync::atomic::Ordering::Relaxed);
    sender.join().unwrap();
}
