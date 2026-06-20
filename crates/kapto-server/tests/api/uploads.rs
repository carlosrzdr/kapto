// SPDX-License-Identifier: Apache-2.0

//! Uploads by destination and by episode, retrying them and testing a
//! destination.

use std::path::Path;

use axum::http::StatusCode;
use kapto_server::api::ApiState;
use kapto_store::{
    Deletion, DeletionReason, JobFailure, JobState, RemoteCopy, UploadCounts, UploadedObject,
};
use kapto_testkit::scratch;
use serde_json::{Value, json};

use crate::common::{call, finalised_episode, state};

/// The API's state with destinations `nas`, a folder in `folder`, and
/// `cloud`, whose credentials are named but not given.
fn uploading_state(folder: &Path) -> ApiState {
    let file = kapto_config::ConfigFile {
        path: folder.join("kapto.toml"),
        contents: format!(
            "[[upload.destination]]\nname = \"nas\"\nurl = \"file://{}\"\nprefix = \"fleet-a\"\n\
             [[upload.destination]]\nname = \"cloud\"\nurl = \"s3://cloud-bucket\"\n\
             credentials = {{ env = \"KAPTO_TEST_NEVER_SET\" }}\n",
            folder.join("nas").display()
        ),
    };
    let inputs = kapto_config::Inputs {
        file: Some(&file),
        env: &|_| None,
        cli: &[],
    };
    ApiState {
        destinations: kapto_config::DeploymentConfig::resolve(&inputs)
            .unwrap()
            .upload_destinations,
        ..state(folder)
    }
}

#[tokio::test]
async fn uploads_are_shown_by_destination_and_by_episode_with_where_each_object_went() {
    let folder = scratch("uploads");
    let data = folder.join("data");
    let (going, failed) = (
        "20260310T120000.000Z-00000001",
        "20260310T120100.000Z-00000002",
    );
    finalised_episode(&data, going, 0);
    finalised_episode(&data, failed, 60);
    let state = uploading_state(&folder);
    let store = state.store.clone();
    // `going` is up at `nas` and halfway at `cloud`; `failed` failed at `nas`.
    store.start_upload(going, "nas", "fleet-a/one").unwrap();
    let object = UploadedObject {
        path: "recording.mcap".into(),
        key: "fleet-a/one/recording.mcap".into(),
        bytes: 15,
        sha256: "a".repeat(64),
        e_tag: None,
    };
    store.uploaded(going, "nas", vec![object]).unwrap();
    store.start_upload(going, "cloud", "fleet-b/one").unwrap();
    let counts = UploadCounts {
        bytes_sent: 8,
        bytes_total: 16,
        throttled: 3,
    };
    store.upload_progress(going, "cloud", "{}", counts).unwrap();
    store.start_upload(failed, "nas", "fleet-a/two").unwrap();
    let failure = JobFailure {
        code: "upload.unreachable".into(),
        message: "connection reset".into(),
    };
    store.upload_failed(failed, "nas", failure).unwrap();

    let (status, overview) = call(state, "GET", "/api/uploads", None).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    let nas = &overview["destinations"][0];
    assert_eq!(
        (
            nas["name"].clone(),
            nas["url"].clone(),
            nas["uploaded"].clone(),
            nas["failed"].clone()
        ),
        (
            json!("nas"),
            json!(format!("file://{}", folder.join("nas").display())),
            json!(1),
            json!(1)
        )
    );
    let cloud = &overview["destinations"][1];
    assert_eq!(
        (
            cloud["uploading"].clone(),
            cloud["bytes_sent"].clone(),
            cloud["bytes_total"].clone()
        ),
        (json!(1), json!(8), json!(16))
    );
    assert_eq!(cloud["throttled"], 3);
    // Only those not done, each with why it failed if it did.
    let listed = overview["uploads"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{overview}");
    let failed_one = listed
        .iter()
        .find(|upload| upload["state"] == "failed")
        .unwrap();
    assert_eq!(failed_one["failure_code"], "upload.unreachable");

    let (status, uploads) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{going}/uploads"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{uploads}");
    assert_eq!(uploads["stored_here"], true);
    assert_eq!(uploads["deletion"], Value::Null);
    let at_nas = uploads["uploads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|upload| upload["destination"] == "nas")
        .unwrap();
    assert_eq!(at_nas["objects"][0]["key"], "fleet-a/one/recording.mcap");
}

#[tokio::test]
async fn an_episode_deleted_here_is_still_listed_and_shown_with_where_its_copies_and_its_results_are()
 {
    let folder = scratch("uploads-deleted");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    let state = uploading_state(&folder);
    let store = state.store.clone();
    store.start_upload(id, "nas", "fleet-a/one").unwrap();
    let object = UploadedObject {
        path: "recording.mcap".into(),
        key: "fleet-a/one/recording.mcap".into(),
        bytes: 15,
        sha256: "a".repeat(64),
        e_tag: None,
    };
    store.uploaded(id, "nas", vec![object]).unwrap();
    let summary = std::fs::read_to_string(data.join(id).join("episode.json")).unwrap();
    store
        .record_deletion(Deletion {
            episode: id.to_owned(),
            deleted_utc_ns: 0,
            reason: DeletionReason::AfterUpload,
            deleted_by: "worker".into(),
            copies: vec![RemoteCopy {
                destination: "nas".into(),
                key: "fleet-a/one".into(),
            }],
            summary,
        })
        .unwrap();
    std::fs::remove_dir_all(data.join(id)).unwrap();
    drop(state);

    // Listed after the index is rebuilt, as stored only remotely.
    let (_, list) = call(uploading_state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(
        (list[0]["id"].clone(), list[0]["stored_here"].clone()),
        (json!(id), json!(false))
    );
    // Shown from the summary kept, its files those uploaded.
    let (status, detail) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["stored_here"], false);
    assert_eq!(
        detail["files"],
        json!([{"path": "recording.mcap", "bytes": 15}])
    );
    let (_, uploads) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{id}/uploads"),
        None,
    )
    .await;
    assert_eq!(uploads["stored_here"], false);
    assert_eq!(
        (
            uploads["deletion"]["reason"].clone(),
            uploads["deletion"]["destinations"].clone()
        ),
        (json!("after_upload"), json!(["nas"]))
    );
    // What was worked out from it is still shown, kept in kapto.db; its
    // replay, made from its recording, can't be.
    for path in ["qa", "statistics", "events"] {
        let (status, answer) = call(
            uploading_state(&folder),
            "GET",
            &format!("/api/episodes/{id}/{path}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {answer}");
    }
    let (status, _) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{id}/replay"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An episode neither here nor deleted here isn't found.
    let unknown = "20260310T130000.000Z-00000009";
    let (status, error) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{unknown}/uploads"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "api.episode_not_found");
}

#[tokio::test]
async fn failed_uploads_are_tried_again_when_asked_and_asking_with_none_failed_is_refused() {
    let folder = scratch("uploads-retry");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    let state = uploading_state(&folder);
    let store = state.store.clone();
    store.start_upload(id, "nas", "fleet-a/one").unwrap();
    let uri = format!("/api/episodes/{id}/uploads/retry");
    let (status, error) = call(state, "POST", &uri, None).await;
    assert_eq!(
        (status, error["code"].clone()),
        (StatusCode::CONFLICT, json!("upload.nothing_to_retry"))
    );

    let failure = JobFailure {
        code: "upload.unreachable".into(),
        message: "connection reset".into(),
    };
    store.upload_failed(id, "nas", failure).unwrap();
    let (status, retried) = call(uploading_state(&folder), "POST", &uri, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{retried}");
    assert_eq!(retried, json!({"destinations": ["nas"]}));
    let job = store.reader().unwrap().job("upload", id).unwrap().unwrap();
    assert_eq!(job.state, JobState::Queued);

    let (status, _) = call(
        uploading_state(&folder),
        "POST",
        "/api/episodes/..%2Fx/uploads/retry",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_destination_is_tested_as_an_upload_uses_it_and_one_that_cant_be_says_why() {
    let folder = scratch("uploads-test");
    let test = |name: &str| format!("/api/uploads/destinations/{name}/test");
    let (status, tested) = call(uploading_state(&folder), "POST", &test("nas"), None).await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(tested["removed"], true);
    assert!(
        tested["key"]
            .as_str()
            .unwrap()
            .starts_with("fleet-a/.kapto-probe-"),
        "{tested}"
    );
    // Nothing left behind.
    assert_eq!(
        std::fs::read_dir(folder.join("nas").join("fleet-a"))
            .unwrap()
            .count(),
        0
    );

    let (status, error) = call(uploading_state(&folder), "POST", &test("cloud"), None).await;
    assert_eq!(
        (status, error["code"].clone()),
        (StatusCode::CONFLICT, json!("upload.credentials_missing")),
        "{error}"
    );
    let (status, error) = call(uploading_state(&folder), "POST", &test("elsewhere"), None).await;
    assert_eq!(
        (status, error["code"].clone()),
        (StatusCode::NOT_FOUND, json!("upload.no_destination"))
    );
}
