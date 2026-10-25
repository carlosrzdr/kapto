// SPDX-License-Identifier: Apache-2.0

//! The inputs each fuzz target starts from, in the layout its target reads
//! (see the comment at the top of each in `fuzz/fuzz_targets`).

use std::io;
use std::path::Path;
use std::time::Duration;

use kapto_capture::tap::wire::{Summary, encode};
use kapto_capture::{
    Batch, ChannelSpec, Compression, LossCounters, Recording, StreamIndex, WriterConfig,
};
use kapto_core::{ClockMap, ClockSample, RecvTime};
use kapto_decode::field::Endianness;
use kapto_decode::{FieldSpec, PathField, PathFormat, PathLayout, ScalarType, StructLayout};

/// Writes every target's seeds into `seeds/<target>/`, replacing those of
/// an earlier run.
pub(super) fn write(root: &Path, seeds: &Path) -> io::Result<()> {
    let _ = std::fs::remove_dir_all(seeds);
    for target in super::TARGETS {
        std::fs::create_dir_all(seeds.join(target))?;
    }
    let put = |target: &str, name: &str, bytes: &[u8]| {
        std::fs::write(seeds.join(target).join(name), bytes)
    };
    from_testdata(root, &put)?;
    layouts(&put)?;
    recordings(seeds, &put)
}

/// How a seed is written: its target, its name and its bytes.
type Put<'a> = dyn Fn(&str, &str, &[u8]) -> io::Result<()> + 'a;

/// The reference messages of `testdata/`, for the decoders.
fn from_testdata(root: &Path, put: &Put<'_>) -> io::Result<()> {
    let reference = std::fs::read_to_string(root.join("testdata/cdr/reference.json"))?;
    let reference: serde_json::Value =
        serde_json::from_str(&reference).map_err(io::Error::other)?;
    let messages = reference["messages"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (index, (name, message)) in (0_u8..).zip(&messages) {
        let cdr = unhex(message["cdr"].as_str().unwrap_or_default());
        put("cdr", name, &[&[index][..], &cdr].concat())?;
        let definition = format!(
            "{}\n{}\0",
            message["type"].as_str().unwrap_or_default(),
            message["definition"].as_str().unwrap_or_default()
        );
        put(
            "msg_definition",
            name,
            &[definition.as_bytes(), &cdr].concat(),
        )?;
        if name == "image" {
            // The second kind of camera, a ROS 2 raw image.
            put("camera_frame", name, &[&[1][..], &cdr].concat())?;
        }
    }
    put(
        "camera_frame",
        "jpeg",
        &[2, 0, 0xff, 0xd8, 0xff, 0xe0, 0, 16],
    )?;

    for case in std::fs::read_dir(root.join("testdata/protobuf/cases"))? {
        let path = case?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "binpb")
            && let Some(name) = path.file_name().and_then(|name| name.to_str())
        {
            put("protobuf", name, &std::fs::read(&path)?)?;
        }
    }
    Ok(())
}

/// Layouts as recordings store them with payloads to decode, and a tap
/// datagram.
fn layouts(put: &Put<'_>) -> io::Result<()> {
    let mut array = FieldSpec::new("joints", ScalarType::I16, 4);
    array.count = 3;
    array.endianness = Endianness::Big;
    array.scale = Some(0.001);
    let layout = StructLayout {
        fields: vec![FieldSpec::new("counter", ScalarType::U32, 0), array],
    };
    let payload: Vec<u8> = (1..=10).collect();
    let schema = layout.to_schema_json();
    put(
        "struct_layout",
        "layout",
        &[schema.as_bytes(), &[0], &payload].concat(),
    )?;

    let mut position = PathField::new("position", "joints.position", ScalarType::F64);
    position.count = 2;
    let fields = vec![
        position,
        PathField::new("shoulder", "effort[name=shoulder]", ScalarType::F32),
        PathField::new("ok", "[0].ok", ScalarType::Bool),
    ];
    let schema = PathLayout {
        format: PathFormat::Json,
        fields,
    }
    .to_schema_json();
    let json = br#"{"joints":{"position":[0.5,1.5,2]},"name":["elbow","shoulder"],"effort":[1,2]}"#;
    put(
        "path_layout",
        "json",
        &[&[0], schema.as_bytes(), &[0], json].concat(),
    )?;
    // {"joints": {"position": [1, 2.5]}} in MessagePack.
    let msgpack = [
        0x81, 0xa6, b'j', b'o', b'i', b'n', b't', b's', 0x81, 0xa8, b'p', b'o', b's', b'i', b't',
        b'i', b'o', b'n', 0x92, 0x01, 0xcb, 0x40, 0x04, 0, 0, 0, 0, 0, 0,
    ];
    put(
        "path_layout",
        "msgpack",
        &[&[1], schema.as_bytes(), &[0], &msgpack].concat(),
    )?;

    let mut datagram = Vec::new();
    encode(
        &Summary::single(3, 1_000, &[0.5, -1.0, f64::NAN]),
        &mut datagram,
    );
    put("tap_wire", "summary", &datagram)?;

    Ok(())
}

/// Recordings, whole and cut off, uncompressed and compressed.
fn recordings(seeds: &Path, put: &Put<'_>) -> io::Result<()> {
    let scratch = seeds.join("recordings");
    std::fs::create_dir_all(&scratch)?;
    for (name, compression) in [("plain", Compression::None), ("zstd", Compression::Zstd)] {
        let path = scratch.join(name);
        recording(&path, compression)?;
        let whole = std::fs::read(&path)?;
        put("mcap_seek", name, &whole)?;
        put("mcap_salvage", name, &whole)?;
        put(
            "mcap_salvage",
            &format!("{name}_cut"),
            &whole[..whole.len() * 2 / 3],
        )?;
    }
    std::fs::remove_dir_all(&scratch)
}

/// A small finalised recording: a `camera` channel and a `joints` one, in
/// several chunks.
fn recording(path: &Path, compression: Compression) -> io::Result<()> {
    let failed = |error: kapto_core::KaptoError| io::Error::other(error.to_string());
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: 1_000_000_000_000,
        uncertainty: Duration::ZERO,
    });
    let mut streams = Vec::new();
    for (index, name) in [(0_u16, "joints"), (1, "camera")] {
        let id = name.parse().map_err(io::Error::other)?;
        streams.push(ChannelSpec::from((StreamIndex::from(index), id)));
    }
    let config = WriterConfig {
        compression,
        chunk_bytes: 128,
        ..WriterConfig::default()
    };
    let mut recording = Recording::create(path, &streams, &clock, &config).map_err(failed)?;
    let counters = LossCounters::new(2);
    for n in 0_u8..6 {
        let mut batch = Batch::new(2, 64);
        batch.push(0, &u64::from(n).to_le_bytes());
        batch.push(1, &[n; 48]);
        batch.set_recv_time(RecvTime::from_nanos(u64::from(n) * 33_000_000));
        recording.write(&batch, &counters).map_err(failed)?;
    }
    recording.finish(&clock).map(|_| ()).map_err(failed)
}

/// Bytes written as hexadecimal; anything else is left out.
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}
