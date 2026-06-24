// SPDX-License-Identifier: Apache-2.0

//! `kapto worker`: the jobs serve queues, done and reported back over
//! serve's job socket.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]
#![cfg_attr(
    target_os = "linux",
    allow(
        unsafe_code,
        reason = "a thread's I/O priority is only readable through libc's unsafe syscall function"
    )
)]

mod common;

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use kapto_capture::control::{ControlServer, Response, Status};
use kapto_capture::{Batch, ChannelSchema, ChannelSpec, LossCounters, Recording, WriterConfig};
use kapto_config::UploadWhen;
use kapto_core::{ClockMap, ClockSample, RecvTime, StreamId};
use kapto_decode::{FieldSpec, STRUCT_ENCODING, ScalarType, StructLayout};
use kapto_server::jobs::{
    Destination, EVENTS, JOB_SOCKET, JobServer, QA, REPLAY, UPLOAD, Uploads, VERIFY,
};
use kapto_server::replays::REPLAY_CACHE;
use kapto_server::retention::Retention;
use kapto_store::db::DATABASE_FILE;
use kapto_store::manifest::FINALISED_FILE;
use kapto_store::{
    Episode, Event, JobState, Manifest, Phase, StreamSummary, UploadState, Verdict, Writer,
};
use kapto_testkit::{scratch, wait_until};
use serde_json::json;

/// A finalised episode `id` in `data`: a recording and its manifest.
fn finalised(data: &Path, id: &str) {
    let folder = data.join(id);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("recording.mcap"), b"what was recorded").unwrap();
    Manifest::of_directory(&folder, &id.parse().unwrap())
        .unwrap()
        .write(&folder)
        .unwrap();
    std::fs::write(folder.join(FINALISED_FILE), b"").unwrap();
}

/// Writes the `episode.json` of episode `id` in `data`: one stream of 2000
/// messages, of which Kapto dropped `dropped`.
fn summarised(data: &Path, id: &str, dropped: u64) {
    let start = 1_772_700_400_000_000_000;
    let stream = StreamSummary {
        written: 2000 - dropped,
        dropped_by_us: dropped,
        ..kapto_testkit::stream("arm_joints", 2000)
    };
    kapto_testkit::episode(id, (start, start + 10_000_000_000), vec![stream])
        .write(&data.join(id))
        .unwrap();
}

/// The destination `site`, taking every finalised episode.
fn site() -> Destination {
    Destination {
        name: "site".to_owned(),
        when: UploadWhen::Finalised,
        retry_failed_after: Duration::from_hours(1),
    }
}

/// Each thread of process `pid`, by name, with its nice value and, on
/// Linux, its I/O priority class (3 is idle).
#[cfg(target_os = "linux")]
fn priorities(pid: u32) -> Vec<(String, i32, i64)> {
    let mut threads: Vec<_> = std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .map(|entry| {
            let task = entry.unwrap().path();
            let tid: i64 = task.file_name().unwrap().to_str().unwrap().parse().unwrap();
            let name = std::fs::read_to_string(task.join("comm")).unwrap();
            let stat = std::fs::read_to_string(task.join("stat")).unwrap();
            // After the name in brackets, from the third field on.
            let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..].split(' ').collect();
            let nice: i32 = fields[16].parse().unwrap();
            // SAFETY: ioprio_get takes integers and reads no memory of ours.
            let ioprio = unsafe { libc::syscall(libc::SYS_ioprio_get, 1, tid) };
            (name.trim().to_owned(), nice, ioprio >> 13)
        })
        .collect();
    threads.sort();
    threads
}

#[test]
fn the_worker_verifies_each_episode_at_low_priority_reports_what_differs_and_waits_for_serve() {
    let folder = scratch("worker");
    let data = folder.join("data");
    let (intact, changed, unknown) = (
        "20260310T120000.000Z-00000001",
        "20260310T120500.000Z-00000002",
        "20260310T121000.000Z-00000003",
    );
    for id in [intact, changed, unknown] {
        finalised(&data, id);
    }
    // Changed on disk after it was finalised.
    std::fs::write(
        data.join(changed).join("recording.mcap"),
        b"what was recorded?",
    )
    .unwrap();

    // Started before serve listens: it keeps trying.
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        // Ends with the test even when it fails part-way.
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));

    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(VERIFY, intact).unwrap();
    store.enqueue(VERIFY, changed).unwrap();
    store.enqueue("transcribe", unknown).unwrap();
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();

    let reader = store.reader().unwrap();
    wait_until("every job to end", || {
        reader
            .jobs(10)
            .unwrap()
            .iter()
            .all(|job| matches!(job.state, JobState::Done | JobState::Failed))
    });
    let ended: Vec<_> = reader
        .jobs(10)
        .unwrap()
        .into_iter()
        .rev()
        .map(|job| {
            (
                job.episode,
                job.state,
                job.failure.map(|failure| failure.code),
            )
        })
        .collect();
    assert_eq!(
        ended,
        [
            (intact.to_owned(), JobState::Done, None),
            (
                changed.to_owned(),
                JobState::Failed,
                Some("manifest.mismatch".to_owned())
            ),
            (
                unknown.to_owned(),
                JobState::Failed,
                Some("worker.unknown_job".to_owned())
            ),
        ]
    );
    // Every thread at low priority, so jobs never compete with capture.
    #[cfg(target_os = "linux")]
    {
        let threads = priorities(worker.id());
        let names: Vec<&str> = threads.iter().map(|(name, ..)| name.as_str()).collect();
        assert_eq!(names, ["kapto", "wrk-cpu-0", "wrk-sched", "wrk-watch"]);
        for (name, nice, class) in &threads {
            assert_eq!((*nice, *class), (10, 3), "{name}");
        }
    }

    // SIGTERM stops it at once, cleanly.
    Command::new("kill")
        .args(["-TERM", &worker.id().to_string()])
        .status()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = worker.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the worker didn't stop");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success());
}

#[test]
fn the_worker_starts_no_job_while_capture_records_an_episode_unless_told_not_to_wait() {
    let folder = scratch("worker-waits");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised(&data, id);
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(VERIFY, id).unwrap();
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();

    // A stand-in for capture, recording until told otherwise.
    let socket = folder.join("capture.sock");
    let capture = ControlServer::listen(&socket).unwrap();
    let recording = Arc::new(AtomicBool::new(true));
    let answering = Arc::new(AtomicBool::new(true));
    let stand_in = std::thread::spawn({
        let (recording, answering) = (Arc::clone(&recording), Arc::clone(&answering));
        move || {
            while answering.load(Ordering::Relaxed) {
                for pending in capture.pending() {
                    let episode = recording.load(Ordering::Relaxed).then(|| id.to_owned());
                    pending.answer(Response::Status(Status {
                        episode,
                        ..Status::default()
                    }));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    });

    let start_worker = |flags: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_kapto"))
            .arg("worker")
            // Ends with the test even when it fails part-way.
            .args(["--parent", &std::process::id().to_string()])
            .arg("--data-dir")
            .arg(&data)
            .arg("--control-socket")
            .arg(&socket)
            .args(flags)
            .env_clear()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut worker = start_worker(&[]);
    let reader = store.reader().unwrap();
    let state = || reader.jobs(1).unwrap()[0].state;
    // Long enough for the worker to have asked capture and serve a few times.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(state(), JobState::Queued);

    recording.store(false, Ordering::Relaxed);
    wait_until("the job to be done", || state() == JobState::Done);
    let _ = worker.kill();
    let _ = worker.wait();

    // Told not to wait, a worker does the next job during the recording.
    recording.store(true, Ordering::Relaxed);
    let next = "20260310T120500.000Z-00000002";
    finalised(&data, next);
    store.enqueue(VERIFY, next).unwrap();
    let mut worker = start_worker(&["--worker-pause-while-recording", "false"]);
    wait_until("the job to be done during the recording", || {
        state() == JobState::Done
    });
    let _ = worker.kill();
    let _ = worker.wait();
    answering.store(false, Ordering::Relaxed);
    stand_in.join().unwrap();
}

#[test]
fn a_job_stopped_part_way_is_given_back_without_counting_an_attempt() {
    let folder = scratch("worker-gives-back");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised(&data, id);
    // Long to check: a file of 1 GiB (over ten seconds in a test build),
    // sparse so it takes no room on the disk. Its manifest was made for the
    // small one, which doesn't matter: the check is stopped long before it
    // ends.
    let recording = std::fs::OpenOptions::new()
        .write(true)
        .open(data.join(id).join("recording.mcap"))
        .unwrap();
    recording.set_len(1 << 30).unwrap();
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(VERIFY, id).unwrap();
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();

    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        // Ends with the test even when it fails part-way.
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .args(["--worker-pause-while-recording", "false"])
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let reader = store.reader().unwrap();
    let job = || reader.jobs(1).unwrap().remove(0);
    wait_until("the job to start", || job().state == JobState::Running);
    let started = Instant::now();

    Command::new("kill")
        .args(["-TERM", &worker.id().to_string()])
        .status()
        .unwrap();
    let status = loop {
        if let Some(status) = worker.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the worker didn't stop"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    let given_back = job();
    assert_eq!(
        (given_back.state, given_back.attempts),
        (JobState::Queued, 0)
    );
}

/// Writes episode `id`'s recording in `data`: a real one, of a shoulder
/// turning at 1 rad/s for a second, carrying the mapping that names it and
/// `qa` as its `qa.toml` if given.
fn recorded(data: &Path, id: &str, qa: Option<&str>) {
    let layout = StructLayout {
        fields: vec![FieldSpec::new("shoulder", ScalarType::F64, 0)],
    };
    let arm = ChannelSpec {
        stream: 0,
        id: "arm_joints".parse::<StreamId>().unwrap(),
        schema: Some(ChannelSchema {
            message_encoding: STRUCT_ENCODING.to_owned(),
            name: "arm_joints".to_owned(),
            encoding: STRUCT_ENCODING.to_owned(),
            data: layout.to_schema_json().into_bytes(),
            metadata: BTreeMap::new(),
        }),
    };
    let clock = ClockMap::new(ClockSample::now());
    let mut recording = Recording::create(
        &data.join(id).join("recording.mcap"),
        &[arm],
        &clock,
        &WriterConfig::default(),
    )
    .unwrap();
    let mapping = "[joints]\nshoulder = \"arm_joints.shoulder\"\n";
    recording
        .attach("mapping.toml", "application/toml", 0, mapping.as_bytes())
        .unwrap();
    if let Some(qa) = qa {
        recording
            .attach("qa.toml", "application/toml", 0, qa.as_bytes())
            .unwrap();
    }
    let counters = LossCounters::new(1);
    for step in 0..100_u32 {
        let mut batch = Batch::new(1, 8);
        batch.push(0, &(0.01 * f64::from(step)).to_le_bytes());
        batch.set_recv_time(RecvTime::from_nanos(u64::from(step) * 10_000_000 + 1));
        recording.write(&batch, &counters).unwrap();
    }
    recording.finish(&clock).unwrap();
}

/// Checks that `run` has the statistics of a recording written by
/// [`recorded`].
fn has_its_statistics(run: &kapto_store::QaRun) {
    // With its statistics: from the summary, and by role from the recording.
    assert_eq!(run.statistics["episode.duration_s"], json!({"value": 10.0}));
    assert_eq!(
        run.statistics["streams.arm_joints.rate_hz"],
        json!({"value": 200.0})
    );
    let speed = &run.statistics["joints.shoulder.speed"]["distribution"];
    assert_eq!(speed["count"], 99);
    let (min, max) = (
        speed["min"].as_f64().unwrap(),
        speed["max"].as_f64().unwrap(),
    );
    assert!(
        (min - 1.0).abs() < 1e-9 && (max - 1.0).abs() < 1e-9,
        "{speed}"
    );
}

/// Checks that every check and detector ran on an episode written by
/// [`recorded`] with no `qa.toml`, each with its defaults.
fn each_check_ran(run: &kapto_store::QaRun) {
    assert_eq!(
        run.results
            .iter()
            .map(|result| (result.check.as_str(), result.verdict))
            .collect::<Vec<_>>(),
        [
            ("streams.complete", Verdict::Passed),
            ("streams.present", Verdict::Passed),
            ("streams.empty", Verdict::NotApplicable),
            ("episode.short", Verdict::Passed),
            ("episode.long", Verdict::Passed),
            // No camera.
            ("camera.format", Verdict::NotApplicable),
            ("camera.frames", Verdict::NotApplicable),
            ("motion.jump", Verdict::Passed),
            // One joint: still can't be told from frozen.
            ("motion.frozen", Verdict::NotApplicable),
            // No gripper; and moving the whole second.
            ("motion.chatter", Verdict::NotApplicable),
            ("motion.idle", Verdict::Passed),
            // Not indexed, so in a session of its own, and of a package
            // and task with no episode learned from.
            ("anomalies.statistics", Verdict::NotApplicable),
            ("anomalies.autoencoder", Verdict::NotApplicable),
        ]
    );
}

#[test]
fn the_worker_checks_each_episode_with_the_quality_settings_its_recording_carries() {
    let folder = scratch("worker-qa");
    let data = folder.join("data");
    let id = |n: u32| format!("20260310T12{n:02}00.000Z-0000000{n}");
    let (complete, dropped, lenient, off, broken, unreadable) =
        (id(1), id(2), id(3), id(4), id(5), id(6));
    for (episode, drops, qa) in [
        // No `qa.toml`: every check with its defaults.
        (&complete, 0, None),
        (&dropped, 12, None),
        (
            &lenient,
            12,
            Some("qa_params_version = \"7\"\n[streams.complete]\nmax_dropped = 20\n"),
        ),
        (
            &off,
            12,
            Some(
                "qa_params_version = \"8\"\n[streams.complete]\nenabled = false\n\
                 [streams.present]\nenabled = false\n[streams.empty]\nenabled = false\n\
                 [episode.short]\nenabled = false\n[episode.long]\nenabled = false\n\
                 [camera.format]\nenabled = false\n[camera.frames]\nenabled = false\n\
                 [motion.jump]\nenabled = false\n[motion.frozen]\nenabled = false\n\
                 [motion.chatter]\nenabled = false\n[motion.idle]\nenabled = false\n\
                 [anomalies.statistics]\nenabled = false\n\
                 [anomalies.autoencoder]\nenabled = false\n",
            ),
        ),
        (
            &broken,
            0,
            Some("qa_params_version = \"9\"\nmax_dropped = 20\n"),
        ),
    ] {
        finalised(&data, episode);
        summarised(&data, episode, drops);
        recorded(&data, episode, qa);
    }
    finalised(&data, &unreadable);
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    for episode in [&complete, &dropped, &lenient, &off, &broken, &unreadable] {
        store.enqueue(QA, episode).unwrap();
    }
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        // Ends with the test even when it fails part-way.
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let reader = store.reader().unwrap();
    wait_until("every job to end", || {
        reader
            .jobs(10)
            .unwrap()
            .iter()
            .all(|job| matches!(job.state, JobState::Done | JobState::Failed))
    });
    let run = |episode: &str| reader.qa(episode).unwrap();
    let passed = run(&complete).unwrap();
    assert!(passed.accepted());
    assert_eq!(passed.params_version, None);
    has_its_statistics(&passed);
    each_check_ran(&passed);
    let failed = run(&dropped).unwrap();
    assert!(!failed.accepted());
    assert_eq!(failed.results[0].values["dropped_by_us"], 12.0);
    // The same drops, with the threshold the recording carries.
    let allowed = run(&lenient).unwrap();
    assert!(allowed.accepted());
    assert_eq!(allowed.params_version.as_deref(), Some("7"));
    // Checks and detectors turned off aren't run at all.
    let unchecked = run(&off).unwrap();
    assert_eq!(
        (unchecked.results.len(), unchecked.params_version.as_deref()),
        (0, Some("8"))
    );

    // Settings that can't be read, or no `episode.json`: the job fails,
    // saying why, and records no results.
    let jobs = reader.jobs(10).unwrap();
    let failure = |episode: &str| {
        jobs.iter()
            .find(|job| job.episode == episode)
            .and_then(|job| job.failure.as_ref())
            .map(|failure| failure.code.as_str())
    };
    assert_eq!(failure(&broken), Some("qa.unknown_key"));
    assert_eq!(failure(&unreadable), Some("episode.unreadable"));
    assert_eq!(run(&broken), None);
    assert_eq!(run(&unreadable), None);
    worker.kill().unwrap();
    worker.wait().unwrap();
}

/// Writes the recording of episode `id` in `data`: a few messages of one
/// stream, and `events` on its channel of events.
fn recorded_with_events(data: &Path, id: &str, events: &[Event]) {
    let clock = ClockMap::new(ClockSample::now());
    let arm = ChannelSpec::from((0, "arm_joints".parse::<StreamId>().unwrap()));
    let mut recording = Recording::create(
        &data.join(id).join("recording.mcap"),
        &[arm],
        &clock,
        &WriterConfig::default(),
    )
    .unwrap();
    let counters = LossCounters::new(1);
    for (sequence, event) in events.iter().enumerate() {
        let mut batch = Batch::new(1, 4);
        batch.push(0, &u32::try_from(sequence).unwrap().to_le_bytes());
        batch.set_recv_time(RecvTime::from_nanos(u64::try_from(sequence).unwrap() + 1));
        recording.write(&batch, &counters).unwrap();
        recording.write_event(event).unwrap();
    }
    recording.finish(&clock).unwrap();
}

#[test]
fn the_worker_reads_each_episodes_events_from_its_recording_into_the_index() {
    let folder = scratch("worker-events");
    let data = folder.join("data");
    let id = |n: u32| format!("20260311T12{n:02}00.000Z-0000000{n}");
    let (marked, quiet, unreadable) = (id(1), id(2), id(3));
    let event = |kind: &str, phase, at_utc_ns, source: &str| Event {
        kind: kind.to_owned(),
        at_utc_ns,
        until_utc_ns: None,
        phase,
        id: (phase != Phase::Moment).then(|| "i1".to_owned()),
        source: source.to_owned(),
        text: None,
        attributes: BTreeMap::new(),
    };
    let events = vec![
        event("intervention", Phase::Begin, 1_000, "page"),
        event("note", Phase::Moment, 1_500, "api"),
        event("kapto.stream_lost", Phase::Moment, 1_700, "kapto"),
        event("intervention", Phase::End, 2_000, "page"),
    ];
    for (episode, held) in [(&marked, events.as_slice()), (&quiet, &[][..])] {
        finalised(&data, episode);
        summarised(&data, episode, 0);
        recorded_with_events(&data, episode, held);
    }
    // Its recording isn't one.
    finalised(&data, &unreadable);
    summarised(&data, &unreadable, 0);
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    for episode in [&marked, &quiet, &unreadable] {
        store.enqueue(EVENTS, episode).unwrap();
    }
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let reader = store.reader().unwrap();
    wait_until("every job to end", || {
        reader
            .jobs(10)
            .unwrap()
            .iter()
            .all(|job| matches!(job.state, JobState::Done | JobState::Failed))
    });
    // In the order the recording holds them, whoever sent them.
    assert_eq!(reader.events(&marked).unwrap(), Some(events));
    // None, told from not read.
    assert_eq!(reader.events(&quiet).unwrap(), Some(Vec::new()));
    let jobs = reader.jobs(10).unwrap();
    let failed = jobs.iter().find(|job| job.episode == unreadable).unwrap();
    assert_eq!(
        failed.failure.as_ref().map(|failure| failure.code.as_str()),
        Some("recording.unreadable")
    );
    assert_eq!(reader.events(&unreadable).unwrap(), None);
    worker.kill().unwrap();
    worker.wait().unwrap();
}

/// Writes the recording of episode `id` in `data` as one made without a
/// robot package: a second of the sample arm's stream, carrying the
/// arm's `streams.toml` alone.
fn arm_recorded(data: &Path, id: &str) {
    let streams = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7/streams.toml");
    let clock = ClockMap::new(ClockSample::now());
    let arm = ChannelSpec::from((0, "arm_joints".parse::<StreamId>().unwrap()));
    let mut recording = Recording::create(
        &data.join(id).join("recording.mcap"),
        &[arm],
        &clock,
        &WriterConfig::default(),
    )
    .unwrap();
    recording
        .attach(
            "streams.toml",
            "application/toml",
            0,
            &std::fs::read(streams).unwrap(),
        )
        .unwrap();
    let counters = LossCounters::new(1);
    for sequence in 0..2000_u32 {
        // Sequence number, the robot's clock, seven joints and the gripper.
        let mut payload = sequence.to_le_bytes().to_vec();
        payload.extend(u64::from(sequence).to_le_bytes());
        for _ in 0..8 {
            payload.extend((f64::from(sequence) / 2000.0).to_le_bytes());
        }
        let mut batch = Batch::new(1, payload.len());
        batch.push(0, &payload);
        batch.set_recv_time(RecvTime::from_nanos(u64::from(sequence) * 500_000 + 1));
        recording.write(&batch, &counters).unwrap();
    }
    recording.finish(&clock).unwrap();
}

#[test]
fn the_worker_makes_each_replay_asked_for_saying_what_it_lacks_and_empties_the_cache_as_its_settings_say()
 {
    let folder = scratch("worker-replay");
    let data = folder.join("data");
    let id = |n: u32| format!("20260310T12{n:02}00.000Z-0000000{n}");
    let (watched, unreadable, unopened, gone) = (id(1), id(2), id(3), id(4));
    for episode in [&watched, &unreadable, &unopened] {
        finalised(&data, episode);
        summarised(&data, episode, 0);
    }
    arm_recorded(&data, &watched);
    // Left in the cache by earlier runs: a replay unopened for eight days,
    // one of an episode since deleted, and one never finished.
    let cache = data.join(REPLAY_CACHE);
    std::fs::create_dir_all(&cache).unwrap();
    for name in [
        format!("{unopened}.view"),
        format!("{gone}.view"),
        format!("{watched}.view.partial"),
    ] {
        std::fs::write(cache.join(name), b"an earlier replay").unwrap();
    }
    File::options()
        .write(true)
        .open(cache.join(format!("{unopened}.view")))
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_hours(8 * 24))
        .unwrap();

    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(REPLAY, &watched).unwrap();
    store.enqueue(REPLAY, &unreadable).unwrap();
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        // Ends with the test even when it fails part-way.
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .args(["--replay-cache-age", "7"])
        // So the disk the tests run on is never low.
        .args(["--disk-warn-below", "1"])
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let reader = store.reader().unwrap();
    wait_until("every job to end", || {
        reader
            .jobs(10)
            .unwrap()
            .iter()
            .all(|job| matches!(job.state, JobState::Done | JobState::Failed))
    });
    let mut ended: Vec<_> = reader
        .jobs(10)
        .unwrap()
        .into_iter()
        .map(|job| (job.episode, job.failure.map(|failure| failure.code)))
        .collect();
    ended.sort();
    assert_eq!(
        ended,
        [
            (watched.clone(), None),
            (unreadable.clone(), Some("recording.unreadable".to_owned())),
        ]
    );
    // Made without a package, its replay has no robot, and says so.
    let made = reader.job(REPLAY, &watched).unwrap().unwrap();
    let warnings: Vec<&str> = made
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect();
    assert_eq!(warnings, ["viewer.replay_without_robot"]);
    // Only the replay just made is left, and complete.
    let mut cached: Vec<String> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    cached.sort();
    assert_eq!(cached, [format!("{watched}.view")]);
    let view = std::fs::read(cache.join(&cached[0])).unwrap();
    assert!(
        view.starts_with(b"KVRV") && view.ends_with(b"KVRV"),
        "not a whole view"
    );
    worker.kill().unwrap();
    worker.wait().unwrap();
}

#[test]
fn the_worker_flags_an_episode_unusual_for_its_session_without_rejecting_it() {
    let folder = scratch("worker-anomalies");
    let data = folder.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    let id = |n: u32| format!("20260310T12{n:02}00.000Z-0000000{n}");
    // Recorded alike, the same day with no package, profile or task: one
    // session. The last lost data on the way, as none of the others did.
    let alike: Vec<String> = (1..=5).map(id).collect();
    let unusual = id(6);
    let qa = "qa_params_version = \"1\"\n[streams.complete]\nmax_dropped = 50\n";
    for (episode, dropped) in alike
        .iter()
        .map(|episode| (episode, 0))
        .chain([(&unusual, 40)])
    {
        finalised(&data, episode);
        summarised(&data, episode, dropped);
        recorded(&data, episode, Some(qa));
        store
            .index(Episode::read(&data.join(episode)).unwrap().into())
            .unwrap();
        // Oldest first, so each is checked after those before it.
        store.enqueue(QA, episode).unwrap();
    }
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let reader = store.reader().unwrap();
    wait_until("every job to end", || {
        reader
            .jobs(10)
            .unwrap()
            .iter()
            .all(|job| job.state == JobState::Done)
    });
    let anomalies = |episode: &str| {
        let run = reader.qa(episode).unwrap().unwrap();
        let result = run
            .results
            .iter()
            .find(|result| result.check == "anomalies.statistics")
            .unwrap()
            .clone();
        (run.accepted(), result)
    };
    // The first has no other checked episode to be compared with.
    let (_, first) = anomalies(&alike[0]);
    assert_eq!(first.verdict, Verdict::NotApplicable, "{}", first.message);
    let (accepted, unusual) = anomalies(&unusual);
    assert_eq!(unusual.verdict, Verdict::Failed, "{}", unusual.message);
    assert_eq!(unusual.values["session.episodes"], 5.0);
    assert!(unusual.values["streams.arm_joints.dropped_ratio"] > 3.5);
    assert!(accepted, "a flag never rejects");
    worker.kill().unwrap();
    worker.wait().unwrap();
}

#[test]
fn the_worker_learns_a_model_of_a_package_and_task_and_flags_with_it_naming_its_version() {
    let folder = scratch("worker-autoencoder");
    let data = folder.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    let id = |n: u32| format!("20260310T12{n:02}00.000Z-0000000{n}");
    let qa = "qa_params_version = \"1\"\n[streams.complete]\nmax_dropped = 50\n\
              [anomalies.statistics]\nenabled = false\n\
              [anomalies.autoencoder]\nmin_episodes = 3\nretrain_after = 2\n";
    // Five alike, then one that lost data on the way, as none before did.
    for n in 1..=6 {
        let episode = id(n);
        finalised(&data, &episode);
        summarised(&data, &episode, if n == 6 { 40 } else { 0 });
        recorded(&data, &episode, Some(qa));
        store
            .index(Episode::read(&data.join(&episode)).unwrap().into())
            .unwrap();
        store.enqueue(QA, &episode).unwrap();
    }
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let reader = store.reader().unwrap();
    wait_until("every job to end", || {
        reader
            .jobs(10)
            .unwrap()
            .iter()
            .all(|job| job.state == JobState::Done)
    });
    let found = |n: u32| {
        let run = reader.qa(&id(n)).unwrap().unwrap();
        let accepted = run.accepted();
        let result = run
            .results
            .into_iter()
            .find(|result| result.check == "anomalies.autoencoder")
            .unwrap();
        (accepted, result)
    };
    let (_, first) = found(1);
    assert_eq!(first.verdict, Verdict::NotApplicable);
    assert_eq!(
        first.message,
        "not trained yet: 3 more checked episodes of this robot package and task needed"
    );
    // Learned before checking the fourth, from the first three; kept, not
    // learned again for the fifth, one new episode later.
    let (_, fourth) = found(4);
    assert_eq!(fourth.verdict, Verdict::Passed, "{}", fourth.message);
    assert_eq!(fourth.values["model.version"], 1.0);
    assert_eq!(found(5).1.values["model.version"], 1.0);
    // Two new ones later, learned again; the sixth stands out.
    let (accepted, sixth) = found(6);
    assert_eq!(sixth.verdict, Verdict::Failed, "{}", sixth.message);
    assert_eq!(sixth.values["model.version"], 2.0);
    assert!(accepted, "a flag never rejects");
    let model = reader
        .latest_model(&id(6), "anomalies.autoencoder")
        .unwrap()
        .unwrap();
    assert_eq!(model.version, 2);
    assert_eq!(model.episodes, (1..=5).map(id).collect::<Vec<_>>());
    worker.kill().unwrap();
    worker.wait().unwrap();
}

#[test]
fn the_statistics_its_qa_toml_turns_off_are_neither_kept_nor_compared() {
    let folder = scratch("worker-statistics");
    let data = folder.join("data");
    let episode = "20260310T120100.000Z-00000001";
    let qa = "qa_params_version = \"1\"\n[statistics]\n\
              exclude = [\"joints.*.jerk\", \"streams.*.rate_hz\", \"joints.typo.speed\"]\n\
              include = [\"streams.arm_joints.rate_hz\"]\n";
    finalised(&data, episode);
    summarised(&data, episode, 0);
    recorded(&data, episode, Some(qa));
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(QA, episode).unwrap();
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let reader = store.reader().unwrap();
    wait_until("the job to end", || {
        reader.jobs(1).unwrap()[0].state == JobState::Done
    });
    let kept = reader.qa(episode).unwrap().unwrap().statistics;
    assert!(!kept.contains_key("joints.shoulder.jerk"));
    assert!(kept.contains_key("joints.shoulder.speed"));
    // Turned back on.
    assert!(kept.contains_key("streams.arm_joints.rate_hz"));
    worker.kill().unwrap();
    let output = worker.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        said.contains(
            "`joints.typo.speed` in the `[statistics]` of its qa.toml names none of its statistics"
        ),
        "{said}"
    );
}

/// Records, indexes and queues six alike episodes, the last having lost
/// data on the way as none before did, with `qa` as their `qa.toml`, has a
/// worker check them, and gives each one's autoencoder result in order.
fn autoencoded(test: &str, qa: &str) -> Vec<kapto_store::QaResult> {
    let folder = scratch(test);
    let data = folder.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    let id = |n: u32| format!("20260310T12{n:02}00.000Z-0000000{n}");
    for n in 1..=6 {
        let episode = id(n);
        finalised(&data, &episode);
        summarised(&data, &episode, if n == 6 { 40 } else { 0 });
        recorded(&data, &episode, Some(qa));
        store
            .index(Episode::read(&data.join(&episode)).unwrap().into())
            .unwrap();
        store.enqueue(QA, &episode).unwrap();
    }
    let _serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let reader = store.reader().unwrap();
    wait_until("every job to end", || {
        reader
            .jobs(10)
            .unwrap()
            .iter()
            .all(|job| job.state == JobState::Done)
    });
    worker.kill().unwrap();
    worker.wait().unwrap();
    (1..=6)
        .map(|n| {
            reader
                .qa(&id(n))
                .unwrap()
                .unwrap()
                .results
                .into_iter()
                .find(|result| result.check == "anomalies.autoencoder")
                .unwrap()
        })
        .collect()
}

#[test]
fn the_neural_autoencoder_is_learned_when_qa_toml_asks_for_it_and_flags_with_it() {
    let found = autoencoded(
        "worker-neural",
        "qa_params_version = \"1\"\n[streams.complete]\nmax_dropped = 50\n\
         [anomalies.statistics]\nenabled = false\n\
         [anomalies.autoencoder]\nmodel = \"neural\"\nneurons = 8\nmin_episodes = 5\n",
    );
    assert_eq!(
        found[4].verdict,
        Verdict::NotApplicable,
        "{}",
        found[4].message
    );
    // Learned from the first five before checking the sixth, which stands
    // out.
    assert_eq!(found[5].verdict, Verdict::Failed, "{}", found[5].message);
    assert!(
        found[5]
            .message
            .starts_with("unlike the 5 episodes its neural model learned from"),
        "{}",
        found[5].message
    );
    assert_eq!(found[5].values["model.version"], 1.0);
}

#[test]
fn learning_that_passes_its_time_limit_stops_and_says_so() {
    let found = autoencoded(
        "worker-time-limit",
        "qa_params_version = \"1\"\n[anomalies.statistics]\nenabled = false\n\
         [anomalies.autoencoder]\nmodel = \"neural\"\nmin_episodes = 5\ntrain_seconds = 0.001\n",
    );
    assert_eq!(found[5].verdict, Verdict::NotApplicable);
    assert_eq!(
        found[5].message,
        "not trained yet: learning stopped at its 0.001 s time limit"
    );
}

#[test]
fn the_worker_uploads_each_verified_episode_on_its_own_lane_at_low_priority() {
    let folder = scratch("worker-upload");
    let data = folder.join("data");
    let bucket = folder.join("bucket");
    let episode = "20260310T120000.000Z-00000001";
    // Its summary first, so the manifest lists it, as recording does.
    std::fs::create_dir_all(data.join(episode)).unwrap();
    summarised(&data, episode, 0);
    finalised(&data, episode);
    let config = folder.join("kapto.toml");
    std::fs::write(
        &config,
        format!(
            "[[upload.destination]]\nname = \"site\"\nurl = \"file://{}\"\nprefix = \"fleet-a\"\n",
            bucket.display()
        ),
    )
    .unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--config")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(VERIFY, episode).unwrap();
    let uploads = Some(Uploads {
        destinations: vec![site()],
        data_dir: data.clone(),
        retention: Retention::default(),
    });
    let _serve =
        JobServer::listen_with_uploads(&data.join(JOB_SOCKET), store.clone(), uploads).unwrap();
    let reader = store.reader().unwrap();
    wait_until("the episode to be uploaded", || {
        reader
            .upload(episode, "site")
            .unwrap()
            .is_some_and(|upload| upload.state == UploadState::Uploaded)
    });

    let upload = reader.upload(episode, "site").unwrap().unwrap();
    let key = format!("fleet-a/unpackaged/2026-03-05/{episode}");
    assert_eq!(upload.key, key);
    // Every byte of its files counted as sent, for its status.
    let total: u64 = ["recording.mcap", "manifest.json", "episode.json"]
        .iter()
        .map(|file| {
            std::fs::metadata(data.join(episode).join(file))
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(
        (upload.counts.bytes_sent, upload.counts.bytes_total),
        (total, total)
    );
    let there = bucket.join(&key);
    for file in ["recording.mcap", "manifest.json"] {
        assert_eq!(
            std::fs::read(there.join(file)).unwrap(),
            std::fs::read(data.join(episode).join(file)).unwrap(),
            "{file}"
        );
    }
    let marker = std::fs::read(there.join("_SUCCESS")).unwrap();
    let marker = kapto_store::upload_marker::UploadMarker::parse(&marker).unwrap();
    assert_eq!(marker.episode, episode);
    let paths: Vec<&str> = upload
        .objects
        .iter()
        .map(|object| object.path.as_str())
        .collect();
    assert_eq!(paths, ["episode.json", "manifest.json", "recording.mcap"]);
    assert_eq!(upload.objects[2].key, format!("{key}/recording.mcap"));
    let job = reader.job(UPLOAD, episode).unwrap().unwrap();
    assert_eq!((job.state, job.failure), (JobState::Done, None));

    // The upload lane and its runtime's helpers share the worker's low
    // priority, so uploads never compete with capture for the CPU or disk.
    #[cfg(target_os = "linux")]
    {
        let threads = priorities(worker.id());
        let names: Vec<&str> = threads.iter().map(|(name, ..)| name.as_str()).collect();
        assert!(names.contains(&"wrk-upload"), "{names:?}");
        for (name, nice, class) in &threads {
            assert_eq!((*nice, *class), (10, 3), "{name}");
        }
    }
    worker.kill().unwrap();
    let _ = worker.wait();
}

#[test]
fn a_destinations_credentials_are_read_from_where_they_are_named_and_never_shown() {
    let folder = scratch("worker-upload-secret");
    let data = folder.join("data");
    let episode = "20260310T120000.000Z-00000003";
    std::fs::create_dir_all(data.join(episode)).unwrap();
    summarised(&data, episode, 0);
    finalised(&data, episode);
    let config = folder.join("kapto.toml");
    // Nothing listens on the discard port: the upload fails at once, and
    // what is said about it is checked for the secret.
    std::fs::write(
        &config,
        "[[upload.destination]]\nname = \"site\"\nurl = \"s3://robot-data\"\n\
         endpoint = \"http://127.0.0.1:9\"\ncredentials = { env = \"SITE\" }\nmax_attempts = 1\n",
    )
    .unwrap();
    let secret = "kapto-s3cr3t-never-shown";
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--config")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .env("SITE_ACCESS_KEY_ID", "kapto")
        .env("SITE_SECRET_ACCESS_KEY", secret)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(VERIFY, episode).unwrap();
    let uploads = Some(Uploads {
        destinations: vec![site()],
        data_dir: data.clone(),
        retention: Retention::default(),
    });
    let _serve =
        JobServer::listen_with_uploads(&data.join(JOB_SOCKET), store.clone(), uploads).unwrap();
    let reader = store.reader().unwrap();
    wait_until("the upload to fail", || {
        reader
            .upload(episode, "site")
            .unwrap()
            .is_some_and(|upload| upload.state == UploadState::Failed)
    });
    worker.kill().unwrap();
    let output = worker.wait_with_output().unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let failure = reader
        .upload(episode, "site")
        .unwrap()
        .unwrap()
        .failure
        .unwrap();
    assert_eq!(failure.code, "upload.unreachable", "{failure:?}");
    assert!(said.contains("not uploaded"), "{said}");
    for text in [&said, &failure.message, &format!("{failure:?}")] {
        assert!(!text.contains(secret), "the secret was shown: {text}");
    }
}

#[test]
fn an_upload_stopped_by_sigterm_is_given_back_after_its_file_and_carries_on_from_the_next() {
    let folder = scratch("worker-upload-stopped");
    let data = folder.join("data");
    let bucket = folder.join("bucket");
    let episode = "20260310T120000.000Z-00000004";
    // A first file long to upload in a test build, so the stop comes while
    // it goes up; the upload stops after it, before the next.
    let files = data.join(episode);
    std::fs::create_dir_all(&files).unwrap();
    let large: Vec<u8> = (0..64u32 << 20).map(|at| (at % 251) as u8).collect();
    std::fs::write(files.join("attachment.bin"), large).unwrap();
    summarised(&data, episode, 0);
    finalised(&data, episode);
    let config = folder.join("kapto.toml");
    std::fs::write(
        &config,
        format!(
            "[[upload.destination]]\nname = \"site\"\nurl = \"file://{}\"\n",
            bucket.display()
        ),
    )
    .unwrap();
    let start_worker = || {
        Command::new(env!("CARGO_BIN_EXE_kapto"))
            .arg("worker")
            .args(["--parent", &std::process::id().to_string()])
            .arg("--config")
            .arg(&config)
            .arg("--data-dir")
            .arg(&data)
            .env_clear()
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(VERIFY, episode).unwrap();
    let uploads = Some(Uploads {
        destinations: vec![site()],
        data_dir: data.clone(),
        retention: Retention::default(),
    });
    let _serve =
        JobServer::listen_with_uploads(&data.join(JOB_SOCKET), store.clone(), uploads).unwrap();
    let reader = store.reader().unwrap();
    let upload = || reader.upload(episode, "site").unwrap();

    let worker = start_worker();
    wait_until("the upload to start", || upload().is_some());
    Command::new("kill")
        .args(["-TERM", &worker.id().to_string()])
        .status()
        .unwrap();
    let output = worker.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{said}");
    assert!(said.contains("stopped part-way, given back"), "{said}");
    let job = reader.job(UPLOAD, episode).unwrap().unwrap();
    assert_eq!((job.state, job.attempts), (JobState::Queued, 0));
    let stopped = upload().unwrap();
    assert_eq!(stopped.state, UploadState::Uploading, "stopped, not failed");
    assert!(!bucket.join(&stopped.key).join("_SUCCESS").exists());

    let mut worker = start_worker();
    wait_until("the upload to carry on and finish", || {
        upload().is_some_and(|upload| upload.state == UploadState::Uploaded)
    });
    assert_eq!(upload().unwrap().key, stopped.key);
    worker.kill().unwrap();
    let _ = worker.wait();
}

#[test]
#[ignore = "needs the RustFS server `cargo xtask storage` starts"]
fn the_worker_uploads_to_s3_compatible_storage_in_parts_with_credentials_by_reference() {
    use object_store::ObjectStoreExt as _;

    let folder = scratch("worker-upload-s3");
    let data = folder.join("data");
    let episode = "20260310T120000.000Z-00000002";
    let recording: Vec<u8> = (0..=250_u8).cycle().take(12 << 20).collect();
    let there = data.join(episode);
    std::fs::create_dir_all(&there).unwrap();
    summarised(&data, episode, 0);
    std::fs::write(there.join("recording.mcap"), &recording).unwrap();
    Manifest::of_directory(&there, &episode.parse().unwrap())
        .unwrap()
        .write(&there)
        .unwrap();
    std::fs::write(there.join(FINALISED_FILE), b"").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let bucket = runtime.block_on(kapto_upload::rustfs::bucket("worker"));
    let config = folder.join("kapto.toml");
    std::fs::write(
        &config,
        format!(
            "[[upload.destination]]\nname = \"site\"\nurl = \"s3://worker\"\n\
             endpoint = \"{}\"\npart_size = 5\ncredentials = {{ env = \"RUSTFS\" }}\n",
            kapto_upload::rustfs::endpoint()
        ),
    )
    .unwrap();
    // The server's credentials by reference, under names of the
    // destination's own; only the region is the provider's.
    let credentials = [
        ("RUSTFS_ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID"),
        ("RUSTFS_SECRET_ACCESS_KEY", "AWS_SECRET_ACCESS_KEY"),
        ("AWS_REGION", "AWS_REGION"),
    ]
    .map(|(name, from)| (name, std::env::var(from).unwrap()));
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--config")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .envs(credentials)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.enqueue(VERIFY, episode).unwrap();
    let uploads = Some(Uploads {
        destinations: vec![site()],
        data_dir: data.clone(),
        retention: Retention::default(),
    });
    let _serve =
        JobServer::listen_with_uploads(&data.join(JOB_SOCKET), store.clone(), uploads).unwrap();
    let reader = store.reader().unwrap();
    wait_until("the episode to be uploaded", || {
        reader
            .upload(episode, "site")
            .unwrap()
            .is_some_and(|upload| upload.state != UploadState::Uploading)
    });

    let upload = reader.upload(episode, "site").unwrap().unwrap();
    assert_eq!(upload.state, UploadState::Uploaded, "{:?}", upload.failure);
    let key = format!("unpackaged/2026-03-05/{episode}");
    let object = |file: &str| object_store::path::Path::from(format!("{key}/{file}"));
    let uploaded = runtime.block_on(async {
        bucket
            .get(&object("recording.mcap"))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
    });
    assert!(
        uploaded.as_ref() == recording.as_slice(),
        "the recording came back different"
    );
    let marker = runtime.block_on(async {
        bucket
            .get(&object("_SUCCESS"))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
    });
    let marker = kapto_store::upload_marker::UploadMarker::parse(&marker).unwrap();
    assert_eq!(marker.episode, episode);
    // S3 tags a file put in parts with its parts' count after a dash.
    let recorded = upload
        .objects
        .iter()
        .find(|object| object.path == "recording.mcap");
    let tag = recorded.and_then(|object| object.e_tag.clone()).unwrap();
    assert!(tag.trim_matches('"').ends_with("-3"), "{tag}");
    worker.kill().unwrap();
    let _ = worker.wait();
}

/// Deleting an episode once it is uploaded.
fn after_upload() -> Retention {
    Retention {
        delete_local: kapto_config::DeleteLocal::AfterUpload,
        min_verified_destinations: 1,
        free_below_bytes: 0,
    }
}

#[test]
fn an_episode_uploaded_and_verified_is_deleted_here_and_stays_listed_with_where_it_went() {
    let folder = scratch("worker-retention");
    let data = folder.join("data");
    let bucket = folder.join("bucket");
    let episode = "20260310T120000.000Z-00000005";
    std::fs::create_dir_all(data.join(episode)).unwrap();
    summarised(&data, episode, 0);
    finalised(&data, episode);
    let config = folder.join("kapto.toml");
    std::fs::write(
        &config,
        format!(
            "[[upload.destination]]\nname = \"site\"\nurl = \"file://{}\"\n",
            bucket.display()
        ),
    )
    .unwrap();
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store
        .index(Episode::read(&data.join(episode)).unwrap().into())
        .unwrap();
    store.enqueue(VERIFY, episode).unwrap();
    let uploads = Some(Uploads {
        destinations: vec![site()],
        data_dir: data.clone(),
        retention: after_upload(),
    });
    let _serve =
        JobServer::listen_with_uploads(&data.join(JOB_SOCKET), store.clone(), uploads).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--config")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let reader = store.reader().unwrap();
    // Its folder is renamed aside before it is removed: both gone, or the
    // worker may be killed between the two.
    wait_until("the episode to be deleted here", || {
        reader.deletion(episode).unwrap().is_some()
            && !data.join(episode).exists()
            && !data.join(format!(".deleting-{episode}")).exists()
    });
    worker.kill().unwrap();
    let _ = worker.wait();

    // Its copy is there, the audit says where, and the index, rebuilt as
    // when serve starts, still lists it.
    let deletion = reader.deletion(episode).unwrap().unwrap();
    let key = &deletion.copies[0].key;
    assert_eq!(deletion.copies[0].destination, "site");
    assert!(bucket.join(key).join("_SUCCESS").exists());
    assert!(!data.join(format!(".deleting-{episode}")).exists());
    store.rebuild(&data).unwrap();
    let listed = reader.episodes(10).unwrap();
    assert_eq!(listed[0].id, episode);
}

#[test]
fn a_deletion_cut_off_after_its_folder_was_set_aside_is_finished_or_put_back_when_the_worker_starts()
 {
    let folder = scratch("worker-retention-aside");
    let data = folder.join("data");
    let (uploaded, unverified) = (
        "20260310T120000.000Z-00000006",
        "20260310T120500.000Z-00000007",
    );
    std::fs::create_dir_all(&data).unwrap();
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    for episode in [uploaded, unverified] {
        std::fs::create_dir_all(data.join(episode)).unwrap();
        summarised(&data, episode, 0);
        finalised(&data, episode);
        store
            .index(Episode::read(&data.join(episode)).unwrap().into())
            .unwrap();
        // Set aside, as a crash would have left them.
        std::fs::rename(
            data.join(episode),
            data.join(format!(".deleting-{episode}")),
        )
        .unwrap();
    }
    store.start_upload(uploaded, "site", "fleet-a/six").unwrap();
    store.uploaded(uploaded, "site", Vec::new()).unwrap();
    let uploads = Some(Uploads {
        destinations: vec![site()],
        data_dir: data.clone(),
        retention: after_upload(),
    });
    let _serve =
        JobServer::listen_with_uploads(&data.join(JOB_SOCKET), store.clone(), uploads).unwrap();
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("worker")
        .args(["--parent", &std::process::id().to_string()])
        .arg("--data-dir")
        .arg(&data)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let aside = |episode: &str| data.join(format!(".deleting-{episode}"));
    wait_until("both set aside to be dealt with", || {
        !aside(uploaded).exists() && !aside(unverified).exists()
    });
    worker.kill().unwrap();
    let _ = worker.wait();
    let reader = store.reader().unwrap();
    // Verified elsewhere: gone, and recorded.
    assert!(reader.deletion(uploaded).unwrap().is_some());
    assert!(!data.join(uploaded).exists());
    // Verified nowhere: refused, and back whole.
    assert_eq!(reader.deletion(unverified).unwrap(), None);
    assert!(data.join(unverified).join(FINALISED_FILE).exists());
}
