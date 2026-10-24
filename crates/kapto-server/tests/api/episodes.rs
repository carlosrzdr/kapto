// SPDX-License-Identifier: Apache-2.0

//! The finalised episodes: listed, filtered and shown in full, with what
//! the worker found in them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use axum::http::StatusCode;
use kapto_core::ErrorCode;
use kapto_server::api::ApiState;
use kapto_server::index;
use kapto_store::{
    CheckKind, Episode, OperationMode, PackageIdentity, QaResult, QaRun, Recovered, SessionProfile,
    SessionTask, TimeRange, Verdict,
};
use kapto_testkit::scratch;
use serde_json::{Value, json};

use crate::common::{
    FakeCapture, call, finalised_episode, finalised_episode_with, recording, state,
};

#[tokio::test]
async fn each_episode_names_its_robot_package_or_null_if_unchecked() {
    let folder = scratch("episode-package");
    let data = folder.join("data");
    let checked = "20260310T120000.000Z-00000001";
    let package = PackageIdentity {
        id: "arm7".into(),
        version: "1.0.0".into(),
        hash: "9f2c".into(),
    };
    finalised_episode_with(&data, checked, 0, Some(package));
    finalised_episode(&data, "20260310T120100.000Z-00000002", 60);

    let (_, list) = call(state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(list[0]["package"], Value::Null, "the newest is unchecked");
    assert_eq!(
        list[1]["package"],
        json!({"id": "arm7", "version": "1.0.0", "hash": "9f2c"})
    );
    let (_, detail) = call(
        state(&folder),
        "GET",
        &format!("/api/episodes/{checked}"),
        None,
    )
    .await;
    assert_eq!(detail["package"]["id"], "arm7");
}

#[tokio::test]
async fn an_episode_names_its_profile_and_task_in_the_list_and_shows_them_in_full() {
    let folder = scratch("episode-session");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    finalised_episode(&data, "20260310T120500.000Z-00000002", 300);
    // Recorded with a profile and a task.
    let episode_folder = data.join(id);
    let recorded = Episode::read(&episode_folder).unwrap().with_session(
        Some(SessionProfile {
            id: "lab_ana".into(),
            hash: None,
            operator: Some("Ana".into()),
            environment: None,
            mode: Some(OperationMode::Teleoperated),
            tags: vec!["shift-a".into()],
            notes: None,
        }),
        Some(SessionTask {
            id: "pick_and_place".into(),
            hash: None,
            name: Some("Pick and place".into()),
            description: None,
            prompt: Some("Pick up the red cube".into()),
            objects: Vec::new(),
            subtasks: vec!["reach".into(), "grasp".into()],
        }),
    );
    recorded.write(&episode_folder).unwrap();

    let (_, list) = call(state(&folder), "GET", "/api/episodes", None).await;
    let session = |episode: &Value| {
        ["profile", "mode", "task", "task_name"].map(|field| episode[field].clone())
    };
    assert_eq!(
        session(&list[0]),
        [Value::Null, Value::Null, Value::Null, Value::Null]
    );
    assert_eq!(
        session(&list[1]),
        [
            json!("lab_ana"),
            json!("teleoperated"),
            json!("pick_and_place"),
            json!("Pick and place")
        ]
    );
    let (_, detail) = call(state(&folder), "GET", &format!("/api/episodes/{id}"), None).await;
    assert_eq!(detail["profile"]["operator"], "Ana");
    assert_eq!(detail["profile"]["tags"], json!(["shift-a"]));
    assert_eq!(detail["task"]["prompt"], "Pick up the red cube");
    assert_eq!(detail["task"]["subtasks"], json!(["reach", "grasp"]));
}

#[tokio::test]
async fn a_recovered_episode_says_so_in_the_list_and_in_full_and_others_say_null() {
    let folder = scratch("episode-recovered");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    finalised_episode(&data, "20260310T120500.000Z-00000002", 300);
    let episode_folder = data.join(id);
    Episode::read(&episode_folder)
        .unwrap()
        .with_recovered(Recovered {
            at_utc_ns: 1_772_701_000_000_000_000,
            bytes_cut: 4096,
            last_message_utc_ns: Some(1_772_700_400_900_000_000),
            reason: "recording stopped without finishing it".into(),
        })
        .write(&episode_folder)
        .unwrap();

    let recovered = json!({
        "at_utc_ns": 1_772_701_000_000_000_000_i64,
        "bytes_cut": 4096,
        "last_message_utc_ns": 1_772_700_400_900_000_000_i64,
        "reason": "recording stopped without finishing it",
    });
    let (_, list) = call(state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(list[0]["recovered"], Value::Null);
    assert_eq!(list[1]["recovered"], recovered);
    let (_, detail) = call(state(&folder), "GET", &format!("/api/episodes/{id}"), None).await;
    assert_eq!(detail["recovered"], recovered);
}

#[tokio::test]
async fn episodes_lists_the_finalised_ones_newest_first_and_nothing_else() {
    let folder = scratch("episodes");
    let data = folder.join("data");
    finalised_episode(&data, "20260310T120000.000Z-00000001", 0);
    finalised_episode(&data, "20260310T120500.000Z-00000002", 300);
    std::fs::create_dir_all(data.join("20260310T121000.000Z-00000003")).unwrap();
    std::fs::create_dir_all(data.join("notes")).unwrap();

    let (status, body) = call(state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "20260310T120500.000Z-00000002",
            "20260310T120000.000Z-00000001"
        ]
    );
    assert_eq!(body[0]["streams"][0]["written"], 2000);

    let (_, body) = call(state(&folder), "GET", "/api/episodes?limit=1", None).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

/// The IDs `/api/episodes` lists.
async fn listed(state: ApiState) -> Vec<String> {
    let (_, body) = call(state, "GET", "/api/episodes", None).await;
    body.as_array()
        .unwrap()
        .iter()
        .map(|episode| episode["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn an_episode_capture_announces_finalised_is_listed_at_once_and_one_added_meanwhile_after_a_restart()
 {
    let folder = scratch("indexed");
    let data = folder.join("data");
    let serving = state(&folder);
    let running = Arc::new(AtomicBool::new(true));
    let indexing = thread::spawn({
        let (store, data, running) = (serving.store.clone(), data.clone(), Arc::clone(&running));
        let announced = serving.announcements.subscribe();
        move || index::keep_up_to_date(&store, &data, announced, &running)
    });

    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    serving
        .announcements
        .announce(kapto_capture::control::Announcement::EpisodeEnded {
            episode: id.into(),
            finalised: true,
        });
    let mut tries = 0;
    while listed(serving.clone()).await.is_empty() && tries < 100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tries += 1;
    }
    assert_eq!(listed(serving.clone()).await, [id]);

    // Copied in without capture announcing it: listed once serve starts
    // again and rebuilds the index.
    let copied = "20260310T120500.000Z-00000002";
    finalised_episode(&data, copied, 300);
    assert_eq!(listed(serving.clone()).await, [id]);
    let restarted = state(&folder);
    assert_eq!(listed(restarted.clone()).await, [copied, id]);
    // Its events are queued to be read; those of the one announced already
    // were, and aren't queued twice.
    let events: Vec<String> = restarted
        .store
        .reader()
        .unwrap()
        .jobs(10)
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == "events")
        .map(|job| job.episode)
        .collect();
    assert_eq!(events, [copied, id]);

    running.store(false, Ordering::Relaxed);
    indexing.join().unwrap();
}

#[tokio::test]
async fn an_episode_capture_announces_recovered_is_listed_and_queued_to_be_checked() {
    let folder = scratch("recovered-listed");
    let data = folder.join("data");
    let serving = state(&folder);
    let running = Arc::new(AtomicBool::new(true));
    let indexing = thread::spawn({
        let (store, data, running) = (serving.store.clone(), data.clone(), Arc::clone(&running));
        let announced = serving.announcements.subscribe();
        move || index::keep_up_to_date(&store, &data, announced, &running)
    });

    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    serving
        .announcements
        .announce(kapto_capture::control::Announcement::EpisodeRecovered { episode: id.into() });
    let mut tries = 0;
    while listed(serving.clone()).await.is_empty() && tries < 100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tries += 1;
    }
    assert_eq!(listed(serving.clone()).await, [id]);
    let jobs = serving.store.reader().unwrap().jobs(10).unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (job.kind.as_str(), job.episode.as_str()))
            .collect::<Vec<_>>(),
        [("qa", id), ("events", id), ("verify", id)]
    );

    running.store(false, Ordering::Relaxed);
    indexing.join().unwrap();
}

#[tokio::test]
async fn an_episode_stopped_through_the_api_is_listed_as_soon_as_stop_answers() {
    let folder = scratch("stopped-listed");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    let serving = state(&folder);
    // What capture finalised just before answering.
    let id = "20260310T120000.000Z-1a2b3c4d";
    finalised_episode(&serving.data_dir, id, 0);
    let body = Some(("application/json", "{}"));
    let (status, _) = call(serving.clone(), "POST", "/api/episodes/stop", body).await;
    assert_eq!(status, StatusCode::OK);
    // Its files are queued to be checked, then its events read and its
    // quality checked: listed newest first.
    let jobs = serving.store.reader().unwrap().jobs(10).unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (job.kind.as_str(), job.episode.as_str()))
            .collect::<Vec<_>>(),
        [("qa", id), ("events", id), ("verify", id)]
    );
    assert_eq!(listed(serving).await, [id]);
}

#[tokio::test]
async fn an_unreadable_episode_index_is_an_error_with_its_cause() {
    let folder = scratch("no-index");
    let state = state(&folder);
    std::fs::remove_file(folder.join("data/kapto.db")).unwrap();
    let (status, body) = call(state, "GET", "/api/episodes", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "store.open_failed");
}

#[tokio::test]
async fn an_episode_in_full_has_its_streams_files_buffers_and_durability() {
    let folder = scratch("episode");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);

    let (status, body) = call(state(&folder), "GET", &format!("/api/episodes/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
    assert_eq!(body["folder"], data.join(id).display().to_string());
    let started = body["started_utc_ns"].as_i64().unwrap();
    assert_eq!(
        body["ended_utc_ns"].as_i64().unwrap() - started,
        1_000_000_000
    );
    assert_eq!(
        (
            body["commanded_start_utc_ns"].as_i64().unwrap() - started,
            body["commanded_stop_utc_ns"].as_i64().unwrap() - started
        ),
        (200_000_000, 800_000_000)
    );
    let stream = &body["streams"][0];
    assert_eq!(stream["stream"], "arm_joints");
    assert_eq!(stream["loss_detection"], "sequence");
    assert_eq!(stream["written"], 2000);
    assert_eq!(stream["disconnects"], Value::Null);
    let files: Vec<&str> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert!(
        files.contains(&"recording.mcap") && files.contains(&"episode.json"),
        "{files:?}"
    );
    assert_eq!(body["buffers"]["peak_batches"], 1);
    assert_eq!(body["durability"]["syncs"], 2);
    assert_eq!(body["clock"]["steps"], 0);
}

#[tokio::test]
async fn an_episode_that_isnt_there_or_isnt_an_id_is_refused_with_its_cause() {
    let folder = scratch("episode-missing");
    let data = folder.join("data");
    // Being recorded, so not finalised.
    std::fs::create_dir_all(data.join("20260310T121000.000Z-00000003")).unwrap();
    for id in [
        "20260310T121000.000Z-00000003",
        "20260310T120000.000Z-0000000f",
    ] {
        let (status, body) =
            call(state(&folder), "GET", &format!("/api/episodes/{id}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body["code"], "api.episode_not_found");
        assert_eq!(body["remedy"], ErrorCode::ApiEpisodeNotFound.remedy());
    }
    // Never a path to anywhere else.
    let (status, body) = call(state(&folder), "GET", "/api/episodes/..%2F..%2Fetc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "api.invalid_request");
}

/// A run of `streams.complete` on episode `id`, as the worker reports it.
fn checked(id: &str, verdict: Verdict) -> QaRun {
    QaRun {
        statistics: std::collections::BTreeMap::new(),
        episode: id.to_owned(),
        params_version: Some("7".to_owned()),
        computed_utc_ns: 1_772_700_500_000_000_000,
        results: vec![QaResult {
            check: "streams.complete".to_owned(),
            version: 1,
            kind: CheckKind::Gate,
            verdict,
            message: "Kapto dropped 12 messages: arm_joints (12 of 2000 received)".to_owned(),
            values: std::collections::BTreeMap::from([("dropped_by_us".to_owned(), 12.0)]),
            ranges: vec![TimeRange {
                start_utc_ns: 1_772_700_410_000_000_000,
                end_utc_ns: 1_772_700_411_000_000_000,
            }],
        }],
        model: None,
    }
}

#[tokio::test]
async fn an_episodes_quality_results_are_shown_and_episodes_are_listed_and_filtered_by_them() {
    let folder = scratch("quality");
    let data = folder.join("data");
    let (accepted, rejected, unchecked) = (
        "20260310T120000.000Z-00000001",
        "20260310T120500.000Z-00000002",
        "20260310T121000.000Z-00000003",
    );
    for (n, id) in [accepted, rejected, unchecked].into_iter().enumerate() {
        finalised_episode(&data, id, i64::try_from(n).unwrap() * 300);
    }
    let serving = state(&folder);
    serving
        .store
        .record_qa(checked(accepted, Verdict::Passed))
        .unwrap();
    serving
        .store
        .record_qa(checked(rejected, Verdict::Failed))
        .unwrap();

    let (status, body) = call(
        serving.clone(),
        "GET",
        &format!("/api/episodes/{rejected}/qa"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "accepted": false,
            "params_version": "7",
            "computed_utc_ns": 1_772_700_500_000_000_000_i64,
            "checks": [{
                "check": "streams.complete",
                "version": 1,
                "kind": "gate",
                "verdict": "failed",
                "message": "Kapto dropped 12 messages: arm_joints (12 of 2000 received)",
                "values": {"dropped_by_us": 12.0},
                "ranges": [{
                    "start_utc_ns": 1_772_700_410_000_000_000_i64,
                    "end_utc_ns": 1_772_700_411_000_000_000_i64,
                }],
            }],
        })
    );
    // Not checked yet: nothing decided, nothing found.
    let (status, body) = call(
        serving.clone(),
        "GET",
        &format!("/api/episodes/{unchecked}/qa"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"accepted": null, "params_version": null, "computed_utc_ns": null, "checks": []})
    );

    let listed = |query: &'static str| {
        let serving = serving.clone();
        async move {
            let (_, body) = call(serving, "GET", query, None).await;
            body.as_array()
                .unwrap()
                .iter()
                .map(|episode| {
                    (
                        episode["id"].as_str().unwrap().to_owned(),
                        episode["accepted"].clone(),
                    )
                })
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        listed("/api/episodes").await,
        [
            (unchecked.to_owned(), Value::Null),
            (rejected.to_owned(), json!(false)),
            (accepted.to_owned(), json!(true)),
        ]
    );
    assert_eq!(
        listed("/api/episodes?accepted=false").await,
        [(rejected.to_owned(), json!(false))]
    );
    assert_eq!(
        listed("/api/episodes?accepted=true").await,
        [(accepted.to_owned(), json!(true))]
    );
}

#[tokio::test]
async fn an_episodes_statistics_are_summarised_and_none_until_it_is_checked() {
    use kapto_qa::episode_statistics::Statistic;
    use kapto_qa::statistics::Distribution;

    let folder = scratch("statistics");
    let data = folder.join("data");
    let (checked_id, unchecked) = (
        "20260310T120000.000Z-00000001",
        "20260310T120500.000Z-00000002",
    );
    finalised_episode(&data, checked_id, 0);
    finalised_episode(&data, unchecked, 300);
    let serving = state(&folder);
    let mut run = checked(checked_id, Verdict::Passed);
    let speeds = Distribution::of((1..=100).map(f64::from));
    run.statistics = std::collections::BTreeMap::from([
        (
            "episode.duration_s".to_owned(),
            serde_json::to_value(Statistic::Value(10.0)).unwrap(),
        ),
        (
            "joints.joint_1.speed".to_owned(),
            serde_json::to_value(Statistic::Distribution(speeds)).unwrap(),
        ),
        // A form this version doesn't read: left out.
        ("from.the_future".to_owned(), json!({"sketch": [1, 2]})),
    ]);
    serving.store.record_qa(run).unwrap();

    let (status, body) = call(
        serving.clone(),
        "GET",
        &format!("/api/episodes/{checked_id}/statistics"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["computed_utc_ns"], 1_772_700_500_000_000_000_i64);
    let statistics = body["statistics"].as_array().unwrap();
    assert_eq!(statistics.len(), 2);
    assert_eq!(statistics[0]["name"], "episode.duration_s");
    assert_eq!(statistics[0]["value"], 10.0);
    assert_eq!(statistics[0]["count"], Value::Null);
    let speed = &statistics[1];
    assert_eq!(speed["name"], "joints.joint_1.speed");
    assert_eq!(
        (
            &speed["count"],
            &speed["min"],
            &speed["max"],
            &speed["mean"]
        ),
        (&json!(100), &json!(1.0), &json!(100.0), &json!(50.5))
    );
    for (field, exact) in [("p50", 51.0), ("p90", 90.0), ("p99", 99.0)] {
        let estimate = speed[field].as_f64().unwrap();
        assert!(
            (estimate - exact).abs() <= exact * 0.01,
            "{field}: {estimate}"
        );
    }

    let (status, body) = call(
        serving.clone(),
        "GET",
        &format!("/api/episodes/{unchecked}/statistics"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"computed_utc_ns": null, "statistics": []}));
    let (status, body) = call(serving, "GET", "/api/episodes/notanid/statistics", None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::BAD_REQUEST, &json!("api.invalid_request"))
    );
}

#[tokio::test]
async fn the_quality_of_an_episode_that_isnt_there_or_isnt_an_id_is_refused_with_its_cause() {
    let folder = scratch("quality-missing");
    let (status, body) = call(
        state(&folder),
        "GET",
        "/api/episodes/20260310T120000.000Z-0000000f/qa",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "api.episode_not_found");
    let (status, body) = call(state(&folder), "GET", "/api/episodes/notanid/qa", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "api.invalid_request");
    let (status, _) = call(state(&folder), "GET", "/api/episodes?accepted=maybe", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A moment or one end of a stretch of `kind`, sent through the API.
fn marked(
    kind: &str,
    phase: kapto_store::Phase,
    id: Option<&str>,
    at_utc_ns: i64,
) -> kapto_store::Event {
    kapto_store::Event {
        kind: kind.to_owned(),
        at_utc_ns,
        until_utc_ns: None,
        phase,
        id: id.map(str::to_owned),
        source: "api".to_owned(),
        text: None,
        attributes: std::collections::BTreeMap::new(),
    }
}

#[tokio::test]
async fn an_episodes_events_are_shown_with_its_unended_stretches_and_episodes_found_by_kind() {
    use kapto_store::Phase;

    let folder = scratch("episode-events");
    let data = folder.join("data");
    let (marked_one, quiet, unread) = (
        "20260310T120000.000Z-00000001",
        "20260310T120100.000Z-00000002",
        "20260310T120200.000Z-00000003",
    );
    for (n, id) in [marked_one, quiet, unread].into_iter().enumerate() {
        finalised_episode(&data, id, i64::try_from(n).unwrap() * 60);
    }
    let api = state(&folder);
    // Not read yet: said so, and nothing.
    let (status, body) = call(
        api.clone(),
        "GET",
        &format!("/api/episodes/{unread}/events"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"read": false, "events": [], "unended": []}));

    api.store
        .record_events(
            marked_one,
            vec![
                marked("intervention", Phase::Begin, Some("i1"), 10),
                marked("note", Phase::Moment, None, 20),
                marked("intervention", Phase::End, Some("i1"), 30),
                marked("intervention", Phase::Begin, Some("i2"), 40),
            ],
        )
        .unwrap();
    api.store.record_events(quiet, Vec::new()).unwrap();
    let (_, body) = call(
        api.clone(),
        "GET",
        &format!("/api/episodes/{marked_one}/events"),
        None,
    )
    .await;
    assert_eq!(body["read"], true);
    let kinds: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["intervention", "note", "intervention", "intervention"]
    );
    // The second intervention was never ended.
    assert_eq!(body["unended"].as_array().unwrap().len(), 1);
    assert_eq!(body["unended"][0]["id"], "i2");
    assert_eq!(body["unended"][0]["phase"], "begin");

    // Found by kind; each listed with its events by kind, or null if not read.
    let (_, all) = call(api.clone(), "GET", "/api/episodes", None).await;
    let counts: Vec<(&str, &Value)> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|episode| (episode["id"].as_str().unwrap(), &episode["events"]))
        .collect();
    assert_eq!(
        counts,
        [
            (unread, &Value::Null),
            (quiet, &json!({})),
            (marked_one, &json!({"intervention": 3, "note": 1})),
        ]
    );
    let (status, found) = call(api.clone(), "GET", "/api/episodes?event=note", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(found[0]["id"], marked_one);
    let (_, none) = call(api.clone(), "GET", "/api/episodes?event=grasp", None).await;
    assert_eq!(none, json!([]));

    // An episode that isn't there, or an ID that isn't one, says why.
    let (status, body) = call(
        api.clone(),
        "GET",
        "/api/episodes/20260310T120000.000Z-0000000f/events",
        None,
    )
    .await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("api.episode_not_found"))
    );
    let (status, body) = call(api, "GET", "/api/episodes/notanid/events", None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::BAD_REQUEST, &json!("api.invalid_request"))
    );
}

#[tokio::test]
async fn an_episode_deleted_by_hand_waits_in_the_bin_as_it_was_until_put_back() {
    let folder = scratch("episode-bin");
    let data = folder.join("data");
    let (kept, binned) = (
        "20260310T120000.000Z-00000001",
        "20260310T120100.000Z-00000002",
    );
    finalised_episode(&data, kept, 0);
    finalised_episode(&data, binned, 60);
    let files = |folder: &std::path::Path| {
        let mut names: Vec<_> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = files(&data.join(binned));

    let (status, body) = call(
        state(&folder),
        "DELETE",
        &format!("/api/episodes/{binned}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["episode"], binned);
    // Moved as it is into the bin, recorded as deleted from the API.
    assert!(!data.join(binned).exists());
    assert_eq!(files(&data.join(".bin").join(binned)), before);
    let reader = state(&folder).store.reader().unwrap();
    assert_eq!(reader.binned(binned).unwrap().unwrap().from, "API");

    // Listed only in the bin, and still shown in full from there.
    let ids = |list: &Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|episode| episode["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, list) = call(state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(ids(&list), [kept]);
    let (_, bin) = call(state(&folder), "GET", "/api/episodes?bin=true", None).await;
    assert_eq!(ids(&bin), [binned]);
    assert_eq!(bin[0]["binned_utc_ns"], body["binned_utc_ns"]);
    let (status, detail) = call(
        state(&folder),
        "GET",
        &format!("/api/episodes/{binned}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["binned_utc_ns"], body["binned_utc_ns"]);

    // Deleted again: it is in the bin already.
    let (status, again) = call(
        state(&folder),
        "DELETE",
        &format!("/api/episodes/{binned}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(again["code"], "api.episode_in_bin");
    assert_eq!(again["remedy"], ErrorCode::ApiEpisodeInBin.remedy());

    // Put back as it was, listed again.
    let (status, _) = call(
        state(&folder),
        "POST",
        &format!("/api/episodes/{binned}/restore"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(files(&data.join(binned)), before);
    let (_, list) = call(state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(ids(&list), [binned, kept]);
    assert_eq!(list[0]["binned_utc_ns"], Value::Null);
    let (status, refused) = call(
        state(&folder),
        "POST",
        &format!("/api/episodes/{binned}/restore"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(refused["code"], "api.episode_not_in_bin");
}

#[tokio::test]
async fn only_a_finalised_episode_can_be_deleted_and_one_found_in_the_bin_at_start_is_recorded() {
    let folder = scratch("episode-bin-refused");
    let data = folder.join("data");
    let (status, missing) = call(
        state(&folder),
        "DELETE",
        "/api/episodes/20260310T120000.000Z-00000009",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["code"], "api.episode_not_found");
    let (status, invalid) = call(
        state(&folder),
        "DELETE",
        "/api/episodes/..%2Fkapto.db",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");

    // Moved into the bin, but a crash before it was recorded: recorded when
    // serve next starts, and listed in the bin.
    let lost = "20260310T120200.000Z-00000003";
    finalised_episode(&data, lost, 120);
    std::fs::create_dir_all(data.join(".bin")).unwrap();
    std::fs::rename(data.join(lost), data.join(".bin").join(lost)).unwrap();
    let state = state(&folder);
    kapto_server::episode_bin::reconcile(&data, &state.store, 42).unwrap();
    let recorded = state.store.reader().unwrap().binned(lost).unwrap().unwrap();
    assert_eq!(
        (recorded.from.as_str(), recorded.binned_utc_ns),
        (kapto_server::episode_bin::FOUND_AT_START, 42)
    );
    let (_, bin) = call(state, "GET", "/api/episodes?bin=true", None).await;
    assert_eq!(bin[0]["id"], lost);
}

#[tokio::test]
async fn emptying_the_bin_queues_every_episode_in_it_to_be_deleted_for_good() {
    let folder = scratch("episode-bin-empty");
    let data = folder.join("data");
    let (one, two) = (
        "20260310T120000.000Z-00000001",
        "20260310T120100.000Z-00000002",
    );
    finalised_episode(&data, one, 0);
    finalised_episode(&data, two, 60);
    call(
        state(&folder),
        "DELETE",
        &format!("/api/episodes/{one}"),
        None,
    )
    .await;
    // Its room counted apart, until the bin is emptied.
    let (_, storage) = call(state(&folder), "GET", "/api/storage", None).await;
    assert_eq!(storage["episodes"], 1);
    assert!(storage["bin_bytes"].as_u64().unwrap() > 0, "{storage}");
    let (status, emptied) = call(state(&folder), "POST", "/api/bin/empty", None).await;
    assert_eq!(status, StatusCode::OK, "{emptied}");
    assert_eq!(emptied["queued"], 1);
    let reader = state(&folder).store.reader().unwrap();
    let deleting = reader.job("delete", one).unwrap().unwrap();
    assert_eq!(deleting.state, kapto_store::JobState::Queued);
    assert_eq!(reader.job("delete", two).unwrap(), None);
}
