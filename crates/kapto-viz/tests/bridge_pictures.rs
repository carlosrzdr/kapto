// SPDX-License-Identifier: Apache-2.0

//! Cameras' frames through the bridge: read from the frame socket as
//! capture sends them, each picture sent to a page as the viewer stream's
//! specification says, a frame that can't be shown named, and a page that
//! stops reading missing pictures without holding up the bridge.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use kapto_capture::Batch;
use kapto_capture::tap::frames::{FrameConfig, FrameFeed, FrameSender, frames};
use kapto_config::{ConfigFile, parse_mapping, parse_streams};
use kapto_core::{ClockSample, ErrorCode, RecvTime};
use kapto_robot::{JointMap, Kinematics, RobotModel, load_urdf};
use kapto_testkit::scratch;
use kapto_viz::camera_history::CameraHistorySettings;
use kapto_viz::page_pictures::PageShare;
use kapto_viz::picture::CameraKind;
use kapto_viz::stream::{LOOKED_BACK_MAGIC, PICTURE_HEADER, PICTURE_MAGIC, STREAM_VERSION};
use kapto_viz::{Bridge, BridgeSettings, LiveRobot, Plots};
use serde_json::Value;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

type Page = WebSocket<MaybeTlsStream<TcpStream>>;

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

/// A bridge whose streams 1 and 2 are a ROS 2 compressed camera and a raw
/// one, and capture's end of the frame socket.
fn bridge(folder: &Path) -> (Bridge, BridgeSettings, Capture) {
    bridge_sharing(folder, PageShare::default())
}

/// A bridge sending each page its cameras' pictures by `share`.
fn bridge_sharing(folder: &Path, share: PageShare) -> (Bridge, BridgeSettings, Capture) {
    bridge_keeping(folder, share, CameraHistorySettings::default().every)
}

/// The same, keeping a picture of each camera for looking back every
/// `every`.
fn bridge_keeping(
    folder: &Path,
    share: PageShare,
    every: Duration,
) -> (Bridge, BridgeSettings, Capture) {
    let port = TcpListener::bind((LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut settings = BridgeSettings::new(LOCALHOST, (port, 7700), folder.join("tap.sock"));
    settings.frame_socket = Some(folder.join("frames.sock"));
    settings.cameras = vec![(1, CameraKind::CompressedImage), (2, CameraKind::Image)];
    settings.page_share = share;
    settings.camera_history.every = every;
    let (robot, live) = arm7();
    let bridge = Bridge::start(&settings, &robot, live, &Plots::new(&[])).unwrap();
    let capture = Capture::new(&folder.join("frames.sock"));
    (bridge, settings, capture)
}

/// Capture sending frames, as `kapto record` does.
struct Capture {
    feed: FrameFeed,
    sender: FrameSender,
    /// Each frame is offered a tenth of a second after the last, so each
    /// is due.
    tick: u64,
}

impl Capture {
    fn new(socket: &Path) -> Self {
        let config = FrameConfig {
            cameras: vec![(1, 1 << 20), (2, 1 << 20)],
            min_interval: Duration::from_millis(30),
        };
        let (feed, sender, _) = frames(&config, socket);
        Self {
            feed,
            sender,
            tick: 1,
        }
    }

    fn send(&mut self, stream: u16, frame: &[u8]) {
        let mut batch = Batch::new(1, frame.len());
        assert!(batch.push(stream, frame));
        batch.set_recv_time(RecvTime::from_nanos(self.tick * 100_000_000));
        self.tick += 1;
        self.feed.offer(&batch);
        let deadline = Instant::now() + Duration::from_secs(5);
        self.sender.send_until(deadline);
    }
}

/// Writes a message as CDR, little-endian.
struct Cdr(Vec<u8>);

impl Cdr {
    fn new() -> Self {
        Self(vec![0, 1, 0, 0])
    }

    fn u32(mut self, value: u32) -> Self {
        while !(self.0.len() - 4).is_multiple_of(4) {
            self.0.push(0);
        }
        self.0.extend(value.to_le_bytes());
        self
    }

    fn bytes(self, data: &[u8]) -> Self {
        let mut cdr = self.u32(u32::try_from(data.len()).unwrap());
        cdr.0.extend(data);
        cdr
    }

    fn string(self, text: &str) -> Self {
        let mut with_nul = text.as_bytes().to_vec();
        with_nul.push(0);
        self.bytes(&with_nul)
    }

    fn header(self) -> Self {
        self.u32(0).u32(0).string("camera")
    }
}

fn compressed_image(data: &[u8]) -> Vec<u8> {
    Cdr::new().header().string("jpeg").bytes(data).0
}

fn image(width: u32, height: u32, encoding: &str, data: &[u8]) -> Vec<u8> {
    let step = u32::try_from(data.len()).unwrap() / height;
    let mut cdr = Cdr::new().header().u32(height).u32(width).string(encoding);
    cdr.0.push(0);
    cdr.u32(step).bytes(data).0
}

fn a_jpeg(shade: u8) -> Vec<u8> {
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 90)
        .encode(&[shade; 16 * 16 * 3], 16, 16, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    jpeg
}

/// Connects as a page, returning it with its opening message.
fn watch(port: u16) -> (Page, Value) {
    let (mut page, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();
    let Message::Text(opening) = page.read().unwrap() else {
        panic!("no opening message")
    };
    (page, serde_json::from_str(&opening).unwrap())
}

/// A picture message as the specification lays it out.
#[derive(Debug)]
struct ReadPicture {
    stream: u16,
    format: u8,
    size: (u32, u32),
    received_utc_ns: i64,
    sent_utc_ns: i64,
    picture: Vec<u8>,
}

fn read_picture(bytes: &[u8]) -> ReadPicture {
    let u16_at = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let i64_at = |at: usize| i64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    assert_eq!(&bytes[..4], PICTURE_MAGIC);
    assert_eq!(u16_at(4), STREAM_VERSION);
    assert_eq!(&bytes[9..12], [0, 0, 0]);
    let length = usize::try_from(u32_at(36)).unwrap();
    assert_eq!(bytes.len(), PICTURE_HEADER + length);
    ReadPicture {
        stream: u16_at(6),
        format: bytes[8],
        size: (u32_at(12), u32_at(16)),
        received_utc_ns: i64_at(20),
        sent_utc_ns: i64_at(28),
        picture: bytes[PICTURE_HEADER..].to_vec(),
    }
}

/// The page's next picture, past frames and marks.
fn next_picture(page: &mut Page) -> ReadPicture {
    loop {
        if let Message::Binary(bytes) = page.read().unwrap()
            && bytes.starts_with(PICTURE_MAGIC)
        {
            return read_picture(&bytes);
        }
    }
}

/// The next thing the page is told about a camera, past everything else.
fn next_camera_said(page: &mut Page) -> Value {
    loop {
        if let Message::Text(text) = page.read().unwrap()
            && let Ok(said) = serde_json::from_str::<Value>(&text)
            && said.get("camera").is_some()
        {
            return said["camera"].clone();
        }
    }
}

#[test]
fn a_compressed_cameras_pictures_reach_a_page_byte_for_byte_and_a_raw_ones_as_jpeg() {
    let folder = scratch("bridge-pictures");
    let (bridge, settings, mut capture) = bridge(&folder);
    let (mut page, opening) = watch(settings.stream_port);
    assert_eq!(opening["cameras"], serde_json::json!([1, 2]));

    let jpeg = a_jpeg(90);
    let before = ClockSample::now().utc_ns;
    capture.send(1, &compressed_image(&jpeg));
    let picture = next_picture(&mut page);
    assert_eq!(
        (picture.stream, picture.format, picture.size),
        (1, 0, (0, 0))
    );
    assert_eq!(
        picture.picture, jpeg,
        "the camera's own JPEG, byte for byte"
    );
    assert!(picture.sent_utc_ns >= before, "sent now, on the wall clock");
    assert!(picture.received_utc_ns <= picture.sent_utc_ns);

    capture.send(2, &image(64, 32, "rgb8", &[30, 200, 30].repeat(64 * 32)));
    let picture = next_picture(&mut page);
    assert_eq!(
        (picture.stream, picture.format, picture.size),
        (2, 0, (64, 32))
    );
    let mut decoder = jpeg_decoder::Decoder::new(picture.picture.as_slice());
    let pixels = decoder.decode().unwrap();
    let info = decoder.info().unwrap();
    assert_eq!((info.width, info.height), (64, 32));
    assert!(pixels[1] > 180 && pixels[0] < 60, "{:?}", &pixels[..3]);

    let totals = bridge.camera_totals().unwrap();
    assert_eq!((totals.frames, totals.shown, totals.unshown), (2, 2, 0));
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_page_connecting_is_shown_each_cameras_newest_picture_and_told_why_one_isnt_shown() {
    let folder = scratch("bridge-pictures-late");
    let (bridge, settings, mut capture) = bridge(&folder);
    capture.send(1, &compressed_image(&a_jpeg(10)));
    capture.send(1, &compressed_image(&a_jpeg(250)));
    capture.send(2, &image(2, 2, "yuv422", &[0; 8]));
    // Until the bridge has read all three.
    let deadline = Instant::now() + Duration::from_secs(5);
    while bridge.camera_totals().unwrap().frames < 3 {
        assert!(Instant::now() < deadline, "{:?}", bridge.camera_totals());
        std::thread::sleep(Duration::from_millis(10));
    }

    let (mut page, _) = watch(settings.stream_port);
    let said = next_camera_said(&mut page);
    assert_eq!(said["stream"], 2);
    assert_eq!(
        said["unshown"],
        "raw pictures in `yuv422`, which the live view can't show: only rgb8, bgr8, rgba8, bgra8 and mono8, and depth in 16UC1, mono16 or 32FC1"
    );
    let picture = next_picture(&mut page);
    assert_eq!(picture.picture, a_jpeg(250), "the newest");

    // A picture shown again clears it.
    capture.send(2, &image(2, 2, "mono8", &[7; 4]));
    let said = next_camera_said(&mut page);
    assert_eq!(
        (said["stream"].clone(), said["unshown"].clone()),
        (2.into(), Value::Null)
    );
    assert_eq!(bridge.camera_totals().unwrap().unshown, 1);
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_page_that_stops_reading_misses_pictures_and_never_holds_up_the_bridge() {
    let folder = scratch("bridge-pictures-slow");
    let (bridge, settings, mut capture) = bridge(&folder);
    // Connected, but never read again.
    let (_page, _) = watch(settings.stream_port);
    let big = a_jpeg(1)
        .into_iter()
        .chain(vec![0; 900_000])
        .collect::<Vec<u8>>();
    let started = Instant::now();
    for _ in 0..60 {
        capture.send(1, &compressed_image(&big));
    }
    // A few more once the page has had longer than it takes to look: the
    // picture waiting for it all that while is dropped, not thinned.
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(250));
        capture.send(1, &compressed_image(&big));
    }
    // Every frame read, none of them waiting for the page.
    let deadline = Instant::now() + Duration::from_secs(10);
    while bridge.camera_totals().unwrap().frames < 63 {
        assert!(Instant::now() < deadline, "{:?}", bridge.camera_totals());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(started.elapsed() < Duration::from_secs(10));
    let stream = bridge.stream_stats();
    assert!(stream.pictures_dropped > 0, "{stream:?}");
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_pages_cameras_share_its_pictures_and_it_still_gets_each_ones_newest() {
    let folder = scratch("bridge-pictures-shared");
    // Four a second for the page's two cameras: one every half second each.
    let share = PageShare {
        pictures_per_second: 4,
        camera_max_fps: 30,
    };
    let (bridge, settings, mut capture) = bridge_sharing(&folder, share);
    let (mut page, _) = watch(settings.stream_port);
    for shade in 1..=30 {
        capture.send(1, &compressed_image(&a_jpeg(shade)));
    }
    // The newest arrives once its camera's share allows, with fewer before.
    let newest = a_jpeg(30);
    let mut received = 0;
    loop {
        let picture = next_picture(&mut page);
        received += 1;
        if picture.picture == newest {
            break;
        }
    }
    assert!(received < 30, "{received} of 30 pictures reached the page");
    let stream = bridge.stream_stats();
    assert!(stream.pictures_thinned > 0, "{stream:?}");
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

/// A small raw picture, all `shade`.
fn a_raw_frame(shade: u8) -> Vec<u8> {
    image(16, 8, "mono8", &[shade; 16 * 8])
}

/// Waits until the bridge has read `frames` frames.
fn until_read(bridge: &Bridge, frames: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while bridge.camera_totals().unwrap().frames < frames {
        assert!(Instant::now() < deadline, "{:?}", bridge.camera_totals());
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A raw picture 1280 pixels wide, twice what the look-back keeps.
fn a_wide_raw_frame(shade: u8) -> Vec<u8> {
    image(1280, 8, "mono8", &[shade; 1280 * 8])
}

/// Asks, as a page looking back, for camera `stream`'s picture kept
/// nearest `at`, returning its width and height.
fn looked_back(page: &mut Page, stream: u16, at: i64) -> (u32, u32) {
    page.send(Message::text(format!(
        r#"{{"picture":{{"id":1,"stream":{stream},"at":{at}}}}}"#
    )))
    .unwrap();
    loop {
        if let Message::Binary(answer) = page.read().unwrap()
            && answer.starts_with(LOOKED_BACK_MAGIC)
        {
            let u32_at = |at: usize| u32::from_le_bytes(answer[at..at + 4].try_into().unwrap());
            return (u32_at(12), u32_at(16));
        }
    }
}

#[test]
fn with_no_page_open_a_raw_camera_is_only_made_small_for_looking_back() {
    let folder = scratch("bridge-pictures-unwanted");
    // The look-back keeps a picture an hour: only the first is due.
    let hour = Duration::from_secs(3_600);
    let (bridge, settings, mut capture) = bridge_keeping(&folder, PageShare::default(), hour);
    for shade in 1..=5 {
        capture.send(2, &a_wide_raw_frame(shade));
    }
    until_read(&bridge, 5);
    let totals = bridge.camera_totals().unwrap();
    assert_eq!((totals.shown, totals.not_encoded), (0, 4), "{totals:?}");
    // The first was made small from its pixels, as the look-back keeps it.
    let (mut page, _) = watch(settings.stream_port);
    assert_eq!(looked_back(&mut page, 2, 0), (640, 4));
    // A page open wants the next whole.
    capture.send(2, &a_wide_raw_frame(6));
    let picture = next_picture(&mut page);
    assert_eq!((picture.stream, picture.size), (2, (1280, 8)));
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_raw_camera_is_encoded_only_as_often_as_a_pages_share_takes_it() {
    let folder = scratch("bridge-pictures-share-raw");
    // One picture a second for the page, and none kept for looking back
    // after the first: frames a tenth of a second apart are mostly
    // unwanted.
    let share = PageShare {
        pictures_per_second: 1,
        camera_max_fps: 30,
    };
    let hour = Duration::from_secs(3_600);
    let (bridge, settings, mut capture) = bridge_keeping(&folder, share, hour);
    let (mut page, _) = watch(settings.stream_port);
    capture.send(2, &a_raw_frame(1));
    let first = next_picture(&mut page);
    assert_eq!(first.stream, 2);
    for shade in 2..=4 {
        capture.send(2, &a_raw_frame(shade));
    }
    until_read(&bridge, 4);
    let totals = bridge.camera_totals().unwrap();
    assert_eq!((totals.shown, totals.not_encoded), (1, 3), "{totals:?}");
    drop(bridge);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_frame_socket_that_cannot_be_listened_on_stops_the_bridge_with_its_cause() {
    let folder = scratch("bridge-pictures-busy");
    let mut settings = BridgeSettings::new(LOCALHOST, (0, 7700), folder.join("tap.sock"));
    settings.frame_socket = Some(folder.join("in-the-way"));
    settings.cameras = vec![(1, CameraKind::Jpeg { skip: 0 })];
    std::fs::write(folder.join("in-the-way"), "not a socket").unwrap();
    let (robot, live) = arm7();
    let Err(error) = Bridge::start(&settings, &robot, live, &Plots::new(&[])) else {
        panic!("started with its frame socket in the way")
    };
    assert_eq!(error.code(), ErrorCode::ViewerFrameSocketFailed);
    assert!(error.report().contains("in-the-way"), "{}", error.report());
    // The file in the way is kept.
    assert_eq!(
        std::fs::read_to_string(folder.join("in-the-way")).unwrap(),
        "not a socket"
    );
    std::fs::remove_dir_all(folder).unwrap();
}
