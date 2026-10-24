// SPDX-License-Identifier: Apache-2.0

//! Serve's job socket, spoken to line by line as the worker does.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use kapto_config::{DeleteLocal, UploadWhen};
use kapto_core::ErrorCode;
use kapto_core::clock::utc_now_ns;
use kapto_server::jobs::{Bin, Destination, JOB_SOCKET, JobServer, QA, UPLOAD, Uploads, VERIFY};
use kapto_server::retention::Retention;
use kapto_store::db::DATABASE_FILE;
use kapto_store::{Binned, DeletionReason, JobState, UploadState, Writer};
use kapto_testkit::scratch;
use serde_json::{Value, json};

/// A connection to the job socket.
struct Worker(BufReader<UnixStream>);

impl Worker {
    fn connect(folder: &Path) -> Self {
        Self(BufReader::new(
            UnixStream::connect(folder.join(JOB_SOCKET)).unwrap(),
        ))
    }

    /// Sends one line and reads the answer.
    fn ask(&mut self, line: &str) -> Value {
        let stream = self.0.get_mut();
        stream.write_all(line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        let mut answer = String::new();
        self.0.read_line(&mut answer).unwrap();
        serde_json::from_str(&answer).unwrap()
    }

    /// Whether serve closed the connection.
    fn closed(&mut self) -> bool {
        let mut rest = String::new();
        self.0.read_line(&mut rest).unwrap() == 0
    }
}

const FIRST: &str = "20260305T192345.123Z-1a2b3c4d";
const SECOND: &str = "20260306T080000.000Z-5e6f7a8b";

#[test]
fn the_worker_gets_the_queued_jobs_oldest_first_and_reports_how_each_ended() {
    let folder = scratch("order");
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let first = store.enqueue(VERIFY, FIRST).unwrap();
    let second = store.enqueue(VERIFY, SECOND).unwrap();
    let _server = JobServer::listen(&folder.join(JOB_SOCKET), store.clone()).unwrap();

    let mut worker = Worker::connect(&folder);
    assert_eq!(
        worker.ask(r#"{"request":"hello","version":1}"#),
        json!({"response": "hello", "version": 1})
    );
    assert_eq!(
        worker.ask(r#"{"request":"next"}"#),
        json!({"response": "job", "job": {"id": first, "kind": "verify", "episode": FIRST}})
    );
    let failed = json!({
        "request": "finished",
        "job": first,
        "failure": {"code": "manifest.mismatch", "message": "recording.mcap differs"},
    });
    assert_eq!(
        worker.ask(&failed.to_string()),
        json!({"response": "finished", "was_running": true})
    );
    // Reported twice, the second changes nothing.
    assert_eq!(
        worker.ask(&failed.to_string()),
        json!({"response": "finished", "was_running": false})
    );
    assert_eq!(worker.ask(r#"{"request":"next"}"#)["job"]["id"], second);
    let done = json!({"request": "finished", "job": second, "failure": null});
    assert_eq!(worker.ask(&done.to_string())["was_running"], true);
    assert_eq!(
        worker.ask(r#"{"request":"next"}"#),
        json!({"response": "job", "job": null})
    );

    let jobs = store.reader().unwrap().jobs(10).unwrap();
    assert_eq!(
        jobs.iter().map(|job| job.state).collect::<Vec<_>>(),
        [JobState::Done, JobState::Failed]
    );
    let failure = jobs[1].failure.as_ref().unwrap();
    assert_eq!(
        (failure.code.as_str(), failure.message.as_str()),
        ("manifest.mismatch", "recording.mcap differs")
    );
}

#[test]
fn quality_results_reported_with_a_job_are_recorded_and_kept_when_serve_starts_again() {
    let folder = scratch("qa");
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let job = store.enqueue(QA, FIRST).unwrap();
    let server = JobServer::listen(&folder.join(JOB_SOCKET), store.clone()).unwrap();
    let mut worker = Worker::connect(&folder);
    worker.ask(r#"{"request":"hello","version":1}"#);
    worker.ask(r#"{"request":"next"}"#);

    let results = |episode: &str| {
        json!({
            "request": "finished",
            "job": job,
            "failure": null,
            "qa": {
                "episode": episode,
                "params_version": null,
                "computed_utc_ns": 1_772_700_500_000_000_000_i64,
                "results": [{
                    "check": "streams.complete",
                    "version": 1,
                    "kind": "gate",
                    "verdict": "failed",
                    "message": "Kapto dropped 12 messages: arm_joints (12 of 2000 received)",
                    "values": {"dropped_by_us": 12.0},
                }],
            },
        })
        .to_string()
    };
    // Results for something that isn't an episode are refused, and the job
    // isn't finished without its results.
    let refused = worker.ask(&results("../elsewhere"));
    assert_eq!(refused["code"], ErrorCode::WorkerInvalidRequest.id());
    assert_eq!(
        store.reader().unwrap().jobs(1).unwrap()[0].state,
        JobState::Running
    );
    assert_eq!(
        worker.ask(&results(FIRST)),
        json!({"response": "finished", "was_running": true})
    );
    drop((worker, server, store));

    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let reader = store.reader().unwrap();
    assert_eq!(reader.jobs(1).unwrap()[0].state, JobState::Done);
    let run = reader.qa(FIRST).unwrap().unwrap();
    assert!(!run.accepted());
    assert_eq!(run.results[0].values["dropped_by_us"], 12.0);
}

#[test]
fn a_connection_that_does_not_start_with_hello_in_serves_version_is_refused_and_closed() {
    let folder = scratch("refused");
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    store.enqueue(VERIFY, FIRST).unwrap();
    let _server = JobServer::listen(&folder.join(JOB_SOCKET), store.clone()).unwrap();

    for (line, code) in [
        (r#"{"request":"next"}"#, ErrorCode::WorkerInvalidRequest),
        ("not json", ErrorCode::WorkerInvalidRequest),
        (
            r#"{"request":"hello","version":2}"#,
            ErrorCode::WorkerVersionMismatch,
        ),
    ] {
        let mut worker = Worker::connect(&folder);
        let answer = worker.ask(line);
        assert_eq!(answer["response"], "error", "{line}");
        assert_eq!(answer["code"], code.id(), "{line}");
        assert!(worker.closed(), "{line}");
    }
    // Nothing was handed out, and a worker that greets properly is served.
    let mut worker = Worker::connect(&folder);
    worker.ask(r#"{"request":"hello","version":1}"#);
    assert_eq!(worker.ask(r#"{"request":"next"}"#)["job"]["episode"], FIRST);
}

#[test]
fn the_socket_replaces_an_old_one_is_removed_when_serve_stops_and_a_file_in_its_way_is_refused() {
    let folder = scratch("socket");
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let socket = folder.join(JOB_SOCKET);
    // One left by a serve that was killed.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    let server = JobServer::listen(&socket, store.clone()).unwrap();
    drop(server);
    assert!(!socket.exists());

    std::fs::write(&socket, b"not a socket").unwrap();
    let error = JobServer::listen(&socket, store).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ServerJobSocketFailed);
}

#[test]
fn two_lanes_are_served_at_once_each_given_only_the_kinds_it_asks_for() {
    let folder = scratch("lanes");
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let verify = store.enqueue(VERIFY, FIRST).unwrap();
    let upload = store.enqueue(UPLOAD, SECOND).unwrap();
    let _server = JobServer::listen(&folder.join(JOB_SOCKET), store.clone()).unwrap();

    // Both connected and greeted before either asks for work: one lane
    // never waits for the other to hang up.
    let mut checks = Worker::connect(&folder);
    let mut uploads = Worker::connect(&folder);
    checks.ask(r#"{"request":"hello","version":1}"#);
    uploads.ask(r#"{"request":"hello","version":1}"#);
    let first = uploads.ask(r#"{"request":"next","kinds":["upload"]}"#);
    assert_eq!(
        first["job"]["id"], upload,
        "the older verify job isn't its kind"
    );
    let first = checks.ask(r#"{"request":"next","kinds":["verify","events","qa","replay"]}"#);
    assert_eq!(first["job"]["id"], verify);
    assert_eq!(
        checks.ask(r#"{"request":"next","kinds":["verify"]}"#),
        json!({"response": "job", "job": null})
    );
    // A lane leaving uploads to another still takes a kind nobody does,
    // so it can say so.
    let unknown = store.enqueue("transcribe", FIRST).unwrap();
    store.enqueue(UPLOAD, FIRST).unwrap();
    let next = checks.ask(r#"{"request":"next","except":["upload"]}"#);
    assert_eq!(next["job"]["id"], unknown);
    assert_eq!(
        checks.ask(r#"{"request":"next","except":["upload"]}"#),
        json!({"response": "job", "job": null})
    );
}

#[test]
fn an_upload_is_started_carried_on_and_recorded_as_its_lane_reports_it() {
    let folder = scratch("upload-reports");
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let _server = JobServer::listen(&folder.join(JOB_SOCKET), store.clone()).unwrap();
    let mut lane = Worker::connect(&folder);
    lane.ask(r#"{"request":"hello","version":1}"#);

    let start = |key: &str| {
        json!({"request": "upload_start", "episode": FIRST, "destination": "nas", "key": key})
            .to_string()
    };
    assert_eq!(
        lane.ask(&start("fleet-a/arm7/one")),
        json!({"response": "upload", "key": "fleet-a/arm7/one", "progress": "{}"})
    );
    let progress = json!({
        "request": "upload_progress", "episode": FIRST, "destination": "nas",
        "progress": "{\"files\":{}}",
    });
    assert_eq!(
        lane.ask(&progress.to_string()),
        json!({"response": "saved"})
    );
    // Started again with another layout's key: the first key, and how far
    // it got.
    assert_eq!(
        lane.ask(&start("other/one")),
        json!({"response": "upload", "key": "fleet-a/arm7/one", "progress": "{\"files\":{}}"})
    );
    let uploaded = json!({
        "request": "uploaded", "episode": FIRST, "destination": "nas",
        "objects": [{
            "path": "recording.mcap", "key": "fleet-a/arm7/one/recording.mcap",
            "bytes": 10, "sha256": "a".repeat(64), "e_tag": null,
        }],
    });
    assert_eq!(
        lane.ask(&uploaded.to_string()),
        json!({"response": "saved"})
    );

    let upload = store
        .reader()
        .unwrap()
        .upload(FIRST, "nas")
        .unwrap()
        .unwrap();
    assert_eq!(upload.state, UploadState::Uploaded);
    assert_eq!(upload.objects[0].key, "fleet-a/arm7/one/recording.mcap");
}

/// Destination `name`, taking episodes as `when` says, a failed upload
/// tried again an hour after.
fn destination(name: &str, when: UploadWhen) -> Destination {
    Destination {
        name: name.to_owned(),
        when,
        retry_failed_after: Duration::from_hours(1),
    }
}

/// The upload jobs queued after the jobs of `episodes` end as each says:
/// `(kind, failed, quality accepted)`.
fn queued_uploads(when: Option<UploadWhen>, test: &str, ends: &[(&str, bool, bool)]) -> usize {
    let folder = scratch(test);
    let data = folder.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let uploads = when.map(|when| Uploads {
        destinations: vec![destination("nas", when)],
        data_dir: data.clone(),
        retention: Retention::default(),
    });
    let _server =
        JobServer::listen_with_uploads(&folder.join(JOB_SOCKET), store.clone(), uploads).unwrap();
    let mut worker = Worker::connect(&folder);
    worker.ask(r#"{"request":"hello","version":1}"#);
    // A clean episode, FIRST, and one that lost messages, SECOND.
    for (id, lost) in [(FIRST, 0), (SECOND, 3)] {
        let mut stream = kapto_testkit::stream("arm_joints", 100);
        stream.lost_upstream = lost;
        let episode = kapto_testkit::episode(id, (1, 2), vec![stream]);
        let dir = data.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        episode.write(&dir).unwrap();
    }
    for (index, (kind, failed, accepted)) in ends.iter().enumerate() {
        let episode = if index % 2 == 0 { FIRST } else { SECOND };
        let job = store.enqueue(kind, episode).unwrap();
        worker.ask(r#"{"request":"next","kinds":["verify","qa"]}"#);
        let failure = failed.then(|| json!({"code": "manifest.mismatch", "message": "differs"}));
        let verdict = if *accepted { "passed" } else { "failed" };
        let qa = json!({
            "episode": episode, "params_version": null, "computed_utc_ns": 1,
            "results": [{"check": "streams.complete", "version": 1, "kind": "gate",
                         "verdict": verdict, "message": "", "values": {}}],
        });
        let mut finished = json!({"request": "finished", "job": job, "failure": failure});
        if *kind == QA {
            finished["qa"] = qa;
        }
        assert_eq!(worker.ask(&finished.to_string())["response"], "finished");
    }
    let jobs = store.reader().unwrap().jobs(100).unwrap();
    jobs.iter().filter(|job| job.kind == UPLOAD).count()
}

#[test]
fn an_episode_is_queued_for_upload_once_it_qualifies_as_upload_when_says() {
    // Each episode's files checked: both episodes, but not one whose check
    // failed.
    let verified = [(VERIFY, false, true), (VERIFY, false, true)];
    assert_eq!(
        queued_uploads(Some(UploadWhen::Finalised), "when-finalised", &verified),
        2
    );
    let failed = [(VERIFY, true, true), (VERIFY, true, true)];
    assert_eq!(
        queued_uploads(Some(UploadWhen::Finalised), "when-failed", &failed),
        0
    );
    // Only the episode that lost nothing.
    assert_eq!(
        queued_uploads(Some(UploadWhen::FinalisedClean), "when-clean", &verified),
        1
    );
    // Only once its quality checks accepted it, not when its files were.
    let checked = [(QA, false, true), (QA, false, false), (VERIFY, false, true)];
    assert_eq!(
        queued_uploads(Some(UploadWhen::Accepted), "when-accepted", &checked),
        1
    );
    assert_eq!(
        queued_uploads(Some(UploadWhen::Never), "when-never", &verified),
        0
    );
    assert_eq!(queued_uploads(None, "when-none", &verified), 0);
}

/// Finishes `job` as the worker would, with `qa` if it ran the checks.
fn finish(worker: &mut Worker, job: i64, qa: Option<Value>) {
    let mut finished = json!({"request": "finished", "job": job, "failure": null});
    if let Some(qa) = qa {
        finished["qa"] = qa;
    }
    assert_eq!(worker.ask(&finished.to_string())["response"], "finished");
}

#[test]
fn each_destination_takes_an_episode_as_its_own_when_says_even_one_qualifying_mid_upload() {
    let folder = scratch("when-each");
    let data = folder.join("data");
    let dir = data.join(FIRST);
    std::fs::create_dir_all(&dir).unwrap();
    kapto_testkit::episode(
        FIRST,
        (1, 2),
        vec![kapto_testkit::stream("arm_joints", 100)],
    )
    .write(&dir)
    .unwrap();
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let uploads = Uploads {
        destinations: vec![
            destination("nas", UploadWhen::Finalised),
            destination("cloud", UploadWhen::Accepted),
            destination("archive", UploadWhen::Never),
        ],
        data_dir: data,
        retention: Retention::default(),
    };
    let _server =
        JobServer::listen_with_uploads(&folder.join(JOB_SOCKET), store.clone(), Some(uploads))
            .unwrap();
    let mut worker = Worker::connect(&folder);
    worker.ask(r#"{"request":"hello","version":1}"#);
    let next = |worker: &mut Worker, kind: &str| {
        let line = json!({"request": "next", "kinds": [kind]}).to_string();
        worker.ask(&line)["job"].clone()
    };

    // Its files checked: only the destination taking finalised episodes.
    store.enqueue(VERIFY, FIRST).unwrap();
    let verify = next(&mut worker, VERIFY);
    finish(&mut worker, verify["id"].as_i64().unwrap(), None);
    store.enqueue(QA, FIRST).unwrap();
    let upload = next(&mut worker, UPLOAD);
    assert_eq!(upload["destinations"], json!(["nas"]));
    let start =
        json!({"request": "upload_start", "episode": FIRST, "destination": "nas", "key": "one"});
    worker.ask(&start.to_string());

    // Its checks accept it while that upload runs: it isn't lost.
    let qa = next(&mut worker, QA);
    let accepted = json!({
        "episode": FIRST, "params_version": null, "computed_utc_ns": 1,
        "results": [{"check": "streams.complete", "version": 1, "kind": "gate",
                     "verdict": "passed", "message": "", "values": {}}],
    });
    finish(&mut worker, qa["id"].as_i64().unwrap(), Some(accepted));
    let uploaded =
        json!({"request": "uploaded", "episode": FIRST, "destination": "nas", "objects": []});
    worker.ask(&uploaded.to_string());
    finish(&mut worker, upload["id"].as_i64().unwrap(), None);

    // Then the one taking accepted episodes, and never the one with `never`.
    let upload = next(&mut worker, UPLOAD);
    assert_eq!(upload["destinations"], json!(["cloud"]));
    let start =
        json!({"request": "upload_start", "episode": FIRST, "destination": "cloud", "key": "one"});
    worker.ask(&start.to_string());
    let uploaded =
        json!({"request": "uploaded", "episode": FIRST, "destination": "cloud", "objects": []});
    worker.ask(&uploaded.to_string());
    finish(&mut worker, upload["id"].as_i64().unwrap(), None);
    assert_eq!(next(&mut worker, UPLOAD), Value::Null);
}

#[test]
fn an_upload_job_that_failed_is_not_queued_again_at_once() {
    let folder = scratch("when-failed-upload");
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    let uploads = Uploads {
        destinations: vec![destination("nas", UploadWhen::Finalised)],
        data_dir: folder.join("data"),
        retention: Retention::default(),
    };
    let _server =
        JobServer::listen_with_uploads(&folder.join(JOB_SOCKET), store.clone(), Some(uploads))
            .unwrap();
    let mut worker = Worker::connect(&folder);
    worker.ask(r#"{"request":"hello","version":1}"#);
    store.enqueue(VERIFY, FIRST).unwrap();
    let verify = worker.ask(r#"{"request":"next","kinds":["verify"]}"#)["job"]["id"].clone();
    finish(&mut worker, verify.as_i64().unwrap(), None);
    // Failed before its upload to `nas` could even start, e.g. a store
    // that couldn't be opened.
    let upload = worker.ask(r#"{"request":"next","kinds":["upload"]}"#)["job"]["id"].clone();
    let failed = json!({"request": "finished", "job": upload, "failure":
        {"code": "upload.destination_unusable", "message": "no such bucket"}});
    assert_eq!(worker.ask(&failed.to_string())["response"], "finished");
    assert_eq!(
        worker.ask(r#"{"request":"next","kinds":["upload"]}"#),
        json!({"response": "job", "job": null})
    );
}

#[test]
fn a_failed_upload_is_tried_again_once_its_destinations_retry_failed_after_has_passed() {
    let folder = scratch("retry-failed-upload");
    let data = folder.join("data");
    let dir = data.join(FIRST);
    std::fs::create_dir_all(&dir).unwrap();
    kapto_testkit::episode(
        FIRST,
        (1, 2),
        vec![kapto_testkit::stream("arm_joints", 100)],
    )
    .write(&dir)
    .unwrap();
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    // `nas` tries again at once, `cloud` only an hour after.
    let uploads = Uploads {
        destinations: vec![
            Destination {
                retry_failed_after: Duration::ZERO,
                ..destination("nas", UploadWhen::Finalised)
            },
            destination("cloud", UploadWhen::Finalised),
        ],
        data_dir: data,
        retention: Retention::default(),
    };
    let _server =
        JobServer::listen_with_uploads(&folder.join(JOB_SOCKET), store.clone(), Some(uploads))
            .unwrap();
    let mut worker = Worker::connect(&folder);
    worker.ask(r#"{"request":"hello","version":1}"#);
    store.enqueue(VERIFY, FIRST).unwrap();
    let verify = worker.ask(r#"{"request":"next","kinds":["verify"]}"#)["job"]["id"].clone();
    finish(&mut worker, verify.as_i64().unwrap(), None);

    // Both fail.
    let upload = worker.ask(r#"{"request":"next","kinds":["upload"]}"#)["job"].clone();
    assert_eq!(upload["destinations"], json!(["nas", "cloud"]));
    for name in ["nas", "cloud"] {
        let start = json!({"request": "upload_start", "episode": FIRST,
                           "destination": name, "key": "one"});
        worker.ask(&start.to_string());
        let failed = json!({"request": "upload_failed", "episode": FIRST, "destination": name,
                            "failure": {"code": "upload.unreachable", "message": "reset"}});
        assert_eq!(worker.ask(&failed.to_string())["response"], "saved");
    }
    let failed = json!({"request": "finished", "job": upload["id"], "failure":
        {"code": "upload.unreachable", "message": "reset"}});
    assert_eq!(worker.ask(&failed.to_string())["response"], "finished");

    // The next ask tries `nas` again, by itself; `cloud` waits its hour.
    let again = worker.ask(r#"{"request":"next","kinds":["upload"]}"#)["job"].clone();
    assert_eq!(again["destinations"], json!(["nas"]));
    let start =
        json!({"request": "upload_start", "episode": FIRST, "destination": "nas", "key": "one"});
    worker.ask(&start.to_string());
    let uploaded =
        json!({"request": "uploaded", "episode": FIRST, "destination": "nas", "objects": []});
    worker.ask(&uploaded.to_string());
    finish(&mut worker, again["id"].as_i64().unwrap(), None);
    assert_eq!(
        worker.ask(r#"{"request":"next","kinds":["upload"]}"#),
        json!({"response": "job", "job": null})
    );
    // `cloud` still waits, failed.
    let reader = store.reader().unwrap();
    let cloud = reader.upload(FIRST, "cloud").unwrap().unwrap();
    assert_eq!(cloud.state, UploadState::Failed);
}

/// Serve with `retention`, and FIRST and SECOND indexed, those in
/// `uploaded` uploaded to `nas`; a worker connected to it.
fn retaining(test: &str, retention: Retention, uploaded: &[&str]) -> (Writer, JobServer, Worker) {
    let folder = scratch(test);
    let data = folder.join("data");
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    for (id, start) in [(FIRST, 1), (SECOND, 3)] {
        let episode = kapto_testkit::episode(
            id,
            (start, start + 1),
            vec![kapto_testkit::stream("arm_joints", 100)],
        );
        let dir = data.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        episode.write(&dir).unwrap();
        store.index(episode.into()).unwrap();
    }
    for id in uploaded {
        store
            .start_upload(id, "nas", &format!("fleet-a/{id}"))
            .unwrap();
        store.uploaded(id, "nas", Vec::new()).unwrap();
    }
    let uploads = Uploads {
        destinations: vec![destination("nas", UploadWhen::Never)],
        data_dir: data,
        retention,
    };
    let server =
        JobServer::listen_with_uploads(&folder.join(JOB_SOCKET), store.clone(), Some(uploads))
            .unwrap();
    let mut worker = Worker::connect(&folder);
    worker.ask(r#"{"request":"hello","version":1}"#);
    (store, server, worker)
}

fn deletes(delete_local: DeleteLocal, min_verified_destinations: u32, low: bool) -> Retention {
    Retention {
        delete_local,
        min_verified_destinations,
        // The disk is low below everything, or below nothing.
        free_below_bytes: if low { u64::MAX } else { 0 },
    }
}

const NEXT_DELETE: &str = r#"{"request":"next","kinds":["delete"]}"#;

#[test]
fn an_episode_verified_on_enough_destinations_is_handed_out_to_delete_and_its_deletion_recorded() {
    let (store, _server, mut worker) = retaining(
        "retention-after-upload",
        deletes(DeleteLocal::AfterUpload, 1, false),
        &[FIRST],
    );
    let job = worker.ask(NEXT_DELETE)["job"].clone();
    assert_eq!(
        (job["kind"].as_str(), job["episode"].as_str()),
        (Some("delete"), Some(FIRST))
    );
    let deleting = json!({"request": "deleting", "episode": FIRST, "summary": "{}"});
    assert_eq!(worker.ask(&deleting.to_string())["response"], "saved");
    finish(&mut worker, job["id"].as_i64().unwrap(), None);
    let deletion = store.reader().unwrap().deletion(FIRST).unwrap().unwrap();
    assert_eq!(deletion.reason, DeletionReason::AfterUpload);
    assert_eq!(deletion.copies[0].key, format!("fleet-a/{FIRST}"));
    // SECOND is uploaded nowhere: neither handed out nor let go if asked.
    assert_eq!(worker.ask(NEXT_DELETE)["job"], Value::Null);
    let refused = json!({"request": "deleting", "episode": SECOND, "summary": "{}"});
    let answer = worker.ask(&refused.to_string());
    assert_eq!(answer["code"], "store.deletion_refused", "{answer}");
    assert_eq!(store.reader().unwrap().deletion(SECOND).unwrap(), None);
}

#[test]
fn each_delete_local_setting_lets_go_what_it_says_and_never_one_on_too_few_destinations() {
    let handed_out = |test: &str, retention: Retention, uploaded: &[&str]| {
        let (_store, _server, mut worker) = retaining(test, retention, uploaded);
        let mut episodes = Vec::new();
        while let Some(episode) = worker.ask(NEXT_DELETE)["job"]["episode"]
            .as_str()
            .map(str::to_owned)
        {
            episodes.push(episode);
            // Only the first pass queues: one a test, as at most every few
            // seconds.
            if episodes.len() > 2 {
                break;
            }
        }
        episodes
    };
    let both = [FIRST, SECOND];
    assert_eq!(
        handed_out(
            "retention-never",
            deletes(DeleteLocal::Never, 1, true),
            &both
        ),
        [""; 0]
    );
    assert_eq!(
        handed_out(
            "retention-room",
            deletes(DeleteLocal::WhenDiskLow, 1, false),
            &both
        ),
        [""; 0]
    );
    // The disk low: only the oldest, the next looked for once it is gone.
    assert_eq!(
        handed_out(
            "retention-low",
            deletes(DeleteLocal::WhenDiskLow, 1, true),
            &both
        ),
        [FIRST]
    );
    assert_eq!(
        handed_out(
            "retention-two",
            deletes(DeleteLocal::AfterUpload, 2, false),
            &both
        ),
        [""; 0]
    );
}

#[test]
fn a_deletion_is_not_handed_out_while_another_job_of_the_episode_is_waiting() {
    let (store, _server, mut worker) = retaining(
        "retention-busy",
        deletes(DeleteLocal::AfterUpload, 1, false),
        &[FIRST],
    );
    // An upload of it to another destination waiting.
    store.enqueue(UPLOAD, FIRST).unwrap();
    assert_eq!(worker.ask(NEXT_DELETE)["job"], Value::Null);
    let delete = store
        .reader()
        .unwrap()
        .job("delete", FIRST)
        .unwrap()
        .unwrap();
    assert_eq!((delete.state, delete.failure), (JobState::Done, None));
    assert_eq!(store.reader().unwrap().deletion(FIRST).unwrap(), None);
}

/// Serve with no upload destination, emptying the bin as `free_below_bytes`
/// says the disk is low, and FIRST and SECOND indexed, in `data`.
fn binning(test: &str, free_below_bytes: u64) -> (Writer, JobServer, Worker) {
    let folder = scratch(test);
    let data = folder.join("data");
    // Its free space is read, so it is there.
    std::fs::create_dir_all(&data).unwrap();
    let store = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
    for (id, start) in [(FIRST, 1), (SECOND, 3)] {
        let episode = kapto_testkit::episode(
            id,
            (start, start + 1),
            vec![kapto_testkit::stream("arm_joints", 100)],
        );
        store.index(episode.into()).unwrap();
    }
    let server = JobServer::listen(&folder.join(JOB_SOCKET), store.clone()).unwrap();
    server.empty_bin_by(Bin {
        data_dir: data,
        keep: Duration::from_hours(7 * 24),
        free_below_bytes,
    });
    let mut worker = Worker::connect(&folder);
    worker.ask(r#"{"request":"hello","version":1}"#);
    (store, server, worker)
}

fn binned(store: &Writer, episode: &str, days_ago: i64) {
    store
        .record_binned(Binned {
            episode: episode.to_owned(),
            binned_utc_ns: utc_now_ns() - days_ago * 86_400_000_000_000,
            from: "web UI".to_owned(),
        })
        .unwrap();
}

#[test]
fn an_episode_in_the_bin_longer_than_it_keeps_one_is_emptied_without_any_upload_recorded_by_hand() {
    let (store, _server, mut worker) = binning("bin-kept-days", 0);
    binned(&store, FIRST, 8);
    binned(&store, SECOND, 0);
    let waiting = store.enqueue(VERIFY, FIRST).unwrap();
    let job = worker.ask(NEXT_DELETE)["job"].clone();
    assert_eq!(job["episode"], FIRST, "only the one past its days: {job}");
    let deleting = json!({"request": "deleting", "episode": FIRST, "summary": "{}"});
    assert_eq!(worker.ask(&deleting.to_string())["response"], "saved");
    finish(&mut worker, job["id"].as_i64().unwrap(), None);
    let reader = store.reader().unwrap();
    let deletion = reader.deletion(FIRST).unwrap().unwrap();
    assert_eq!(
        (deletion.reason, deletion.copies.len()),
        (DeletionReason::ByHand, 0)
    );
    assert_eq!(reader.binned(FIRST).unwrap(), None);
    // What it had waiting ended with it; the other stays in the bin.
    assert_eq!(
        reader.job_with_id(waiting).unwrap().unwrap().state,
        JobState::Done
    );
    assert!(reader.binned(SECOND).unwrap().is_some());
    assert_eq!(worker.ask(NEXT_DELETE)["job"], Value::Null);
}

#[test]
fn while_the_disk_is_low_the_episode_longest_in_the_bin_goes_first() {
    let (store, _server, mut worker) = binning("bin-disk-low", u64::MAX);
    binned(&store, SECOND, 2);
    binned(&store, FIRST, 1);
    let job = worker.ask(NEXT_DELETE)["job"].clone();
    assert_eq!(job["episode"], SECOND, "{job}");
    // One at a time: no more go than the room needs.
    assert_eq!(worker.ask(NEXT_DELETE)["job"], Value::Null);
}
