// SPDX-License-Identifier: Apache-2.0

//! Replays: asked for, made once, and sent as stretches, robots, meshes
//! and openings.

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use kapto_core::{ErrorCode, KaptoError};
use kapto_server::api::{ApiState, router};
use kapto_server::jobs::REPLAY;
use kapto_server::replays::view_file;
use kapto_server::scene::RobotScene;
use kapto_store::JobFailure;
use kapto_store::replay_view::{VIEW_MAGIC, VIEW_VERSION};
use kapto_testkit::scratch;
use kapto_viz::stream::{OpeningStream, OpeningValue};
use kapto_viz::view::{ViewContents, ViewWriter};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::common::{call, finalised_episode, get_bytes, state};

/// Asks for episode `id`'s replay through the router.
async fn ask_replay(state: ApiState, id: &str) -> (StatusCode, Value) {
    let uri = format!("/api/episodes/{id}/replay");
    call(state, "POST", &uri, Some(("application/json", "{}"))).await
}

/// Where episode `id`'s replay is, through the router.
async fn replay_status(state: ApiState, id: &str) -> Value {
    let (status, body) = call(state, "GET", &format!("/api/episodes/{id}/replay"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Does the replay job the API queued, as the worker would: makes its view
/// in the cache and reports it done with `warnings`. Jobs queued before it,
/// such as reading the episode's events, are done first.
fn made(state: &ApiState, id: &str, warnings: Vec<JobFailure>) -> std::path::PathBuf {
    let job = loop {
        let job = state.store.claim().unwrap().unwrap();
        if job.kind == REPLAY {
            break job;
        }
        state.store.finish(job.id, None, Vec::new()).unwrap();
    };
    assert_eq!((job.kind.as_str(), job.episode.as_str()), (REPLAY, id));
    view(state, id, VIEW_VERSION);
    state.store.finish(job.id, None, warnings).unwrap();
    view_file(&state.data_dir, id).unwrap()
}

/// Writes the start of episode `id`'s replay view, of version `version`.
fn view(state: &ApiState, id: &str, version: u16) {
    let mut start = VIEW_MAGIC.to_vec();
    start.extend(version.to_le_bytes());
    let path = view_file(&state.data_dir, id).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, start).unwrap();
}

#[tokio::test]
async fn a_replay_whose_view_is_of_another_version_is_made_again_when_asked_for() {
    let folder = scratch("replay-other-version");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    ask_replay(api.clone(), id).await;
    made(&api, id, Vec::new());
    assert_eq!(replay_status(api.clone(), id).await["state"], "ready");

    view(&api, id, VIEW_VERSION + 1);
    assert_eq!(replay_status(api.clone(), id).await["state"], "not_made");
    let (status, body) = ask_replay(api.clone(), id).await;
    assert_eq!(
        (status, &body["state"]),
        (StatusCode::ACCEPTED, &json!("queued"))
    );
}

#[tokio::test]
async fn a_replay_asked_for_twice_is_made_once_saying_what_it_lacks() {
    let folder = scratch("replay-asked");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    assert_eq!(replay_status(api.clone(), id).await["state"], "not_made");

    let (status, body) = ask_replay(api.clone(), id).await;
    assert_eq!(
        (status, &body["state"]),
        (StatusCode::ACCEPTED, &json!("queued"))
    );
    let (status, _) = ask_replay(api.clone(), id).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // Queued once.
    let replays = api.store.reader().unwrap().jobs(10).unwrap();
    assert_eq!(replays.iter().filter(|job| job.kind == REPLAY).count(), 1);

    let without_robot = JobFailure::from(&KaptoError::new(ErrorCode::ViewerReplayWithoutRobot));
    made(&api, id, vec![without_robot]);
    let ready = replay_status(api.clone(), id).await;
    assert_eq!(ready["state"], "ready");
    // The view's size: its magic and version.
    assert_eq!(ready["bytes"], 6);
    assert_eq!(ready["lacks"][0]["code"], "viewer.replay_without_robot");
    assert_eq!(
        ready["lacks"][0]["remedy"],
        ErrorCode::ViewerReplayWithoutRobot.remedy()
    );
    // Made, asking again queues nothing.
    let (status, body) = ask_replay(api.clone(), id).await;
    assert_eq!((status, &body["state"]), (StatusCode::OK, &json!("ready")));
    assert_eq!(api.store.claim().unwrap(), None);
}

#[tokio::test]
async fn a_replay_that_failed_or_left_the_cache_is_made_again_when_asked_for() {
    let folder = scratch("replay-again");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    ask_replay(api.clone(), id).await;
    let path = made(&api, id, Vec::new());
    // Removed from the cache: not made, until asked for again.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(replay_status(api.clone(), id).await["state"], "not_made");
    let (status, body) = ask_replay(api.clone(), id).await;
    assert_eq!(
        (status, &body["state"]),
        (StatusCode::ACCEPTED, &json!("queued"))
    );

    // Failing, it says why; asked for again, it is tried again.
    let job = api.store.claim().unwrap().unwrap();
    assert_eq!(replay_status(api.clone(), id).await["state"], "making");
    let low = KaptoError::new(ErrorCode::ViewerReplayDiskLow).detail("9 MiB free");
    api.store
        .finish(job.id, Some(JobFailure::from(&low)), Vec::new())
        .unwrap();
    let failed = replay_status(api.clone(), id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["failure"]["code"], "viewer.replay_disk_low");
    assert_eq!(failed["failure"]["message"], low.message());
    assert_eq!(
        failed["failure"]["remedy"],
        ErrorCode::ViewerReplayDiskLow.remedy()
    );
    let (_, body) = ask_replay(api.clone(), id).await;
    assert_eq!(
        (&body["state"], &body["failure"]),
        (&json!("queued"), &Value::Null)
    );
}

#[tokio::test]
async fn a_replay_not_made_or_of_an_episode_that_isnt_there_is_refused_with_its_cause() {
    let folder = scratch("replay-refused");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let (status, body) = call(
        state(&folder),
        "GET",
        &format!("/api/episodes/{id}/replay/opening"),
        None,
    )
    .await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("viewer.replay_not_ready"))
    );
    assert_eq!(body["remedy"], ErrorCode::ViewerReplayNotReady.remedy());
    let other = "20260310T120000.000Z-0000000f";
    for uri in [
        format!("/api/episodes/{other}/replay"),
        format!("/api/episodes/{other}/replay/opening"),
    ] {
        let (status, body) = call(state(&folder), "GET", &uri, None).await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::NOT_FOUND, &json!("api.episode_not_found")),
            "{uri}"
        );
    }
    let (status, body) = ask_replay(state(&folder), other).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("api.episode_not_found"))
    );
    let (status, body) = ask_replay(state(&folder), "..%2Fkapto.db").await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::BAD_REQUEST, &json!("api.invalid_request"))
    );
}

/// Writes episode `id`'s replay view: a message every millisecond of its
/// one second, from `START`, its value the millisecond.
fn viewed(state: &ApiState, id: &str) {
    viewed_with(state, id, ViewContents::default());
}

/// The same, with the robot and meshes in `contents`.
fn viewed_with(state: &ApiState, id: &str, contents: ViewContents) {
    let path = view_file(&state.data_dir, id).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = ViewWriter::create(&path, 16_666_666).unwrap();
    for ms in 0..1000_u32 {
        let time = START + i64::from(ms) * 1_000_000;
        writer.message(0, time, &[f64::from(ms)]).unwrap();
    }
    let value = |name: &str, plotted| OpeningValue {
        name: name.to_owned(),
        plotted,
    };
    let streams = vec![OpeningStream {
        name: "arm_joints".to_owned(),
        values: vec![value("sequence", false), value("joint_positions[0]", true)],
    }];
    writer
        .finish(ViewContents {
            links: vec!["base_link".to_owned(), "link_1".to_owned()],
            streams,
            ..contents
        })
        .unwrap();
}

/// When the episodes [`finalised_episode`] makes at second 0 begin.
const START: i64 = 1_772_700_400_000_000_000;

/// A stretch of episode `id`'s replay through the router: its status and
/// body.
async fn window(state: ApiState, id: &str, query: &str) -> (StatusCode, Vec<u8>) {
    let request = HttpRequest::get(format!("/api/episodes/{id}/replay/window?{query}"))
        .body(Body::empty())
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, body.to_vec())
}

fn code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn a_stretch_of_a_replay_is_its_view_merged_into_the_points_asked_for() {
    let folder = scratch("replay-window");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    viewed(&api, id);
    let (status, body) = window(
        api,
        id,
        &format!("from={START}&to={}&points=10", START + 999_000_000),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..4], b"KVHI");
    // The span is the episode's, whatever was asked for.
    let at = |offset: usize| i64::from_le_bytes(body[offset..offset + 8].try_into().unwrap());
    assert_eq!((at(12), at(20)), (START, START + 999_000_000));
    // No poses, one stream in ten parts.
    let count = |offset: usize| u32::from_le_bytes(body[offset..offset + 4].try_into().unwrap());
    assert_eq!((count(28), count(32), count(36)), (0, 0, 1));
    assert_eq!(count(44), 1, "one value");
    assert_eq!(count(48), 10, "ten parts");
}

#[tokio::test]
async fn a_stretch_of_a_replay_not_made_unreadable_or_outside_its_episode_is_refused() {
    let folder = scratch("replay-window-refused");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    let whole = format!("from={START}&to={}&points=10", START + 999_000_000);
    let (status, body) = window(api.clone(), id, &whole).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::NOT_FOUND, "viewer.replay_not_ready".to_owned())
    );

    // Of this version, but cut off after its start.
    view(&api, id, VIEW_VERSION);
    let (status, body) = window(api.clone(), id, &whole).await;
    assert_eq!(
        (status, code(&body)),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "viewer.replay_unreadable".to_owned()
        )
    );

    viewed(&api, id);
    for query in [
        format!("from={START}&to={}&points=10", START - 1),
        format!(
            "from={}&to={}&points=10",
            START + 2_000_000_000,
            START + 3_000_000_000
        ),
        format!("from={START}&to={START}&points=0"),
        format!("from={START}&to={START}&points=10001"),
        format!("from={START}&points=10"),
        "from=soon&to=later&points=10".to_owned(),
    ] {
        let (status, body) = window(api.clone(), id, &query).await;
        assert_eq!(
            (status, code(&body)),
            (StatusCode::BAD_REQUEST, "api.invalid_request".to_owned()),
            "{query}"
        );
    }
    let (status, body) = window(api, "20260310T120000.000Z-0000000f", &whole).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::NOT_FOUND, "api.episode_not_found".to_owned())
    );
}

#[tokio::test]
async fn a_replays_robot_and_meshes_are_the_ones_its_view_carries() {
    let folder = scratch("replay-scene");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    let scene_uri = format!("/api/episodes/{id}/replay/scene");
    let mesh_uri = |n: &str| format!("/api/episodes/{id}/replay/meshes/{n}");
    let (status, body) = call(api.clone(), "GET", &scene_uri, None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("viewer.replay_not_ready"))
    );

    // Recorded without a robot.
    viewed(&api, id);
    let (status, body) = call(api.clone(), "GET", &scene_uri, None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("viewer.replay_without_robot"))
    );

    // Recorded with a robot whose second mesh couldn't be read.
    let scene = RobotScene {
        name: "arm7".to_owned(),
        root: "base_link".to_owned(),
        meshes: 2,
        ..RobotScene::default()
    };
    viewed_with(
        &api,
        id,
        ViewContents {
            scene: Some(scene),
            meshes: vec![b"KMSH a mesh".to_vec(), Vec::new()],
            ..ViewContents::default()
        },
    );
    let (status, body) = call(api.clone(), "GET", &scene_uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"name": "arm7", "root": "base_link", "links": [], "meshes": 2})
    );
    let (status, kind, bytes) = get_bytes(api.clone(), &mesh_uri("0")).await;
    assert_eq!(
        (status, kind.as_str(), bytes.as_slice()),
        (
            StatusCode::OK,
            "application/octet-stream",
            &b"KMSH a mesh"[..]
        )
    );
    for (n, expected, cause) in [
        ("1", StatusCode::CONFLICT, "robot.mesh_unreadable"),
        ("2", StatusCode::NOT_FOUND, "viewer.mesh_not_found"),
        ("first", StatusCode::BAD_REQUEST, "api.invalid_request"),
    ] {
        let (status, body) = call(api.clone(), "GET", &mesh_uri(n), None).await;
        assert_eq!((status, &body["code"]), (expected, &json!(cause)), "{n}");
    }
}

#[tokio::test]
async fn a_replays_opening_names_its_links_and_values_and_gives_its_span_once_made() {
    let folder = scratch("replay-opening");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    let uri = format!("/api/episodes/{id}/replay/opening");
    let (status, body) = call(api.clone(), "GET", &uri, None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("viewer.replay_not_ready"))
    );

    viewed(&api, id);
    let (status, body) = call(api.clone(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "start_utc_ns": START,
            "end_utc_ns": START + 999_000_000,
            "links": ["base_link", "link_1"],
            "streams": [{
                "name": "arm_joints",
                "values": [
                    {"name": "sequence", "plotted": false},
                    {"name": "joint_positions[0]", "plotted": true}
                ]
            }],
            // A recording that can't be read for its cameras has none.
            "cameras": [],
            // A view made without a layout: the page arranges it as it
            // arranges the live view.
            "layout": null,
            "roles": []
        })
    );

    view(&api, id, VIEW_VERSION);
    let (status, body) = call(api, "GET", &uri, None).await;
    assert_eq!(
        (status, &body["code"]),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!("viewer.replay_unreadable")
        )
    );
}
