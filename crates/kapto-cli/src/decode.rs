// SPDX-License-Identifier: Apache-2.0

//! `kapto decode`: a recording's messages as named values.
//!
//! Each stream recorded with a layout is decoded with the layout stored in
//! the recording. Streams without one are shown as raw bytes.
//!
//! With `--pcap` and `--package`, the messages come from a packet capture
//! instead, decoded with the package's `streams.toml`: each UDP datagram
//! goes to the stream it was sent to, so an integrator can check a package
//! against a robot's traffic before recording anything.

use std::collections::BTreeMap;
use std::io::{self, BufWriter, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use kapto_capture::finalise::RECORDING_FILE;
use kapto_capture::reader::{
    ChannelSummary, RecordedChannel, RecordedMessage, RecordedSchema, read_recording,
    recorded_channels,
};
use kapto_config::{
    MAX_MESSAGE_BYTES, StreamDeclaration, UdpDeclaration, parse_streams, read_config_file,
};
use kapto_core::{ErrorCode, KaptoError, say, utc_iso8601};
use kapto_decode::{DecodeError, Decoder, DecoderDeclaration, Value};
use kapto_robot::load_package;

use crate::SettingFlags;
use crate::pcap::{Datagram, read_capture};

/// How `kapto decode` prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Format {
    /// The first messages of each stream, in aligned columns, for people.
    Table,
    /// Every message of one stream, comma-separated, for other tools.
    Csv,
}

/// Arguments of `kapto decode`.
#[derive(Args)]
pub(crate) struct DecodeArgs {
    /// An episode folder, or its recording.mcap.
    #[arg(required_unless_present = "pcap", conflicts_with = "pcap")]
    path: Option<PathBuf>,
    /// A packet capture (pcap or pcapng, e.g. from `tcpdump -w`) to decode
    /// instead, with the streams of --package.
    #[arg(long, value_name = "FILE", requires = "package")]
    pcap: Option<PathBuf>,
    /// The robot package whose streams.toml says which UDP port each stream
    /// arrives on and how to decode it: a folder, or a package ID.
    #[arg(long, value_name = "PACKAGE", requires = "pcap")]
    package: Option<String>,
    /// Only this stream; repeat for several. Every stream if left out.
    #[arg(long = "stream", value_name = "NAME")]
    streams: Vec<String>,
    /// table: the first messages of each stream; csv: every message of one
    /// stream.
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
    /// Messages per stream. The table shows 10 unless told otherwise; CSV
    /// writes every message.
    #[arg(long, value_name = "MESSAGES")]
    limit: Option<u64>,
}

/// Messages per stream in a table when `--limit` isn't given.
const TABLE_LIMIT: u64 = 10;
/// Raw bytes shown per message in a table.
const RAW_PREVIEW_BYTES: usize = 16;

pub(crate) fn decode(flags: &SettingFlags, args: &DecodeArgs) -> ExitCode {
    let result = Input::of(flags, args).and_then(|input| match args.format {
        Format::Table => table(&input, args),
        Format::Csv => csv(&input, args),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            say!("{}", error.report());
            ExitCode::FAILURE
        }
    }
}

/// Where the messages come from.
enum Input {
    /// A recording.
    Recording(PathBuf),
    /// A packet capture, with the streams to sort its datagrams into.
    Capture {
        bytes: Vec<u8>,
        streams: Vec<StreamDeclaration>,
    },
}

impl Input {
    fn of(flags: &SettingFlags, args: &DecodeArgs) -> Result<Self, KaptoError> {
        let (Some(capture), Some(package)) = (&args.pcap, &args.package) else {
            let path = args.path.as_deref().unwrap_or(Path::new("."));
            return Ok(Self::Recording(recording_path(path)));
        };
        let (folder, _) = crate::package::find(flags, package).map_err(first)?;
        let package = load_package(&folder).map_err(first)?;
        let file = read_config_file(&package.streams(), true)?
            .ok_or_else(|| KaptoError::new(ErrorCode::PackageMissingFile))?;
        let streams = parse_streams(&file).map_err(first)?;
        for stream in streams.iter().filter(|stream| stream.udp.is_none()) {
            say!(
                "{} doesn't arrive over UDP, so it isn't looked for in the capture.",
                stream.id
            );
        }
        let bytes = std::fs::read(capture).map_err(|error| {
            KaptoError::new(ErrorCode::DecodeCaptureInvalid)
                .detail(error.to_string())
                .context("file", capture.display().to_string())
        })?;
        Ok(Self::Capture {
            bytes,
            streams: streams
                .into_iter()
                .filter(|stream| stream.udp.is_some())
                .collect(),
        })
    }

    /// Every stream, in order: the recording's channels, or the package's
    /// UDP streams with their decoders stored as a recording would.
    fn channels(&self) -> Result<Vec<ChannelSummary>, KaptoError> {
        match self {
            Self::Recording(path) => recorded_channels(path),
            Self::Capture { streams, .. } => Ok(streams
                .iter()
                .map(|stream| ChannelSummary {
                    channel: channel(stream),
                    messages: None,
                })
                .collect()),
        }
    }

    /// Calls `each` with every message in order until it breaks; whether
    /// the input stopped part-way.
    fn read(
        &self,
        mut each: impl FnMut(&RecordedChannel, RecordedMessage<'_>) -> ControlFlow<()>,
    ) -> Result<bool, KaptoError> {
        match self {
            Self::Recording(path) => Ok(read_recording(path, each)?.cut_off),
            Self::Capture { bytes, streams } => {
                let channels: Vec<RecordedChannel> = streams.iter().map(channel).collect();
                let mut counts = vec![0_u32; streams.len()];
                let mut elsewhere = 0_u64;
                let passed = read_capture(bytes, |datagram| {
                    let stream = streams.iter().position(|stream| {
                        stream
                            .udp
                            .as_ref()
                            .is_some_and(|udp| sent_to(udp, &datagram))
                    });
                    let Some(stream) = stream else {
                        elsewhere += 1;
                        return ControlFlow::Continue(());
                    };
                    let message = RecordedMessage {
                        sequence: counts[stream],
                        log_time: datagram.time_ns,
                        publish_time: datagram.time_ns,
                        data: datagram.payload,
                    };
                    counts[stream] += 1;
                    each(&channels[stream], message)
                })?;
                if elsewhere > 0 {
                    say!(
                        "{elsewhere} UDP datagram(s) in the capture went to no stream of the package."
                    );
                }
                if passed.fragments > 0 {
                    say!(
                        "{} packet(s) were fragments of datagrams too large for one packet, which can't be read on their own.",
                        passed.fragments
                    );
                }
                if passed.cut_short > 0 {
                    say!(
                        "{} packet(s) were cut short when captured; capture with a larger snapshot length (tcpdump -s 0).",
                        passed.cut_short
                    );
                }
                Ok(false)
            }
        }
    }
}

fn first(errors: Vec<KaptoError>) -> KaptoError {
    errors
        .into_iter()
        .next()
        .unwrap_or_else(|| KaptoError::new(ErrorCode::PackageNotFound))
}

/// A package's stream as a recording would have stored it.
fn channel(stream: &StreamDeclaration) -> RecordedChannel {
    let stored = stream
        .decoder
        .as_ref()
        .map(|decoder| decoder.stored(stream.id.as_str()));
    RecordedChannel {
        topic: stream.id.to_string(),
        message_encoding: stored
            .as_ref()
            .map_or("kapto.raw", |stored| stored.message_encoding)
            .to_owned(),
        metadata: stored
            .as_ref()
            .map(|stored| stored.channel_metadata.clone())
            .unwrap_or_default(),
        schema: stored.map(|stored| RecordedSchema {
            name: stored.schema_name,
            encoding: stored.schema_encoding.to_owned(),
            data: stored.schema_data,
        }),
    }
}

/// Whether a datagram is one the stream would have received: to its port,
/// at its address or multicast group, from an allowed source. Bound to
/// `0.0.0.0` a socket gets IPv4 only; bound to `[::]`, both.
fn sent_to(udp: &UdpDeclaration, datagram: &Datagram<'_>) -> bool {
    let destination = datagram.destination;
    let bound = udp.bind.ip();
    let address = match udp.multicast_group {
        Some(group) => destination.ip() == group,
        None if bound.is_unspecified() => bound.is_ipv6() || destination.is_ipv4(),
        None => destination.ip() == bound,
    };
    destination.port() == udp.bind.port()
        && address
        && (udp.allowed_sources.is_empty() || udp.allowed_sources.contains(&datagram.source.ip()))
}

/// An episode folder means its recording file.
pub(crate) fn recording_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join(RECORDING_FILE)
    } else {
        path.to_owned()
    }
}

/// How one stream's messages are shown.
enum View {
    /// Decoded with its stored layout, into these columns.
    Decoded {
        decoder: Box<dyn Decoder>,
        columns: Vec<String>,
    },
    /// Shown as raw bytes: no layout was stored, or it couldn't be used.
    Raw,
}

impl View {
    /// The view for `channel`; a stored layout that can't be used is
    /// reported on standard error and the stream shown raw.
    fn of(channel: &RecordedChannel) -> Self {
        let Some(declaration) = channel.schema.as_ref().and_then(|schema| {
            DecoderDeclaration::from_stored(
                &schema.name,
                &schema.encoding,
                &schema.data,
                &channel.metadata,
            )
        }) else {
            return Self::Raw;
        };
        let decoder = declaration.and_then(|declaration| {
            // A stored layout is bounded by the largest message any stream
            // may have: the recording's own stream sizes aren't read here.
            declaration
                .compile(MAX_MESSAGE_BYTES as usize)
                .map_err(|errors| {
                    errors
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| KaptoError::new(ErrorCode::DecoderInvalidSchema))
                })
        });
        match decoder {
            Ok(decoder) => {
                let columns = decoder
                    .fields()
                    .iter()
                    .flat_map(|field| {
                        (0..field.count).map(move |index| {
                            if field.count == 1 {
                                field.name.clone()
                            } else {
                                format!("{}[{index}]", field.name)
                            }
                        })
                    })
                    .collect();
                Self::Decoded { decoder, columns }
            }
            Err(error) => {
                let error = error.context("stream", channel.topic.clone());
                say!("{}\n(showing this stream as raw bytes)\n", error.report());
                Self::Raw
            }
        }
    }

    /// Why a message wasn't decoded, naming its field where it can.
    fn explain(&self, error: DecodeError) -> String {
        match self {
            Self::Decoded { decoder, .. } => error.describe(decoder.fields()),
            Self::Raw => error.to_string(),
        }
    }

    /// The units line of a table: each column's unit, if any.
    fn units(&self) -> String {
        match self {
            Self::Decoded { decoder, .. } => {
                let units: Vec<String> = decoder
                    .fields()
                    .iter()
                    .filter_map(|field| {
                        let unit = field.unit.as_deref()?;
                        Some(format!("{} in {unit}", field.name))
                    })
                    .collect();
                if units.is_empty() {
                    String::new()
                } else {
                    format!("; {}", units.join(", "))
                }
            }
            Self::Raw => String::new(),
        }
    }
}

/// The streams to show, in recording order.
///
/// # Errors
///
/// `recording.unknown_stream` naming every stream asked for that isn't in
/// the recording.
fn selected(
    channels: Vec<ChannelSummary>,
    wanted: &[String],
) -> Result<Vec<ChannelSummary>, KaptoError> {
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|name| {
            !channels
                .iter()
                .any(|summary| &summary.channel.topic == *name)
        })
        .collect();
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|name| name.as_str()).collect();
        return Err(KaptoError::new(ErrorCode::RecordingUnknownStream)
            .detail(format!("`{}`", names.join("`, `")))
            .context("streams in the recording", stream_list(&channels)));
    }
    if wanted.is_empty() {
        return Ok(channels);
    }
    Ok(channels
        .into_iter()
        .filter(|summary| wanted.contains(&summary.channel.topic))
        .collect())
}

fn stream_list(channels: &[ChannelSummary]) -> String {
    channels
        .iter()
        .map(|summary| summary.channel.topic.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One decoded message as text cells, or why it couldn't be decoded.
fn cells(
    view: &View,
    message: &RecordedMessage<'_>,
    values: &mut Vec<Value>,
) -> Result<Vec<String>, DecodeError> {
    match view {
        View::Decoded { decoder, .. } => {
            values.resize(decoder.value_count(), Value::default());
            decoder.decode(message.data, values)?;
            Ok(values.iter().map(ToString::to_string).collect())
        }
        View::Raw => {
            let preview: Vec<String> = message
                .data
                .iter()
                .take(RAW_PREVIEW_BYTES)
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let more = if message.data.len() > RAW_PREVIEW_BYTES {
                " …"
            } else {
                ""
            };
            Ok(vec![
                message.data.len().to_string(),
                format!("{}{more}", preview.join(" ")),
            ])
        }
    }
}

/// The first messages of each selected stream, one aligned table per stream.
fn table(input: &Input, args: &DecodeArgs) -> Result<(), KaptoError> {
    let streams = selected(input.channels()?, &args.streams)?;
    let limit = args.limit.unwrap_or(TABLE_LIMIT);
    let mut views: BTreeMap<String, (View, Vec<Vec<String>>)> = streams
        .iter()
        .map(|summary| {
            (
                summary.channel.topic.clone(),
                (View::of(&summary.channel), Vec::new()),
            )
        })
        .collect();
    let mut values = Vec::new();
    let cut_off = input.read(|channel, message| {
        if let Some((view, rows)) = views.get_mut(&channel.topic)
            && (rows.len() as u64) < limit
        {
            let mut row = vec![
                utc_iso8601(i64::try_from(message.log_time).unwrap_or(i64::MAX)),
                message.sequence.to_string(),
            ];
            match cells(view, &message, &mut values) {
                Ok(cells) => row.extend(cells),
                Err(error) => row.push(format!("not decoded: {}", view.explain(error))),
            }
            rows.push(row);
        }
        if views.values().all(|(_, rows)| rows.len() as u64 >= limit) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })?;

    let mut out = BufWriter::new(io::stdout().lock());
    for summary in &streams {
        let Some((view, rows)) = views.get(&summary.channel.topic) else {
            continue;
        };
        let mut header = vec!["RECEIVED (UTC)".to_owned(), "INDEX".to_owned()];
        match view {
            View::Decoded { columns, .. } => header.extend(columns.iter().cloned()),
            View::Raw => header.extend(["BYTES".to_owned(), "PAYLOAD (hex)".to_owned()]),
        }
        let total = summary
            .messages
            .map_or_else(String::new, |messages| format!(" of {messages}"));
        let shown = format!(
            "{} ({}){}: showing {}{total} messages",
            summary.channel.topic,
            summary.channel.message_encoding,
            view.units(),
            rows.len()
        );
        if write_table(&mut out, &shown, &header, rows).is_err() {
            // The reader went away (e.g. `| head`): stop quietly.
            return Ok(());
        }
    }
    if cut_off {
        let _ = writeln!(
            out,
            "The recording stops part-way (it was cut off, e.g. by a crash); every complete message before that point is shown."
        );
    }
    Ok(())
}

fn write_table(
    out: &mut impl Write,
    title: &str,
    header: &[String],
    rows: &[Vec<String>],
) -> io::Result<()> {
    let mut widths: Vec<usize> = header.iter().map(|cell| cell.chars().count()).collect();
    // A note in place of values ("not decoded: …") runs on past the columns
    // instead of widening one of them.
    for row in rows.iter().filter(|row| row.len() == header.len()) {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.chars().count());
            }
        }
    }
    writeln!(out, "{title}\n")?;
    for row in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let width = widths.get(index).copied().unwrap_or(0);
                format!("{cell:<width$}")
            })
            .collect();
        writeln!(out, "{}", line.join("  ").trim_end())?;
    }
    writeln!(out)
}

/// Every message of one stream as CSV: when it was received (UTC
/// nanoseconds and ISO 8601), its index in the stream, then one column per
/// value.
fn csv(input: &Input, args: &DecodeArgs) -> Result<(), KaptoError> {
    let channels = input.channels()?;
    let all = stream_list(&channels);
    let streams = selected(channels, &args.streams)?;
    let [summary] = streams.as_slice() else {
        return Err(KaptoError::new(ErrorCode::DecodeCsvNeedsOneStream)
            .detail(format!("{} streams selected", streams.len()))
            .context("streams in the recording", all));
    };
    let view = View::of(&summary.channel);
    let View::Decoded { columns, .. } = &view else {
        return Err(KaptoError::new(ErrorCode::DecodeNoLayout)
            .detail(format!("`{}`", summary.channel.topic)));
    };
    let topic = summary.channel.topic.clone();
    let limit = args.limit.unwrap_or(u64::MAX);

    let mut out = BufWriter::new(io::stdout().lock());
    if writeln!(
        out,
        "received_utc_ns,received_utc,index,{}",
        columns.join(",")
    )
    .is_err()
    {
        return Ok(());
    }
    let mut written = 0_u64;
    let mut not_decoded: Option<(u64, DecodeError)> = None;
    let mut values = Vec::new();
    let mut closed = false;
    let cut_off = input.read(|channel, message| {
        if channel.topic != topic {
            return ControlFlow::Continue(());
        }
        match cells(&view, &message, &mut values) {
            Ok(cells) => {
                let utc = utc_iso8601(i64::try_from(message.log_time).unwrap_or(i64::MAX));
                if writeln!(
                    out,
                    "{},{utc},{},{}",
                    message.log_time,
                    message.sequence,
                    cells.join(",")
                )
                .is_err()
                {
                    closed = true;
                    return ControlFlow::Break(());
                }
                written += 1;
            }
            Err(error) => {
                let count = not_decoded.map_or(0, |(count, _)| count);
                not_decoded = Some((count + 1, not_decoded.map_or(error, |(_, first)| first)));
            }
        }
        if written >= limit {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })?;
    if closed || out.flush().is_err() {
        return Ok(());
    }
    if let Some((count, first)) = not_decoded {
        say!(
            "{count} message(s) of `{topic}` weren't decoded and are left out; the first: {}.",
            view.explain(first)
        );
    }
    if cut_off {
        say!(
            "The recording stops part-way (it was cut off, e.g. by a crash); every complete message before that point is included."
        );
    }
    Ok(())
}
