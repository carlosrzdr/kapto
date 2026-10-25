// SPDX-License-Identifier: Apache-2.0

//! What a capture process keeps between episodes, and how it runs one.
//!
//! The inputs (UDP sockets, TCP connections) and the live view stay open for
//! as long as the process runs. Each phase, idle or recording, gets fresh
//! rings, counters and loss tracking, so an episode's counts describe that
//! episode only. Between phases no thread reads the inputs for a few
//! milliseconds; the operating system's socket buffers hold what arrives
//! meanwhile.
//!
//! Threads: `cap-rx-udp-0` receives every UDP stream and `cap-rx-tcp-<n>`
//! the n-th TCP stream, each into its own ring; `cap-writer` writes every
//! ring to the episode, or while idle only offers each batch to the live
//! view; `cap-tap` sends the live view's samples and cameras' frames; the
//! calling thread
//! watches, samples the clocks and decides when the phase ends.
//!
//! The writer also keeps, in every phase, the newest messages of each
//! stream kept for late subscribers (`/tf_static`), and each episode starts
//! with them: sent once, perhaps long before, they hold for every episode.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use kapto_capture::DurabilityStats;
use kapto_capture::finalise::{RECORDING_FILE, Staged};
use kapto_capture::post_roll::NOT_STOPPED;
use kapto_capture::tap::frames::{FrameStats, FrameTotals};
use kapto_capture::tap::{LatestValues, TapFeed, TapStats};
use kapto_capture::writer::{CARRIED_RECORD, capture_record, carried_record};
use kapto_capture::{
    Accounting, ChannelSpec, Drainer, HandedOver, Kept, KeptHeld, LossCounters, LossDetection,
    PostRoll, PreRoll, Recording, RingStats, StreamIndex, StreamTotals, Waiting, WriterConfig,
    discard, ring,
};
use kapto_config::{MAX_DATAGRAM_BYTES, StreamDeclaration};
use kapto_core::clock::monotonic_now;
use kapto_core::{ClockMap, ClockSample, EpisodeId, ErrorCode, KaptoError, RecvTime, StreamId};
use kapto_store::records::{
    CAPTURE_RECORD, EPISODE_RECORD, EpisodeEnd, EpisodeStart, PREFLIGHT_RECORD, SUMMARY_RECORD,
};
use kapto_store::{
    BufferSummary, Episode, Event, PackageIdentity, Preflight, SessionProfile, SessionTask,
    StreamSummary,
};
#[cfg(feature = "ros2")]
use kapto_transport::ros2::Ros2Transport;
use kapto_transport::{
    Receiver, ReceiverConfig, TcpTransport, Transport, TransportStats, UdpOptions, UdpTransport,
};

use crate::record::{Plan, Source};
use crate::viewer_tap;

/// A result that may carry several problems, reported together.
pub(crate) type Outcome<T> = Result<T, Vec<KaptoError>>;

/// The open inputs and live view of a capture process.
pub(crate) struct Station {
    pub(crate) plan: Plan,
    data_dir: PathBuf,
    /// One per input, in plan order; empty while a phase runs, and for good
    /// if a receiver thread died with its input.
    transports: Vec<Box<dyn Transport>>,
    tap: Option<TapFeed>,
    tap_stats: Option<Arc<TapStats>>,
    /// The cameras' counters, if the live view runs and a stream is one.
    frame_stats: Option<Arc<FrameStats>>,
    tap_thread: Option<JoinHandle<()>>,
    /// Each stream's newest decoded values, while the live view runs.
    latest: Option<Arc<LatestValues>>,
    /// The robot package the streams were declared in, named in each
    /// episode; `None` records them `unchecked`.
    pub(crate) package: Option<PackageIdentity>,
    /// The files every episode's recording carries: the package's, or the
    /// stream declarations of an `unchecked` one.
    pub(crate) carried: Carried,
    /// The session profile and task the next episode is recorded with,
    /// kept in it.
    pub(crate) session: (Option<SessionProfile>, Option<SessionTask>),
    /// Pre-flight's latest run as the next episode keeps it, if it ran.
    pub(crate) preflight: Option<Preflight>,
    /// How each episode's recording is written (`capture.compression`).
    pub(crate) writer: WriterConfig,
    /// The rings of the phase that ended last, in plan order: how full the
    /// writer let them get sizes the next phase's.
    last_rings: Vec<Arc<RingStats>>,
    /// The newest messages of the streams kept for late subscribers, from
    /// every phase so far.
    kept: Kept,
    /// The last seconds of each stream with a pre-roll, from every phase so
    /// far.
    pre_roll: PreRoll,
    /// The rings the last idle phase handed over as it ended, for the
    /// episode starting.
    handed: Vec<HandedOver>,
    /// The last idle phase's loss detection, per input, to go on into the
    /// episode starting.
    accounting: Vec<Accounting>,
    /// When the episode being recorded was stopped, and each stream's
    /// post-roll after it.
    post_roll: PostRoll,
}

/// The files a recording carries, and the package files it doesn't.
#[derive(Clone, Debug, Default)]
pub(crate) struct Carried {
    /// Each file attached, as it was when capture loaded it.
    pub(crate) files: Vec<CarriedFile>,
    /// Whether the package's meshes and textures are among the files
    /// (`capture.attach_meshes`).
    pub(crate) meshes_attached: bool,
    /// The SHA-256 of each package file left out, by its path.
    pub(crate) left_out: BTreeMap<String, String>,
}

/// One file a recording carries.
#[derive(Clone, Debug)]
pub(crate) struct CarriedFile {
    /// Its name in the recording: its path in the package.
    pub(crate) name: String,
    /// Its media type, e.g. `application/toml`.
    pub(crate) media_type: &'static str,
    /// What it holds.
    pub(crate) data: Vec<u8>,
}

/// How an episode being recorded goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// Keep recording.
    Record,
    /// Stop and finalise it.
    Finalise,
    /// Stop and discard it: nothing of it is kept.
    Discard,
}

/// An episode recorded: to be closed, or discarded already.
pub(crate) struct Recorded {
    pub(crate) episode: EpisodeId,
    /// What closing it needs; `None` if it was discarded.
    pub(crate) closing: Option<Closing>,
    /// A receiver's failure, which ended the episode early. The episode is
    /// still finalised, with everything received until then.
    pub(crate) receivers: Outcome<()>,
}

/// What `episode.json` says of an episode besides how its file was
/// synced: its start and end in UTC, its buffers, its streams, and when
/// Start and Stop were asked.
type Description = (
    (i64, i64),
    BufferSummary,
    Vec<StreamSummary>,
    (i64, Option<i64>),
);

/// What `episode.json` says of an episode, given how its file was synced.
type Describe = Box<dyn FnOnce(&ClockMap, DurabilityStats) -> Episode + Send>;

/// An episode recorded and not yet closed, with everything closing it
/// needs, so it can be closed on a thread of its own while capture
/// receives again.
pub(crate) struct Closing {
    pub(crate) episode: EpisodeId,
    recording: Recording,
    staged: Staged,
    clock: ClockMap,
    describe: Describe,
    /// Each stream's counts, printed once it is finalised.
    table: String,
}

impl Closing {
    /// Closes the episode: the file's index and summary, synced, then
    /// `episode.json`, the folder out of staging, the manifest and
    /// `_FINALISED` last. Its folder.
    ///
    /// # Errors
    ///
    /// The step that failed; the episode is left for recovery.
    pub(crate) fn close(self) -> Outcome<PathBuf> {
        let Self {
            recording,
            staged,
            clock,
            describe,
            table,
            ..
        } = self;
        kapto_core::failpoint!("closing.index");
        let durability = recording.finish(&clock).map_err(one)?;
        kapto_core::failpoint!("closing.episode_json");
        describe(&clock, durability)
            .write(&staged.dir())
            .map_err(one)?;
        let finalised = staged.finalise().map_err(one)?;
        println!("Finalised {}", finalised.dir().display());
        print!("{table}");
        Ok(finalised.dir().to_path_buf())
    }
}

/// What a running phase shows: read by the watching thread.
pub(crate) struct Live<'a> {
    pub(crate) counters: &'a LossCounters,
    rings: &'a [Arc<RingStats>],
    transports: &'a [Arc<TransportStats>],
    specs: &'a [ChannelSpec],
    /// Whether any stream arrives over TCP, and disconnects are worth
    /// showing.
    tcp: bool,
    /// The live view's counters, if it runs.
    tap: Option<&'a TapStats>,
    /// The cameras' counters, if the live view runs and a stream is one.
    frames: Option<&'a FrameStats>,
    /// Each stream's newest decoded values, if the live view runs.
    pub(crate) latest: Option<&'a LatestValues>,
    /// Where events go to be written into the episode's recording, while
    /// one is recorded.
    events: Option<&'a mpsc::Sender<Event>>,
    /// Which streams kept for late subscribers have a message kept.
    pub(crate) kept: KeptHeld,
    /// Where a start waiting is noted, for the pre-roll.
    waiting: Waiting,
    /// Where Stop is noted, for the post-roll.
    post_roll: PostRoll,
}

impl Station {
    /// Opens every input and starts the live view, which sends to
    /// `tap_socket`, and the cameras' frames to `frame_socket` at most
    /// `camera_max_fps` a second each, for as long as the station is open.
    /// Without a socket the station works without a live view.
    ///
    /// # Errors
    ///
    /// An input that can't be opened, e.g. a port already in use.
    pub(crate) fn open(
        plan: Plan,
        declarations: &[StreamDeclaration],
        (tap_socket, frame_socket, camera_max_fps): (&Path, &Path, u32),
        data_dir: PathBuf,
    ) -> Result<Self, KaptoError> {
        let transports = plan
            .inputs
            .iter()
            .map(|input| open_transport(&input.source))
            .collect::<Result<Vec<_>, _>>()?;
        // A frame larger than a datagram is never tapped: the tap sends
        // datagrams, so its slots stop there and a larger message is
        // skipped and counted.
        let largest = plan.inputs.iter().map(|input| {
            input
                .ring
                .max_message_len
                .min(usize::try_from(MAX_DATAGRAM_BYTES).unwrap_or(usize::MAX))
        });
        let cameras = viewer_tap::Cameras {
            streams: &plan.cameras,
            socket: frame_socket,
            max_fps: camera_max_fps,
        };
        let opened = viewer_tap::open(
            tap_socket,
            declarations,
            largest.max().unwrap_or(0),
            &cameras,
        );
        let (tap, tap_stats, frame_stats, tap_thread, latest) = match opened {
            Some(opened) => {
                let viewer = opened.tap;
                match thread::Builder::new()
                    .name("cap-tap".into())
                    .spawn(move || viewer.run())
                {
                    Ok(handle) => (
                        Some(opened.feed),
                        Some(opened.stats),
                        opened.frames,
                        Some(handle),
                        Some(opened.latest),
                    ),
                    Err(error) => {
                        eprintln!(
                            "warning: no live view: {}",
                            thread_failed("cap-tap", &error.to_string()).message()
                        );
                        (None, None, None, None, None)
                    }
                }
            }
            None => (None, None, None, None, None),
        };
        Ok(Self {
            kept: Kept::new(&plan.kept),
            pre_roll: PreRoll::new(&plan.pre_roll),
            handed: Vec::new(),
            accounting: Vec::new(),
            post_roll: PostRoll::new(&plan.post_roll),
            plan,
            data_dir,
            transports,
            tap,
            tap_stats,
            frame_stats,
            tap_thread,
            latest,
            package: None,
            carried: Carried::default(),
            session: (None, None),
            preflight: None,
            writer: WriterConfig::default(),
            last_rings: Vec::new(),
        })
    }

    /// The live view's counters, if it runs.
    pub(crate) fn tap_stats(&self) -> Option<&TapStats> {
        self.tap_stats.as_deref()
    }

    /// The cameras' counters, if the live view runs and a stream is one.
    pub(crate) fn frame_stats(&self) -> Option<&FrameStats> {
        self.frame_stats.as_deref()
    }

    /// Receives without recording, feeding only the live view, while
    /// `keep_going` says so. It is asked about every tenth of a second, with
    /// the phase's counts and how long it has run. What arrived after it
    /// said to stop is the next episode's.
    ///
    /// # Errors
    ///
    /// A receiver that failed or stopped, which also ends the phase.
    pub(crate) fn idle(
        &mut self,
        keep_going: &mut dyn FnMut(&Live<'_>, Duration) -> bool,
    ) -> Outcome<()> {
        let counters = Arc::new(LossCounters::new(self.plan.streams.len()));
        self.accounting.clear();
        let end = self.run(Job::Discard, &counters, keep_going)?;
        self.handed = end.handed;
        self.accounting = end.accounting;
        end.receivers
    }

    /// What the pre-roll keeps and the memory set aside for it, said when
    /// capture starts; `None` without a pre-rolled stream.
    pub(crate) fn pre_roll_said(&self) -> Option<String> {
        if self.pre_roll.is_empty() {
            return None;
        }
        let streams: Vec<String> = self
            .plan
            .pre_roll
            .iter()
            .filter(|held| !held.seconds.is_zero())
            .filter_map(|held| {
                let name = self.plan.streams.get(usize::from(held.stream))?.id.as_str();
                Some(format!("{name} {} s", held.seconds.as_secs_f64()))
            })
            .collect();
        #[allow(
            clippy::cast_precision_loss,
            reason = "a size shown to a tenth of a MiB"
        )]
        let mib = self.pre_roll.memory_bytes() as f64 / f64::from(1 << 20);
        Some(format!(
            "Pre-roll: each episode starts with the last seconds before it of {}, and with every \
             other stream from when it was asked; {mib:.1} MiB set aside.",
            streams.join(", ")
        ))
    }

    /// Records one episode, asked for at `asked`, into the data directory
    /// while `next` says so, then discards it or hands back what closing it
    /// needs, as `next` says.
    /// `started` is told the episode's ID and buffer sizes once it is being
    /// recorded. The pre-roll it starts with is reckoned from `asked`.
    ///
    /// # Errors
    ///
    /// The episode couldn't be created, written or discarded.
    pub(crate) fn record(
        &mut self,
        asked: RecvTime,
        started: &mut dyn FnMut(&EpisodeId, &BufferSummary),
        next: &mut dyn FnMut(&Live<'_>, Duration) -> Next,
    ) -> Outcome<Recorded> {
        let counters = Arc::new(LossCounters::new(self.plan.streams.len()));
        let episode = EpisodeId::generate();
        std::fs::create_dir_all(&self.data_dir).map_err(|error| {
            vec![
                KaptoError::new(ErrorCode::CaptureWriteFailed)
                    .detail(error.to_string())
                    .context("folder", self.data_dir.display().to_string()),
            ]
        })?;
        let staged = Staged::create(&self.data_dir, &episode).map_err(one)?;
        let mut clock = ClockMap::new(ClockSample::now());
        let begun = monotonic_now();
        let mut recording = Recording::create(
            &staged.recording_path(),
            &self.plan.streams,
            &clock,
            &self.writer,
        )
        .map_err(one)?;
        let (started_utc_ns, carried) = self
            .write_start(&mut recording, &episode, (&clock, asked, begun), &counters)
            .map_err(one)?;
        let configs = self.plan.inputs.iter().map(|input| &input.ring);
        let buffers = BufferSummary {
            // A ring always has at least the two batches in use.
            batches: configs.clone().map(|ring| ring.batches.max(2) as u64).sum(),
            peak_batches: 0,
            memory_bytes: configs.map(|ring| ring.memory_bytes() as u64).sum(),
        };
        started(&episode, &buffers);

        let mut next_second = 1;
        let mut ending = Next::Finalise;
        let end = self.run(
            Job::Write(Box::new(recording)),
            &counters,
            &mut |live, elapsed| {
                // The clock map gets a sample a second, so a wall-clock step
                // during the episode is caught.
                if elapsed >= Duration::from_secs(next_second) {
                    clock.record(ClockSample::now());
                    next_second += 1;
                }
                match next(live, elapsed) {
                    Next::Record => true,
                    stop => {
                        ending = stop;
                        false
                    }
                }
            },
        )?;
        clock.record(ClockSample::now());
        let ended = monotonic_now();
        let mut recording = end
            .writer?
            .ok_or_else(|| vec![thread_failed("cap-writer", "it returned no recording")])?;
        let arrivals = |stream| arrivals(&self.plan, &end.transports, stream);
        let buffers = buffer_summary(&self.plan, &end.rings);
        let streams = stream_summaries(&self.plan, &counters, &arrivals, &carried);
        // How it went, before the file is closed: a recording without this
        // record wasn't finished.
        let commands = self.commands(&clock, asked);
        let summary = EpisodeEnd::new(clock.to_utc_ns(ended), &clock, &buffers, &streams)
            .with_commands(commands);
        recording
            .write_record(SUMMARY_RECORD, summary.record())
            .map_err(one)?;
        if ending == Next::Discard {
            recording.finish(&clock).map_err(one)?;
            staged.discard().map_err(one)?;
            println!("Aborted episode {episode}; nothing of it was kept.");
            return Ok(Recorded {
                episode,
                closing: None,
                receivers: end.receivers,
            });
        }
        let table = summary_table(&self.plan, &counters, &arrivals);
        let description = (
            (started_utc_ns, clock.to_utc_ns(ended)),
            buffers,
            streams,
            commands,
        );
        let closing = self.closing(episode, (recording, staged, clock), description, table);
        Ok(Recorded {
            episode,
            closing: Some(closing),
            receivers: end.receivers,
        })
    }

    /// What closing `episode`, its file written and still open, needs:
    /// `episode.json` described from the phase's end and what this station
    /// records with.
    fn closing(
        &self,
        episode: EpisodeId,
        (recording, staged, clock): (Recording, Staged, ClockMap),
        (times, buffers, streams, commands): Description,
        table: String,
    ) -> Closing {
        let (package, session, preflight) = (
            self.package.clone(),
            self.session.clone(),
            self.preflight.clone(),
        );
        let sync_interval = self.writer.sync_interval;
        let describe: Describe = Box::new(move |clock, durability| {
            Episode::new(
                &episode,
                times,
                RECORDING_FILE,
                clock,
                (buffers, durability.summary(sync_interval)),
                streams,
            )
            .with_commands(commands)
            .with_package(package)
            .with_session(session.0, session.1)
            .with_preflight(preflight)
        });
        Closing {
            episode,
            recording,
            staged,
            clock,
            describe,
            table,
        }
    }

    /// When Start was asked (`asked`) and Stop, if it was, in UTC.
    fn commands(&self, clock: &ClockMap, asked: RecvTime) -> (i64, Option<i64>) {
        let stopped = self.post_roll.stopped();
        let stop = (stopped != NOT_STOPPED).then(|| clock.to_utc_ns(RecvTime::from_nanos(stopped)));
        (clock.to_utc_ns(asked), stop)
    }

    /// Writes what `recording` is, before its first message, so the file
    /// alone says so even if it is cut off: the episode, its package,
    /// profile and task, the capture settings, and the files it carries.
    fn write_start_records(
        &self,
        recording: &mut Recording,
        episode: &EpisodeId,
        started_utc_ns: i64,
    ) -> Result<(), KaptoError> {
        let start = EpisodeStart::new(
            episode,
            started_utc_ns,
            self.package.clone(),
            self.session.clone(),
        );
        let buffers: Vec<_> = self
            .plan
            .inputs
            .iter()
            .map(|input| (input.thread.as_str(), &input.ring))
            .collect();
        recording.write_record(EPISODE_RECORD, start.record())?;
        if let Some(preflight) = &self.preflight {
            recording.write_record(PREFLIGHT_RECORD, preflight.record())?;
        }
        recording.write_record(
            CAPTURE_RECORD,
            capture_record(
                &self.writer,
                &buffers,
                self.carried.meshes_attached,
                &self.carried.left_out,
            ),
        )?;
        for file in &self.carried.files {
            recording.attach(&file.name, file.media_type, started_utc_ns, &file.data)?;
        }
        Ok(())
    }

    /// Starts `recording`: the records that say what it is, the kept
    /// messages, the pre-roll, then what the idle phase handed over. The
    /// episode spans its data, so it starts with its first message, the
    /// pre-roll's oldest if it holds any; the idle phase's loss detection
    /// goes on from there. Returns when it starts and how many each kept
    /// stream started with.
    fn write_start(
        &mut self,
        recording: &mut Recording,
        episode: &EpisodeId,
        (clock, asked, begun): (&ClockMap, RecvTime, RecvTime),
        counters: &Arc<LossCounters>,
    ) -> Result<(i64, BTreeMap<StreamIndex, u64>), KaptoError> {
        let handed = std::mem::take(&mut self.handed);
        let first = self
            .pre_roll
            .earliest(asked, begun)
            .into_iter()
            .chain(handed.iter().filter_map(HandedOver::earliest))
            .fold(begun, RecvTime::min);
        let started_utc_ns = clock.to_utc_ns(first);
        self.pre_roll.waiting().set(None);
        self.post_roll.stop(None);
        recording.set_post_roll(self.post_roll.clone());
        for accounting in &mut self.accounting {
            accounting.resume(Arc::clone(counters), first);
        }
        self.write_start_records(recording, episode, started_utc_ns)?;
        let carried = self.write_kept(recording, started_utc_ns)?;
        recording.write_pre_roll(&self.pre_roll, (asked, begun, first), counters)?;
        recording.write_handed_over(
            (handed, first),
            counters,
            (&mut self.kept, &mut self.pre_roll),
        )?;
        Ok((started_utc_ns, carried))
    }

    /// Starts `recording` with the messages kept from before it, at its
    /// start, and says when each was received. Returns how many each kept
    /// stream started with.
    fn write_kept(
        &self,
        recording: &mut Recording,
        started_utc_ns: i64,
    ) -> Result<BTreeMap<StreamIndex, u64>, KaptoError> {
        let carried = recording.write_kept(&self.kept, started_utc_ns)?;
        let mut counts: BTreeMap<StreamIndex, u64> =
            self.plan.kept.iter().map(|kept| (kept.stream, 0)).collect();
        for (stream, _) in &carried {
            *counts.entry(*stream).or_default() += 1;
        }
        if carried.is_empty() {
            return Ok(counts);
        }
        let named: Vec<(&str, i64)> = carried
            .iter()
            .filter_map(|(stream, received)| {
                let spec = self
                    .plan
                    .streams
                    .iter()
                    .find(|spec| spec.stream == *stream)?;
                Some((spec.id.as_str(), *received))
            })
            .collect();
        recording.write_record(CARRIED_RECORD, carried_record(&named))?;
        Ok(counts)
    }

    /// Runs one phase: receivers into fresh rings, the writer doing `job`,
    /// until `keep_going` says to stop or a receiver stops. Every input is
    /// put back for the next phase.
    fn run(
        &mut self,
        job: Job,
        counters: &Arc<LossCounters>,
        keep_going: &mut dyn FnMut(&Live<'_>, Duration) -> bool,
    ) -> Outcome<PhaseEnd> {
        if self.transports.len() != self.plan.inputs.len() {
            return Err(vec![thread_failed(
                "cap-rx",
                "an input was lost when its receiver stopped unexpectedly",
            )]);
        }
        self.grow_rings();
        let accounting = self.accounting(matches!(job, Job::Write(_)), counters);
        let mut receivers = Vec::new();
        let mut drainers = Vec::new();
        let mut rings = Vec::new();
        let mut transport_stats = Vec::new();
        let inputs = self.plan.inputs.iter().zip(accounting);
        for ((input, accounting), transport) in inputs.zip(self.transports.drain(..)) {
            // Each phase counts from zero; no receiver runs right now.
            transport.stats().reset();
            transport_stats.push(Arc::clone(transport.stats()));
            let (filler, drainer, ring_stats) = ring(&input.ring);
            rings.push(Arc::clone(&ring_stats));
            drainers.push(drainer);
            receivers.push((
                input.thread.clone(),
                Receiver::new(
                    transport,
                    (filler, ring_stats),
                    accounting,
                    Arc::clone(counters),
                    ReceiverConfig::default(),
                ),
            ));
        }
        // Events reach the writer between its batches, while it records.
        let (send_events, events) = mpsc::channel();
        let recording = matches!(job, Job::Write(_));
        let live = Live {
            events: recording.then_some(&send_events),
            counters,
            rings: &rings,
            transports: &transport_stats,
            specs: &self.plan.streams,
            tcp: self
                .plan
                .inputs
                .iter()
                .any(|input| matches!(input.source, Source::Tcp(_))),
            tap: self.tap_stats.as_deref(),
            frames: self.frame_stats.as_deref(),
            latest: self.latest.as_deref(),
            kept: self.kept.held(),
            waiting: self.pre_roll.waiting(),
            post_roll: self.post_roll.clone(),
        };
        let tap = self.tap.take();
        let (inputs, receiver_outcome, (writer_outcome, handed), tap) = run_threads(
            receivers,
            (
                job,
                drainers,
                tap,
                events,
                &mut self.kept,
                &mut self.pre_roll,
            ),
            counters,
            &live,
            keep_going,
        )?;
        let (transports, accounting): (Vec<_>, Vec<_>) = inputs.into_iter().unzip();
        self.transports = transports;
        self.tap = tap;
        if self.transports.len() != self.plan.inputs.len() {
            return Err(vec![thread_failed(
                "cap-rx",
                "a receiver stopped unexpectedly and its input was lost",
            )]);
        }
        self.last_rings.clone_from(&rings);
        Ok(PhaseEnd {
            receivers: receiver_outcome,
            writer: writer_outcome,
            rings,
            transports: transport_stats,
            handed,
            accounting: accounting.into_iter().flatten().collect(),
        })
    }

    /// Each input's loss detection for a phase counting into `counters`. An
    /// episode (`recording`) goes on with the idle phase's before it, if
    /// each input's is there, so a gap across its start is found, and
    /// leaves out what comes after each stream's post-roll; an idle phase
    /// starts afresh.
    fn accounting(&mut self, recording: bool, counters: &Arc<LossCounters>) -> Vec<Accounting> {
        let carried = std::mem::take(&mut self.accounting);
        if recording && carried.len() == self.plan.inputs.len() {
            return carried
                .into_iter()
                .map(|mut accounting| {
                    accounting.set_post_roll(self.post_roll.clone());
                    accounting
                })
                .collect();
        }
        self.plan
            .inputs
            .iter()
            .map(|input| {
                let streams = input.source.streams();
                let detections: Vec<(StreamIndex, LossDetection)> = self
                    .plan
                    .detections
                    .iter()
                    .filter(|(stream, _)| streams.contains(stream))
                    .copied()
                    .collect();
                let mut accounting = Accounting::new(Arc::clone(counters), &detections);
                if recording {
                    accounting.set_post_roll(self.post_roll.clone());
                }
                accounting
            })
            .collect()
    }

    /// Gives each input whose ring the writer let fill up in the last phase
    /// a larger one for this phase, and says so: a writer held up that long
    /// once is likely to be again, by the same disk or the same load.
    fn grow_rings(&mut self) {
        for (input, last) in self.plan.inputs.iter_mut().zip(&self.last_rings) {
            let batches = input.ring.batches;
            let next = grown(
                batches,
                last.max_queued(),
                last.dropped_batches(),
                input.max_batches,
            );
            if next > batches {
                input.ring.batches = next;
                eprintln!(
                    "{}: the writer fell behind, filling {} of {batches} buffer batches{}; \
                     from now on it has {next} ({} MiB). Check what else writes to the disk \
                     holding the data directory.",
                    input.thread,
                    last.max_queued(),
                    match last.dropped_batches() {
                        0 => String::new(),
                        dropped => format!(" and dropping {dropped}"),
                    },
                    input.ring.memory_bytes().div_ceil(1024 * 1024),
                );
            }
        }
        self.last_rings.clear();
    }
}

/// The batches a ring gets after a phase in which it held `batches`, the
/// writer let `peak` of them wait at once and `dropped` were discarded:
/// twice as many if it came within a quarter of full or dropped any, at
/// most `max`; otherwise as many as before.
fn grown(batches: usize, peak: usize, dropped: u64, max: usize) -> usize {
    let nearly_full = peak.saturating_mul(4) >= batches.saturating_mul(3);
    if nearly_full || dropped > 0 {
        batches.saturating_mul(2).min(max).max(batches)
    } else {
        batches
    }
}

impl Drop for Station {
    fn drop(&mut self) {
        // The tap's reader finishes once its feed is gone.
        drop(self.tap.take());
        if let Some(thread) = self.tap_thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the writer does during a phase.
enum Job {
    /// Write every batch into this episode.
    Write(Box<Recording>),
    /// Offer every batch to the live view, then discard it.
    Discard,
}

struct PhaseEnd {
    receivers: Outcome<()>,
    /// The recording, for a [`Job::Write`] phase.
    writer: Outcome<Option<Recording>>,
    rings: Vec<Arc<RingStats>>,
    transports: Vec<Arc<TransportStats>>,
    /// The rings a [`Job::Discard`] phase handed over as it ended.
    handed: Vec<HandedOver>,
    /// A [`Job::Discard`] phase's loss detection, per input.
    accounting: Vec<Accounting>,
}

type Threads = (
    Vec<(Box<dyn Transport>, Option<Accounting>)>,
    Outcome<()>,
    (Outcome<Option<Recording>>, Vec<HandedOver>),
    Option<TapFeed>,
);

/// Starts the receiver and writer threads, asks `keep_going` every tenth of
/// a second until it says stop or a receiver stops, then stops them all.
/// Returns the inputs, each side's outcome and the live view's feed.
fn run_threads(
    receivers: Vec<(String, Receiver<Box<dyn Transport>>)>,
    (job, mut drainers, mut tap, events, kept, pre_roll): (
        Job,
        Vec<Drainer>,
        Option<TapFeed>,
        mpsc::Receiver<Event>,
        &mut Kept,
        &mut PreRoll,
    ),
    counters: &Arc<LossCounters>,
    live: &Live<'_>,
    keep_going: &mut dyn FnMut(&Live<'_>, Duration) -> bool,
) -> Outcome<Threads> {
    let stop = AtomicBool::new(false);
    let stop = &stop;
    // An idle phase ends because an episode starts, or for good: its loss
    // detection goes on into the episode, unsettled.
    let hand_over = matches!(job, Job::Discard);
    thread::scope(|scope| {
        let mut receiving = Vec::new();
        for (name, mut receiver) in receivers {
            let spawned =
                thread::Builder::new()
                    .name(name.clone())
                    .spawn_scoped(scope, move || {
                        let result = receiver.run(stop);
                        if hand_over {
                            let (transport, accounting) = receiver.hand_over();
                            (result, (transport, Some(accounting)))
                        } else {
                            (result, (receiver.finish(), None))
                        }
                    });
            match spawned {
                Ok(handle) => receiving.push((name, handle)),
                Err(error) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(vec![thread_failed(&name, &error.to_string())]);
                }
            }
        }
        let writer_counters = Arc::clone(counters);
        let writing = thread::Builder::new()
            .name("cap-writer".to_owned())
            .spawn_scoped(scope, move || match job {
                Job::Write(mut recording) => {
                    if let Some(feed) = tap.take() {
                        recording.set_tap(feed);
                    }
                    let result = recording.drain_with_events(
                        &mut drainers,
                        &writer_counters,
                        &events,
                        kept,
                        pre_roll,
                    );
                    let tap = recording.take_tap();
                    ((result.map(|()| Some(*recording)), Vec::new()), tap)
                }
                Job::Discard => {
                    let handed = discard(
                        drainers,
                        tap.as_mut(),
                        (kept, pre_roll),
                        stop,
                        WriterConfig::default().idle_wait,
                    );
                    ((Ok(None), handed), tap)
                }
            });
        let writing = match writing {
            Ok(writing) => writing,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                return Err(vec![thread_failed("cap-writer", &error.to_string())]);
            }
        };

        let start = monotonic_now();
        while !receiving.iter().any(|(_, handle)| handle.is_finished())
            && keep_going(live, monotonic_now().saturating_since(start))
        {
            thread::sleep(Duration::from_millis(100));
        }
        stop.store(true, Ordering::Relaxed);

        let mut errors = Vec::new();
        let mut transports = Vec::new();
        for (name, handle) in receiving {
            match handle.join() {
                Ok((result, transport)) => {
                    transports.push(transport);
                    if let Err(error) = result {
                        errors.push(error);
                    }
                }
                Err(_) => errors.push(thread_failed(&name, "it stopped unexpectedly")),
            }
        }
        let receivers = if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        };
        let (writer, tap) = match writing.join() {
            Ok(((result, handed), tap)) => ((result.map_err(|error| vec![error]), handed), tap),
            Err(_) => (
                (
                    Err(vec![thread_failed("cap-writer", "it stopped unexpectedly")]),
                    Vec::new(),
                ),
                None,
            ),
        };
        Ok((transports, receivers, writer, tap))
    })
}

impl Live<'_> {
    /// Notes that a start asked at `asked` waits for recording to begin, or
    /// with `None` that none does: the streams without a pre-roll of their
    /// own are held from then, so the episode starts with them too.
    pub(crate) fn start_waiting(&self, asked: Option<RecvTime>) {
        self.waiting.set(asked);
    }

    /// Notes that Stop was asked now: each stream's messages after its
    /// post-roll are left out of the episode. Returns how long the episode
    /// goes on, the longest post-roll.
    pub(crate) fn stop(&self) -> Duration {
        self.post_roll.stop(Some(monotonic_now()));
        self.post_roll.longest()
    }

    /// Hands `event` to the writer to be written into the episode's
    /// recording, between batches. Returns whether it will be: only while an
    /// episode is recorded.
    pub(crate) fn record_event(&self, event: &Event) -> bool {
        self.events
            .is_some_and(|events| events.send(event.clone()).is_ok())
    }
    /// Totals over every stream. Drops are read from where they happen, since
    /// they are only added to the counters when the phase ends.
    pub(crate) fn line(&self, seconds: u64) -> String {
        let (mut received, mut written, mut dropped, mut lost) = (0, 0, 0, 0);
        let (mut disconnects, mut rejected) = (0, 0);
        for spec in self.specs {
            let stream = spec.stream;
            let totals = self.counters.totals(stream);
            let truncated: u64 = self.transports.iter().map(|t| t.truncated(stream)).sum();
            let ring_dropped: u64 = self.rings.iter().map(|r| r.dropped_messages(stream)).sum();
            disconnects += self
                .transports
                .iter()
                .map(|t| t.disconnects(stream))
                .sum::<u64>();
            rejected += self
                .transports
                .iter()
                .map(|t| t.rejected(stream))
                .sum::<u64>();
            received += totals.received + truncated;
            written += totals.written;
            dropped += totals.dropped_by_us + ring_dropped + truncated;
            lost += totals.lost_upstream;
        }
        let peak: usize = self.rings.iter().map(|ring| ring.max_queued()).sum();
        let batches: usize = self.rings.iter().map(|ring| ring.batches()).sum();
        let mut line = format!(
            "{seconds:>5} s  received {received}  written {written}  dropped {dropped}  lost upstream {lost}  buffer peak {peak}/{batches}"
        );
        if self.tcp {
            let _ = write!(line, "  disconnects {disconnects}");
        }
        // Datagrams from elsewhere aren't the robot's: said once any came.
        if rejected > 0 {
            let _ = write!(line, "  rejected {rejected}");
        }
        // Shown once a viewer has taken a sample, so a recording without one
        // keeps a short line.
        if let Some(tap) = self.tap.filter(|tap| tap.sent() > 0) {
            let dropped = tap.viewer_slow() + tap.skipped();
            let _ = write!(line, "  viewer sent {} dropped {dropped}", tap.sent());
        }
        // Likewise once a viewer has taken a frame.
        let frames = self.frames.map(FrameStats::totals).unwrap_or_default();
        let sent: u64 = frames.iter().map(|camera| camera.sent).sum();
        if sent > 0 {
            let dropped: u64 = frames
                .iter()
                .map(|camera| camera.superseded + camera.skipped)
                .sum();
            let _ = write!(line, "  frames sent {sent} dropped {dropped}");
        }
        line
    }

    /// What became of camera `stream`'s frames on their way to the live
    /// view, if it is a camera and the live view runs.
    pub(crate) fn live_view(&self, stream: &StreamId) -> Option<FrameTotals> {
        let spec = self.specs.iter().find(|spec| &spec.id == stream)?;
        self.frames?
            .totals()
            .into_iter()
            .find(|camera| camera.stream == spec.stream)
    }

    /// Each stream's counts so far, with drops read from where they happen.
    pub(crate) fn streams(&self) -> impl Iterator<Item = (&StreamId, StreamTotals)> + '_ {
        self.specs.iter().map(|spec| {
            let stream = spec.stream;
            let mut totals = self.counters.totals(stream);
            let truncated: u64 = self.transports.iter().map(|t| t.truncated(stream)).sum();
            let ring_dropped: u64 = self.rings.iter().map(|r| r.dropped_messages(stream)).sum();
            totals.received += truncated;
            totals.dropped_by_us += ring_dropped + truncated;
            (&spec.id, totals)
        })
    }
}

fn open_transport(source: &Source) -> Result<Box<dyn Transport>, KaptoError> {
    match source {
        Source::Udp(sockets) => {
            let transport = UdpTransport::open(sockets, UdpOptions::default())?;
            for warning in transport.warnings() {
                // Recording goes ahead.
                eprintln!("{}\n", warning.warning_report());
            }
            Ok(Box::new(transport))
        }
        Source::Tcp(config) => Ok(Box::new(TcpTransport::open(config)?)),
        #[cfg(feature = "ros2")]
        Source::Ros2 { node, topics } => Ok(Box::new(Ros2Transport::open(node, topics)?)),
        #[cfg(not(feature = "ros2"))]
        Source::Ros2 { topics, .. } => Err(KaptoError::new(ErrorCode::TransportRos2NotBuilt)
            .detail(format!(
                "{} of the streams are ROS 2 topics, which this Kapto can't subscribe to",
                topics.len()
            ))),
    }
}

/// Each stream's counts, as written to `episode.json`.
fn stream_summaries(
    plan: &Plan,
    counters: &LossCounters,
    arrivals: &dyn Fn(StreamIndex) -> Arrivals,
    carried: &BTreeMap<StreamIndex, u64>,
) -> Vec<StreamSummary> {
    plan.streams
        .iter()
        .zip(&plan.detections)
        .map(|(spec, (_, detection))| {
            let mut summary = counters.totals(spec.stream).summary(&spec.id, detection);
            let arrivals = arrivals(spec.stream);
            summary.disconnects = arrivals.disconnects;
            summary.rejected = arrivals.rejected;
            summary.carried = carried.get(&spec.stream).copied();
            let totals = counters.totals(spec.stream);
            let pre_rolled = plan
                .pre_roll
                .iter()
                .any(|held| held.stream == spec.stream && !held.seconds.is_zero());
            summary.pre_roll = pre_rolled.then_some(totals.pre_roll);
            let post_rolled = plan
                .post_roll
                .get(usize::from(spec.stream))
                .is_some_and(|post_roll| !post_roll.is_zero());
            summary.post_roll = post_rolled.then_some(totals.post_roll);
            summary
        })
        .collect()
}

/// Every ring together, as one entry in `episode.json`. The peak adds each
/// ring's peak, so it is never lower than the real one.
fn buffer_summary(plan: &Plan, rings: &[Arc<RingStats>]) -> BufferSummary {
    let mut total = BufferSummary {
        batches: 0,
        peak_batches: 0,
        memory_bytes: 0,
    };
    for (input, ring) in plan.inputs.iter().zip(rings) {
        let summary = ring.summary(&input.ring);
        total.batches += summary.batches;
        total.peak_batches += summary.peak_batches;
        total.memory_bytes += summary.memory_bytes;
    }
    total
}

/// What a stream's transport saw besides its messages, where it can.
#[derive(Clone, Copy, Debug, Default)]
struct Arrivals {
    /// How often its connection was lost, if it arrives over TCP.
    disconnects: Option<u64>,
    /// Datagrams from sources not allowed to send to it, if it arrives
    /// over UDP from allowed sources only.
    rejected: Option<u64>,
}

/// What `stream`'s transport saw besides its messages.
fn arrivals(plan: &Plan, transports: &[Arc<TransportStats>], stream: StreamIndex) -> Arrivals {
    plan.inputs
        .iter()
        .zip(transports)
        .find_map(|(input, transport)| match &input.source {
            Source::Tcp(tcp) if tcp.stream == stream => Some(Arrivals {
                disconnects: Some(transport.disconnects(stream)),
                rejected: None,
            }),
            Source::Udp(sockets) => sockets
                .iter()
                .find(|socket| socket.stream == stream && !socket.allowed_sources.is_empty())
                .map(|_| Arrivals {
                    disconnects: None,
                    rejected: Some(transport.rejected(stream)),
                }),
            _ => None,
        })
        .unwrap_or_default()
}

/// Each stream's counts, as printed once its episode is finalised.
fn summary_table(
    plan: &Plan,
    counters: &LossCounters,
    arrivals: &dyn Fn(StreamIndex) -> Arrivals,
) -> String {
    let mut table = format!(
        "{:<24} {:>10} {:>10} {:>8} {:>8}  LOSS DETECTION\n",
        "STREAM", "RECEIVED", "WRITTEN", "DROPPED", "LOST"
    );
    for (spec, (_, detection)) in plan.streams.iter().zip(&plan.detections) {
        let (index, id) = (spec.stream, &spec.id);
        let totals = counters.totals(index);
        let detection = match detection {
            LossDetection::Sequence(_) => "sequence",
            LossDetection::RateGap(_) => "rate gap (estimate)",
            LossDetection::Undetectable => "none",
        };
        let arrivals = arrivals(index);
        let disconnects = match arrivals.disconnects {
            Some(1) => ", 1 disconnect".to_owned(),
            Some(count) => format!(", {count} disconnects"),
            None => String::new(),
        };
        let rejected = match arrivals.rejected {
            Some(0) | None => String::new(),
            Some(count) => format!(", {count} from sources not allowed"),
        };
        let _ = writeln!(
            table,
            "{:<24} {:>10} {:>10} {:>8} {:>8}  {detection}{disconnects}{rejected}",
            id.as_str(),
            totals.received,
            totals.written,
            totals.dropped_by_us,
            totals.lost_upstream
        );
    }
    table
}

pub(crate) fn thread_failed(thread: &str, detail: &str) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureThreadFailed).detail(format!("{thread}: {detail}"))
}

pub(crate) fn one(error: KaptoError) -> Vec<KaptoError> {
    vec![error]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_the_writer_let_fill_up_doubles_for_the_next_phase_up_to_its_limit() {
        // Three quarters full: close enough to losing messages.
        assert_eq!(grown(400, 300, 0, 6_400), 800);
        assert_eq!(grown(400, 50, 3, 6_400), 800, "anything dropped");
        assert_eq!(grown(400, 299, 0, 6_400), 400, "kept up");
        assert_eq!(grown(4_000, 4_000, 9, 6_400), 6_400, "at most the limit");
        assert_eq!(grown(6_400, 6_400, 9, 6_400), 6_400);
    }
}
