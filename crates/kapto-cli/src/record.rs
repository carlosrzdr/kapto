// SPDX-License-Identifier: Apache-2.0

//! `kapto record`: records one episode from the UDP, TCP and ROS 2 streams
//! declared in a `streams.toml`, for a fixed time; and the plan every capture process
//! works out from those declarations: its streams, inputs and buffers.
//!
//! The threads and the episode itself are the station's; see
//! [`crate::station`]. The main thread prints a live line once a second, and
//! at the end the episode is finished, summarised in `episode.json` and
//! finalised.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use kapto_capture::loss::{ByteOrder, RateGap, SequenceField, SequenceWidth};
use kapto_capture::{
    ChannelSchema, ChannelSpec, KeptStream, LossDetection, PreRollStream, RingConfig, StreamIndex,
    recover_interrupted,
};
use kapto_config::{
    DecoderDeclaration, Durability, FramingDeclaration, MAX_DATAGRAM_BYTES, PACKAGE_FILE,
    Reliability, Ros2Declaration, SequenceDeclaration, SequenceType, StreamDeclaration,
    TcpDeclaration, TcpEndpointDeclaration, msg_files, parse_streams, read_config_file,
};
use kapto_core::clock::monotonic_now;
use kapto_core::{ClockSample, EpisodeId, ErrorCode, KaptoError, StreamId};
use kapto_decode::{CDR_ENCODING, ROS2MSG_ENCODING, TOPIC_METADATA};
use kapto_robot::{Package, is_mesh_or_texture, load_package};
use kapto_store::PackageIdentity;
use kapto_transport::ros2::{self, Qos, Ros2Subscription};
use kapto_transport::{
    Framing, LengthOrder, ReceiverConfig, TcpConfig, TcpEndpoint, UdpSocketConfig,
};

use crate::SettingFlags;
use crate::station::{Carried, CarriedFile, Next, Outcome, Station};
use crate::viewer_tap;

/// How a recording session sizes its buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecordSettings {
    /// How long the writer may stall before messages are dropped.
    pub(crate) max_writer_stall: Duration,
    /// Messages per batch.
    pub(crate) batch_capacity: usize,
    /// The slot size for streams that don't declare `max_message_bytes`: a
    /// full Ethernet frame's UDP payload.
    pub(crate) default_max_message_bytes: u32,
    /// The most bytes set aside for one stream's pre-roll
    /// (`capture.pre_roll_max`).
    pub(crate) pre_roll_max_bytes: usize,
    /// How long a start may wait for pre-flight to end: the streams without
    /// a pre-roll are held that long while one waits.
    pub(crate) start_wait: Duration,
}

impl RecordSettings {
    /// The settings `deployment` gives, the rest their defaults.
    pub(crate) fn of(deployment: &kapto_config::DeploymentConfig) -> Self {
        let mib = usize::try_from(deployment.pre_roll_max.value).unwrap_or(usize::MAX);
        Self {
            pre_roll_max_bytes: mib.saturating_mul(1 << 20),
            // Pre-flight's disk test may outlast its window by a little.
            start_wait: deployment.preflight_window.value + Duration::from_secs(1),
            ..Self::default()
        }
    }
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            max_writer_stall: Duration::from_secs(2),
            batch_capacity: 64,
            default_max_message_bytes: 1472,
            pre_roll_max_bytes: 64 << 20,
            start_wait: Duration::from_secs(6),
        }
    }
}

/// A stream's decoder as the schema stored with its channel, so the
/// recording can be decoded without its `streams.toml`.
fn channel_schema(id: &StreamId, decoder: &DecoderDeclaration) -> ChannelSchema {
    let stored = decoder.stored(id.as_str());
    ChannelSchema {
        message_encoding: stored.message_encoding.to_owned(),
        name: stored.schema_name,
        encoding: stored.schema_encoding.to_owned(),
        data: stored.schema_data,
        metadata: stored.channel_metadata,
    }
}

/// The schema stored with a stream's channel: its decoder's; for a ROS 2
/// topic without one, the topic's type and definition as rosbag2 stores them,
/// so ROS 2's tools read it, with no fields of Kapto's; none otherwise. A
/// topic's channel also names the topic, as the channel is named after the
/// stream.
fn stored_schema(declaration: &StreamDeclaration) -> Option<ChannelSchema> {
    let mut schema = match (&declaration.decoder, &declaration.ros2) {
        (Some(decoder), _) => channel_schema(&declaration.id, decoder),
        (None, Some(topic)) => ChannelSchema {
            message_encoding: CDR_ENCODING.to_owned(),
            name: topic.message_type.clone(),
            encoding: ROS2MSG_ENCODING.to_owned(),
            data: topic.definition.clone().into_bytes(),
            metadata: BTreeMap::new(),
        },
        (None, None) => return None,
    };
    if let Some(topic) = &declaration.ros2 {
        schema
            .metadata
            .insert(TOPIC_METADATA.to_owned(), topic.topic.clone());
    }
    Some(schema)
}

/// Everything a session needs, worked out from the stream declarations.
#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) streams: Vec<ChannelSpec>,
    pub(crate) detections: Vec<(StreamIndex, LossDetection)>,
    /// The streams whose messages are kept for late subscribers: their
    /// newest start every episode.
    pub(crate) kept: Vec<KeptStream>,
    /// The streams with a pre-roll: their last seconds before an episode
    /// starts it.
    pub(crate) pre_roll: Vec<PreRollStream>,
    /// Each stream's post-roll, by stream index: how long after Stop it
    /// goes on being recorded.
    pub(crate) post_roll: Vec<Duration>,
    /// One per receiver thread: every UDP stream together, each TCP stream
    /// on its own, then every ROS 2 topic together.
    pub(crate) inputs: Vec<Input>,
    /// The streams that are cameras, with their largest frame: the live
    /// view gets their newest frames.
    pub(crate) cameras: Vec<(StreamIndex, usize)>,
}

/// What one receiver thread receives, and the ring it fills.
#[derive(Debug)]
pub(crate) struct Input {
    /// The thread's name.
    pub(crate) thread: String,
    pub(crate) source: Source,
    pub(crate) ring: RingConfig,
    /// The most batches its ring may grow to when the writer keeps falling
    /// behind: [`GROWTH_LIMIT`] times those planned.
    pub(crate) max_batches: usize,
}

/// How many times its planned size an input's ring may grow to.
pub(crate) const GROWTH_LIMIT: usize = 16;

#[derive(Debug)]
pub(crate) enum Source {
    Udp(Vec<UdpSocketConfig>),
    Tcp(TcpConfig),
    /// Topics taken by one node of Kapto's, named `node`.
    Ros2 {
        #[cfg_attr(
            not(feature = "ros2"),
            allow(dead_code, reason = "only a Kapto built with ROS 2 starts a node")
        )]
        node: String,
        topics: Vec<Ros2Subscription>,
    },
}

impl Source {
    /// The streams it receives.
    pub(crate) fn streams(&self) -> Vec<StreamIndex> {
        match self {
            Self::Udp(sockets) => sockets.iter().map(|socket| socket.stream).collect(),
            Self::Tcp(tcp) => vec![tcp.stream],
            Self::Ros2 { topics, .. } => topics.iter().map(|topic| topic.stream).collect(),
        }
    }
}

/// Works out the session from the declarations in `file`.
///
/// # Errors
///
/// `config.invalid_value` for every stream `kapto record` can't receive:
/// one without a `udp`, `tcp` or `ros2` table, or without `rate_hz` to size
/// its buffers, unless its messages are kept for late subscribers, whose
/// buffers are sized for as many as the subscription keeps.
pub(crate) fn plan(
    declarations: &[StreamDeclaration],
    file: &Path,
    settings: &RecordSettings,
) -> Result<Plan, Vec<KaptoError>> {
    let invalid = |detail: String| {
        KaptoError::new(ErrorCode::ConfigInvalidValue)
            .detail(detail)
            .context("file", file.display().to_string())
    };
    if declarations.is_empty() {
        return Err(vec![invalid("no streams are declared".to_owned())]);
    }
    let mut errors = Vec::new();
    let mut plan = Plan::new(declarations, settings);
    let mut sockets = Vec::new();
    let (mut udp_rate, mut udp_slot) = (0_u64, 1_u32);
    let mut topics = Vec::new();
    let (mut ros2_rate, mut ros2_slot) = (0_u64, 1_u32);
    let mut large_topics = Vec::new();
    let ring = |rate: u64, slot: u32| {
        let latency = ReceiverConfig::default().max_batch_latency;
        RingConfig::for_rate(
            rate,
            settings.max_writer_stall,
            latency,
            batch_capacity(rate, slot, latency, settings.batch_capacity),
            usize::try_from(slot).unwrap_or(usize::MAX),
            declarations.len(),
        )
    };
    for (index, declaration) in (0..=StreamIndex::MAX).zip(declarations) {
        let id = &declaration.id;
        let has_input =
            declaration.udp.is_some() || declaration.tcp.is_some() || declaration.ros2.is_some();
        let Some(rate_hz) = sizing_rate(declaration).filter(|_| has_input) else {
            errors.push(invalid(format!(
                "stream `{id}` needs a `udp`, `tcp` or `ros2` table and `rate_hz`: `kapto record` sizes its buffers from the rates (only a ROS 2 topic kept for late subscribers, `durability = \"transient_local\"`, may leave it out)"
            )));
            continue;
        };
        let slot = declaration
            .max_message_bytes
            .unwrap_or(settings.default_max_message_bytes);
        if let Some(udp) = &declaration.udp {
            let mut socket = UdpSocketConfig::new(index, udp.bind);
            socket.multicast_group = udp.multicast_group;
            socket.allowed_sources.clone_from(&udp.allowed_sources);
            sockets.push(socket);
            udp_rate += u64::from(rate_hz);
            udp_slot = udp_slot.max(slot);
        } else if let Some(tcp) = &declaration.tcp {
            // A fixed size larger than the default slot needs a larger slot.
            let slot = match tcp.framing {
                FramingDeclaration::Fixed(size) => slot.max(u32::from(size)),
                _ => slot,
            };
            // The UDP input goes first, after the loop: so far every input
            // is a TCP one.
            plan.inputs.push(Input {
                thread: format!("cap-rx-tcp-{}", plan.inputs.len()),
                source: Source::Tcp(tcp_config(index, tcp, slot)),
                ring: ring(u64::from(rate_hz), slot),
                max_batches: ring(u64::from(rate_hz), slot).batches * GROWTH_LIMIT,
            });
        } else if let Some(topic) = &declaration.ros2 {
            if slot > MAX_DATAGRAM_BYTES {
                large_topics.push((index, id.clone(), topic, rate_hz, slot));
            } else {
                topics.push(subscription(index, topic));
                ros2_rate += u64::from(rate_hz);
                ros2_slot = ros2_slot.max(slot);
            }
        }
        plan.streams.push(ChannelSpec {
            stream: index,
            id: id.clone(),
            schema: stored_schema(declaration),
        });
        plan.detections.push((index, loss_detection(declaration)));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    if !sockets.is_empty() {
        plan.inputs.insert(
            0,
            Input {
                thread: "cap-rx-udp-0".to_owned(),
                source: Source::Udp(sockets),
                ring: ring(udp_rate, udp_slot),
                max_batches: ring(udp_rate, udp_slot).batches * GROWTH_LIMIT,
            },
        );
    }
    plan.inputs.extend(ros2_inputs(
        topics,
        (ros2_rate, ros2_slot),
        large_topics,
        &ring,
    ));
    Ok(plan)
}

/// A large ROS 2 topic: its stream, name, declaration, rate and slot size.
type LargeTopic<'a> = (StreamIndex, StreamId, &'a Ros2Declaration, u32, u32);

/// The ROS 2 inputs: the small topics together, at their total rate and
/// largest slot, then each large topic on a receiver, a node and a ring of
/// its own, so its frames never hold up the small topics.
fn ros2_inputs(
    topics: Vec<Ros2Subscription>,
    (ros2_rate, ros2_slot): (u64, u32),
    large_topics: Vec<LargeTopic<'_>>,
    ring: &dyn Fn(u64, u32) -> RingConfig,
) -> Vec<Input> {
    let mut inputs = Vec::new();
    if !topics.is_empty() {
        inputs.push(Input {
            thread: "cap-rx-ros2".to_owned(),
            source: Source::Ros2 {
                node: "kapto".to_owned(),
                topics,
            },
            ring: ring(ros2_rate, ros2_slot),
            max_batches: ring(ros2_rate, ros2_slot).batches * GROWTH_LIMIT,
        });
    }
    for (number, (index, id, topic, rate_hz, slot)) in (1..).zip(large_topics) {
        inputs.push(Input {
            thread: format!("cap-rx-ros2-{number}"),
            source: Source::Ros2 {
                node: format!("kapto_{id}"),
                topics: vec![subscription(index, topic)],
            },
            ring: ring(u64::from(rate_hz), slot),
            max_batches: ring(u64::from(rate_hz), slot).batches * GROWTH_LIMIT,
        });
    }
    inputs
}

/// Messages a batch holds: as many as arrive in the batch wait for a
/// stream of messages larger than a datagram, at least one (a camera's
/// frame each), so its ring is sized in frames; `capacity` otherwise.
fn batch_capacity(rate: u64, slot: u32, batch_wait: Duration, capacity: usize) -> usize {
    if slot <= MAX_DATAGRAM_BYTES {
        return capacity;
    }
    let arriving = (u128::from(rate) * batch_wait.as_nanos()).div_ceil(1_000_000_000);
    usize::try_from(arriving)
        .unwrap_or(capacity)
        .clamp(1, capacity.max(1))
}

/// The streams whose newest messages start every episode, each kept as
/// many deep as its subscription keeps, in slots as large as those of the
/// ring it goes through, so a message is kept whole whenever it was
/// received whole.
fn kept_streams(declarations: &[StreamDeclaration], settings: &RecordSettings) -> Vec<KeptStream> {
    let size = |declaration: &StreamDeclaration| {
        declaration
            .max_message_bytes
            .unwrap_or(settings.default_max_message_bytes)
    };
    let small = declarations
        .iter()
        .filter(|declaration| declaration.ros2.is_some())
        .map(size)
        .filter(|&bytes| bytes <= MAX_DATAGRAM_BYTES)
        .max()
        .unwrap_or(settings.default_max_message_bytes);
    (0..=StreamIndex::MAX)
        .zip(declarations)
        .filter(|(_, declaration)| declaration.kept_for_late_subscribers())
        .filter_map(|(stream, declaration)| {
            let topic = declaration.ros2.as_ref()?;
            let own = size(declaration);
            let slot = if own > MAX_DATAGRAM_BYTES { own } else { small };
            Some(KeptStream {
                stream,
                depth: usize::try_from(topic.qos.depth).unwrap_or(usize::MAX),
                max_message_len: usize::try_from(slot).unwrap_or(usize::MAX),
            })
        })
        .collect()
}

impl Plan {
    /// The plan for `declarations` before any stream is added to it: what
    /// is kept for later episodes, and the cameras.
    fn new(declarations: &[StreamDeclaration], settings: &RecordSettings) -> Self {
        Self {
            streams: Vec::new(),
            detections: Vec::new(),
            kept: kept_streams(declarations, settings),
            pre_roll: pre_roll_streams(declarations, settings),
            post_roll: declarations
                .iter()
                .map(|declaration| declaration.post_roll.unwrap_or_default())
                .collect(),
            inputs: Vec::new(),
            cameras: cameras(declarations, settings),
        }
    }
}

/// The streams with a pre-roll, each with the memory its seconds need at
/// its rate and largest message, up to `settings.pre_roll_max_bytes`, and
/// room in its index for a quarter more messages than its rate sends. Once
/// one has a pre-roll, every other stream with a rate gets one of no
/// seconds, held while a start waits, sized for `settings.start_wait`; the
/// streams without one are kept for late subscribers, and start every
/// episode anyway.
fn pre_roll_streams(
    declarations: &[StreamDeclaration],
    settings: &RecordSettings,
) -> Vec<PreRollStream> {
    if declarations
        .iter()
        .all(|declaration| declaration.pre_roll.is_none())
    {
        return Vec::new();
    }
    (0..=StreamIndex::MAX)
        .zip(declarations)
        .filter_map(|(stream, declaration)| {
            let seconds = declaration.pre_roll.unwrap_or(Duration::ZERO);
            let held = if seconds.is_zero() {
                // No longer than the longest pre-roll, whatever the window.
                settings.start_wait.min(Duration::from_secs(60))
            } else {
                seconds
            };
            let rate = f64::from(declaration.rate_hz?);
            let largest = declaration
                .max_message_bytes
                .unwrap_or(settings.default_max_message_bytes);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "at most 60 s at 100 kHz: well within usize"
            )]
            let messages = (rate * held.as_secs_f64() * 1.25).ceil() as usize + 8;
            let bytes = messages
                .saturating_mul(usize::try_from(largest).unwrap_or(usize::MAX))
                .min(settings.pre_roll_max_bytes);
            Some(PreRollStream {
                stream,
                seconds,
                bytes,
                messages,
            })
        })
        .collect()
}

/// The streams that are cameras, with their largest frame.
fn cameras(
    declarations: &[StreamDeclaration],
    settings: &RecordSettings,
) -> Vec<(StreamIndex, usize)> {
    (0..=StreamIndex::MAX)
        .zip(declarations)
        .filter(|(_, declaration)| declaration.is_camera())
        .map(|(stream, declaration)| {
            let bytes = declaration
                .max_message_bytes
                .unwrap_or(settings.default_max_message_bytes);
            (stream, usize::try_from(bytes).unwrap_or(usize::MAX))
        })
        .collect()
}

/// The rate a stream's buffers are sized for: its own, or, for a topic kept
/// for late subscribers, as many a second as the subscription keeps.
fn sizing_rate(declaration: &StreamDeclaration) -> Option<u32> {
    let kept = declaration
        .ros2
        .as_ref()
        .filter(|_| declaration.kept_for_late_subscribers())
        .map(|topic| topic.qos.depth);
    declaration.rate_hz.or(kept)
}

/// How a stream's losses are found. One without a rate says nothing by
/// falling silent.
fn loss_detection(declaration: &StreamDeclaration) -> LossDetection {
    match (declaration.sequence, declaration.rate_hz) {
        (Some(sequence), _) => LossDetection::Sequence(sequence_field(sequence)),
        (None, Some(rate_hz)) => LossDetection::RateGap(RateGap {
            settle: declaration
                .loss_settle_ms
                .map_or(RateGap::DEFAULT_SETTLE, |ms| {
                    Duration::from_millis(u64::from(ms))
                }),
            batch_latency: ReceiverConfig::default().max_batch_latency,
            ..RateGap::for_rate(rate_hz)
        }),
        (None, None) => LossDetection::Undetectable,
    }
}

fn subscription(stream: StreamIndex, topic: &Ros2Declaration) -> Ros2Subscription {
    Ros2Subscription {
        stream,
        topic: topic.topic.clone(),
        message_type: topic.message_type.clone(),
        qos: Qos {
            reliability: match topic.qos.reliability {
                Reliability::BestEffort => ros2::Reliability::BestEffort,
                Reliability::Reliable => ros2::Reliability::Reliable,
            },
            durability: match topic.qos.durability {
                Durability::Volatile => ros2::Durability::Volatile,
                Durability::TransientLocal => ros2::Durability::TransientLocal,
            },
            depth: usize::try_from(topic.qos.depth).unwrap_or(usize::MAX),
        },
    }
}

fn tcp_config(stream: StreamIndex, tcp: &TcpDeclaration, max_message_bytes: u32) -> TcpConfig {
    let endpoint = match tcp.endpoint {
        TcpEndpointDeclaration::Connect(address) => TcpEndpoint::Connect(address),
        TcpEndpointDeclaration::Listen(address) => TcpEndpoint::Listen(address),
    };
    let framing = match &tcp.framing {
        FramingDeclaration::LengthPrefix {
            header_bytes,
            length_offset,
            length_bytes,
            byte_order,
            includes_header,
        } => Framing::LengthPrefix {
            header_bytes: usize::from(*header_bytes),
            length_offset: usize::from(*length_offset),
            length_bytes: usize::from(*length_bytes),
            order: match byte_order {
                kapto_config::ByteOrder::Little => LengthOrder::Little,
                kapto_config::ByteOrder::Big => LengthOrder::Big,
            },
            includes_header: *includes_header,
        },
        FramingDeclaration::Delimiter(delimiter) => Framing::Delimiter(delimiter.clone()),
        FramingDeclaration::Fixed(size) => Framing::Fixed(usize::from(*size)),
    };
    let slot = usize::try_from(max_message_bytes).unwrap_or(usize::MAX);
    let mut config = TcpConfig::new(stream, endpoint, framing, slot);
    if let Some(ms) = tcp.reconnect_initial_ms {
        config.reconnect_initial = Duration::from_millis(u64::from(ms));
    }
    if let Some(ms) = tcp.reconnect_max_ms {
        config.reconnect_max = Duration::from_millis(u64::from(ms));
    }
    config
}

fn sequence_field(sequence: SequenceDeclaration) -> SequenceField {
    SequenceField {
        offset: usize::from(sequence.offset),
        width: match sequence.kind {
            SequenceType::U8 => SequenceWidth::U8,
            SequenceType::U16 => SequenceWidth::U16,
            SequenceType::U32 => SequenceWidth::U32,
            SequenceType::U64 => SequenceWidth::U64,
        },
        byte_order: match sequence.byte_order {
            kapto_config::ByteOrder::Little => ByteOrder::Little,
            kapto_config::ByteOrder::Big => ByteOrder::Big,
        },
    }
}

/// Runs `kapto record`.
pub(crate) fn record(
    flags: &SettingFlags,
    streams: &Path,
    mapping: Option<&Path>,
    duration: Duration,
) -> ExitCode {
    match session(flags, streams, mapping, duration) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}

/// Finishes every episode in `data_dir` that recording left unfinished, by
/// a crash, a kill or a power cut, saying what was recovered and what
/// couldn't be, and tells `recovered` each one finalised.
pub(crate) fn recover(data_dir: &Path, recovered: &mut dyn FnMut(&EpisodeId)) {
    for outcome in recover_interrupted(data_dir, ClockSample::now().utc_ns) {
        match outcome {
            Ok(done) => {
                match done.salvaged {
                    Some(salvaged) => eprintln!(
                        "Recovered episode {}, which recording left unfinished: {} messages kept, {} bytes cut off its end. Finalised {}",
                        done.episode,
                        salvaged.messages,
                        salvaged.cut_bytes,
                        done.folder.display(),
                    ),
                    None => eprintln!(
                        "Finalised episode {}, whose finalising was cut short: {}",
                        done.episode,
                        done.folder.display(),
                    ),
                }
                recovered(&done.episode);
            }
            Err(error) => eprintln!(
                "An episode recording left unfinished couldn't be recovered, and is left where it is.\n{}",
                error.report()
            ),
        }
    }
}

/// Reads and checks `streams`, and works out the plan for its streams.
pub(crate) fn load_plan(
    streams: &Path,
    settings: &RecordSettings,
) -> Outcome<(Vec<StreamDeclaration>, Plan)> {
    let file = read_config_file(streams, true)
        .map_err(|error| vec![error])?
        .ok_or_else(|| vec![KaptoError::new(ErrorCode::ConfigFileNotFound)])?;
    let declarations = parse_streams(&file)?;
    let plan = plan(&declarations, streams, settings)?;
    Ok((declarations, plan))
}

/// The robot package `streams` was declared in: the one whose
/// `package.toml`, in the same folder, names it as its streams. `None`,
/// and episodes recorded from it are `unchecked`, if there is none. One
/// that can't be loaded is reported and treated the same.
///
/// With it, the files every recording carries, read now: the package's,
/// with its meshes and textures if `attach_meshes`, otherwise naming them
/// with their SHA-256; or, for `unchecked` episodes, the stream
/// declarations and `mapping` alone. Either way, also the `.msg` files its
/// ROS 2 messages use that aren't among them, so a recording reads back
/// without the package or a ROS 2 installation.
pub(crate) fn package_of(
    streams: &Path,
    mapping: Option<&Path>,
    attach_meshes: bool,
) -> (Option<PackageIdentity>, Carried) {
    let (identity, mut carried) = package_and_files(streams, mapping, attach_meshes);
    carried
        .files
        .extend(msg_files_not_carried(streams, identity.is_some()));
    (identity, carried)
}

/// The `.msg` files the ROS 2 messages declared in `streams` use, beside
/// it as a package keeps them (`msg/<package>/msg/<Type>.msg`), that a
/// recording doesn't carry already: those read from the ROS 2 installation,
/// and also the package folder's when there is no package to carry them.
fn msg_files_not_carried(streams: &Path, in_package: bool) -> Vec<CarriedFile> {
    // Recording reads and checks the file before this: one that can't be
    // read now has nothing more to carry.
    let Ok(Some(file)) = read_config_file(streams, true) else {
        return Vec::new();
    };
    let Ok(declarations) = parse_streams(&file) else {
        return Vec::new();
    };
    msg_files(&file, &declarations)
        .into_iter()
        .filter(|(_, msg)| msg.installed || !in_package)
        .map(|(name, msg)| CarriedFile {
            media_type: media_type(&name),
            name,
            data: msg.text.into_bytes(),
        })
        .collect()
}

fn package_and_files(
    streams: &Path,
    mapping: Option<&Path>,
    attach_meshes: bool,
) -> (Option<PackageIdentity>, Carried) {
    let package = loaded_package(streams);
    let carried = package
        .as_ref()
        .map(|package| carried_from(package, attach_meshes));
    match (package, carried) {
        (Some(package), Some(Ok(carried))) => (
            Some(PackageIdentity {
                id: package.manifest.id,
                version: package.manifest.version,
                hash: package.hash,
            }),
            carried,
        ),
        (Some(package), Some(Err(error))) => {
            eprintln!(
                "warning: the robot package in {} can't be read, so episodes are recorded unchecked:\n\n{}\n",
                package.folder.display(),
                error.report()
            );
            (None, declarations_alone(streams, mapping))
        }
        _ => (None, declarations_alone(streams, mapping)),
    }
}

/// The files of `package` a recording carries, and those it leaves out:
/// its meshes and textures, unless `attach_meshes`.
fn carried_from(package: &Package, attach_meshes: bool) -> Result<Carried, KaptoError> {
    let left_out = |path: &str| !attach_meshes && is_mesh_or_texture(path);
    let attached = package.contents(|file| !left_out(&file.path))?;
    Ok(Carried {
        files: attached
            .into_iter()
            .map(|(file, data)| CarriedFile {
                media_type: media_type(&file.path),
                name: file.path.clone(),
                data,
            })
            .collect(),
        meshes_attached: attach_meshes,
        left_out: package
            .files
            .iter()
            .filter(|file| left_out(&file.path))
            .map(|file| (file.path.clone(), file.sha256.clone()))
            .collect(),
    })
}

/// What an `unchecked` recording carries: the stream declarations it was
/// recorded from, under their file name, and the mapping if given, as
/// `mapping.toml` whatever its file is called, where the checks look for it.
fn declarations_alone(streams: &Path, mapping: Option<&Path>) -> Carried {
    let streams_name = streams
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("streams.toml");
    let files = std::iter::once((streams, streams_name))
        .chain(mapping.map(|mapping| (mapping, "mapping.toml")))
        .filter_map(|(path, name)| match std::fs::read(path) {
            Ok(data) => Some(CarriedFile {
                name: name.to_owned(),
                media_type: media_type(name),
                data,
            }),
            Err(error) => {
                eprintln!(
                    "warning: {} can't be read again, so recordings won't carry it: {error}",
                    path.display()
                );
                None
            }
        })
        .collect();
    Carried {
        files,
        meshes_attached: false,
        left_out: BTreeMap::new(),
    }
}

/// The media type of a file, by its extension.
fn media_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("toml") => "application/toml",
        Some("urdf" | "xml" | "xacro" | "sdf") => "application/xml",
        Some("md") => "text/markdown",
        Some("txt" | "proto" | "msg") => "text/plain",
        Some("json") => "application/json",
        Some("yaml" | "yml") => "application/yaml",
        Some("stl") => "model/stl",
        Some("obj") => "model/obj",
        Some("dae") => "model/vnd.collada+xml",
        Some("glb") => "model/gltf-binary",
        Some("gltf") => "model/gltf+json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    }
}

/// The package `streams` was declared in, loaded, if there is one.
pub(crate) fn loaded_package(streams: &Path) -> Option<Package> {
    let folder = streams
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !folder.join(PACKAGE_FILE).is_file() {
        return None;
    }
    match load_package(folder) {
        Ok(package) => {
            let same = |a: &Path, b: &Path| matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
            same(&package.streams(), streams).then_some(package)
        }
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!(
                "warning: the robot package in {} can't be loaded, so episodes are recorded unchecked:\n\n{}\n",
                folder.display(),
                reports.join("\n\n")
            );
            None
        }
    }
}

/// What a recording is checked against, for the line printed when it
/// starts.
pub(crate) fn describe(package: Option<&PackageIdentity>) -> String {
    package.map_or_else(
        || "no robot package declares these streams: episodes are recorded unchecked".to_owned(),
        |package| {
            format!(
                "robot package {} {} (hash {})",
                package.id,
                package.version,
                package.hash.get(..12).unwrap_or(&package.hash)
            )
        },
    )
}

fn session(
    flags: &SettingFlags,
    streams: &Path,
    mapping: Option<&Path>,
    duration: Duration,
) -> Outcome<()> {
    let (_, deployment) = crate::config::load(flags)?;
    let (declarations, plan) = load_plan(streams, &RecordSettings::of(&deployment))?;
    recover(&deployment.data_dir.value, &mut |_| {});
    let writer = crate::capture::writer_config(&deployment);
    let mut station = Station::open(
        plan,
        &declarations,
        crate::capture::live_view(&deployment),
        deployment.data_dir.value.clone(),
    )
    .map_err(|error| vec![error])?;
    (station.package, station.carried) =
        package_of(streams, mapping, deployment.attach_meshes.value);
    station.writer = writer;
    eprintln!("Recording with {}.", describe(station.package.as_ref()));
    let streams = station.plan.streams.len();
    let mut next_second = 1;
    let finished = station.record(
        monotonic_now(),
        &mut |episode, buffers| {
            eprintln!(
                "Recording episode {episode} from {streams} streams for {} s (buffers: {} batches, {} MiB). Started.",
                duration.as_secs(),
                buffers.batches,
                buffers.memory_bytes / (1024 * 1024),
            );
        },
        &mut |live, elapsed| {
            if elapsed >= duration {
                return Next::Finalise;
            }
            if elapsed >= Duration::from_secs(next_second) {
                eprintln!("{}", live.line(next_second));
                next_second += 1;
            }
            Next::Record
        },
    )?;
    if let Some(stats) = station.tap_stats() {
        println!("{}", viewer_tap::summary(stats));
    }
    if let Some(stats) = station.frame_stats() {
        for line in viewer_tap::frames_summary(stats, &declarations) {
            println!("{line}");
        }
    }
    finished.receivers
}

#[cfg(test)]
mod tests {
    use kapto_config::{ConfigFile, parse_streams_with};

    use std::path::PathBuf;

    use super::*;

    fn declarations(text: &str) -> Vec<StreamDeclaration> {
        parse_streams(&ConfigFile {
            path: PathBuf::from("streams.toml"),
            contents: text.to_owned(),
        })
        .unwrap()
    }

    #[test]
    fn a_camera_topic_gets_a_receiver_a_node_and_a_ring_of_its_own_in_frames() {
        let file = ConfigFile {
            path: PathBuf::from("streams.toml"),
            contents:             "[streams.joints]\nrate_hz = 2000\n\
             ros2 = { topic = \"/joint_states\", type = \"sensor_msgs/msg/JointState\" }\n\
             [streams.tf_static]\n\
             ros2 = { topic = \"/tf_static\", type = \"tf2_msgs/msg/TFMessage\", \
             qos = { reliability = \"reliable\", durability = \"transient_local\", depth = 4 } }\n\
             [streams.camera]\nrate_hz = 30\nmax_message_bytes = 1048576\n\
             ros2 = { topic = \"/camera/image_raw\", type = \"sensor_msgs/msg/Image\" }\n\
             [streams.wrist]\nrate_hz = 30\nmax_message_bytes = 300000\n\
             tcp = { listen = \"127.0.0.1:9100\", framing = { kind = \"length_prefix\", length_bytes = 4 } }\n"
                .to_owned(),
        };
        // Each type a one-field message: its definition doesn't matter here.
        let streams =
            parse_streams_with(&file, &mut |_, _| Some("float64 data\n".to_owned())).unwrap();
        let plan = plan(
            &streams,
            Path::new("streams.toml"),
            &RecordSettings::default(),
        )
        .unwrap();

        let inputs: Vec<(&str, usize, usize, usize)> = plan
            .inputs
            .iter()
            .map(|input| {
                (
                    input.thread.as_str(),
                    input.ring.batch_capacity,
                    input.ring.max_message_len,
                    input.ring.batches,
                )
            })
            .collect();
        // The camera: a frame a batch, 60 for a 2 s stall at 30 fps and the
        // 2 in use; the small topics keep their batches and slots.
        assert_eq!(
            inputs,
            [
                ("cap-rx-tcp-0", 1, 300_000, 62),
                ("cap-rx-ros2", 64, 1472, 1002),
                ("cap-rx-ros2-1", 1, 1 << 20, 62),
            ]
        );
        let Source::Ros2 { node, topics } = &plan.inputs[2].source else {
            panic!("not ROS 2")
        };
        assert_eq!(node, "kapto_camera");
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].topic, "/camera/image_raw");
        // The fixed transforms are kept in the small topics' slots.
        assert_eq!(plan.kept[0].max_message_len, 1472);
    }

    proptest::proptest! {
        #[test]
        fn a_batch_holds_what_arrives_in_its_wait_at_least_one_and_never_more_than_a_small_one(
            rate in 1_u64..100_000,
            slot in 1_u32..=kapto_config::MAX_MESSAGE_BYTES,
            capacity in 1_usize..256,
        ) {
            let wait = Duration::from_millis(2);
            let held = batch_capacity(rate, slot, wait, capacity);
            proptest::prop_assert!((1..=capacity).contains(&held));
            if slot <= MAX_DATAGRAM_BYTES {
                proptest::prop_assert_eq!(held, capacity);
            } else if rate * 2 <= 1_000 {
                proptest::prop_assert_eq!(held, 1);
            } else {
                let arriving = usize::try_from((rate * 2).div_ceil(1_000)).unwrap();
                proptest::prop_assert_eq!(held, arriving.min(capacity));
            }
        }
    }

    #[test]
    fn every_ros_2_topic_is_received_by_one_input_after_the_others_and_stored_as_rosbag2_does() {
        let file = ConfigFile {
            path: PathBuf::from("streams.toml"),
            contents: "[streams.arm]\nrate_hz = 2000\nudp = { bind = \"127.0.0.1:9000\" }\n\
                       [streams.force]\nrate_hz = 500\n\
                       ros2 = { topic = \"/force\", type = \"std_msgs/msg/Float64\", qos = { reliability = \"reliable\", depth = 50 } }\n\
                       [streams.grip]\nrate_hz = 100\n\
                       ros2 = { topic = \"/grip\", type = \"std_msgs/msg/Float64\" }\n\
                       decoder = { kind = \"cdr\", fields = [{ name = \"width\", type = \"f64\", path = \"data\" }] }\n"
                .to_owned(),
        };
        let mut float64 = |package: &str, name: &str| {
            (package == "std_msgs" && name == "Float64").then(|| "float64 data\n".to_owned())
        };
        let streams = parse_streams_with(&file, &mut float64).unwrap();
        let plan = plan(&streams, &file.path, &RecordSettings::default()).unwrap();

        let threads: Vec<&str> = plan
            .inputs
            .iter()
            .map(|input| input.thread.as_str())
            .collect();
        assert_eq!(threads, ["cap-rx-udp-0", "cap-rx-ros2"]);
        let Source::Ros2 { topics, .. } = &plan.inputs[1].source else {
            panic!("not ROS 2")
        };
        assert_eq!(
            topics[0],
            Ros2Subscription {
                stream: 1,
                topic: "/force".to_owned(),
                message_type: "std_msgs/msg/Float64".to_owned(),
                qos: Qos {
                    reliability: ros2::Reliability::Reliable,
                    durability: ros2::Durability::Volatile,
                    depth: 50,
                },
            }
        );
        assert_eq!(plan.inputs[1].source.streams(), [1, 2]);
        // Without a decoder, as rosbag2 stores the topic; with one, its
        // fields too.
        let force = plan.streams[1].schema.as_ref().unwrap();
        assert_eq!(
            (
                force.message_encoding.as_str(),
                force.encoding.as_str(),
                force.name.as_str()
            ),
            ("cdr", "ros2msg", "std_msgs/msg/Float64")
        );
        assert_eq!(force.data, b"float64 data\n");
        // The topic it was received on, the channel being named "force".
        assert_eq!(
            force.metadata,
            BTreeMap::from([("ros2.topic".to_owned(), "/force".to_owned())])
        );
        let grip = plan.streams[2].schema.as_ref().unwrap();
        let keys: Vec<&str> = grip.metadata.keys().map(String::as_str).collect();
        assert_eq!(keys, ["kapto.fields", "ros2.topic"]);
    }

    #[test]
    fn the_plan_sizes_buffers_from_the_rates_and_picks_each_streams_loss_detection() {
        let streams = declarations(
            "[streams.arm]\nrate_hz = 2000\nudp = { bind = \"127.0.0.1:9000\" }\n\
             sequence = { offset = 0, type = \"u32\" }\n\
             [streams.gripper]\nrate_hz = 1000\nmax_message_bytes = 2048\n\
             udp = { bind = \"127.0.0.1:9001\", allowed_sources = [\"10.0.0.5\"] }\n\
             [streams.wrist]\nrate_hz = 500\nloss_settle_ms = 250\n\
             udp = { bind = \"127.0.0.1:9002\" }\n",
        );
        let plan = plan(
            &streams,
            Path::new("streams.toml"),
            &RecordSettings::default(),
        )
        .unwrap();
        // 3000 messages/s, faster than a batch fills in 2 ms: at most 500
        // batches a second, a batch each 2 ms, for 2 s, plus 2 in use.
        let udp = &plan.inputs[0];
        assert_eq!(udp.thread, "cap-rx-udp-0");
        assert_eq!(udp.ring.batches, 1_002);
        assert_eq!(udp.max_batches, 16 * 1_002);
        assert_eq!(udp.ring.max_message_len, 2048);
        let Source::Udp(sockets) = &udp.source else {
            panic!("not udp")
        };
        assert_eq!(
            sockets[1].allowed_sources,
            ["10.0.0.5".parse::<std::net::IpAddr>().unwrap()]
        );
        assert!(matches!(
            plan.detections[0],
            (0, LossDetection::Sequence(_))
        ));
        let settles = |index: usize| match plan.detections[index] {
            (_, LossDetection::RateGap(gap)) => gap.settle,
            _ => panic!("not estimated from its rate"),
        };
        assert_eq!(settles(1), RateGap::DEFAULT_SETTLE);
        assert_eq!(settles(2), Duration::from_millis(250));
        assert_eq!(plan.streams[1].id.as_str(), "gripper");
        assert!(plan.streams.iter().all(|spec| spec.schema.is_none()));
    }

    #[test]
    fn once_a_stream_has_a_pre_roll_every_other_with_a_rate_is_held_while_a_start_waits() {
        let streams = declarations(
            "[streams.arm]\nrate_hz = 1000\npre_roll_s = 2\nmax_message_bytes = 100\n\
             udp = { bind = \"127.0.0.1:9000\" }\n\
             [streams.wrist]\nrate_hz = 100\nmax_message_bytes = 1000\n\
             udp = { bind = \"127.0.0.1:9001\" }\n",
        );
        let settings = RecordSettings::default();
        let held = plan(&streams, Path::new("streams.toml"), &settings).unwrap();
        // 2 s at 1 kHz and 6 s at 100 Hz, a quarter more and 8.
        assert_eq!(
            held.pre_roll,
            [
                PreRollStream {
                    stream: 0,
                    seconds: Duration::from_secs(2),
                    bytes: 2_508 * 100,
                    messages: 2_508,
                },
                PreRollStream {
                    stream: 1,
                    seconds: Duration::ZERO,
                    bytes: 758 * 1_000,
                    messages: 758,
                },
            ]
        );
        let without =
            declarations("[streams.arm]\nrate_hz = 1000\nudp = { bind = \"127.0.0.1:9000\" }\n");
        let none = plan(&without, Path::new("streams.toml"), &settings).unwrap();
        assert_eq!(none.pre_roll, []);
    }

    #[test]
    fn a_stream_with_a_struct_layout_records_it_as_its_channels_schema() {
        let streams = declarations(
            "[streams.arm]\nrate_hz = 2000\nudp = { bind = \"127.0.0.1:9000\" }\n\
             [streams.arm.decoder]\nkind = \"struct\"\n\
             [[streams.arm.decoder.fields]]\nname = \"sequence\"\ntype = \"u32\"\noffset = 0\n",
        );
        let plan = plan(
            &streams,
            Path::new("streams.toml"),
            &RecordSettings::default(),
        )
        .unwrap();
        let schema = plan.streams[0].schema.as_ref().unwrap();
        assert_eq!(schema.message_encoding, "kapto.struct/v1");
        assert_eq!(schema.encoding, "kapto.struct/v1");
        assert_eq!(schema.name, "arm");
        let layout = kapto_decode::StructLayout::from_schema_json(
            std::str::from_utf8(&schema.data).unwrap(),
        )
        .unwrap();
        assert_eq!(layout.fields[0].name, "sequence");
    }

    #[test]
    fn streams_without_an_input_or_a_rate_are_reported_together() {
        let streams = declarations(
            "[streams.a]\nrate_hz = 10\n[streams.b]\nudp = { bind = \"127.0.0.1:9000\" }\n",
        );
        let errors = plan(
            &streams,
            Path::new("streams.toml"),
            &RecordSettings::default(),
        )
        .unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .all(|error| error.code() == ErrorCode::ConfigInvalidValue)
        );
        assert!(
            errors[1]
                .message()
                .contains("stream `b` needs a `udp`, `tcp` or `ros2` table and `rate_hz`")
        );
    }

    #[test]
    fn a_topic_kept_for_late_subscribers_needs_no_rate_and_infers_no_loss_from_silence() {
        let topic = |name: &str, durability: &str| {
            format!(
                "[streams.{name}]\n\
                 ros2 = {{ topic = \"/{name}\", type = \"std_msgs/msg/Float64\", \
                 qos = {{ durability = \"{durability}\", depth = 4 }} }}\n"
            )
        };
        let declarations = |text: &str| {
            let file = ConfigFile {
                path: PathBuf::from("streams.toml"),
                contents: text.to_owned(),
            };
            parse_streams_with(&file, &mut |_: &str, _: &str| {
                Some("float64 data\n".to_owned())
            })
            .unwrap()
        };
        let kept = declarations(&topic("tf_static", "transient_local"));
        let plan = plan(&kept, Path::new("streams.toml"), &RecordSettings::default()).unwrap();
        assert!(matches!(
            plan.detections[0],
            (0, LossDetection::Undetectable)
        ));
        assert_eq!(
            plan.kept,
            [KeptStream {
                stream: 0,
                depth: 4,
                max_message_len: usize::try_from(
                    RecordSettings::default().default_max_message_bytes
                )
                .unwrap(),
            }]
        );
        // Sized as for a stream of its depth's rate.
        let sized = declarations(
            "[streams.tf]\nrate_hz = 4\n\
             ros2 = { topic = \"/tf\", type = \"std_msgs/msg/Float64\" }\n",
        );
        let sized = super::plan(
            &sized,
            Path::new("streams.toml"),
            &RecordSettings::default(),
        );
        assert_eq!(
            plan.inputs[0].ring.batches,
            sized.unwrap().inputs[0].ring.batches
        );

        let volatile = declarations(&topic("tf", "volatile"));
        let errors = super::plan(
            &volatile,
            Path::new("streams.toml"),
            &RecordSettings::default(),
        )
        .unwrap_err();
        assert!(
            errors[0].message().contains("transient_local"),
            "{}",
            errors[0]
        );
    }

    #[test]
    fn each_tcp_stream_gets_its_own_thread_and_ring_after_the_udp_streams() {
        let streams = declarations(
            "[streams.odometry]\nrate_hz = 100\n\
             tcp = { connect = \"10.0.0.1:7000\", reconnect_initial_ms = 20, reconnect_max_ms = 900, \
             framing = { kind = \"length_prefix\", length_bytes = 2, byte_order = \"big\" } }\n\
             [streams.arm]\nrate_hz = 2000\nudp = { bind = \"127.0.0.1:9000\" }\n\
             [streams.imu]\nrate_hz = 1000\n\
             tcp = { listen = \"0.0.0.0:7001\", framing = { kind = \"fixed\", size = 4000 } }\n",
        );
        let plan = plan(
            &streams,
            Path::new("streams.toml"),
            &RecordSettings::default(),
        )
        .unwrap();
        let threads: Vec<_> = plan
            .inputs
            .iter()
            .map(|input| input.thread.as_str())
            .collect();
        assert_eq!(threads, ["cap-rx-udp-0", "cap-rx-tcp-0", "cap-rx-tcp-1"]);
        let Source::Tcp(odometry) = &plan.inputs[1].source else {
            panic!("not tcp")
        };
        assert_eq!(odometry.stream, 0);
        assert_eq!(
            odometry.endpoint,
            TcpEndpoint::Connect("10.0.0.1:7000".parse().unwrap())
        );
        assert_eq!(
            odometry.framing,
            Framing::LengthPrefix {
                header_bytes: 2,
                length_offset: 0,
                length_bytes: 2,
                order: LengthOrder::Big,
                includes_header: false,
            }
        );
        assert_eq!(odometry.reconnect_initial, Duration::from_millis(20));
        assert_eq!(odometry.reconnect_max, Duration::from_millis(900));
        // Each ring is sized for its own stream's rate: 100 messages a
        // second, each a batch of its own, for 2 s.
        assert_eq!(plan.inputs[1].ring.batches, 202);
        assert!(plan.inputs[1].ring.batches < plan.inputs[0].ring.batches);
        // A fixed size larger than the default slot widens that stream's slot.
        let Source::Tcp(imu) = &plan.inputs[2].source else {
            panic!("not tcp")
        };
        assert_eq!((imu.stream, imu.max_message_bytes), (2, 4000));
        assert_eq!(plan.inputs[2].ring.max_message_len, 4000);
    }

    #[test]
    fn a_file_without_streams_is_refused() {
        let errors = plan(&[], Path::new("streams.toml"), &RecordSettings::default()).unwrap_err();
        assert!(errors[0].message().contains("no streams are declared"));
    }
}
