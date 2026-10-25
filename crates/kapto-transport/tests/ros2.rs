// SPDX-License-Identifier: Apache-2.0

//! ROS 2 topics through rcl, against a real node in a process of its own, as
//! a robot's would be. Only with the `ros2` feature, run in the ROS 2 image
//! over each middleware by `cargo xtask ros2`.

#![cfg(feature = "ros2")]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]
#![allow(
    unsafe_code,
    reason = "a global allocator can only be written with unsafe trait methods"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use kapto_capture::Batch;
use kapto_core::ErrorCode;
use kapto_decode::cdr::{assemble, msg_files_in};
use kapto_decode::{CdrLayout, DecoderDeclaration, PathField, ScalarType, Value};
use kapto_transport::Transport;
use kapto_transport::ros2::{Durability, Qos, Reliability, Ros2Subscription, Ros2Transport};

struct CountingAllocator;

thread_local! {
    // A `const` initialiser: reading it never allocates.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

// SAFETY: every method delegates to the system allocator with the arguments
// it was given, which upholds `GlobalAlloc`'s contract; counting has no effect
// on the memory handed out. Memory rcl and the middleware allocate in C
// doesn't come through here: this counts Kapto's own.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// One test at a time: `r2r`, which lists the topics, sets up ROS 2's
/// logging for the whole process when it starts, and Fast DDS crashes if
/// Kapto's own node starts in another thread meanwhile. Kapto never does
/// both in one process: listing topics is a command of its own.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static ROS2: Mutex<()> = Mutex::new(());
    ROS2.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `ros2 topic pub` publishing on `topic`, stopped when dropped.
struct Publisher(Child);

impl Publisher {
    fn start(topic: &str) -> Self {
        let child = Command::new("ros2")
            .args(["topic", "pub", "--rate", "5", topic])
            .args(["std_msgs/msg/String", "{data: kapto}"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self(child)
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_topic_published_on_the_machine_is_listed_with_its_type() {
    let _ros2 = one_at_a_time();
    let name = format!("/kapto_test_{}", std::process::id());
    let _publisher = Publisher::start(&name);

    // Long enough for the publisher, a Python program, to start.
    let topics = kapto_transport::ros2::topics(Duration::from_secs(4)).unwrap();
    let topic = topics.iter().find(|topic| topic.name == name);
    assert_eq!(
        topic.map(|topic| topic.types.clone()),
        Some(vec!["std_msgs/msg/String".to_owned()]),
        "{topics:?}"
    );
    let names: Vec<&str> = topics.iter().map(|topic| topic.name.as_str()).collect();
    assert!(names.is_sorted(), "{names:?}");
}

/// What `ros2 topic info --verbose` says of `topic` from a process of its
/// own, once it says Kapto's node subscribes to it, or after 20 s.
fn topic_info(topic: &str) -> String {
    let started = Instant::now();
    loop {
        let output = Command::new("ros2")
            .args(["topic", "info", "--verbose", topic])
            .output()
            .unwrap();
        let info = String::from_utf8_lossy(&output.stdout).into_owned();
        if info.contains("Node name: kapto_test") || started.elapsed() > Duration::from_secs(20) {
            return info;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn a_topic_is_subscribed_to_with_its_type_and_qos_as_ros_2_reports_them() {
    let _ros2 = one_at_a_time();
    let topic = format!("/kapto_subscribed_{}", std::process::id());
    let subscription = |topic: &str, reliability, durability, depth| Ros2Subscription {
        stream: 0,
        topic: topic.to_owned(),
        // Not among the types `r2r` was built with: loaded by name.
        message_type: "sensor_msgs/msg/JointState".to_owned(),
        qos: Qos {
            reliability,
            durability,
            depth,
        },
    };
    let reliable = format!("{topic}_reliable");
    let _transport = Ros2Transport::open(
        "kapto_test",
        &[
            subscription(&topic, Reliability::BestEffort, Durability::Volatile, 10),
            subscription(
                &reliable,
                Reliability::Reliable,
                Durability::TransientLocal,
                250,
            ),
        ],
    )
    .unwrap();

    let info = topic_info(&topic);
    assert!(info.contains("Type: sensor_msgs/msg/JointState"), "{info}");
    assert!(info.contains("Subscription count: 1"), "{info}");
    assert!(info.contains("Reliability: BEST_EFFORT"), "{info}");
    assert!(info.contains("Durability: VOLATILE"), "{info}");
    let info = topic_info(&reliable);
    assert!(info.contains("Reliability: RELIABLE"), "{info}");
    assert!(info.contains("Durability: TRANSIENT_LOCAL"), "{info}");
    // The depth isn't checked: it is the subscriber's own, which Fast DDS
    // doesn't announce to other nodes ("UNKNOWN").
}

/// Publishes `count` joint states on `topic` once Kapto has subscribed,
/// reliably, the `n`th with one joint at position `n` whose name is
/// `name_bytes` long.
const PUBLISHER: &str = r"
import sys, time, rclpy
from rclpy.node import Node
from rclpy.qos import QoSProfile, ReliabilityPolicy
from sensor_msgs.msg import JointState
topic, count, name_bytes = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rclpy.init()
node = Node('kapto_test_publisher')
qos = QoSProfile(depth=count, reliability=ReliabilityPolicy.RELIABLE)
publisher = node.create_publisher(JointState, topic, qos)
while publisher.get_subscription_count() == 0:
    time.sleep(0.05)
time.sleep(0.5)
for n in range(count):
    publisher.publish(JointState(name=['j' * name_bytes], position=[float(n)]))
    time.sleep(0.002)
time.sleep(2)
";

/// The publisher above, in a process of its own, stopped when dropped.
struct JointStates(Child);

impl JointStates {
    fn publish(topic: &str, count: usize, name_bytes: usize) -> Self {
        let child = Command::new("python3")
            .args(["-c", PUBLISHER, topic])
            .arg(count.to_string())
            .arg(name_bytes.to_string())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        Self(child)
    }
}

impl Drop for JointStates {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Each joint state's first position, read with its definition from the
/// ROS 2 installation: `None` for one that doesn't read whole.
fn first_positions(messages: &[Vec<u8>]) -> Vec<Option<f64>> {
    let share: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("AMENT_PREFIX_PATH").unwrap())
            .map(|prefix| prefix.join("share"))
            .collect();
    let type_name = "sensor_msgs/msg/JointState";
    let decoder = DecoderDeclaration::Cdr(CdrLayout {
        type_name: type_name.to_owned(),
        definition: assemble(type_name, &mut msg_files_in(&share)).unwrap(),
        fields: vec![PathField::new("position", "position[0]", ScalarType::F64)],
    })
    .compile(65_507)
    .unwrap();
    messages
        .iter()
        .map(|message| {
            let mut out = [Value::default()];
            decoder.decode(message, &mut out).ok()?;
            match out[0] {
                Value::Float(position) => Some(position),
                _ => None,
            }
        })
        .collect()
}

fn joint_states(stream: u16, topic: &str) -> Ros2Subscription {
    Ros2Subscription {
        stream,
        topic: topic.to_owned(),
        message_type: "sensor_msgs/msg/JointState".to_owned(),
        qos: Qos {
            reliability: Reliability::Reliable,
            durability: Durability::Volatile,
            depth: 1000,
        },
    }
}

/// A message taken: its stream, its bytes and its source time.
type Taken = (
    u16,
    Vec<u8>,
    Option<u64>,
    Option<kapto_capture::Publication>,
);

/// Receives `stream` from `transport` in batches of `slot_len`-byte slots
/// until `count` messages were kept or counted as truncated, or 30 s: the
/// messages kept, and how many allocations receiving made after the first.
fn receive_until(
    transport: &mut Ros2Transport,
    stream: u16,
    count: u64,
    slot_len: usize,
) -> (Vec<Taken>, u64) {
    let mut received = Vec::new();
    let mut batch = Batch::new(64, slot_len);
    let mut allocated = 0;
    let handled = |received: &[_], transport: &Ros2Transport| {
        received.len() as u64 + transport.stats().truncated(stream)
    };
    let started = Instant::now();
    while handled(&received, transport) < count && started.elapsed() < Duration::from_secs(30) {
        let warm = handled(&received, transport) > 0;
        let before = allocations();
        transport
            .receive(&mut batch, Duration::from_millis(100))
            .unwrap();
        if warm {
            allocated += allocations() - before;
        }
        // Copied out after counting, which allocates.
        received.extend(
            batch
                .messages()
                .enumerate()
                .map(|(index, (stream, message))| {
                    (
                        stream,
                        message.to_vec(),
                        batch.source_time(index),
                        batch.publication(index),
                    )
                }),
        );
        batch.clear();
    }
    (received, allocated)
}

#[test]
fn messages_from_another_process_arrive_whole_and_in_order_without_kapto_allocating() {
    let _ros2 = one_at_a_time();
    let topic = format!("/kapto_joints_{}", std::process::id());
    let mut transport = Ros2Transport::open("kapto_test", &[joint_states(1, &topic)]).unwrap();
    let _publisher = JointStates::publish(&topic, 500, 1);

    let (received, allocated) = receive_until(&mut transport, 1, 500, 256);
    assert_eq!(received.len(), 500);
    assert!(received.iter().all(|(stream, _, _, _)| *stream == 1));
    // Each with when its publisher sent it, on the same machine's clock
    // here: within the last minute, and never earlier than the one before.
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let published: Vec<u64> = received
        .iter()
        .map(|(_, _, time, _)| time.unwrap())
        .collect();
    let minute_ago = now.checked_sub(Duration::from_secs(60)).unwrap();
    let minute_ago = u64::try_from(minute_ago.as_nanos()).unwrap();
    assert!(published[0] > minute_ago, "{published:?}");
    assert!(published.is_sorted(), "{published:?}");
    // Each with its publisher's count of it, one after another, where the
    // middleware gives it: Cyclone DDS doesn't.
    let counts: Vec<Option<(u8, u64)>> = received
        .iter()
        .map(|(_, _, _, publication)| publication.map(|each| (each.publisher, each.number)))
        .collect();
    if std::env::var("RMW_IMPLEMENTATION").as_deref() == Ok("rmw_cyclonedds_cpp") {
        assert!(counts.iter().all(Option::is_none), "{counts:?}");
    } else {
        let first = counts[0].unwrap().1;
        let expected: Vec<Option<(u8, u64)>> = (first..first + 500)
            .map(|number| Some((0, number)))
            .collect();
        assert_eq!(counts, expected);
    }
    // Each whole, as ROS 2 serialised it, in the order published. Not
    // compared byte for byte with another serialisation: ROS 2 leaves the
    // padding between fields uncleared, so two serialisations differ there.
    let messages: Vec<Vec<u8>> = received
        .into_iter()
        .map(|(_, message, _, _)| message)
        .collect();
    let expected: Vec<Option<f64>> = (0..500).map(|n| Some(f64::from(n))).collect();
    assert_eq!(first_positions(&messages), expected);
    assert_eq!(allocated, 0, "Kapto allocated while receiving");
    assert_eq!(transport.stats().truncated(1), 0);
}

#[test]
fn a_message_larger_than_its_slot_is_counted_as_truncated_and_the_rest_kept() {
    let _ros2 = one_at_a_time();
    let topic = format!("/kapto_long_names_{}", std::process::id());
    let mut transport = Ros2Transport::open("kapto_test", &[joint_states(0, &topic)]).unwrap();
    // Each message is over 300 bytes, its joint's name alone 300.
    let _publisher = JointStates::publish(&topic, 20, 300);

    let (received, _) = receive_until(&mut transport, 0, 20, 256);
    assert_eq!(received, []);
    assert_eq!(transport.stats().truncated(0), 20);
}

#[test]
fn a_type_not_installed_here_is_refused_naming_it() {
    let _ros2 = one_at_a_time();
    let missing = Ros2Subscription {
        stream: 0,
        topic: "/kapto_nothing".to_owned(),
        message_type: "kapto_missing_msgs/msg/Nothing".to_owned(),
        qos: Qos {
            reliability: Reliability::BestEffort,
            durability: Durability::Volatile,
            depth: 10,
        },
    };
    let Err(error) = Ros2Transport::open("kapto_test", &[missing]) else {
        panic!("subscribed to a type that isn't installed");
    };
    assert_eq!(error.code(), ErrorCode::TransportRos2TypeUnavailable);
    assert!(error.to_string().contains("kapto_missing_msgs"), "{error}");
}
