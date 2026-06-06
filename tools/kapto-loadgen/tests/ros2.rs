// SPDX-License-Identifier: Apache-2.0

//! The generator publishing on ROS 2 topics, read back by a subscriber in
//! another process, as a robot's consumer would, with ROS 2 deserialising
//! the messages itself. Only with the `ros2` feature, run in the ROS 2 image
//! over each middleware by `cargo xtask ros2`.

#![cfg(feature = "ros2")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use futures::{FutureExt, StreamExt};
use kapto_loadgen::pacing::Schedule;
use kapto_loadgen::payload::{JOINTS, gripper_at, joints_at};
use r2r::sensor_msgs::msg::{CompressedImage, JointState};
use r2r::{Context, Node, QosProfile};

const RATE_HZ: u32 = 2_000;

/// Ticks searched for the one the first message received was sent at:
/// discovery takes well under the run's few seconds.
const TICKS_SEARCHED: u64 = 6 * RATE_HZ as u64;

/// The tick whose sample `message` is, of `stream`, if any of the first
/// [`TICKS_SEARCHED`] is.
fn tick_of(stream: u16, message: &JointState) -> Option<u64> {
    let schedule = Schedule::new(Instant::now(), RATE_HZ);
    (0..TICKS_SEARCHED).find(|&tick| {
        let seconds = schedule.offset(tick).as_secs_f64();
        message.position[..JOINTS] == joints_at(stream, seconds)
    })
}

/// Checks that `messages`, all received on stream `stream`'s topic, are
/// consecutive samples from the generator from its first, each exactly as
/// it was sent, and that their stamps keep to the rate within 1 %.
fn check_stream(stream: u16, messages: &[JointState]) {
    assert!(
        messages.len() >= RATE_HZ as usize,
        "stream {stream}: only {} messages",
        messages.len()
    );
    let names = [
        "joint_1", "joint_2", "joint_3", "joint_4", "joint_5", "joint_6", "joint_7", "gripper",
    ];
    assert!(messages.iter().all(|message| message.name == names));

    let first = tick_of(stream, &messages[0])
        .unwrap_or_else(|| panic!("stream {stream}: the first message isn't a sample sent"));
    // It waited for this subscriber, so none went before it could arrive.
    assert_eq!(
        first, 0,
        "stream {stream}: the first {first} messages missed"
    );
    let schedule = Schedule::new(Instant::now(), RATE_HZ);
    for (tick, message) in (first..).zip(messages) {
        let seconds = schedule.offset(tick).as_secs_f64();
        let mut expected = joints_at(stream, seconds).to_vec();
        expected.push(gripper_at(stream, seconds));
        assert_eq!(message.position, expected, "stream {stream}, tick {tick}");
    }

    let nanoseconds = |message: &JointState| {
        let stamp = &message.header.stamp;
        i64::from(stamp.sec) * 1_000_000_000 + i64::from(stamp.nanosec)
    };
    let span = nanoseconds(&messages[messages.len() - 1]) - nanoseconds(&messages[0]);
    #[allow(
        clippy::cast_precision_loss,
        reason = "a few seconds of nanoseconds and messages, far below 2^52"
    )]
    let rate = (messages.len() - 1) as f64 * 1e9 / span as f64;
    let expected = f64::from(RATE_HZ);
    assert!(
        (rate - expected).abs() <= expected * 0.01,
        "stream {stream}: {rate:.1} Hz"
    );
}

#[test]
fn two_streams_at_2_khz_arrive_on_their_topics_every_message_from_the_first_in_order_at_the_rate() {
    let context = Context::create().unwrap();
    let mut node = Node::create(context, "kapto_loadgen_test", "").unwrap();
    let qos = QosProfile {
        depth: 1000,
        ..QosProfile::default()
    };
    let mut topics: Vec<_> = ["/joint_states", "/joint_states_1"]
        .into_iter()
        .map(|topic| node.subscribe::<JointState>(topic, qos.clone()).unwrap())
        .collect();

    let mut generator = Command::new(env!("CARGO_BIN_EXE_kapto-loadgen"))
        .args(["--transport", "ros2", "--streams", "2", "--duration", "4"])
        .args(["--rate", &RATE_HZ.to_string()])
        .args(["--wait-for-subscribers", "10"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

    let mut received: Vec<Vec<JointState>> = vec![Vec::new(), Vec::new()];
    let mut ended: Option<Instant> = None;
    let started = Instant::now();
    // Until a second after the generator ended, for what is still on its way.
    while ended.is_none_or(|ended| ended.elapsed() < Duration::from_secs(1)) {
        assert!(started.elapsed() < Duration::from_secs(30), "no end");
        // r2r hands over one message per topic per spin, into a channel
        // of 10 that drops what doesn't fit: emptied after every spin.
        node.spin_once(Duration::from_millis(1));
        for (topic, messages) in topics.iter_mut().zip(&mut received) {
            while let Some(Some(message)) = topic.next().now_or_never() {
                messages.push(message);
            }
        }
        if ended.is_none() && generator.try_wait().unwrap().is_some() {
            ended = Some(Instant::now());
        }
    }
    assert!(generator.wait().unwrap().success());

    for (stream, messages) in (0..).zip(&received) {
        check_stream(stream, messages);
    }
}

#[test]
fn waiting_for_subscribers_fails_naming_the_topics_nobody_subscribed_to() {
    // The other test subscribes to the first two topics at most.
    let output = Command::new(env!("CARGO_BIN_EXE_kapto-loadgen"))
        .args(["--transport", "ros2", "--streams", "4", "--duration", "1"])
        .args(["--wait-for-subscribers", "0.5"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("/joint_states_2, /joint_states_3 within 0.5 s"),
        "{stderr}"
    );
}

#[test]
fn a_camera_publishes_every_frame_as_a_compressed_jpeg_image_at_its_rate() {
    let context = Context::create().unwrap();
    let mut node = Node::create(context, "kapto_loadgen_camera_test", "").unwrap();
    let qos = QosProfile {
        depth: 10,
        ..QosProfile::default()
    };
    let mut topic = node
        .subscribe::<CompressedImage>("/camera/image_raw/compressed", qos)
        .unwrap();
    let mut generator = Command::new(env!("CARGO_BIN_EXE_kapto-loadgen"))
        .args(["--transport", "ros2", "--camera", "320x240", "--rate", "30"])
        .args(["--duration", "3", "--wait-for-subscribers", "10"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

    let mut frames = Vec::new();
    let mut ended: Option<Instant> = None;
    let started = Instant::now();
    while ended.is_none_or(|ended| ended.elapsed() < Duration::from_secs(1)) {
        assert!(started.elapsed() < Duration::from_secs(30), "no end");
        node.spin_once(Duration::from_millis(1));
        while let Some(Some(frame)) = topic.next().now_or_never() {
            frames.push(frame);
        }
        if ended.is_none() && generator.try_wait().unwrap().is_some() {
            ended = Some(Instant::now());
        }
    }
    assert!(generator.wait().unwrap().success());

    assert_eq!(frames.len(), 90, "every frame, from the first");
    for frame in &frames {
        assert_eq!(frame.format, "jpeg");
        let mut decoder = jpeg_decoder::Decoder::new(frame.data.as_slice());
        decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (320, 240));
    }
    let nanoseconds = |frame: &CompressedImage| {
        i64::from(frame.header.stamp.sec) * 1_000_000_000 + i64::from(frame.header.stamp.nanosec)
    };
    let span = nanoseconds(&frames[frames.len() - 1]) - nanoseconds(&frames[0]);
    #[allow(clippy::cast_precision_loss, reason = "90 frames over 3 s")]
    let rate = (frames.len() - 1) as f64 * 1e9 / span as f64;
    assert!((rate - 30.0).abs() <= 0.3, "{rate:.2} frames a second");
}
