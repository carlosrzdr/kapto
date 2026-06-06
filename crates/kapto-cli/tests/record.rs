// SPDX-License-Identifier: Apache-2.0

//! `kapto record` as a user runs it: the real binary records the synthetic
//! robot, or a robot connecting over TCP, and the finalised episode holds
//! every message it sent.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{free_tcp_ports, free_udp_port};
use kapto_loadgen::Transport;
use kapto_store::{Episode, LossDetectionKind, verify_episode};
use kapto_testkit::TempDir;

/// The first of `count` consecutive loopback ports free now for UDP and TCP.
/// Each test runs in its own process, so the search starts at a place that
/// depends on the process: tests running at the same time don't pick the
/// same ports before either has bound them.
fn free_ports(count: u16) -> u16 {
    let bases: Vec<u16> = (20_000..60_000).step_by(97).collect();
    let start = usize::try_from(std::process::id()).unwrap() % bases.len();
    for &base in bases[start..].iter().chain(&bases[..start]) {
        let free = (0..count).all(|offset| {
            let address = ("127.0.0.1", base + offset);
            UdpSocket::bind(address).is_ok() && TcpListener::bind(address).is_ok()
        });
        if free {
            return base;
        }
    }
    panic!("no {count} consecutive free ports on loopback");
}

/// A streams.toml for `count` load-generator streams from `port`, over UDP
/// or, for a generator that connects, TCP, with the sequence number where
/// the load generator puts it.
fn streams_file(dir: &Path, port: u16, count: u16, rate_hz: u32, transport: Transport) -> PathBuf {
    let mut text = String::new();
    for stream in 0..count {
        let address = format!("127.0.0.1:{}", port + stream);
        let input = match transport {
            Transport::TcpConnect => format!(
                "tcp = {{ listen = \"{address}\", framing = {{ kind = \"length_prefix\", length_bytes = 4 }} }}"
            ),
            _ => format!("udp = {{ bind = \"{address}\" }}"),
        };
        let _ = write!(
            text,
            "[streams.joints_{stream}]\nrate_hz = {rate_hz}\n{input}\n\
             sequence = {{ offset = 0, type = \"u32\" }}\n\n"
        );
    }
    let path = dir.join("streams.toml");
    std::fs::write(&path, text).unwrap();
    path
}

/// Starts `kapto record` and returns once it has started recording. Live
/// samples go to `tap.sock` in the data directory's parent folder.
fn start_recording(streams: &Path, data: &Path, seconds: u32) -> (Child, Vec<String>) {
    start_recording_with(streams, data, seconds, &[], &[])
}

/// The same, with `args` added to the command, and only `env` in its
/// environment.
fn start_recording_with(
    streams: &Path,
    data: &Path,
    seconds: u32,
    args: &[&str],
    env: &[(&str, &Path)],
) -> (Child, Vec<String>) {
    let tap_socket = data.parent().unwrap().join("tap.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["record", "--duration", &seconds.to_string(), "--streams"])
        .arg(streams)
        .args(args)
        .arg("--data-dir")
        .arg(data)
        .arg("--tap-socket")
        .arg(tap_socket)
        .env_clear()
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let mut seen = Vec::new();
    loop {
        let line = lines
            .next()
            .unwrap_or_else(|| panic!("kapto record exited early: {seen:#?}"));
        let line = line.unwrap();
        let started = line.ends_with("Started.");
        seen.push(line);
        if started {
            break;
        }
    }
    // Keep reading stderr so the process never blocks on a full pipe.
    std::thread::spawn(move || lines.for_each(drop));
    (child, seen)
}

/// Records `count` streams at `rate_hz` sent for `send_seconds`, and checks
/// that every message sent is in the finalised episode.
fn record_load(name: &str, count: u16, rate_hz: u32, send_seconds: u64, transport: Transport) {
    let dir = TempDir::new(name);
    let port = free_ports(count);
    let streams = streams_file(&dir.0, port, count, rate_hz, transport);
    let data = dir.0.join("data");
    let record_seconds = u32::try_from(send_seconds).unwrap() + 2;
    let (child, _) = start_recording(&streams, &data, record_seconds);

    let config = kapto_loadgen::Config {
        target: SocketAddr::from(([127, 0, 0, 1], port)),
        streams: count,
        rate_hz,
        duration: Some(Duration::from_secs(send_seconds)),
        spin: Duration::from_micros(100),
        transport,
        wait_for_subscribers: None,
        camera: None,
    };
    let (sent, _) = kapto_loadgen::run(&config, |_, _| {}).unwrap();
    assert_eq!(sent.send_errors, 0);
    let per_stream = sent.ticks;

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let episode_dir = std::fs::read_dir(&data)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| !path.ends_with(".recording"))
        .unwrap();

    let manifest = verify_episode(&episode_dir).unwrap();
    assert!(
        manifest
            .files
            .iter()
            .any(|file| file.path == "recording.mcap")
    );
    let episode = Episode::read(&episode_dir).unwrap();
    assert_eq!(episode.streams.len(), usize::from(count));
    for stream in &episode.streams {
        assert_eq!(stream.loss_detection, LossDetectionKind::Sequence);
        assert_eq!(
            (
                stream.received,
                stream.written,
                stream.dropped_by_us,
                stream.lost_upstream
            ),
            (per_stream, per_stream, 0, 0),
            "{}",
            stream.id
        );
    }

    let bytes = std::fs::read(episode_dir.join("recording.mcap")).unwrap();
    let in_file = mcap::MessageStream::new(&bytes).unwrap().count() as u64;
    assert_eq!(in_file, per_stream * u64::from(count));
}

#[test]
fn four_streams_at_2_khz_are_recorded_with_every_message_sent() {
    record_load("small", 4, 2_000, 2, Transport::Udp);
}

#[test]
fn four_streams_at_2_khz_over_tcp_are_recorded_with_every_message_sent() {
    record_load("small-tcp", 4, 2_000, 2, Transport::TcpConnect);
}

/// The full load: 32 000 messages a second for 30 s. Run it on a quiet
/// machine with `cargo nextest run -p kapto-cli --run-ignored all record`.
#[test]
#[ignore = "takes 35 s at full load; run it explicitly"]
fn sixteen_streams_at_2_khz_for_30_seconds_are_recorded_with_every_message_sent() {
    record_load("full", 16, 2_000, 30, Transport::Udp);
}

#[test]
fn a_viewer_that_stops_reading_never_costs_the_recording_a_message() {
    use std::os::unix::net::UnixDatagram;

    let dir = TempDir::new("stalled-viewer");
    let port = free_ports(4);
    // The load generator's streams, with their layout, so samples are sent.
    let mut text = String::new();
    for stream in 0..4 {
        let _ = write!(
            text,
            "[streams.joints_{stream}]\nrate_hz = 2000\n\
             udp = {{ bind = \"127.0.0.1:{}\" }}\n\
             sequence = {{ offset = 0, type = \"u32\" }}\n\
             decoder = {{ kind = \"struct\", fields = [\
               {{ name = \"joint_positions\", type = \"f64\", offset = 12, count = 7 }} ] }}\n",
            port + stream
        );
    }
    let streams = dir.0.join("streams.toml");
    std::fs::write(&streams, text).unwrap();
    // A viewer that binds its socket and then never reads, as one that hangs.
    let _viewer = UnixDatagram::bind(dir.0.join("tap.sock")).unwrap();
    let data = dir.0.join("data");
    let (child, _) = start_recording(&streams, &data, 4);

    let config = kapto_loadgen::Config {
        target: SocketAddr::from(([127, 0, 0, 1], port)),
        streams: 4,
        rate_hz: 2_000,
        duration: Some(Duration::from_secs(2)),
        spin: Duration::from_micros(100),
        transport: Transport::Udp,
        wait_for_subscribers: None,
        camera: None,
    };
    let (sent, _) = kapto_loadgen::run(&config, |_, _| {}).unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");

    let episode_dir = std::fs::read_dir(&data)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| !path.ends_with(".recording"))
        .unwrap();
    for stream in Episode::read(&episode_dir).unwrap().streams {
        assert_eq!(
            (stream.received, stream.written, stream.dropped_by_us),
            (sent.ticks, sent.ticks, 0),
            "{}",
            stream.id
        );
    }
    // The viewer's socket filled up and the rest of its samples were dropped.
    let live = stdout
        .lines()
        .find(|line| line.starts_with("Live view:"))
        .unwrap();
    let slow: u64 = live
        .split(", ")
        .find_map(|part| part.strip_suffix(" dropped because it was slow"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(slow > 0, "{live}");
}

#[test]
fn the_viewer_gets_fewer_samples_than_messages_and_still_sees_a_one_message_spike() {
    use std::os::unix::net::UnixDatagram;

    use kapto_capture::tap::wire::decode;

    let dir = TempDir::new("spike");
    let port = free_udp_port();
    let streams = dir.0.join("streams.toml");
    std::fs::write(
        &streams,
        format!(
            "[streams.force]\nrate_hz = 500\nudp = {{ bind = \"127.0.0.1:{port}\" }}\n\
             sequence = {{ offset = 0, type = \"u32\" }}\n\
             decoder = {{ kind = \"struct\", fields = [\
               {{ name = \"newtons\", type = \"f64\", offset = 4 }} ] }}\n"
        ),
    )
    .unwrap();
    let viewer = UnixDatagram::bind(dir.0.join("tap.sock")).unwrap();
    viewer.set_nonblocking(true).unwrap();
    let data = dir.0.join("data");
    let (child, _) = start_recording(&streams, &data, 2);

    // A flat signal with one message far above it, followed by a pause, so
    // that message is the newest of its batch and offered to the tap.
    let robot = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sent: u32 = 300;
    for number in 0..sent {
        let newtons: f64 = if number == 150 { 90.0 } else { 1.0 };
        let message = [number.to_le_bytes().as_slice(), &newtons.to_le_bytes()].concat();
        robot
            .send_to(&message, ("127.0.0.1", port.parse::<u16>().unwrap()))
            .unwrap();
        let pause = if number == 150 { 10 } else { 2 };
        std::thread::sleep(Duration::from_millis(pause));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let mut samples = Vec::new();
    let mut buffer = vec![0; 4096];
    while let Ok(length) = viewer.recv(&mut buffer) {
        samples.push(decode(&buffer[..length]).unwrap());
    }
    let summarised: u32 = samples.iter().map(|sample| sample.messages).sum();
    assert!(summarised <= sent, "{summarised} messages summarised");
    assert!(
        samples.len() < usize::try_from(sent / 2).unwrap(),
        "{} samples for {sent} messages: not decimated",
        samples.len()
    );
    let highest = samples
        .iter()
        .map(|sample| sample.maximums[0])
        .fold(f64::NEG_INFINITY, f64::max);
    assert_eq!(highest, 90.0, "the spike was lost");
    let lowest = samples
        .iter()
        .map(|sample| sample.minimums[0])
        .fold(f64::INFINITY, f64::min);
    assert_eq!(lowest, 1.0);
}

/// Sends messages `numbers` over `robot`: each a little-endian sequence
/// number and four bytes of payload.
fn send_numbered(robot: &mut TcpStream, numbers: std::ops::Range<u32>) {
    let bytes: Vec<u8> = numbers
        .flat_map(|number| [number.to_le_bytes(), *b"odom"].concat())
        .collect();
    robot.write_all(&bytes).unwrap();
}

#[test]
fn a_tcp_stream_is_recorded_across_a_reconnection_and_its_disconnect_counted() {
    let dir = TempDir::new("tcp");
    let [port] = free_tcp_ports();
    let streams = dir.0.join("streams.toml");
    std::fs::write(
        &streams,
        format!(
            "[streams.odometry]\nrate_hz = 1000\nsequence = {{ offset = 0, type = \"u32\" }}\n\
             tcp = {{ listen = \"127.0.0.1:{port}\", framing = {{ kind = \"fixed\", size = 8 }} }}\n"
        ),
    )
    .unwrap();
    let data = dir.0.join("data");
    let (child, _) = start_recording(&streams, &data, 2);

    let mut first = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    send_numbered(&mut first, 0..100);
    drop(first);
    let mut second = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    send_numbered(&mut second, 100..200);
    let output = child.wait_with_output().unwrap();
    drop(second);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("sequence, 1 disconnect"), "{stdout}");

    let episode_dir = std::fs::read_dir(&data)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| !path.ends_with(".recording"))
        .unwrap();
    let episode = Episode::read(&episode_dir).unwrap();
    let odometry = &episode.streams[0];
    assert_eq!(
        (
            odometry.received,
            odometry.written,
            odometry.dropped_by_us,
            odometry.lost_upstream,
            odometry.disconnects
        ),
        (200, 200, 0, 0, Some(1))
    );
}

/// Records for a second from `streams`, with `args` added to the command,
/// and returns the episode's `episode.json`, the name of every file the
/// recording carries, and what `kapto record` printed before starting.
fn record_a_second(
    streams: &Path,
    data: &Path,
    args: &[&str],
) -> (Episode, Vec<String>, Vec<String>) {
    record_a_second_in(streams, data, args, &[])
}

/// The same, with only `env` in its environment.
fn record_a_second_in(
    streams: &Path,
    data: &Path,
    args: &[&str],
    env: &[(&str, &Path)],
) -> (Episode, Vec<String>, Vec<String>) {
    let (child, seen) = start_recording_with(streams, data, 1, args, env);
    assert!(child.wait_with_output().unwrap().status.success());
    let episode_dir = std::fs::read_dir(data)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| !path.ends_with(".recording"))
        .unwrap();
    let episode = Episode::read(&episode_dir).unwrap();
    let bytes = std::fs::read(episode_dir.join(&episode.recording)).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    let carried = summary
        .attachment_indexes
        .iter()
        .map(|index| index.name.clone())
        .collect();
    (episode, carried, seen)
}

#[test]
fn an_episode_killed_mid_recording_is_recovered_by_the_next_recording_before_it_starts() {
    let dir = TempDir::new("record-killed");
    let streams = streams_file(&dir.0, free_ports(1), 1, 1000, Transport::Udp);
    let data = dir.0.join("data");
    let (mut child, _) = start_recording(&streams, &data, 60);
    // Longer than the writer takes to put what it wrote on disk.
    std::thread::sleep(Duration::from_millis(1500));
    // SIGKILL: nothing of it gets to finish the episode.
    child.kill().unwrap();
    child.wait().unwrap();
    let killed = std::fs::read_dir(data.join(".recording"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();

    let (child, seen) = start_recording(&streams, &data, 1);
    assert!(
        seen.iter()
            .any(|line| line.starts_with(&format!("Recovered episode {killed}"))),
        "{seen:#?}"
    );
    assert!(child.wait_with_output().unwrap().status.success());
    let recovered = data.join(&killed);
    verify_episode(&recovered).unwrap();
    assert!(Episode::read(&recovered).unwrap().recovered.is_some());
}

#[test]
fn an_episode_names_the_robot_package_its_streams_were_declared_in_or_none() {
    let dir = TempDir::new("package");
    let package = dir.0.join("arm7");
    let arm7 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7");
    std::fs::create_dir_all(package.join("urdf")).unwrap();
    for file in ["package.toml", "mapping.toml", "urdf/arm7.urdf"] {
        std::fs::copy(arm7.join(file), package.join(file)).unwrap();
    }
    std::fs::create_dir_all(package.join("meshes")).unwrap();
    std::fs::write(package.join("meshes/base.stl"), b"solid b\nendsolid b\n").unwrap();
    let port = free_udp_port();
    let streams = std::fs::read_to_string(arm7.join("streams.toml"))
        .unwrap()
        .replace("127.0.0.1:9000", &format!("127.0.0.1:{port}"));
    std::fs::write(package.join("streams.toml"), &streams).unwrap();

    let (episode, carried, seen) =
        record_a_second(&package.join("streams.toml"), &dir.0.join("data"), &[]);
    // The mesh is left out by default, and carried when asked for.
    assert!(
        !carried.iter().any(|name| name == "meshes/base.stl"),
        "{carried:?}"
    );
    let (_, with_meshes, _) = record_a_second(
        &package.join("streams.toml"),
        &dir.0.join("data-meshes"),
        &["--attach-meshes", "true"],
    );
    assert_eq!(
        with_meshes,
        [
            "mapping.toml",
            "meshes/base.stl",
            "package.toml",
            "streams.toml",
            "urdf/arm7.urdf"
        ]
    );
    let loaded = kapto_robot::load_package(&package).unwrap();
    let identity = episode.package.expect("recorded with the package");
    assert_eq!(
        (identity.id.as_str(), identity.hash.as_str()),
        ("arm7", loaded.hash.as_str())
    );
    assert_eq!(identity.version, loaded.manifest.version);
    assert!(
        seen.iter().any(|line| line.contains("robot package arm7")),
        "{seen:#?}"
    );

    // The same streams, outside any package: unchecked, and said so.
    let loose = dir.0.join("streams.toml");
    std::fs::write(&loose, &streams).unwrap();
    let (episode, carried, seen) = record_a_second(&loose, &dir.0.join("data-loose"), &[]);
    assert_eq!(episode.package, None);
    // It carries what it was recorded from: the stream declarations alone.
    assert_eq!(carried, ["streams.toml"]);
    assert!(
        seen.iter().any(|line| line.contains("unchecked")),
        "{seen:#?}"
    );

    // Given its mapping too, it carries that as well, where the checks
    // look for it, so they know the joints.
    let mapping = dir.0.join("arm-mapping.toml");
    std::fs::copy(arm7.join("mapping.toml"), &mapping).unwrap();
    let (episode, mut carried, _) = record_a_second(
        &loose,
        &dir.0.join("data-loose-mapped"),
        &["--mapping", mapping.to_str().unwrap()],
    );
    assert_eq!(episode.package, None);
    carried.sort();
    assert_eq!(carried, ["mapping.toml", "streams.toml"]);
}

#[test]
fn a_recording_carries_the_msg_files_it_needs_beyond_the_package_and_all_of_them_without_one() {
    let dir = TempDir::new("msg-files");
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    // Header is the package's own; Time only the ROS 2 installation has.
    let installation = dir.0.join("ros");
    write(
        &installation.join("share/builtin_interfaces/msg/Time.msg"),
        "int32 sec\nuint32 nanosec\n",
    );
    let package = dir.0.join("arm7");
    let arm7 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7");
    for file in ["package.toml", "mapping.toml", "urdf/arm7.urdf"] {
        write(
            &package.join(file),
            &std::fs::read_to_string(arm7.join(file)).unwrap(),
        );
    }
    write(
        &package.join("msg/std_msgs/msg/Header.msg"),
        "builtin_interfaces/Time stamp\nstring frame_id\n",
    );
    let [arm_port, stamp_port] = [free_udp_port(), free_udp_port()];
    let streams = std::fs::read_to_string(arm7.join("streams.toml"))
        .unwrap()
        .replace("127.0.0.1:9000", &format!("127.0.0.1:{arm_port}"))
        + &format!(
            "\n[streams.stamp]\nrate_hz = 10\nudp = {{ bind = \"127.0.0.1:{stamp_port}\" }}\n\
             decoder = {{ kind = \"cdr\", type = \"std_msgs/msg/Header\", fields = [\n\
               {{ name = \"seconds\", type = \"i32\", path = \"stamp.sec\" }} ] }}\n"
        );
    write(&package.join("streams.toml"), &streams);
    let env = [("AMENT_PREFIX_PATH", installation.as_path())];

    // With the package: its files, and the one the installation gave.
    let (_, carried, _) = record_a_second_in(
        &package.join("streams.toml"),
        &dir.0.join("data"),
        &[],
        &env,
    );
    assert_eq!(
        carried,
        [
            "mapping.toml",
            "msg/std_msgs/msg/Header.msg",
            "package.toml",
            "streams.toml",
            "urdf/arm7.urdf",
            "msg/builtin_interfaces/msg/Time.msg",
        ]
    );

    // Without one: the declarations, and every `.msg` file they use.
    let loose = dir.0.join("loose");
    write(&loose.join("streams.toml"), &streams);
    write(
        &loose.join("msg/std_msgs/msg/Header.msg"),
        "builtin_interfaces/Time stamp\nstring frame_id\n",
    );
    let (_, mut carried, _) = record_a_second_in(
        &loose.join("streams.toml"),
        &dir.0.join("data-loose"),
        &[],
        &env,
    );
    carried.sort();
    assert_eq!(
        carried,
        [
            "msg/builtin_interfaces/msg/Time.msg",
            "msg/std_msgs/msg/Header.msg",
            "streams.toml"
        ]
    );
}

#[test]
fn a_stream_kapto_record_cannot_receive_is_reported_before_recording() {
    let dir = TempDir::new("invalid");
    let streams = dir.0.join("streams.toml");
    std::fs::write(&streams, "[streams.arm]\nrate_hz = 100\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["record", "--duration", "1", "--streams"])
        .arg(&streams)
        .arg("--data-dir")
        .arg(dir.0.join("data"))
        .env_clear()
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("config.invalid_value"), "{stderr}");
    assert!(
        stderr.contains("stream `arm` needs a `udp`, `tcp` or `ros2` table and `rate_hz`"),
        "{stderr}"
    );
    assert!(
        !dir.0.join("data").exists(),
        "nothing is created before recording starts"
    );
}

/// The compression of each chunk of a second of one stream recorded with
/// `args`.
fn chunk_compressions(name: &str, args: &[&str]) -> Vec<String> {
    let dir = TempDir::new(name);
    let port = free_ports(1);
    let streams = streams_file(&dir.0, port, 1, 1_000, Transport::Udp);
    let data = dir.0.join("data");
    let (child, _) = start_recording_with(&streams, &data, 2, args, &[]);
    let config = kapto_loadgen::Config {
        target: SocketAddr::from(([127, 0, 0, 1], port)),
        streams: 1,
        rate_hz: 1_000,
        duration: Some(Duration::from_secs(1)),
        spin: Duration::from_micros(100),
        transport: Transport::Udp,
        wait_for_subscribers: None,
        camera: None,
    };
    kapto_loadgen::run(&config, |_, _| {}).unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    let episode_dir = std::fs::read_dir(&data)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| !path.ends_with(".recording"))
        .unwrap();
    let episode = Episode::read(&episode_dir).unwrap();
    let bytes = std::fs::read(episode_dir.join(&episode.recording)).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    summary
        .chunk_indexes
        .iter()
        .map(|chunk| chunk.compression.clone())
        .collect()
}

#[test]
fn recordings_are_compressed_with_lz4_unless_capture_compression_says_otherwise() {
    let lz4 = chunk_compressions("record-lz4", &[]);
    assert_ne!(lz4, Vec::<String>::new());
    assert!(
        lz4.iter().all(|compression| compression == "lz4"),
        "{lz4:?}"
    );
    let none = chunk_compressions("record-uncompressed", &["--capture-compression", "none"]);
    assert_ne!(none, Vec::<String>::new());
    assert!(none.iter().all(String::is_empty), "{none:?}");
    let zstd = chunk_compressions("record-zstd", &["--capture-compression", "zstd"]);
    assert!(
        zstd.iter().all(|compression| compression == "zstd"),
        "{zstd:?}"
    );
}
