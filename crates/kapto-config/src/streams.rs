// SPDX-License-Identifier: Apache-2.0

//! Stream declarations: what a robot sends, from its `streams.toml`.
//!
//! Each `[streams.<id>]` table declares one stream. Streams are numbered in
//! the order they appear in the file.
//!
//! ```toml
//! [streams.arm_joints]
//! rate_hz = 2000
//! udp = { bind = "0.0.0.0:9000" }
//!
//! [streams.arm_joints.sequence]
//! offset = 0
//! type = "u32"
//! byte_order = "little"
//!
//! [streams.arm_joints.decoder]
//! kind = "struct"
//!
//! [[streams.arm_joints.decoder.fields]]
//! name = "joint_positions"
//! type = "f64"
//! offset = 12
//! count = 7
//! unit = "rad"
//! ```
//!
//! A JSON or MessagePack stream (`kind = "json"` or `"msgpack"`) gives each
//! field a `path` through the message instead of an `offset` and
//! `byte_order`, e.g. `path = "joints.position"`.
//!
//! A stream of ROS 2 messages (`kind = "cdr"`) names their `type` too, e.g.
//! `type = "sensor_msgs/msg/JointState"`, and finds fields by path the same
//! way, e.g. `path = "position[2]"`. The type's definition is put together
//! from the `.msg` files in the `msg/` folder beside `streams.toml`, laid out
//! as ROS 2 installs them: `msg/sensor_msgs/msg/JointState.msg`, and the
//! file of every type it uses. A file not there is looked for in the ROS 2
//! installation Kapto runs in, if any.
//!
//! A stream can be a ROS 2 topic instead of arriving over UDP or TCP, e.g.
//! `ros2 = { topic = "/joint_states", type = "sensor_msgs/msg/JointState" }`;
//! its decoder is then `kind = "cdr"`, of the topic's type.

mod camera;
mod msg_files;
mod ros2;
mod tcp;

use std::ffi::OsStr;
use std::net::{IpAddr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::PathBuf;

use kapto_core::{ErrorCode, KaptoError, StreamId};
use kapto_decode::cdr::{assemble, msg_files_in};
use kapto_decode::{
    CdrLayout, Endianness, FieldSpec, PathField, PathFormat, PathLayout, ScalarType, StructLayout,
};
use toml_edit::{Document, Item, TableLike};

use crate::file::ConfigFile;
use crate::located::{at_key, at_line, line_of, syntax_error, unknown_key};
use crate::suggest::closest;

pub use camera::{CAMERA_MESSAGE_TYPES, CameraDeclaration, PixelEncoding};
pub use kapto_decode::DecoderDeclaration;
pub use msg_files::{MsgFile, msg_files};
pub use ros2::{Durability, QosDeclaration, Reliability, Ros2Declaration};
pub use tcp::{FramingDeclaration, TcpDeclaration, TcpEndpointDeclaration};

/// One stream as declared.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamDeclaration {
    /// The stream's name.
    pub id: StreamId,
    /// Messages per second the robot sends, if it sends at a fixed rate.
    pub rate_hz: Option<u32>,
    /// Where each message carries a sequence number, if it does.
    pub sequence: Option<SequenceDeclaration>,
    /// For a stream whose loss is estimated from its rate: how long, in
    /// milliseconds, after a silence its messages may still arrive late
    /// before what is missing counts as lost, if not the default.
    pub loss_settle_ms: Option<u32>,
    /// The largest message the stream sends, in bytes, if declared. Longer
    /// messages are discarded and counted.
    pub max_message_bytes: Option<u32>,
    /// Where the stream arrives over UDP, if it does.
    pub udp: Option<UdpDeclaration>,
    /// Where the stream arrives over TCP, if it does.
    pub tcp: Option<TcpDeclaration>,
    /// The ROS 2 topic it is, if it is one.
    pub ros2: Option<Ros2Declaration>,
    /// How its payloads decode into named values, if declared.
    pub decoder: Option<DecoderDeclaration>,
    /// What its frames are, for a TCP stream that is a camera.
    pub camera: Option<CameraDeclaration>,
}

impl StreamDeclaration {
    /// Whether its messages are a camera's frames, which the live view
    /// shows as pictures: a ROS 2 topic of a camera's type
    /// ([`CAMERA_MESSAGE_TYPES`]), or a TCP stream with a `camera` table.
    #[must_use]
    pub fn is_camera(&self) -> bool {
        self.camera.is_some()
            || self
                .ros2
                .as_ref()
                .is_some_and(|topic| CAMERA_MESSAGE_TYPES.contains(&topic.message_type.as_str()))
    }

    /// Whether its messages are kept for subscribers that come later, so
    /// one may be all it ever sends: a ROS 2 topic subscribed to with
    /// `durability = "transient_local"`, such as `/tf_static`. It needs no
    /// `rate_hz`.
    #[must_use]
    pub fn kept_for_late_subscribers(&self) -> bool {
        self.ros2
            .as_ref()
            .is_some_and(|topic| topic.qos.durability == Durability::TransientLocal)
    }
}

/// Where a stream arrives over UDP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdpDeclaration {
    /// The local address and port to listen on, e.g. `0.0.0.0:9000`.
    pub bind: SocketAddr,
    /// A multicast group to join, if the robot sends to one.
    pub multicast_group: Option<IpAddr>,
    /// The only robot addresses allowed to send; empty allows any.
    pub allowed_sources: Vec<IpAddr>,
}

/// Where a stream's messages carry their sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceDeclaration {
    /// Byte offset of the number in the payload.
    pub offset: u16,
    /// The number's unsigned integer type.
    pub kind: SequenceType,
    /// Its byte order; little-endian when not given.
    pub byte_order: ByteOrder,
}

/// The unsigned integer type of a sequence number (`type = "u32"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceType {
    /// `"u8"`
    U8,
    /// `"u16"`
    U16,
    /// `"u32"`
    U32,
    /// `"u64"`
    U64,
}

/// The order of a number's bytes (`byte_order = "little"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    /// `"little"`: least significant byte first.
    Little,
    /// `"big"`: most significant byte first.
    Big,
}

const STREAM_KEYS: &[&str] = &[
    "rate_hz",
    "sequence",
    "loss_settle_ms",
    "max_message_bytes",
    "udp",
    "tcp",
    "ros2",
    "decoder",
    "camera",
];
const DECODER_KEYS: &[&str] = &["kind", "type", "fields"];
const FIELD_KEYS: &[&str] = &[
    "name",
    "type",
    "offset",
    "path",
    "count",
    "byte_order",
    "scale",
    "unit",
];
/// A decoder's `kind`.
#[derive(Clone, Copy)]
enum Kind {
    Struct,
    Path(PathFormat),
    Cdr,
}

impl Kind {
    /// Its name in `streams.toml`.
    fn name(self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Path(format) => format.name(),
            Self::Cdr => "cdr",
        }
    }
}

/// Whether a decoder's stream is a ROS 2 topic: its messages are then CDR,
/// of the topic's type.
#[derive(Clone, Copy)]
enum Topic<'a> {
    /// It arrives over UDP or TCP.
    No,
    /// It is this topic.
    Valid(&'a Ros2Declaration),
    /// It is a topic, whose declaration has problems of its own.
    Invalid,
}

/// One decoder field, as its decoder's kind has it.
enum Field {
    Struct(FieldSpec),
    Path(PathField),
}

/// The fields found by path, of a decoder whose kind has them.
fn path_fields(fields: Vec<Field>) -> Vec<PathField> {
    fields
        .into_iter()
        .filter_map(|field| match field {
            Field::Path(field) => Some(field),
            Field::Struct(_) => None,
        })
        .collect()
}

const SEQUENCE_KEYS: &[&str] = &["offset", "type", "byte_order"];
const UDP_KEYS: &[&str] = &["bind", "multicast_group", "allowed_sources"];
const MAX_RATE_HZ: i64 = 1_000_000;
/// The largest offset that still lies inside a UDP datagram.
const MAX_OFFSET: i64 = 65_507;
/// The largest UDP payload: a UDP stream's largest message, and the size
/// above which a TCP or ROS 2 stream's messages go through a ring of their
/// own, as a camera's frames do.
pub const MAX_DATAGRAM_BYTES: u32 = 65_507;
/// The largest message a TCP or ROS 2 stream may declare: a raw 4K RGB
/// frame (24 MiB) fits.
pub const MAX_MESSAGE_BYTES: u32 = 32 << 20;

/// The longest a silence may settle: a minute, past which a message is
/// late enough to be lost.
const MAX_LOSS_SETTLE_MS: i64 = 60_000;

/// Reads the stream declarations in `file`, in file order, with the `.msg`
/// files of its ROS 2 messages from the `msg/` folder beside it, then from
/// the ROS 2 installation Kapto runs in, if any.
///
/// # Errors
///
/// Every problem found, each with the file and line, so they can all be
/// fixed at once: invalid TOML (`config.invalid_syntax`), unknown keys
/// (`stream.unknown_key`), invalid stream names (`stream.invalid_id`),
/// missing or invalid values (`config.invalid_value`), and a missing `.msg`
/// file (`decoder.msg_not_found`).
pub fn parse_streams(file: &ConfigFile) -> Result<Vec<StreamDeclaration>, Vec<KaptoError>> {
    let mut folders = vec![file.path.with_file_name("msg")];
    folders.extend(installed_msg_folders(
        std::env::var_os("AMENT_PREFIX_PATH").as_deref(),
    ));
    parse_streams_with(file, &mut msg_files_in(&folders))
}

/// The `share` folder of each ROS 2 installation in `ament_prefix_path`,
/// which ROS 2's setup scripts set, in its order: where every package
/// installs its `.msg` files.
fn installed_msg_folders(ament_prefix_path: Option<&OsStr>) -> Vec<PathBuf> {
    ament_prefix_path.map_or_else(Vec::new, |paths| {
        std::env::split_paths(paths)
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.join("share"))
            .collect()
    })
}

/// Reads the stream declarations in `file` as [`parse_streams`] does, with
/// the text of each `.msg` file its ROS 2 messages use from `msg_files`,
/// given the type's package and name (`std_msgs`, `Header`): for a
/// `streams.toml` read from somewhere other than its package's folder, such
/// as the recording that carries it.
///
/// # Errors
///
/// As [`parse_streams`].
pub fn parse_streams_with(
    file: &ConfigFile,
    msg_files: &mut dyn FnMut(&str, &str) -> Option<String>,
) -> Result<Vec<StreamDeclaration>, Vec<KaptoError>> {
    let document = Document::parse(file.contents.as_str())
        .map_err(|error| vec![syntax_error(file, &error)])?;
    let mut reader = Reader {
        file,
        msg_files,
        errors: Vec::new(),
    };
    let mut streams = Vec::new();
    if let Some(root) = document.as_item().as_table_like() {
        reader.unknown_keys(root, "", &["streams"]);
        if let Some(table) = reader.table(root, "", "streams") {
            for (name, _) in table.iter() {
                streams.extend(reader.stream(table, name));
            }
        }
    }
    reader.errors.extend(
        port_conflicts(&streams)
            .into_iter()
            .map(|error| error.context("file", file.path.display().to_string())),
    );
    if reader.errors.is_empty() {
        Ok(streams)
    } else {
        Err(reader.errors)
    }
}

/// Two unicast UDP streams can't share a port: a datagram arriving there
/// couldn't be told apart. Multicast streams on one port are told apart by
/// their group. Two TCP streams can't listen on one port either, since a
/// robot connecting there couldn't be told apart.
fn port_conflicts(streams: &[StreamDeclaration]) -> Vec<KaptoError> {
    let listeners: Vec<(&StreamId, SocketAddr, &str)> = streams
        .iter()
        .filter_map(|stream| {
            if let Some(TcpEndpointDeclaration::Listen(address)) =
                stream.tcp.as_ref().map(|tcp| tcp.endpoint)
            {
                return Some((&stream.id, address, "TCP"));
            }
            let udp = stream.udp.as_ref()?;
            udp.multicast_group
                .is_none()
                .then_some((&stream.id, udp.bind, "UDP"))
        })
        .collect();
    let mut errors = Vec::new();
    for (index, (first, a, protocol)) in listeners.iter().enumerate() {
        for (second, b, other) in &listeners[index + 1..] {
            let overlap = a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified();
            if a.port() == b.port() && overlap && protocol == other {
                errors.push(
                    KaptoError::new(ErrorCode::ConfigPortConflict).detail(format!(
                        "streams `{first}` ({a}) and `{second}` ({b}) both listen on {protocol} port {}",
                        a.port()
                    )),
                );
            }
        }
    }
    errors
}

struct Reader<'a> {
    file: &'a ConfigFile,
    msg_files: &'a mut dyn FnMut(&str, &str) -> Option<String>,
    errors: Vec<KaptoError>,
}

impl Reader<'_> {
    fn stream(&mut self, streams: &dyn TableLike, name: &str) -> Option<StreamDeclaration> {
        let path = format!("streams.{name}");
        let id = name.parse::<StreamId>().map_err(|error| {
            let error = at_key(error, self.file, streams, name);
            self.errors.push(error);
        });
        let table = self.table(streams, "streams.", name)?;
        self.unknown_keys(table, &path, STREAM_KEYS);
        let rate_hz = self
            .integer(table, &path, "rate_hz", 1..=MAX_RATE_HZ)
            .and_then(|rate| u32::try_from(rate).ok());
        let sequence = self.table(table, &format!("{path}."), "sequence");
        let sequence = sequence.and_then(|sequence| self.sequence(sequence, table, &path));
        let loss_settle_ms = self
            .integer(table, &path, "loss_settle_ms", 1..=MAX_LOSS_SETTLE_MS)
            .and_then(|ms| u32::try_from(ms).ok());
        if loss_settle_ms.is_some() && (rate_hz.is_none() || table.get("sequence").is_some()) {
            let detail = format!(
                "`{path}.loss_settle_ms` is for a stream whose loss is estimated from its \
                 `rate_hz`, without a `sequence`"
            );
            self.invalid(table, "loss_settle_ms", detail);
        }
        let max_message_bytes = self
            .integer(
                table,
                &path,
                "max_message_bytes",
                1..=i64::from(MAX_MESSAGE_BYTES),
            )
            .and_then(|bytes| u32::try_from(bytes).ok());
        let udp = self.table(table, &format!("{path}."), "udp");
        let udp = udp.and_then(|udp| self.udp(udp, table, &path));
        let tcp = self.table(table, &format!("{path}."), "tcp");
        let tcp = tcp.and_then(|tcp| self.tcp(tcp, table, &path));
        let ros2 = self.table(table, &format!("{path}."), "ros2");
        let ros2 = ros2.and_then(|ros2| self.ros2(ros2, table, &path));
        let arrivals: Vec<&str> = ["udp", "tcp", "ros2"]
            .into_iter()
            .filter(|key| table.get(key).is_some())
            .collect();
        if let [first, second, ..] = arrivals[..] {
            let detail =
                format!("`{path}` has both `{first}` and `{second}`: a stream arrives over one");
            self.invalid(table, second, detail);
        }
        if let (Some(FramingDeclaration::Fixed(size)), Some(max)) =
            (tcp.as_ref().map(|tcp| &tcp.framing), max_message_bytes)
            && u32::from(*size) > max
        {
            let detail =
                format!("`{path}.tcp.framing.size` is {size}, but `max_message_bytes` is {max}");
            self.invalid(table, "max_message_bytes", detail);
        }
        if let (Some(_), Some(max)) = (&udp, max_message_bytes)
            && max > MAX_DATAGRAM_BYTES
        {
            let detail = format!(
                "`{path}.max_message_bytes` is {max}, but a UDP datagram holds at most \
                 {MAX_DATAGRAM_BYTES} bytes: only a TCP or ROS 2 stream's messages can be larger"
            );
            self.invalid(table, "max_message_bytes", detail);
        }
        let camera = self.table(table, &format!("{path}."), "camera");
        let camera = camera.and_then(|camera| self.camera(camera, &path));
        self.camera_fits(
            table,
            &path,
            camera.as_ref(),
            (tcp.as_ref(), max_message_bytes),
        );
        let decoder = self.table(table, &format!("{path}."), "decoder");
        let topic = match (table.get("ros2"), &ros2) {
            (None, _) => Topic::No,
            (Some(_), Some(ros2)) => Topic::Valid(ros2),
            (Some(_), None) => Topic::Invalid,
        };
        let decoder = decoder.and_then(|decoder| self.decoder(decoder, table, &path, topic));
        if let (Some(DecoderDeclaration::Struct(layout)), Some(max)) = (&decoder, max_message_bytes)
        {
            let needed = layout.fields.iter().map(FieldSpec::end).max().unwrap_or(0);
            if needed > usize::try_from(max).unwrap_or(usize::MAX) {
                let detail = format!(
                    "`{path}.decoder` reads up to byte {needed}, but `max_message_bytes` is {max}"
                );
                self.invalid(table, "max_message_bytes", detail);
            }
        }
        Some(StreamDeclaration {
            id: id.ok()?,
            rate_hz,
            sequence,
            loss_settle_ms,
            max_message_bytes,
            udp,
            tcp,
            ros2,
            decoder,
            camera,
        })
    }

    /// A camera's table is for a TCP stream, and its raw frames fit the
    /// stream's messages.
    fn camera_fits(
        &mut self,
        table: &dyn TableLike,
        path: &str,
        camera: Option<&CameraDeclaration>,
        (tcp, max_message_bytes): (Option<&TcpDeclaration>, Option<u32>),
    ) {
        if table.get("camera").is_none() {
            return;
        }
        if table.get("ros2").is_some() {
            let detail = format!(
                "`{path}.camera` is for a TCP stream: a ROS 2 topic is a camera by its type, {}",
                CAMERA_MESSAGE_TYPES.join(" or ")
            );
            self.invalid(table, "camera", detail);
        } else if table.get("tcp").is_none() {
            let detail = format!(
                "`{path}.camera` is for a TCP stream: a camera's frames don't fit in UDP datagrams"
            );
            self.invalid(table, "camera", detail);
        }
        let Some(bytes) = camera.and_then(CameraDeclaration::frame_bytes) else {
            return;
        };
        if let Some(FramingDeclaration::Fixed(size)) = tcp.map(|tcp| &tcp.framing)
            && u64::from(*size) != bytes
        {
            let detail = format!(
                "`{path}.camera` frames are {bytes} bytes, but `tcp.framing.size` is {size}"
            );
            self.invalid(table, "camera", detail);
        }
        if let Some(max) = max_message_bytes
            && u64::from(max) < bytes
        {
            let detail = format!(
                "`{path}.camera` frames are {bytes} bytes, but `max_message_bytes` is {max}"
            );
            self.invalid(table, "camera", detail);
        }
    }

    fn udp(
        &mut self,
        table: &dyn TableLike,
        stream_table: &dyn TableLike,
        stream: &str,
    ) -> Option<UdpDeclaration> {
        let path = format!("{stream}.udp");
        self.unknown_keys(table, &path, UDP_KEYS);
        let bind = self.parsed::<SocketAddr>(
            table,
            &path,
            "bind",
            "an address and port, e.g. \"0.0.0.0:9000\"",
        );
        let bind = match bind {
            Some(bind) if bind.port() == 0 => {
                self.invalid(table, "bind", format!("`{path}.bind` needs a port from 1 to 65535: the robot sends to a fixed port"));
                None
            }
            Some(bind) => Some(bind),
            None => {
                if table.get("bind").is_none() {
                    self.invalid(stream_table, "udp", format!("`{path}` needs `bind`"));
                }
                None
            }
        };
        let group =
            self.parsed::<IpAddr>(table, &path, "multicast_group", "a multicast IP address");
        if let (Some(group), Some(bind)) = (group, bind)
            && (!group.is_multicast() || group.is_ipv4() != bind.is_ipv4())
        {
            let detail = format!(
                "`{path}.multicast_group` must be a multicast address of the same IP version as `bind`"
            );
            self.invalid(table, "multicast_group", detail);
        }
        let allowed_sources = self.addresses(table, &path, "allowed_sources");
        Some(UdpDeclaration {
            bind: bind?,
            multicast_group: group,
            allowed_sources,
        })
    }

    /// A string value parsed as `T`, or `None` if absent or invalid (an error).
    fn parsed<T: std::str::FromStr>(
        &mut self,
        table: &dyn TableLike,
        path: &str,
        name: &str,
        expected: &str,
    ) -> Option<T> {
        let item = table.get(name)?;
        let value = item.as_str().and_then(|text| text.parse().ok());
        if value.is_none() {
            self.invalid(table, name, format!("`{path}.{name}` must be {expected}"));
        }
        value
    }

    /// An array of IP address strings, or empty if absent or invalid (an error).
    fn addresses(&mut self, table: &dyn TableLike, path: &str, name: &str) -> Vec<IpAddr> {
        let Some(item) = table.get(name) else {
            return Vec::new();
        };
        let addresses: Option<Vec<IpAddr>> = item.as_array().and_then(|array| {
            array
                .iter()
                .map(|value| value.as_str().and_then(|text| text.parse().ok()))
                .collect()
        });
        if addresses.is_none() {
            let detail =
                format!("`{path}.{name}` must be a list of IP addresses, e.g. [\"10.0.0.5\"]");
            self.invalid(table, name, detail);
        }
        addresses.unwrap_or_default()
    }

    /// The `sequence` table of a stream. A missing key is reported at the
    /// line of the table itself, since the key has no line of its own.
    fn sequence(
        &mut self,
        table: &dyn TableLike,
        stream_table: &dyn TableLike,
        stream: &str,
    ) -> Option<SequenceDeclaration> {
        let path = format!("{stream}.sequence");
        self.unknown_keys(table, &path, SEQUENCE_KEYS);
        let offset = self.integer(table, &path, "offset", 0..=MAX_OFFSET);
        let kind = self.choice(
            table,
            &path,
            "type",
            &[
                ("u8", SequenceType::U8),
                ("u16", SequenceType::U16),
                ("u32", SequenceType::U32),
                ("u64", SequenceType::U64),
            ],
        );
        let byte_order = self.choice(
            table,
            &path,
            "byte_order",
            &[("little", ByteOrder::Little), ("big", ByteOrder::Big)],
        );
        for (required, missing) in [("offset", offset.is_none()), ("type", kind.is_none())] {
            if missing && table.get(required).is_none() {
                self.invalid(
                    stream_table,
                    "sequence",
                    format!("`{path}` needs `{required}`"),
                );
            }
        }
        let byte_order = match (byte_order, table.get("byte_order")) {
            (Some(order), _) => order,
            (None, None) => ByteOrder::Little,
            (None, Some(_)) => return None,
        };
        Some(SequenceDeclaration {
            offset: u16::try_from(offset?).ok()?,
            kind: kind?,
            byte_order,
        })
    }

    /// The `decoder` table of a stream: its kind and fields, checked as a
    /// whole so every layout problem is reported with its field's line.
    fn decoder(
        &mut self,
        table: &dyn TableLike,
        stream_table: &dyn TableLike,
        stream: &str,
        topic: Topic<'_>,
    ) -> Option<DecoderDeclaration> {
        let path = format!("{stream}.decoder");
        self.unknown_keys(table, &path, DECODER_KEYS);
        let kinds = [
            ("struct", Kind::Struct),
            ("json", Kind::Path(PathFormat::Json)),
            ("msgpack", Kind::Path(PathFormat::MsgPack)),
            ("cdr", Kind::Cdr),
        ];
        let kind = self.choice(table, &path, "kind", &kinds);
        if table.get("kind").is_none() {
            self.invalid(stream_table, "decoder", format!("`{path}` needs `kind`"));
        }
        let on_topic = !matches!(topic, Topic::No);
        if on_topic && kind.is_some_and(|kind| !matches!(kind, Kind::Cdr)) {
            let detail = format!(
                "`{path}.kind` must be \"cdr\" for a ROS 2 topic: its messages are ROS 2's own"
            );
            self.invalid(table, "kind", detail);
        }
        let type_name = self.message_type(table, stream_table, &path, kind, topic);
        let Some(fields) = table.get("fields") else {
            self.invalid(stream_table, "decoder", format!("`{path}` needs `fields`"));
            return None;
        };
        let entries: Option<Vec<&dyn TableLike>> = match fields.as_array_of_tables() {
            Some(tables) => Some(tables.iter().map(|table| table as &dyn TableLike).collect()),
            None => fields.as_array().and_then(|array| {
                array
                    .iter()
                    .map(|value| value.as_inline_table().map(|table| table as &dyn TableLike))
                    .collect()
            }),
        };
        let Some(entries) = entries else {
            let detail =
                format!("`{path}.fields` must be a list of tables, e.g. [[{path}.fields]]");
            self.invalid(table, "fields", detail);
            return None;
        };
        let errors_before = self.errors.len();
        let mut lines = Vec::new();
        let mut fields = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            if let Some(field) = self.field(entry, &format!("{path}.fields[{index}]"), kind) {
                lines.push(line_of(self.file, entry, "name"));
                fields.push(field);
            }
        }
        if self.errors.len() > errors_before {
            return None;
        }
        let declaration = match kind? {
            Kind::Struct => DecoderDeclaration::Struct(StructLayout {
                fields: fields
                    .into_iter()
                    .filter_map(|field| match field {
                        Field::Struct(field) => Some(field),
                        Field::Path(_) => None,
                    })
                    .collect(),
            }),
            Kind::Path(format) => DecoderDeclaration::Path(PathLayout {
                format,
                fields: path_fields(fields),
            }),
            Kind::Cdr => DecoderDeclaration::Cdr(CdrLayout {
                definition: match topic {
                    Topic::Valid(topic) => topic.definition.clone(),
                    Topic::Invalid => return None,
                    Topic::No => self.definition(table, type_name.as_deref()?)?,
                },
                type_name: type_name?,
                fields: path_fields(fields),
            }),
        };
        match declaration.compile() {
            Ok(_) => Some(declaration),
            Err(errors) => {
                for error in errors {
                    // Errors say which field by position, 1-based, since names
                    // may repeat (that being one of the problems).
                    let line = error
                        .context_entries()
                        .iter()
                        .find(|(key, _)| *key == "position")
                        .and_then(|(_, position)| position.parse::<usize>().ok())
                        .and_then(|position| lines.get(position.checked_sub(1)?))
                        .copied()
                        .unwrap_or_else(|| line_of(self.file, table, "fields"));
                    self.errors.push(at_line(error, self.file, line));
                }
                None
            }
        }
    }

    /// A decoder's ROS 2 message `type`: required of a cdr decoder, but for
    /// a ROS 2 topic's, which has the topic's; refused on the others.
    fn message_type(
        &mut self,
        table: &dyn TableLike,
        stream_table: &dyn TableLike,
        path: &str,
        kind: Option<Kind>,
        topic: Topic<'_>,
    ) -> Option<String> {
        let type_name = self.parsed::<String>(
            table,
            path,
            "type",
            "a ROS 2 message type, e.g. \"sensor_msgs/msg/JointState\"",
        );
        if !matches!(topic, Topic::No) {
            let topic_type = match topic {
                Topic::Valid(topic) => Some(&topic.message_type),
                Topic::Invalid | Topic::No => None,
            };
            if let (Some(own), Some(topic_type)) = (&type_name, topic_type)
                && own != topic_type
            {
                let detail = format!(
                    "`{path}.type` is \"{own}\", but the topic's messages are \"{topic_type}\"; a ROS 2 topic's decoder needs no `type`"
                );
                self.invalid(table, "type", detail);
            }
            return topic_type.cloned();
        }
        match kind {
            Some(Kind::Cdr) if table.get("type").is_none() => {
                self.invalid(stream_table, "decoder", format!("`{path}` needs `type`"));
            }
            Some(other @ (Kind::Struct | Kind::Path(_))) if table.get("type").is_some() => {
                let detail = format!(
                    "`{path}.type` is for cdr decoders; a {} decoder has no message type",
                    other.name()
                );
                self.invalid(table, "type", detail);
            }
            _ => {}
        }
        type_name
    }

    /// The definition of `type_name`, put together from its `.msg` files; a
    /// missing or unreadable one is reported at the decoder's `type`.
    fn definition(&mut self, table: &dyn TableLike, type_name: &str) -> Option<String> {
        assemble(type_name, self.msg_files)
            .map_err(|error| self.errors.push(at_key(error, self.file, table, "type")))
            .ok()
    }

    /// One `[[...decoder.fields]]` entry of a decoder of `kind` (`None` if
    /// the kind is invalid, when only what every kind shares is checked).
    /// Name, type and either offset or path are required; a missing one is
    /// reported at the entry's first line.
    fn field(&mut self, table: &dyn TableLike, path: &str, kind: Option<Kind>) -> Option<Field> {
        self.unknown_keys(table, path, FIELD_KEYS);
        let name = self.parsed::<String>(table, path, "name", "a text value");
        let types: Vec<(&str, ScalarType)> =
            ScalarType::ALL.iter().map(|ty| (ty.name(), *ty)).collect();
        let ty = self.choice(table, path, "type", &types);
        let offset = self.integer(table, path, "offset", 0..=MAX_OFFSET);
        let field_path = self.parsed::<String>(
            table,
            path,
            "path",
            "a text value, e.g. \"joints.position\"",
        );
        let count = self.integer(table, path, "count", 1..=i64::from(MAX_DATAGRAM_BYTES));
        let endianness = self.choice(
            table,
            path,
            "byte_order",
            &[("little", Endianness::Little), ("big", Endianness::Big)],
        );
        let scale = self.float(table, path, "scale");
        let unit = self.parsed::<String>(table, path, "unit", "a text value, e.g. \"rad\"");
        let mut required = vec![("name", name.is_none()), ("type", ty.is_none())];
        match kind {
            Some(Kind::Struct) => {
                required.push(("offset", offset.is_none()));
                if table.get("path").is_some() {
                    let detail = format!(
                        "`{path}.path` is for json, msgpack and cdr decoders; a struct field has an `offset`"
                    );
                    self.invalid(table, "path", detail);
                }
            }
            Some(by_path @ (Kind::Path(_) | Kind::Cdr)) => {
                required.push(("path", field_path.is_none()));
                for key in ["offset", "byte_order"] {
                    if table.get(key).is_some() {
                        let detail = format!(
                            "`{path}.{key}` is for struct decoders; a {} field has a `path`",
                            by_path.name()
                        );
                        self.invalid(table, key, detail);
                    }
                }
            }
            None => {}
        }
        for (required, value_missing) in required {
            if value_missing && table.get(required).is_none() {
                let first_key = table.iter().next().map_or("name", |(key, _)| key);
                self.invalid(table, first_key, format!("`{path}` needs `{required}`"));
            }
        }
        let has_invalid = (table.get("count").is_some() && count.is_none())
            || (table.get("byte_order").is_some() && endianness.is_none())
            || (table.get("scale").is_some() && scale.is_none())
            || (table.get("unit").is_some() && unit.is_none());
        if has_invalid {
            return None;
        }
        let count = count
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(1);
        match kind? {
            Kind::Struct => Some(Field::Struct(FieldSpec {
                name: name?,
                ty: ty?,
                offset: usize::try_from(offset?).ok()?,
                count,
                endianness: endianness.unwrap_or_default(),
                scale,
                unit,
            })),
            Kind::Path(_) | Kind::Cdr => Some(Field::Path(PathField {
                name: name?,
                path: field_path?,
                ty: ty?,
                count,
                scale,
                unit,
            })),
        }
    }

    /// A number (whole or fractional), or `None` if absent or invalid (an error).
    fn float(&mut self, table: &dyn TableLike, path: &str, name: &str) -> Option<f64> {
        let item = table.get(name)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "a scale written as a whole number is exact well beyond any useful scale"
        )]
        let value = item
            .as_float()
            .or_else(|| item.as_integer().map(|whole| whole as f64));
        if value.is_none() {
            self.invalid(table, name, format!("`{path}.{name}` must be a number"));
        }
        value
    }

    /// The table at `name`, or `None` if it is absent or not a table (an error).
    fn table<'t>(
        &mut self,
        parent: &'t dyn TableLike,
        prefix: &str,
        name: &str,
    ) -> Option<&'t dyn TableLike> {
        let item = parent.get(name)?;
        let table = item.as_table_like();
        if table.is_none() {
            let detail = format!("`{prefix}{name}` must be a table");
            self.invalid(parent, name, detail);
        }
        table
    }

    fn integer(
        &mut self,
        table: &dyn TableLike,
        path: &str,
        name: &str,
        range: RangeInclusive<i64>,
    ) -> Option<i64> {
        let item = table.get(name)?;
        let value = item.as_integer().filter(|value| range.contains(value));
        if value.is_none() {
            let detail = format!(
                "`{path}.{name}` must be a whole number from {} to {}",
                range.start(),
                range.end()
            );
            self.invalid(table, name, detail);
        }
        value
    }

    fn choice<T: Copy>(
        &mut self,
        table: &dyn TableLike,
        path: &str,
        name: &str,
        options: &[(&str, T)],
    ) -> Option<T> {
        let item: &Item = table.get(name)?;
        let chosen = item.as_str().and_then(|text| {
            options
                .iter()
                .find(|(option, _)| *option == text)
                .map(|&(_, value)| value)
        });
        if chosen.is_none() {
            let names: Vec<String> = options
                .iter()
                .map(|(option, _)| format!("\"{option}\""))
                .collect();
            let detail = format!("`{path}.{name}` must be one of {}", names.join(", "));
            self.invalid(table, name, detail);
        }
        chosen
    }

    fn unknown_keys(&mut self, table: &dyn TableLike, path: &str, known: &[&str]) {
        for (name, _) in table.iter() {
            if known.contains(&name) {
                continue;
            }
            let full = if path.is_empty() {
                name.to_owned()
            } else {
                format!("{path}.{name}")
            };
            let suggestion = closest(name, known.iter().copied());
            let error = unknown_key(ErrorCode::StreamUnknownKey, &full, suggestion);
            self.errors.push(at_key(error, self.file, table, name));
        }
    }

    fn invalid(&mut self, table: &dyn TableLike, name: &str, detail: String) {
        let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(detail);
        self.errors.push(at_key(error, self.file, table, name));
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn parse(contents: &str) -> Result<Vec<StreamDeclaration>, Vec<KaptoError>> {
        parse_streams(&ConfigFile {
            path: PathBuf::from("robot/streams.toml"),
            contents: contents.to_owned(),
        })
    }

    /// Each error as `code line: message`, for compact assertions.
    fn problems(contents: &str) -> Vec<String> {
        described(&parse(contents).unwrap_err())
    }

    fn described(errors: &[KaptoError]) -> Vec<String> {
        errors
            .iter()
            .map(|error| {
                let line = error
                    .context_entries()
                    .iter()
                    .find(|(key, _)| *key == "line")
                    .map_or("?", |(_, line)| line.as_str());
                format!("{} {line}: {}", error.code(), error.message())
            })
            .collect()
    }

    #[test]
    fn streams_are_read_in_file_order_with_their_rate_and_sequence() {
        let streams = parse(
            "[streams.arm_joints]\n\
             rate_hz = 2000\n\
             sequence = { offset = 4, type = \"u16\", byte_order = \"big\" }\n\
             \n\
             [streams.camera_info]\n",
        )
        .unwrap();
        assert_eq!(
            streams,
            [
                StreamDeclaration {
                    id: "arm_joints".parse().unwrap(),
                    rate_hz: Some(2000),
                    sequence: Some(SequenceDeclaration {
                        offset: 4,
                        kind: SequenceType::U16,
                        byte_order: ByteOrder::Big,
                    }),
                    loss_settle_ms: None,
                    max_message_bytes: None,
                    udp: None,
                    tcp: None,
                    ros2: None,
                    decoder: None,
                    camera: None,
                },
                StreamDeclaration {
                    id: "camera_info".parse().unwrap(),
                    rate_hz: None,
                    sequence: None,
                    loss_settle_ms: None,
                    max_message_bytes: None,
                    udp: None,
                    tcp: None,
                    ros2: None,
                    decoder: None,
                    camera: None,
                },
            ]
        );
    }

    #[test]
    fn a_tcp_stream_says_it_is_a_camera_of_jpeg_or_raw_frames() {
        let streams = parse(
            "[streams.front]\n\
             tcp = { listen = \"0.0.0.0:7010\", framing = { kind = \"length_prefix\", length_bytes = 4 } }\n\
             camera = { format = \"jpeg\" }\n\
             [streams.wrist]\n\
             max_message_bytes = 921600\n\
             tcp = { listen = \"0.0.0.0:7011\", framing = { kind = \"length_prefix\", length_bytes = 4 } }\n\
             camera = { format = \"raw\", width = 640, height = 480, encoding = \"rgb8\" }\n\
             [streams.odometry]\n\
             tcp = { listen = \"0.0.0.0:7012\", framing = { kind = \"fixed\", size = 8 } }\n",
        )
        .unwrap();
        assert_eq!(streams[0].camera, Some(CameraDeclaration::Jpeg));
        assert_eq!(
            streams[1].camera,
            Some(CameraDeclaration::Raw {
                width: 640,
                height: 480,
                encoding: PixelEncoding::Rgb8,
            })
        );
        assert_eq!(streams[1].camera.unwrap().frame_bytes(), Some(921_600));
        let cameras: Vec<bool> = streams.iter().map(StreamDeclaration::is_camera).collect();
        assert_eq!(cameras, [true, true, false]);
    }

    #[test]
    fn a_ros_2_topic_is_a_camera_by_its_type_alone() {
        let mut stream = parse("[streams.front]\n").unwrap().remove(0);
        for (message_type, camera) in [
            ("sensor_msgs/msg/CompressedImage", true),
            ("sensor_msgs/msg/Image", true),
            ("sensor_msgs/msg/CameraInfo", false),
        ] {
            stream.ros2 = Some(Ros2Declaration {
                topic: "/camera".into(),
                message_type: message_type.into(),
                definition: String::new(),
                qos: QosDeclaration::default(),
            });
            assert_eq!(stream.is_camera(), camera, "{message_type}");
        }
    }

    #[test]
    fn camera_mistakes_are_reported_with_their_lines() {
        let listen = "tcp = { listen = \"0.0.0.0:7010\", framing = { kind = \"length_prefix\", length_bytes = 4 } }";
        assert_eq!(
            problems(&format!(
                "[streams.a]\n{listen}\ncamera = {{ format = \"png\" }}\n\
                 [streams.b]\nudp = {{ bind = \"0.0.0.0:9000\" }}\ncamera = {{ format = \"jpeg\" }}\n\
                 [streams.c]\ntcp = {{ listen = \"0.0.0.0:7011\", framing = {{ kind = \"fixed\", size = 1000 }} }}\n\
                 camera = {{ format = \"raw\", width = 640, height = 480, encoding = \"rgb8\" }}\n\
                 [streams.d]\n{}\ncamera = {{ format = \"raw\", width = 640, encoding = \"yuv422\" }}\n\
                 [streams.e]\n{}\ncamera = {{ format = \"jpeg\", width = 640 }}\n\
                 [streams.f]\nmax_message_bytes = 1000\n{}\n\
                 camera = {{ format = \"raw\", width = 640, height = 480, encoding = \"mono8\" }}\n\
                 [streams.g]\n{}\ncamera = {{ format = \"raw\", width = 16384, height = 16384, encoding = \"rgba8\" }}\n",
                listen.replace("7010", "7013"),
                listen.replace("7010", "7014"),
                listen.replace("7010", "7015"),
                listen.replace("7010", "7016"),
            )),
            [
                "config.invalid_value 3: Invalid configuration value: `streams.a.camera.format` must be one of \"jpeg\", \"raw\"",
                "config.invalid_value 6: Invalid configuration value: `streams.b.camera` is for a TCP stream: a camera's frames don't fit in UDP datagrams",
                "config.invalid_value 9: Invalid configuration value: `streams.c.camera` frames are 921600 bytes, but `tcp.framing.size` is 1000",
                "config.invalid_value 12: Invalid configuration value: `streams.d.camera.encoding` must be one of \"rgb8\", \"bgr8\", \"rgba8\", \"bgra8\", \"mono8\"",
                "config.invalid_value 12: Invalid configuration value: `streams.d.camera` of raw frames needs `height`",
                "config.invalid_value 15: Invalid configuration value: `streams.e.camera.width` is for raw frames; a JPEG says its own",
                "config.invalid_value 19: Invalid configuration value: `streams.f.camera` frames are 307200 bytes, but `max_message_bytes` is 1000",
                "config.invalid_value 22: Invalid configuration value: `streams.g.camera` frames are 1073741824 bytes, more than the 33554432 a message may be",
            ]
        );
    }

    #[test]
    fn a_ros_2_topic_declaring_a_camera_table_is_told_its_type_says_so() {
        let folder = package_with_msg_files("camera-table");
        let errors = parse_in(
            &folder,
            "[streams.status]\nros2 = { topic = \"/status\", type = \"std_msgs/msg/Header\" }\n\
             camera = { format = \"jpeg\" }\n",
        )
        .unwrap_err();
        std::fs::remove_dir_all(folder).unwrap();
        assert_eq!(
            described(&errors),
            [
                "config.invalid_value 3: Invalid configuration value: `streams.status.camera` is for a TCP stream: a ROS 2 topic is a camera by its type, sensor_msgs/msg/CompressedImage or sensor_msgs/msg/Image"
            ]
        );
    }

    #[test]
    fn a_tcp_or_ros_2_stream_takes_messages_up_to_32_mib_and_a_udp_one_a_datagram() {
        let tcp = |name: &str, bytes: u32, port: u16| {
            format!(
                "[streams.{name}]\nmax_message_bytes = {bytes}\n\
                 tcp = {{ listen = \"127.0.0.1:{port}\", framing = {{ kind = \"length_prefix\", length_bytes = 4 }} }}\n"
            )
        };
        let streams = parse(&tcp("camera", 32 << 20, 9100)).unwrap();
        assert_eq!(streams[0].max_message_bytes, Some(32 << 20));

        let found = problems(&format!(
            "[streams.arm]\nmax_message_bytes = 65508\nudp = {{ bind = \"127.0.0.1:9000\" }}\n{}",
            tcp("camera", (32 << 20) + 1, 9100)
        ));
        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(found[0].contains("`streams.arm.max_message_bytes` is 65508, but a UDP datagram holds at most 65507 bytes: only a TCP or ROS 2 stream's messages can be larger"), "{}", found[0]);
        assert!(
            found[1].contains(
                "`streams.camera.max_message_bytes` must be a whole number from 1 to 33554432"
            ),
            "{}",
            found[1]
        );
    }

    #[test]
    fn a_rate_only_stream_can_say_how_long_a_silence_settles() {
        let streams = parse("[streams.arm]\nrate_hz = 2000\nloss_settle_ms = 250\n").unwrap();
        assert_eq!(streams[0].loss_settle_ms, Some(250));
        assert_eq!(
            parse("[streams.arm]\nrate_hz = 2000\n").unwrap()[0].loss_settle_ms,
            None
        );
    }

    #[test]
    fn settling_a_silence_is_refused_where_loss_isnt_estimated_from_the_rate() {
        let found = problems(
            "[streams.numbered]\nrate_hz = 100\nloss_settle_ms = 250\n\
             sequence = { offset = 0, type = \"u32\" }\n\
             [streams.no_rate]\nloss_settle_ms = 250\n\
             [streams.too_long]\nrate_hz = 100\nloss_settle_ms = 60001\n",
        );
        assert_eq!(found.len(), 3, "{found:#?}");
        assert!(found[0].contains("`streams.numbered.loss_settle_ms` is for a stream whose loss is estimated from its `rate_hz`, without a `sequence`"), "{}", found[0]);
        assert!(
            found[1].starts_with("config.invalid_value 6: Invalid configuration value: `streams.no_rate.loss_settle_ms` is for"),
            "{}",
            found[1]
        );
        assert!(
            found[2].contains(
                "`streams.too_long.loss_settle_ms` must be a whole number from 1 to 60000"
            ),
            "{}",
            found[2]
        );
    }

    #[test]
    fn byte_order_defaults_to_little_endian() {
        let streams = parse("[streams.joints.sequence]\noffset = 0\ntype = \"u32\"\n").unwrap();
        assert_eq!(streams[0].sequence.unwrap().byte_order, ByteOrder::Little);
    }

    #[test]
    fn an_empty_file_declares_no_streams() {
        assert_eq!(parse("").unwrap(), []);
    }

    #[test]
    fn every_problem_is_reported_at_once_with_its_line() {
        let found = problems(
            "[streams.Arm]\n\
             rate_hz = 0\n\
             \n\
             [streams.joints]\n\
             rat_hz = 100\n\
             [streams.joints.sequence]\n\
             type = \"i32\"\n\
             byte_order = \"middle\"\n\
             \n\
             [robot]\n",
        );
        assert_eq!(
            found,
            [
                "stream.unknown_key 10: Unknown key in a stream declaration: `robot`",
                "stream.invalid_id 1: Invalid stream ID: `Arm`",
                "config.invalid_value 2: Invalid configuration value: `streams.Arm.rate_hz` must be a whole number from 1 to 1000000",
                "stream.unknown_key 5: Unknown key in a stream declaration: `streams.joints.rat_hz` (did you mean `rate_hz`?)",
                "config.invalid_value 7: Invalid configuration value: `streams.joints.sequence.type` must be one of \"u8\", \"u16\", \"u32\", \"u64\"",
                "config.invalid_value 8: Invalid configuration value: `streams.joints.sequence.byte_order` must be one of \"little\", \"big\"",
                "config.invalid_value 6: Invalid configuration value: `streams.joints.sequence` needs `offset`",
            ]
        );
    }

    #[test]
    fn values_of_the_wrong_kind_are_rejected() {
        assert_eq!(
            problems("streams = 3\n"),
            ["config.invalid_value 1: Invalid configuration value: `streams` must be a table"]
        );
        assert_eq!(
            problems("[streams]\njoints = \"udp\"\n"),
            [
                "config.invalid_value 2: Invalid configuration value: `streams.joints` must be a table"
            ]
        );
        assert_eq!(
            problems("[streams.joints.sequence]\noffset = 70000\ntype = \"u8\"\n"),
            [
                "config.invalid_value 2: Invalid configuration value: `streams.joints.sequence.offset` must be a whole number from 0 to 65507"
            ]
        );
    }

    #[test]
    fn a_stream_declares_where_it_arrives_over_udp() {
        let streams = parse(
            "[streams.arm]\n\
             max_message_bytes = 512\n\
             udp = { bind = \"0.0.0.0:9000\", allowed_sources = [\"10.0.0.5\", \"::1\"] }\n\
             [streams.gripper.udp]\n\
             bind = \"0.0.0.0:9100\"\n\
             multicast_group = \"239.255.0.1\"\n",
        )
        .unwrap();
        assert_eq!(streams[0].max_message_bytes, Some(512));
        assert_eq!(
            streams[0].udp,
            Some(UdpDeclaration {
                bind: "0.0.0.0:9000".parse().unwrap(),
                multicast_group: None,
                allowed_sources: vec!["10.0.0.5".parse().unwrap(), "::1".parse().unwrap()],
            })
        );
        let gripper = streams[1].udp.as_ref().unwrap();
        assert_eq!(
            gripper.multicast_group,
            Some("239.255.0.1".parse().unwrap())
        );
    }

    #[test]
    fn invalid_udp_endpoints_are_reported() {
        assert_eq!(
            problems(
                "[streams.a.udp]\nbind = \"0.0.0.0:0\"\n\
                 [streams.b.udp]\nbind = \"9000\"\n\
                 [streams.c.udp]\nbind = \"[::]:9001\"\nmulticast_group = \"239.0.0.1\"\n\
                 [streams.d.udp]\nbind = \"0.0.0.0:9002\"\nallowed_sources = [\"robot\"]\n\
                 [streams.e]\nudp = {}\n"
            ),
            [
                "config.invalid_value 2: Invalid configuration value: `streams.a.udp.bind` needs a port from 1 to 65535: the robot sends to a fixed port",
                "config.invalid_value 4: Invalid configuration value: `streams.b.udp.bind` must be an address and port, e.g. \"0.0.0.0:9000\"",
                "config.invalid_value 7: Invalid configuration value: `streams.c.udp.multicast_group` must be a multicast address of the same IP version as `bind`",
                "config.invalid_value 10: Invalid configuration value: `streams.d.udp.allowed_sources` must be a list of IP addresses, e.g. [\"10.0.0.5\"]",
                "config.invalid_value 12: Invalid configuration value: `streams.e.udp` needs `bind`",
            ]
        );
    }

    #[test]
    fn two_unicast_streams_on_one_port_conflict_but_multicast_groups_may_share_it() {
        let conflict = problems(
            "[streams.a]\nudp = { bind = \"0.0.0.0:9000\" }\n\
             [streams.b]\nudp = { bind = \"127.0.0.1:9000\" }\n",
        );
        assert_eq!(
            conflict,
            [
                "config.port_conflict ?: Two listeners are configured on the same port: streams `a` (0.0.0.0:9000) and `b` (127.0.0.1:9000) both listen on UDP port 9000"
            ]
        );
        parse(
            "[streams.a]\nudp = { bind = \"0.0.0.0:9000\", multicast_group = \"239.0.0.1\" }\n\
             [streams.b]\nudp = { bind = \"0.0.0.0:9000\", multicast_group = \"239.0.0.2\" }\n\
             [streams.c]\nudp = { bind = \"127.0.0.1:9001\" }\n\
             [streams.d]\nudp = { bind = \"127.0.0.2:9001\" }\n",
        )
        .unwrap();
    }

    #[test]
    fn a_stream_declares_where_it_arrives_over_tcp_and_how_it_is_framed() {
        let streams = parse(
            "[streams.odometry.tcp]\n\
             connect = \"192.168.1.10:7000\"\n\
             reconnect_initial_ms = 50\n\
             reconnect_max_ms = 2000\n\
             framing = { kind = \"length_prefix\", length_bytes = 2, byte_order = \"big\" }\n\
             [streams.status.tcp]\n\
             listen = \"0.0.0.0:7001\"\n\
             framing = { kind = \"delimiter\", delimiter = \"\\r\\n\" }\n\
             [streams.joints.tcp]\n\
             listen = \"0.0.0.0:7002\"\n\
             [streams.joints.tcp.framing]\n\
             kind = \"length_prefix\"\n\
             header_bytes = 8\n\
             length_offset = 4\n\
             length_bytes = 4\n\
             includes_header = true\n\
             [streams.imu.tcp]\n\
             listen = \"[::]:7003\"\n\
             framing = { kind = \"fixed\", size = 68 }\n",
        )
        .unwrap();
        let tcp: Vec<_> = streams
            .iter()
            .map(|stream| stream.tcp.clone().unwrap())
            .collect();
        assert_eq!(
            tcp[0],
            TcpDeclaration {
                endpoint: TcpEndpointDeclaration::Connect("192.168.1.10:7000".parse().unwrap()),
                framing: FramingDeclaration::LengthPrefix {
                    header_bytes: 2,
                    length_offset: 0,
                    length_bytes: 2,
                    byte_order: ByteOrder::Big,
                    includes_header: false,
                },
                reconnect_initial_ms: Some(50),
                reconnect_max_ms: Some(2000),
            }
        );
        assert_eq!(
            tcp[1].endpoint,
            TcpEndpointDeclaration::Listen("0.0.0.0:7001".parse().unwrap())
        );
        assert_eq!(
            tcp[1].framing,
            FramingDeclaration::Delimiter(b"\r\n".to_vec())
        );
        assert_eq!(
            tcp[2].framing,
            FramingDeclaration::LengthPrefix {
                header_bytes: 8,
                length_offset: 4,
                length_bytes: 4,
                byte_order: ByteOrder::Little,
                includes_header: true,
            }
        );
        assert_eq!(tcp[3].framing, FramingDeclaration::Fixed(68));
        assert_eq!(tcp[3].reconnect_initial_ms, None);
    }

    #[test]
    fn invalid_tcp_endpoints_are_reported() {
        assert_eq!(
            problems(
                "[streams.a.tcp]\nconnect = \"10.0.0.1:7000\"\nlisten = \"0.0.0.0:7000\"\nframing = { kind = \"fixed\", size = 4 }\n\
                 [streams.b]\ntcp = { framing = { kind = \"fixed\", size = 4 } }\n\
                 [streams.c.tcp]\nlisten = \"0.0.0.0:0\"\nframing = { kind = \"fixed\", size = 4 }\n\
                 [streams.d.tcp]\nconnect = \"robot:7000\"\nframing = { kind = \"fixed\", size = 4 }\n\
                 [streams.e]\nudp = { bind = \"0.0.0.0:9000\" }\ntcp = { listen = \"0.0.0.0:9000\", framing = { kind = \"fixed\", size = 4 } }\n\
                 [streams.f.tcp]\nconnect = \"10.0.0.1:7000\"\n\
                 reconnect_initial_ms = 500\nreconnect_max_ms = 100\nframing = { kind = \"fixed\", size = 4 }\n"
            ),
            [
                "config.invalid_value 3: Invalid configuration value: `streams.a.tcp` has both `connect` and `listen`: Kapto either connects to the robot or listens for it",
                "config.invalid_value 6: Invalid configuration value: `streams.b.tcp` needs `connect` or `listen`",
                "config.invalid_value 8: Invalid configuration value: `streams.c.tcp.listen` needs a port from 1 to 65535",
                "config.invalid_value 11: Invalid configuration value: `streams.d.tcp.connect` must be an address and port, e.g. \"192.168.1.10:7000\"",
                "config.invalid_value 15: Invalid configuration value: `streams.e` has both `udp` and `tcp`: a stream arrives over one",
                "config.invalid_value 19: Invalid configuration value: `streams.f.tcp.reconnect_max_ms` (100) is shorter than `reconnect_initial_ms` (500)",
            ]
        );
    }

    #[test]
    fn invalid_framings_are_reported_with_their_lines() {
        assert_eq!(
            problems(
                "[streams.a.tcp]\nlisten = \"0.0.0.0:7000\"\n\
                 [streams.b.tcp]\nlisten = \"0.0.0.0:7001\"\nframing = { kind = \"cobs\" }\n\
                 [streams.c.tcp]\nlisten = \"0.0.0.0:7002\"\nframing = { kind = \"fixed\", delimiter = \"\\n\", size = 4 }\n\
                 [streams.d.tcp]\nlisten = \"0.0.0.0:7003\"\nframing = { kind = \"length_prefix\", length_bytes = 3 }\n\
                 [streams.e.tcp]\nlisten = \"0.0.0.0:7004\"\nframing = { kind = \"length_prefix\", length_bytes = 4, length_offset = 2, header_bytes = 4 }\n\
                 [streams.f.tcp]\nlisten = \"0.0.0.0:7005\"\nframing = { kind = \"delimiter\", delimiter = \"\" }\n\
                 [streams.g.tcp]\nlisten = \"0.0.0.0:7006\"\nframing = { kind = \"delimiter\" }\n"
            ),
            [
                "config.invalid_value 1: Invalid configuration value: `streams.a.tcp` needs `framing`: how the robot marks where each message ends",
                "config.invalid_value 5: Invalid configuration value: `streams.b.tcp.framing.kind` must be one of \"length_prefix\", \"delimiter\", \"fixed\"",
                "config.invalid_value 8: Invalid configuration value: `streams.c.tcp.framing.delimiter` doesn't apply to kind = \"fixed\", which takes `size`",
                "config.invalid_value 11: Invalid configuration value: `streams.d.tcp.framing.length_bytes` must be 1, 2, 4 or 8",
                "config.invalid_value 14: Invalid configuration value: `streams.e.tcp.framing.header_bytes` is 4, but the length ends at byte 6",
                "config.invalid_value 17: Invalid configuration value: `streams.f.tcp.framing.delimiter` can't be empty",
                "config.invalid_value 20: Invalid configuration value: `streams.g.tcp.framing` needs `delimiter`",
            ]
        );
    }

    #[test]
    fn a_fixed_frame_longer_than_the_largest_declared_message_is_reported() {
        assert_eq!(
            problems(
                "[streams.imu]\nmax_message_bytes = 32\n\
                 tcp = { listen = \"0.0.0.0:7000\", framing = { kind = \"fixed\", size = 68 } }\n"
            ),
            [
                "config.invalid_value 2: Invalid configuration value: `streams.imu.tcp.framing.size` is 68, but `max_message_bytes` is 32"
            ]
        );
    }

    #[test]
    fn two_tcp_streams_listening_on_one_port_conflict_but_udp_may_share_its_number() {
        let fixed = "framing = { kind = \"fixed\", size = 4 }";
        let conflict = problems(&format!(
            "[streams.a.tcp]\nlisten = \"0.0.0.0:7000\"\n{fixed}\n\
             [streams.b.tcp]\nlisten = \"127.0.0.1:7000\"\n{fixed}\n"
        ));
        assert_eq!(
            conflict,
            [
                "config.port_conflict ?: Two listeners are configured on the same port: streams `a` (0.0.0.0:7000) and `b` (127.0.0.1:7000) both listen on TCP port 7000"
            ]
        );
        parse(&format!(
            "[streams.a.tcp]\nlisten = \"0.0.0.0:7000\"\n{fixed}\n\
             [streams.b]\nudp = {{ bind = \"0.0.0.0:7000\" }}\n\
             [streams.c.tcp]\nconnect = \"10.0.0.1:7000\"\n{fixed}\n"
        ))
        .unwrap();
    }

    fn layout(contents: &str) -> StructLayout {
        let mut streams = parse(contents).unwrap();
        match streams.remove(0).decoder {
            Some(DecoderDeclaration::Struct(layout)) => layout,
            other => panic!("not a struct decoder: {other:?}"),
        }
    }

    #[test]
    fn a_struct_decoder_declares_its_fields_with_defaults_filled_in() {
        let layout = layout(
            "[streams.arm.decoder]\n\
             kind = \"struct\"\n\
             \n\
             [[streams.arm.decoder.fields]]\n\
             name = \"sequence\"\n\
             type = \"u32\"\n\
             offset = 0\n\
             \n\
             [[streams.arm.decoder.fields]]\n\
             name = \"joint_positions\"\n\
             type = \"i32\"\n\
             offset = 4\n\
             count = 7\n\
             byte_order = \"big\"\n\
             scale = 0.001\n\
             unit = \"rad\"\n",
        );
        assert_eq!(
            layout.fields,
            [
                FieldSpec::new("sequence", ScalarType::U32, 0),
                FieldSpec {
                    count: 7,
                    endianness: Endianness::Big,
                    scale: Some(0.001),
                    unit: Some("rad".to_owned()),
                    ..FieldSpec::new("joint_positions", ScalarType::I32, 4)
                },
            ]
        );
    }

    #[test]
    fn fields_can_also_be_an_inline_list_and_a_whole_number_scale_is_accepted() {
        let layout = layout(
            "[streams.arm]\n\
             decoder = { kind = \"struct\", fields = [ { name = \"a\", type = \"u8\", offset = 0, scale = 2 } ] }\n",
        );
        assert_eq!(layout.fields[0].scale, Some(2.0));
    }

    /// A robot package's folder with `.msg` files for a joint state: its
    /// own, and those of the types it uses.
    fn package_with_msg_files(test: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("kapto-cdr-{}-{test}", std::process::id()));
        let files = [
            (
                "sensor_msgs/msg/JointState.msg",
                "std_msgs/Header header\nstring[] name\nfloat64[] position\n",
            ),
            (
                "std_msgs/msg/Header.msg",
                "builtin_interfaces/Time stamp\nstring frame_id\n",
            ),
            (
                "builtin_interfaces/msg/Time.msg",
                "int32 sec\nuint32 nanosec\n",
            ),
        ];
        for (path, text) in files {
            let path = folder.join("msg").join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        folder
    }

    fn parse_in(folder: &Path, contents: &str) -> Result<Vec<StreamDeclaration>, Vec<KaptoError>> {
        parse_streams(&ConfigFile {
            path: folder.join("streams.toml"),
            contents: contents.to_owned(),
        })
    }

    const JOINT_STATE: &str = r#"
[streams.joints]
udp = { bind = "0.0.0.0:9000" }
decoder = { kind = "cdr", type = "sensor_msgs/msg/JointState", fields = [
  { name = "position", type = "f64", path = "position", count = 7, unit = "rad" },
  { name = "stamp", type = "i32", path = "header.stamp.sec" } ] }
"#;

    #[test]
    fn a_cdr_decoder_puts_its_type_together_from_the_msg_files_beside_streams_toml() {
        let folder = package_with_msg_files("together");
        let streams = parse_in(&folder, JOINT_STATE).unwrap();
        let Some(DecoderDeclaration::Cdr(layout)) = &streams[0].decoder else {
            panic!("{:?}", streams[0].decoder);
        };
        assert_eq!(layout.type_name, "sensor_msgs/msg/JointState");
        assert!(
            layout.definition.starts_with("std_msgs/Header header\n"),
            "{}",
            layout.definition
        );
        assert!(layout.definition.contains("\nMSG: std_msgs/Header\n"));
        assert!(
            layout
                .definition
                .ends_with("MSG: builtin_interfaces/Time\nint32 sec\nuint32 nanosec\n")
        );
        let paths: Vec<&str> = layout
            .fields
            .iter()
            .map(|field| field.path.as_str())
            .collect();
        assert_eq!(paths, ["position", "header.stamp.sec"]);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn cdr_decoder_mistakes_are_reported_with_their_lines() {
        let folder = package_with_msg_files("mistakes");
        std::fs::remove_file(folder.join("msg/builtin_interfaces/msg/Time.msg")).unwrap();
        let errors = parse_in(&folder, JOINT_STATE).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].code(), ErrorCode::DecoderMsgNotFound);
        assert!(
            errors[0]
                .to_string()
                .contains("builtin_interfaces/msg/Time.msg")
        );
        std::fs::remove_dir_all(folder).unwrap();

        assert_eq!(
            problems(
                "[streams.a]\ndecoder = { kind = \"cdr\", fields = [] }\n\
                 [streams.b]\ndecoder = { kind = \"struct\", type = \"std_msgs/msg/String\", fields = [ { name = \"x\", type = \"u8\", offset = 0 } ] }\n\
                 [streams.c]\ndecoder = { kind = \"cdr\", type = \"a/msg/B\", fields = [\n\
                   { name = \"x\", type = \"f64\", offset = 0 } ] }\n"
            ),
            [
                "config.invalid_value 2: Invalid configuration value: `streams.a.decoder` needs `type`",
                "config.invalid_value 4: Invalid configuration value: `streams.b.decoder.type` is for cdr decoders; a struct decoder has no message type",
                "config.invalid_value 7: Invalid configuration value: `streams.c.decoder.fields[0].offset` is for struct decoders; a cdr field has a `path`",
                "config.invalid_value 7: Invalid configuration value: `streams.c.decoder.fields[0]` needs `path`",
            ]
        );
    }

    const TOPICS: &str = r#"
[streams.joints]
ros2 = { topic = "/joint_states", type = "sensor_msgs/msg/JointState" }
decoder = { kind = "cdr", fields = [
  { name = "position", type = "f64", path = "position", count = 7, unit = "rad" } ] }

[streams.status]
rate_hz = 10
ros2 = { topic = "robot/status", type = "std_msgs/msg/Header", qos = { reliability = "reliable", durability = "transient_local", depth = 100 } }
"#;

    #[test]
    fn a_ros_2_topic_is_declared_with_its_type_and_qos_and_its_decoder_takes_its_type() {
        let folder = package_with_msg_files("topics");
        let streams = parse_in(&folder, TOPICS).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
        let joints = streams[0].ros2.as_ref().unwrap();
        assert_eq!(
            (joints.topic.as_str(), joints.message_type.as_str()),
            ("/joint_states", "sensor_msgs/msg/JointState")
        );
        assert!(joints.definition.contains("\nMSG: std_msgs/Header\n"));
        assert_eq!(joints.qos, QosDeclaration::default());
        let Some(DecoderDeclaration::Cdr(layout)) = &streams[0].decoder else {
            panic!("{:?}", streams[0].decoder);
        };
        assert_eq!(
            (&layout.type_name, &layout.definition),
            (&joints.message_type, &joints.definition)
        );

        let status = streams[1].ros2.as_ref().unwrap();
        assert_eq!(status.topic, "robot/status");
        assert_eq!(
            status.qos,
            QosDeclaration {
                reliability: Reliability::Reliable,
                durability: Durability::TransientLocal,
                depth: 100,
            }
        );
        assert_eq!(streams[1].decoder, None);
    }

    #[test]
    fn ros_2_topic_mistakes_are_reported_with_their_lines() {
        assert_eq!(
            problems(
                "[streams.a]\nros2 = { topic = \"/2arm\", type = \"std_msgs/String\", qos = { reliability = \"sometimes\", depth = 0, history = 1 } }\n\
                 [streams.b]\nudp = { bind = \"0.0.0.0:9000\" }\nros2 = { topic = \"/b\" }\n\
                 [streams.c]\nros2 = { topic = \"/c\", type = \"nowhere_msgs/msg/Gone\" }\n"
            ),
            [
                "config.invalid_value 2: Invalid configuration value: `streams.a.ros2.topic` must be a ROS 2 topic name: words of letters, digits and single `_`, separated by `/`, none starting with a digit, e.g. \"/joint_states\"",
                "config.invalid_value 2: Invalid configuration value: `streams.a.ros2.type` must be a ROS 2 message type, `<package>/msg/<Type>`, e.g. \"sensor_msgs/msg/JointState\"",
                "stream.unknown_key 2: Unknown key in a stream declaration: `streams.a.ros2.qos.history`",
                "config.invalid_value 2: Invalid configuration value: `streams.a.ros2.qos.reliability` must be one of \"best_effort\", \"reliable\"",
                "config.invalid_value 2: Invalid configuration value: `streams.a.ros2.qos.depth` must be a whole number from 1 to 100000",
                "config.invalid_value 5: Invalid configuration value: `streams.b.ros2` needs `type`",
                "config.invalid_value 5: Invalid configuration value: `streams.b` has both `udp` and `ros2`: a stream arrives over one",
                "decoder.msg_not_found 7: A ROS 2 message type's definition wasn't found: no `nowhere_msgs/msg/Gone.msg`",
            ]
        );

        let folder = package_with_msg_files("topic-decoders");
        let errors = parse_in(
            &folder,
            "[streams.a]\nros2 = { topic = \"/a\", type = \"sensor_msgs/msg/JointState\" }\n\
             decoder = { kind = \"json\", fields = [{ name = \"p\", type = \"f64\", path = \"position\" }] }\n\
             [streams.b]\nros2 = { topic = \"/b\", type = \"sensor_msgs/msg/JointState\" }\n\
             decoder = { kind = \"cdr\", type = \"std_msgs/msg/Header\", fields = [{ name = \"p\", type = \"f64\", path = \"position\" }] }\n",
        )
        .unwrap_err();
        std::fs::remove_dir_all(folder).unwrap();
        assert_eq!(
            described(&errors),
            [
                "config.invalid_value 3: Invalid configuration value: `streams.a.decoder.kind` must be \"cdr\" for a ROS 2 topic: its messages are ROS 2's own",
                "config.invalid_value 6: Invalid configuration value: `streams.b.decoder.type` is \"std_msgs/msg/Header\", but the topic's messages are \"sensor_msgs/msg/JointState\"; a ROS 2 topic's decoder needs no `type`",
            ]
        );
    }

    #[test]
    fn msg_files_not_in_the_package_are_looked_for_in_each_ros_2_installation_in_order() {
        let paths = std::env::join_paths(["/opt/ros/jazzy", "", "/home/me/ws/install"]).unwrap();
        assert_eq!(
            installed_msg_folders(Some(&paths)),
            [
                PathBuf::from("/opt/ros/jazzy/share"),
                PathBuf::from("/home/me/ws/install/share")
            ]
        );
        assert_eq!(installed_msg_folders(None), Vec::<PathBuf>::new());
    }

    #[test]
    fn json_and_msgpack_decoders_find_each_field_by_its_path() {
        for (kind, format) in [("json", PathFormat::Json), ("msgpack", PathFormat::MsgPack)] {
            let mut streams = parse(&format!(
                "[streams.arm.decoder]\n\
                 kind = \"{kind}\"\n\
                 \n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"sequence\"\n\
                 type = \"u32\"\n\
                 path = \"header.seq\"\n\
                 \n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"position\"\n\
                 type = \"f64\"\n\
                 path = \"joints.position\"\n\
                 count = 7\n\
                 unit = \"rad\"\n"
            ))
            .unwrap();
            assert_eq!(
                streams.remove(0).decoder,
                Some(DecoderDeclaration::Path(PathLayout {
                    format,
                    fields: vec![
                        PathField::new("sequence", "header.seq", ScalarType::U32),
                        PathField {
                            count: 7,
                            unit: Some("rad".to_owned()),
                            ..PathField::new("position", "joints.position", ScalarType::F64)
                        },
                    ],
                }))
            );
        }
    }

    #[test]
    fn struct_and_path_fields_keep_to_their_own_keys() {
        assert_eq!(
            problems(
                "[streams.arm.decoder]\n\
                 kind = \"json\"\n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"a\"\n\
                 type = \"u8\"\n\
                 offset = 0\n\
                 byte_order = \"big\"\n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"b\"\n\
                 type = \"u8\"\n\
                 path = \"b..c\"\n\
                 [streams.leg.decoder]\n\
                 kind = \"struct\"\n\
                 [[streams.leg.decoder.fields]]\n\
                 name = \"a\"\n\
                 type = \"u8\"\n\
                 path = \"a\"\n"
            ),
            [
                "config.invalid_value 6: Invalid configuration value: `streams.arm.decoder.fields[0].offset` is for struct decoders; a json field has a `path`",
                "config.invalid_value 7: Invalid configuration value: `streams.arm.decoder.fields[0].byte_order` is for struct decoders; a json field has a `path`",
                "config.invalid_value 4: Invalid configuration value: `streams.arm.decoder.fields[0]` needs `path`",
                "config.invalid_value 17: Invalid configuration value: `streams.leg.decoder.fields[0].path` is for json, msgpack and cdr decoders; a struct field has an `offset`",
                "config.invalid_value 15: Invalid configuration value: `streams.leg.decoder.fields[0]` needs `offset`",
            ]
        );
        assert_eq!(
            problems(
                "[streams.arm.decoder]\n\
                 kind = \"msgpack\"\n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"b\"\n\
                 type = \"u8\"\n\
                 path = \"b..c\"\n"
            ),
            ["decoder.invalid_layout 4: Invalid message layout: `b..c` has an empty key"]
        );
    }

    #[test]
    fn decoder_mistakes_are_reported_with_their_lines() {
        assert_eq!(
            problems(
                "[streams.arm.decoder]\n\
                 kind = \"protobuf\"\n\
                 fields = []\n\
                 \n\
                 [streams.leg.decoder]\n\
                 kind = \"struct\"\n\
                 fields = 3\n\
                 \n\
                 [streams.hand.decoder]\n\
                 kind = \"struct\"\n"
            ),
            [
                "config.invalid_value 2: Invalid configuration value: `streams.arm.decoder.kind` must be one of \"struct\", \"json\", \"msgpack\", \"cdr\"",
                "config.invalid_value 7: Invalid configuration value: `streams.leg.decoder.fields` must be a list of tables, e.g. [[streams.leg.decoder.fields]]",
                "config.invalid_value 9: Invalid configuration value: `streams.hand.decoder` needs `fields`",
            ]
        );
    }

    #[test]
    fn field_mistakes_are_reported_with_their_lines() {
        assert_eq!(
            problems(
                "[streams.arm.decoder]\n\
                 kind = \"struct\"\n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"a\"\n\
                 type = \"u24\"\n\
                 offset = 0\n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"b\"\n\
                 typ = \"u8\"\n"
            ),
            [
                "config.invalid_value 5: Invalid configuration value: `streams.arm.decoder.fields[0].type` must be one of \"u8\", \"i8\", \"u16\", \"i16\", \"u32\", \"i32\", \"u64\", \"i64\", \"f32\", \"f64\", \"bool\"",
                "stream.unknown_key 9: Unknown key in a stream declaration: `streams.arm.decoder.fields[1].typ` (did you mean `type`?)",
                "config.invalid_value 8: Invalid configuration value: `streams.arm.decoder.fields[1]` needs `type`",
                "config.invalid_value 8: Invalid configuration value: `streams.arm.decoder.fields[1]` needs `offset`",
            ]
        );
    }

    #[test]
    fn layout_problems_point_at_the_field_they_belong_to() {
        assert_eq!(
            problems(
                "[streams.arm.decoder]\n\
                 kind = \"struct\"\n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"a\"\n\
                 type = \"u8\"\n\
                 offset = 0\n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"a\"\n\
                 type = \"bool\"\n\
                 offset = 1\n\
                 scale = 2.0\n"
            ),
            [
                "decoder.invalid_layout 8: Invalid message layout: `a` is declared more than once",
                "decoder.invalid_layout 8: Invalid message layout: `a` is a bool, which can't be scaled",
            ]
        );
    }

    #[test]
    fn a_layout_longer_than_the_largest_declared_message_is_reported() {
        assert_eq!(
            problems(
                "[streams.arm]\n\
                 max_message_bytes = 8\n\
                 [streams.arm.decoder]\n\
                 kind = \"struct\"\n\
                 [[streams.arm.decoder.fields]]\n\
                 name = \"time\"\n\
                 type = \"u64\"\n\
                 offset = 4\n"
            ),
            [
                "config.invalid_value 2: Invalid configuration value: `streams.arm.decoder` reads up to byte 12, but `max_message_bytes` is 8"
            ]
        );
    }

    #[test]
    fn the_unknown_key_remedy_names_every_accepted_key() {
        let remedy = ErrorCode::StreamUnknownKey.remedy();
        let keys = STREAM_KEYS.iter().chain(SEQUENCE_KEYS).chain(UDP_KEYS);
        let keys = keys.chain(tcp::TCP_KEYS).chain(tcp::FRAMING_KEYS);
        let keys = keys.chain(ros2::ROS2_KEYS).chain(ros2::QOS_KEYS);
        let keys = keys.chain(DECODER_KEYS).chain(FIELD_KEYS);
        for key in keys.chain(camera::CAMERA_KEYS) {
            assert!(
                remedy.contains(&format!("`{key}`")),
                "the remedy doesn't name `{key}`"
            );
        }
    }

    #[test]
    fn invalid_toml_is_reported_with_line_and_column() {
        let errors = parse("[streams.joints]\nrate_hz = \n").unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code(), ErrorCode::ConfigInvalidSyntax);
        let context = errors[0].context_entries();
        assert!(context.contains(&("line", "2".to_owned())), "{context:?}");
    }
}
