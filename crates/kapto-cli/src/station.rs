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
//! view; `cap-tap` sends the live view's samples; the calling thread
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

use kapto_capture::finalise::{RECORDING_FILE, Staged};
use kapto_capture::tap::{LatestValues, TapFeed, TapStats};
use kapto_capture::writer::{CARRIED_RECORD, capture_record, carried_record};
use kapto_capture::{
    Accounting, ChannelSpec, Drainer, Kept, KeptHeld, LossCounters, LossDetection, Recording,
    RingStats, StreamIndex, StreamTotals, WriterConfig, discard, ring,
};
use kapto_config::{MAX_DATAGRAM_BYTES, StreamDeclaration};
use kapto_core::clock::monotonic_now;
use kapto_core::{ClockMap, ClockSample, EpisodeId, ErrorCode, KaptoError, StreamId};
use kapto_store::records::{
    CAPTURE_RECORD, EPISODE_RECORD, EpisodeEnd, EpisodeStart, PREFLIGHT_RECORD, SUMMARY_RECORD,
};
use kapto_store::{
    BufferSummary, Episode, Event, PackageIdentity, Preflight, SessionProfile, SessionTask,
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

/// An episode that was finalised or discarded.
pub(crate) struct Finished {
    pub(crate) episode: EpisodeId,
    /// Where it was finalised; `None` if it was discarded.
    pub(crate) folder: Option<PathBuf>,
    /// A receiver's failure, which ended the episode early. The episode is
    /// still finalised, with everything received until then.
    pub(crate) receivers: Outcome<()>,
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
    /// Each stream's newest decoded values, if the live view runs.
    pub(crate) latest: Option<&'a LatestValues>,
    /// Where events go to be written into the episode's recording, while
    /// one is recorded.
    events: Option<&'a mpsc::Sender<Event>>,
    /// Which streams kept for late subscribers have a message kept.
    pub(crate) kept: KeptHeld,
}

impl Station {
    /// Opens every input and starts the live view, which sends to
    /// `tap_socket` for as long as the station is open. Without a socket the
    /// station works without a live view.
    ///
    /// # Errors
    ///
    /// An input that can't be opened, e.g. a port already in use.
    pub(crate) fn open(
        plan: Plan,
        declarations: &[StreamDeclaration],
        tap_socket: &Path,
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
        let (tap, tap_stats, tap_thread, latest) =
            match viewer_tap::open(tap_socket, declarations, largest.max().unwrap_or(0)) {
                Some(opened) => {
                    let viewer = opened.tap;
                    match thread::Builder::new()
                        .name("cap-tap".into())
                        .spawn(move || viewer.run())
                    {
                        Ok(handle) => (
                            Some(opened.feed),
                            Some(opened.stats),
                            Some(handle),
                            Some(opened.latest),
                        ),
                        Err(error) => {
                            eprintln!(
                                "warning: no live view: {}",
                                thread_failed("cap-tap", &error.to_string()).message()
                            );
                            (None, None, None, None)
                        }
                    }
                }
                None => (None, None, None, None),
            };
        Ok(Self {
            kept: Kept::new(&plan.kept),
            plan,
            data_dir,
            transports,
            tap,
            tap_stats,
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

    /// Receives without recording, feeding only the live view, while
    /// `keep_going` says so. It is asked about every tenth of a second, with
    /// the phase's counts and how long it has run.
    ///
    /// # Errors
    ///
    /// A receiver that failed or stopped, which also ends the phase.
    pub(crate) fn idle(
        &mut self,
        keep_going: &mut dyn FnMut(&Live<'_>, Duration) -> bool,
    ) -> Outcome<()> {
        let counters = Arc::new(LossCounters::new(self.plan.streams.len()));
        let end = self.run(Job::Discard, &counters, keep_going)?;
        end.receivers
    }

    /// Records one episode into the data directory while `next` says so,
    /// then finalises or discards it as `next` says. `started` is told the
    /// episode's ID and buffer sizes once it is being recorded.
    ///
    /// # Errors
    ///
    /// The episode couldn't be created, written, finalised or discarded.
    pub(crate) fn record(
        &mut self,
        started: &mut dyn FnMut(&EpisodeId, &BufferSummary),
        next: &mut dyn FnMut(&Live<'_>, Duration) -> Next,
    ) -> Outcome<Finished> {
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
        self.write_start_records(&mut recording, &episode, clock.to_utc_ns(begun))
            .map_err(one)?;
        let carried = self
            .write_kept(&mut recording, clock.to_utc_ns(begun))
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
        let disconnects = |stream| tcp_disconnects(&self.plan, &end.transports, stream);
        let buffers = buffer_summary(&self.plan, &end.rings);
        let streams = stream_summaries(&self.plan, &counters, &disconnects, &carried);
        // How it went, before the file is closed: a recording without this
        // record wasn't finished.
        let summary = EpisodeEnd::new(clock.to_utc_ns(ended), &clock, &buffers, &streams);
        recording
            .write_record(SUMMARY_RECORD, summary.record())
            .map_err(one)?;
        let durability = recording.finish(&clock).map_err(one)?;
        if ending == Next::Discard {
            staged.discard().map_err(one)?;
            println!("Aborted episode {episode}; nothing of it was kept.");
            return Ok(Finished {
                episode,
                folder: None,
                receivers: end.receivers,
            });
        }

        Episode::new(
            &episode,
            (clock.to_utc_ns(begun), clock.to_utc_ns(ended)),
            RECORDING_FILE,
            &clock,
            (buffers, durability.summary(self.writer.sync_interval)),
            streams,
        )
        .with_package(self.package.clone())
        .with_session(self.session.0.clone(), self.session.1.clone())
        .with_preflight(self.preflight.clone())
        .write(&staged.dir())
        .map_err(one)?;
        let finalised = staged.finalise().map_err(one)?;
        print_summary(
            &finalised.dir().display().to_string(),
            &self.plan,
            &counters,
            &disconnects,
        );
        Ok(Finished {
            episode,
            folder: Some(finalised.dir().to_path_buf()),
            receivers: end.receivers,
        })
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
        let mut receivers = Vec::new();
        let mut drainers = Vec::new();
        let mut rings = Vec::new();
        let mut transport_stats = Vec::new();
        for (input, transport) in self.plan.inputs.iter().zip(self.transports.drain(..)) {
            // Each phase counts from zero; no receiver runs right now.
            transport.stats().reset();
            transport_stats.push(Arc::clone(transport.stats()));
            let (filler, drainer, ring_stats) = ring(&input.ring);
            let streams = input.source.streams();
            let detections: Vec<(StreamIndex, LossDetection)> = self
                .plan
                .detections
                .iter()
                .filter(|(stream, _)| streams.contains(stream))
                .copied()
                .collect();
            rings.push(Arc::clone(&ring_stats));
            drainers.push(drainer);
            receivers.push((
                input.thread.clone(),
                Receiver::new(
                    transport,
                    (filler, ring_stats),
                    Accounting::new(Arc::clone(counters), &detections),
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
            latest: self.latest.as_deref(),
            kept: self.kept.held(),
        };
        let tap = self.tap.take();
        let (transports, receiver_outcome, writer_outcome, tap) = run_threads(
            receivers,
            (job, drainers, tap, events, &mut self.kept),
            counters,
            &live,
            keep_going,
        )?;
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
        })
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
}

type Threads = (
    Vec<Box<dyn Transport>>,
    Outcome<()>,
    Outcome<Option<Recording>>,
    Option<TapFeed>,
);

/// Starts the receiver and writer threads, asks `keep_going` every tenth of
/// a second until it says stop or a receiver stops, then stops them all.
/// Returns the inputs, each side's outcome and the live view's feed.
fn run_threads(
    receivers: Vec<(String, Receiver<Box<dyn Transport>>)>,
    (job, mut drainers, mut tap, events, kept): (
        Job,
        Vec<Drainer>,
        Option<TapFeed>,
        mpsc::Receiver<Event>,
        &mut Kept,
    ),
    counters: &Arc<LossCounters>,
    live: &Live<'_>,
    keep_going: &mut dyn FnMut(&Live<'_>, Duration) -> bool,
) -> Outcome<Threads> {
    let stop = AtomicBool::new(false);
    let stop = &stop;
    thread::scope(|scope| {
        let mut receiving = Vec::new();
        for (name, mut receiver) in receivers {
            let spawned =
                thread::Builder::new()
                    .name(name.clone())
                    .spawn_scoped(scope, move || {
                        let result = receiver.run(stop);
                        (result, receiver.finish())
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
                    let result =
                        recording.drain_with_events(&mut drainers, &writer_counters, &events, kept);
                    let tap = recording.take_tap();
                    (result.map(|()| Some(*recording)), tap)
                }
                Job::Discard => {
                    discard(
                        &mut drainers,
                        tap.as_mut(),
                        kept,
                        WriterConfig::default().idle_wait,
                    );
                    (Ok(None), tap)
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
            Ok((result, tap)) => (result.map_err(|error| vec![error]), tap),
            Err(_) => (
                Err(vec![thread_failed("cap-writer", "it stopped unexpectedly")]),
                None,
            ),
        };
        Ok((transports, receivers, writer, tap))
    })
}

impl Live<'_> {
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
        let mut disconnects = 0;
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
        // Shown once a viewer has taken a sample, so a recording without one
        // keeps a short line.
        if let Some(tap) = self.tap.filter(|tap| tap.sent() > 0) {
            let dropped = tap.viewer_slow() + tap.skipped();
            let _ = write!(line, "  viewer sent {} dropped {dropped}", tap.sent());
        }
        line
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
                // A warning is reported like an error, but recording goes ahead.
                eprintln!("{}\n", warning.report().replacen("error[", "warning[", 1));
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
    disconnects: &dyn Fn(StreamIndex) -> Option<u64>,
    carried: &BTreeMap<StreamIndex, u64>,
) -> Vec<kapto_store::StreamSummary> {
    plan.streams
        .iter()
        .zip(&plan.detections)
        .map(|(spec, (_, detection))| {
            let mut summary = counters.totals(spec.stream).summary(&spec.id, detection);
            summary.disconnects = disconnects(spec.stream);
            summary.carried = carried.get(&spec.stream).copied();
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

/// How often `stream`'s connection was lost, if it arrives over TCP.
fn tcp_disconnects(
    plan: &Plan,
    transports: &[Arc<TransportStats>],
    stream: StreamIndex,
) -> Option<u64> {
    plan.inputs
        .iter()
        .zip(transports)
        .find_map(|(input, transport)| match &input.source {
            Source::Tcp(tcp) if tcp.stream == stream => Some(transport.disconnects(stream)),
            _ => None,
        })
}

fn print_summary(
    folder: &str,
    plan: &Plan,
    counters: &LossCounters,
    disconnects: &dyn Fn(StreamIndex) -> Option<u64>,
) {
    println!("Finalised {folder}");
    println!(
        "{:<24} {:>10} {:>10} {:>8} {:>8}  LOSS DETECTION",
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
        let disconnects = match disconnects(index) {
            Some(1) => ", 1 disconnect".to_owned(),
            Some(count) => format!(", {count} disconnects"),
            None => String::new(),
        };
        println!(
            "{:<24} {:>10} {:>10} {:>8} {:>8}  {detection}{disconnects}",
            id.as_str(),
            totals.received,
            totals.written,
            totals.dropped_by_us,
            totals.lost_upstream
        );
    }
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
