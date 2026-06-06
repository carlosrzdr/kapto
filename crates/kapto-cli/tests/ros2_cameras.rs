// SPDX-License-Identifier: Apache-2.0

//! Camera topics recorded whole by `kapto record`, as rosbag2 records them:
//! a `sensor_msgs/msg/CompressedImage` stream from the synthetic camera and
//! a `sensor_msgs/msg/Image` one from a node of its own, each frame far
//! larger than a datagram, each found byte for byte in the recording, which
//! ROS 2's own tools read. Only with the `ros2` feature, run in the ROS 2
//! image over each middleware by `cargo xtask ros2`.

#![cfg(all(unix, feature = "ros2"))]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use kapto_loadgen::camera::Frames;
use kapto_loadgen::{Config, Transport, run};

/// A folder of its own for `test`, with a `streams.toml` declaring one
/// camera topic of `message_type`, its frames up to 2 MiB.
fn package(test: &str, topic: &str, message_type: &str) -> PathBuf {
    let folder =
        std::env::temp_dir().join(format!("kapto-ros2-cameras-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("streams.toml"),
        format!(
            "[streams.camera]\nrate_hz = 30\nmax_message_bytes = 2097152\n\
             ros2 = {{ topic = \"{topic}\", type = \"{message_type}\", \
             qos = {{ reliability = \"reliable\", depth = 10 }} }}\n"
        ),
    )
    .unwrap();
    folder
}

/// `kapto record` on `folder`'s streams for `seconds`, started.
fn record(folder: &Path, seconds: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["record", "--duration", seconds, "--streams"])
        .arg(folder.join("streams.toml"))
        .arg("--data-dir")
        .arg(folder.join("data"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The finished episode's folder once `recording` has ended.
fn episode(folder: &Path, recording: Child) -> PathBuf {
    let output = recording.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_dir(folder.join("data"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("_FINALISED").exists())
        .unwrap()
}

/// Every message recorded on the `camera` channel, in order.
fn camera_messages(episode: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(episode.join("recording.mcap")).unwrap();
    mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .filter(|message| message.channel.topic == "camera")
        .map(|message| message.data.into_owned())
        .collect()
}

/// ROS 2's own tool reads the recording as a bag, with `count` messages of
/// `message_type` on the camera's channel.
fn bag_info_says(episode: &Path, message_type: &str, count: usize) {
    let info = Command::new("ros2")
        .args(["bag", "info", "--storage", "mcap"])
        .arg(episode.join("recording.mcap"))
        .output()
        .unwrap();
    let info = String::from_utf8_lossy(&info.stdout).into_owned();
    let line = info
        .lines()
        .find(|line| line.contains("Topic: camera "))
        .unwrap_or_else(|| panic!("{info}"));
    assert!(line.contains(&format!("Type: {message_type}")), "{line}");
    assert!(line.contains(&format!("Count: {count} ")), "{line}");
}

/// Walks a message serialised as CDR, little-endian, field by field.
struct Cdr<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cdr<'a> {
    fn new(message: &'a [u8]) -> Self {
        // After the 4-byte encapsulation header, which alignment ignores.
        assert_eq!(&message[..2], [0, 1], "little-endian CDR");
        Self {
            bytes: &message[4..],
            at: 0,
        }
    }

    fn u8(&mut self) -> u8 {
        self.at += 1;
        self.bytes[self.at - 1]
    }

    fn u32(&mut self) -> u32 {
        self.at = self.at.next_multiple_of(4);
        self.at += 4;
        u32::from_le_bytes(self.bytes[self.at - 4..self.at].try_into().unwrap())
    }

    /// A string, without its terminating NUL.
    fn string(&mut self) -> &'a str {
        let length = usize::try_from(self.u32()).unwrap();
        self.at += length;
        std::str::from_utf8(&self.bytes[self.at - length..self.at - 1]).unwrap()
    }

    fn bytes(&mut self) -> &'a [u8] {
        let length = usize::try_from(self.u32()).unwrap();
        self.at += length;
        &self.bytes[self.at - length..self.at]
    }

    /// A `std_msgs/msg/Header`: its stamp and frame ID.
    fn header(&mut self) -> &'a str {
        self.u32();
        self.u32();
        self.string()
    }
}

#[test]
fn compressed_camera_frames_are_recorded_whole_every_one_from_the_first() {
    let topic = "/camera/image_raw/compressed";
    let folder = package("compressed", topic, "sensor_msgs/msg/CompressedImage");
    let recording = record(&folder, "10");
    let camera = Config {
        target: "127.0.0.1:9".parse().unwrap(),
        streams: 1,
        rate_hz: 30,
        duration: Some(Duration::from_secs(2)),
        spin: Duration::from_micros(100),
        transport: Transport::Ros2,
        wait_for_subscribers: Some(Duration::from_secs(8)),
        camera: Some((640, 480)),
    };
    let (sent, _) = run(&camera, |_, _| {}).unwrap();
    assert_eq!(sent.send_errors, 0);
    let episode = episode(&folder, recording);

    let messages = camera_messages(&episode);
    assert_eq!(messages.len(), 60, "every frame sent");
    let frames = Frames::new(640, 480, 30).unwrap();
    for (tick, message) in (0..).zip(&messages) {
        let mut cdr = Cdr::new(message);
        cdr.header();
        assert_eq!(cdr.string(), "jpeg");
        assert!(
            cdr.bytes() == frames.frame(0, tick),
            "frame {tick} isn't the one sent"
        );
    }
    bag_info_says(&episode, "sensor_msgs/msg/CompressedImage", messages.len());
    let _ = std::fs::remove_dir_all(folder);
}

/// Publishes 60 raw 640x480 RGB frames on `topic` at 30 a second, once it
/// has a subscriber, as a camera's driver node would: frame `n` every byte
/// `n`.
const RAW_CAMERA: &str = r"
import array, sys, time
import rclpy
from rclpy.qos import QoSProfile, ReliabilityPolicy
from sensor_msgs.msg import Image

rclpy.init()
node = rclpy.create_node('kapto_test_camera')
publisher = node.create_publisher(
    Image, sys.argv[1], QoSProfile(depth=10, reliability=ReliabilityPolicy.RELIABLE))
deadline = time.time() + 15
while publisher.get_subscription_count() == 0 and time.time() < deadline:
    time.sleep(0.05)
for n in range(60):
    image = Image(height=480, width=640, encoding='rgb8', step=640 * 3)
    image.header.frame_id = 'camera'
    image.data = array.array('B', bytes([n]) * (640 * 480 * 3))
    publisher.publish(image)
    time.sleep(1 / 30)
time.sleep(1)
node.destroy_node()
rclpy.shutdown()
";

#[test]
fn raw_camera_frames_of_900_kib_are_recorded_whole() {
    let topic = format!("/kapto_raw_camera_{}", std::process::id());
    let folder = package("raw", &topic, "sensor_msgs/msg/Image");
    let recording = record(&folder, "12");
    let camera = Command::new("python3")
        .args(["-c", RAW_CAMERA, &topic])
        .status()
        .unwrap();
    assert!(camera.success());
    let episode = episode(&folder, recording);

    let messages = camera_messages(&episode);
    assert_eq!(messages.len(), 60, "every frame sent");
    for (n, message) in (0_u8..).zip(&messages) {
        let mut cdr = Cdr::new(message);
        assert_eq!(cdr.header(), "camera");
        assert_eq!((cdr.u32(), cdr.u32()), (480, 640));
        assert_eq!(cdr.string(), "rgb8");
        cdr.u8();
        assert_eq!(cdr.u32(), 640 * 3);
        let data = cdr.bytes();
        assert_eq!(data.len(), 640 * 480 * 3);
        assert!(data.iter().all(|&byte| byte == n), "frame {n} changed");
    }
    bag_info_says(&episode, "sensor_msgs/msg/Image", messages.len());
    let _ = std::fs::remove_dir_all(folder);
}
