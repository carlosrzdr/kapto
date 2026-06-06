// SPDX-License-Identifier: Apache-2.0

//! `kapto decode --pcap … --package …`: a robot's traffic, captured as
//! tcpdump or Wireshark would, decoded with a package's streams.
//!
//! The captures are written here byte by byte from the pcap and pcapng
//! specifications, not with Kapto's reader, so a mistake in one doesn't
//! hide in the other.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use kapto_loadgen::payload::{SIZE, Sample};

/// A folder for one test, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kapto-pcap-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let arm7 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7");
        for file in [
            "package.toml",
            "streams.toml",
            "mapping.toml",
            "urdf/arm7.urdf",
        ] {
            let to = path.join("arm7").join(file);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(arm7.join(file), to).unwrap();
        }
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 2026-03-09T14:33:32Z.
const START_NS: u64 = 1_773_066_812_000_000_000;

/// The synthetic robot's message number `seq`, with joint `j` at
/// `seq + j / 10` radians.
fn message(seq: u32) -> Vec<u8> {
    let mut joints = [0.0; 7];
    for (index, joint) in (0_u32..).zip(joints.iter_mut()) {
        *joint = f64::from(seq) + f64::from(index) / 10.0;
    }
    let mut bytes = [0; SIZE];
    Sample {
        seq,
        source_time_ns: 7,
        joints,
        gripper: 0.04,
    }
    .encode(&mut bytes);
    bytes.to_vec()
}

fn udp(source_port: u16, destination_port: u16, payload: &[u8]) -> Vec<u8> {
    let mut segment = Vec::new();
    segment.extend(source_port.to_be_bytes());
    segment.extend(destination_port.to_be_bytes());
    segment.extend(u16::try_from(8 + payload.len()).unwrap().to_be_bytes());
    segment.extend([0, 0]); // no checksum
    segment.extend(payload);
    segment
}

/// An IPv4 packet; `flags_fragment` as in the header (0x2000: more
/// fragments follow).
fn ipv4(protocol: u8, destination: [u8; 4], flags_fragment: u16, body: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x45, 0];
    packet.extend(u16::try_from(20 + body.len()).unwrap().to_be_bytes());
    packet.extend([0, 1]);
    packet.extend(flags_fragment.to_be_bytes());
    packet.extend([64, protocol, 0, 0]);
    packet.extend([192, 168, 1, 20]);
    packet.extend(destination);
    packet.extend(body);
    packet
}

fn ipv6(body: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x60, 0, 0, 0];
    packet.extend(u16::try_from(body.len()).unwrap().to_be_bytes());
    packet.extend([17, 64]);
    packet.extend([0; 15]);
    packet.push(2); // ::2
    packet.extend([0; 15]);
    packet.push(1); // ::1
    packet.extend(body);
    packet
}

/// An Ethernet frame, with a VLAN tag if `vlan`; padded to 60 bytes as a
/// network card would.
fn ethernet(ethertype: u16, vlan: bool, packet: &[u8]) -> Vec<u8> {
    let mut frame = vec![0, 0x1b, 0x21, 1, 2, 3, 0, 0x1b, 0x21, 4, 5, 6];
    if vlan {
        frame.extend([0x81, 0x00, 0x00, 0x05]);
    }
    frame.extend(ethertype.to_be_bytes());
    frame.extend(packet);
    frame.resize(frame.len().max(60), 0);
    frame
}

/// A Linux cooked capture header and packet (`tcpdump -i any`).
fn linux_sll(packet: &[u8]) -> Vec<u8> {
    let mut frame = vec![0, 0, 0, 1, 0, 6, 0, 0x1b, 0x21, 1, 2, 3, 0, 0];
    frame.extend(0x0800_u16.to_be_bytes());
    frame.extend(packet);
    frame
}

/// A pcap file of Ethernet frames, timestamps in microseconds.
fn pcap(frames: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend(0xa1b2_c3d4_u32.to_le_bytes());
    file.extend(2_u16.to_le_bytes());
    file.extend(4_u16.to_le_bytes());
    file.extend([0; 8]);
    file.extend(65_535_u32.to_le_bytes());
    file.extend(1_u32.to_le_bytes()); // Ethernet
    for (time_ns, frame) in frames {
        file.extend(
            u32::try_from(time_ns / 1_000_000_000)
                .unwrap()
                .to_le_bytes(),
        );
        file.extend(
            u32::try_from(time_ns % 1_000_000_000 / 1_000)
                .unwrap()
                .to_le_bytes(),
        );
        let length = u32::try_from(frame.len()).unwrap().to_le_bytes();
        file.extend(length);
        file.extend(length);
        file.extend(frame);
    }
    file
}

/// One pcapng block: type, length, body padded to 4 bytes, length again.
fn block(kind: u32, body: &[u8]) -> Vec<u8> {
    let mut padded = body.to_vec();
    padded.resize(body.len().next_multiple_of(4), 0);
    let length = u32::try_from(12 + padded.len()).unwrap();
    let mut block = Vec::new();
    block.extend(kind.to_le_bytes());
    block.extend(length.to_le_bytes());
    block.extend(padded);
    block.extend(length.to_le_bytes());
    block
}

/// A pcapng file with one Linux cooked interface in nanoseconds.
fn pcapng(frames: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut section = Vec::new();
    section.extend(0x1a2b_3c4d_u32.to_le_bytes());
    section.extend(1_u16.to_le_bytes());
    section.extend(0_u16.to_le_bytes());
    section.extend((-1_i64).to_le_bytes());
    let mut file = block(0x0a0d_0d0a, &section);
    let mut interface = Vec::new();
    interface.extend(113_u16.to_le_bytes()); // LINUX_SLL
    interface.extend([0, 0]);
    interface.extend(65_535_u32.to_le_bytes());
    // if_tsresol = 9: nanoseconds; then the end of options.
    interface.extend([9, 0, 1, 0, 9, 0, 0, 0, 0, 0, 0, 0]);
    file.extend(block(1, &interface));
    for (time_ns, frame) in frames {
        let mut packet = Vec::new();
        packet.extend(0_u32.to_le_bytes());
        packet.extend(u32::try_from(time_ns >> 32).unwrap().to_le_bytes());
        packet.extend(u32::try_from(time_ns & 0xffff_ffff).unwrap().to_le_bytes());
        let length = u32::try_from(frame.len()).unwrap().to_le_bytes();
        packet.extend(length);
        packet.extend(length);
        packet.extend(frame);
        file.extend(block(6, &packet));
    }
    file
}

/// The robot's traffic: three arm messages on port 9000 amid other
/// packets, each kind of which must be passed over.
fn traffic(frame: impl Fn(&[u8]) -> Vec<u8>) -> Vec<(u64, Vec<u8>)> {
    let arm = |seq: u32| {
        let packet = ipv4(17, [127, 0, 0, 1], 0, &udp(40_000, 9000, &message(seq)));
        (START_NS + u64::from(seq) * 500_000, frame(&packet))
    };
    vec![
        arm(0),
        // TCP to the same port, a datagram to another port, and a fragment.
        (START_NS, frame(&ipv4(6, [127, 0, 0, 1], 0, &[0; 20]))),
        (
            START_NS,
            frame(&ipv4(17, [127, 0, 0, 1], 0, &udp(40_000, 9999, b"x"))),
        ),
        (
            START_NS,
            frame(&ipv4(17, [127, 0, 0, 1], 0x2000, &udp(40_000, 9000, b"y"))),
        ),
        arm(1),
        arm(2),
    ]
}

fn kapto(scratch: &Scratch, capture: &[u8], extra: &[&str]) -> Output {
    let path = scratch.0.join("capture.pcap");
    std::fs::write(&path, capture).unwrap();
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("decode")
        .arg("--pcap")
        .arg(&path)
        .arg("--package")
        .arg(scratch.0.join("arm7"))
        .args(extra)
        .env_clear()
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

const EXPECTED_CSV: &str = "\
received_utc_ns,received_utc,index,sequence,source_time,joint_positions[0],joint_positions[1],joint_positions[2],joint_positions[3],joint_positions[4],joint_positions[5],joint_positions[6],gripper
1773066812000000000,2026-03-09T14:33:32.000000000Z,0,0,7,0,0.1,0.2,0.3,0.4,0.5,0.6,0.04
1773066812000500000,2026-03-09T14:33:32.000500000Z,1,1,7,1,1.1,1.2,1.3,1.4,1.5,1.6,0.04
1773066812001000000,2026-03-09T14:33:32.001000000Z,2,2,7,2,2.1,2.2,2.3,2.4,2.5,2.6,0.04
";

#[test]
fn a_pcap_of_ethernet_frames_decodes_with_the_packages_streams() {
    let scratch = Scratch::new("pcap");
    let capture = pcap(&traffic(|packet| ethernet(0x0800, false, packet)));
    let output = kapto(
        &scratch,
        &capture,
        &["--format", "csv", "--stream", "arm_joints"],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), EXPECTED_CSV);
    assert_eq!(
        text(&output.stderr),
        "1 UDP datagram(s) in the capture went to no stream of the package.\n\
         1 packet(s) were fragments of datagrams too large for one packet, which can't be read on their own.\n"
    );
}

#[test]
fn a_pcapng_of_linux_cooked_packets_decodes_the_same() {
    let scratch = Scratch::new("pcapng");
    let capture = pcapng(&traffic(linux_sll));
    let output = kapto(
        &scratch,
        &capture,
        &["--format", "csv", "--stream", "arm_joints"],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), EXPECTED_CSV);
}

#[test]
fn vlan_tags_and_ipv6_are_read_through() {
    let scratch = Scratch::new("vlan");
    // Bound to every address, IPv4 and IPv6.
    let streams = scratch.0.join("arm7/streams.toml");
    let text_before = std::fs::read_to_string(&streams).unwrap();
    std::fs::write(&streams, text_before.replace("127.0.0.1:9000", "[::]:9000")).unwrap();
    let frames = vec![
        (
            START_NS,
            ethernet(
                0x0800,
                true,
                &ipv4(17, [127, 0, 0, 1], 0, &udp(1, 9000, &message(0))),
            ),
        ),
        (
            START_NS + 500_000,
            ethernet(0x86dd, false, &ipv6(&udp(1, 9000, &message(1)))),
        ),
    ];
    let output = kapto(&scratch, &pcap(&frames), &["--format", "csv"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout).lines().count(),
        3,
        "{}",
        text(&output.stdout)
    );
}

#[test]
fn decoding_the_same_capture_twice_gives_the_same_bytes() {
    let scratch = Scratch::new("twice");
    let capture = pcap(&traffic(|packet| ethernet(0x0800, false, packet)));
    let first = kapto(&scratch, &capture, &[]);
    let second = kapto(&scratch, &capture, &[]);
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    assert!(text(&first.stdout).contains("arm_joints (kapto.struct/v1)"));
}

#[test]
fn a_file_that_isnt_a_capture_is_reported_with_its_cause() {
    let scratch = Scratch::new("invalid");
    let output = kapto(&scratch, b"not a capture at all", &[]);
    assert!(!output.status.success());
    assert!(
        text(&output.stderr).starts_with("error[decode.capture_invalid]"),
        "{}",
        text(&output.stderr)
    );
    let both = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["decode", "episode", "--pcap", "x.pcap", "--package", "arm7"])
        .env_clear()
        .output()
        .unwrap();
    assert!(!both.status.success(), "a recording and a capture at once");
}
