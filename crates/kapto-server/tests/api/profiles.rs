// SPDX-License-Identifier: Apache-2.0

//! The session profiles, the tasks and the kinds of event.

use axum::http::StatusCode;
use kapto_testkit::scratch;
use serde_json::{Value, json};

use crate::common::{call, saved, state};

#[tokio::test]
async fn profiles_are_listed_each_available_only_for_the_robot_package_kapto_runs() {
    let folder = scratch("profiles");
    saved(
        &folder,
        "profiles/lab_ana.toml",
        "package = \"arm7\"\noperator = \"Ana\"\nmode = \"teleoperated\"\n",
    );
    saved(&folder, "profiles/cobot_day.toml", "package = \"cobot\"\n");
    saved(
        &folder,
        "profiles/broken.toml",
        "package = \"arm7\"\noperator = \"na\"\n",
    );
    let (status, body) = call(state(&folder), "GET", "/api/profiles", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["package"], "arm7");
    let listed: Vec<(&str, bool)> = body["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|profile| {
            (
                profile["id"].as_str().unwrap(),
                profile["available"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(listed, [("cobot_day", false), ("lab_ana", true)]);
    assert_eq!(body["profiles"][1]["mode"], "teleoperated");
    assert_eq!(body["problems"].as_array().unwrap().len(), 1);
    assert_eq!(body["problems"][0]["code"], "config.invalid_value");

    // Saved while serve runs: listed at the next request.
    saved(&folder, "profiles/lab_ben.toml", "package = \"arm7\"\n");
    let (_, body) = call(state(&folder), "GET", "/api/profiles", None).await;
    assert_eq!(body["profiles"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn tasks_are_listed_with_what_is_wrong_with_the_others() {
    let folder = scratch("tasks");
    let (_, body) = call(state(&folder), "GET", "/api/tasks", None).await;
    assert_eq!(body, json!({"tasks": [], "problems": []}));
    saved(
        &folder,
        "tasks/pick_and_place.toml",
        "name = \"Pick and place\"\nsubtasks = [\"reach\", \"grasp\"]\n",
    );
    saved(&folder, "tasks/broken.toml", "steps = [\"reach\"]\n");
    let (status, body) = call(state(&folder), "GET", "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tasks"][0]["id"], "pick_and_place");
    assert_eq!(body["tasks"][0]["subtasks"], json!(["reach", "grasp"]));
    assert_eq!(body["problems"][0]["code"], "task.unknown_key");
}

#[tokio::test]
async fn the_event_kinds_are_the_vocabulary_files_or_the_defaults_then_each_tasks_subtasks() {
    let folder = scratch("event-kinds");
    let kinds = |body: &Value| -> Vec<String> {
        body["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| {
                format!(
                    "{} {}",
                    kind["kind"].as_str().unwrap(),
                    kind["from"].as_str().unwrap()
                )
            })
            .collect()
    };
    let (status, body) = call(state(&folder), "GET", "/api/event-kinds", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        kinds(&body),
        ["note default", "problem default", "intervention default"]
    );
    saved(
        &folder,
        "events.toml",
        "[kinds.grasp]\nname = \"Grasp\"\ncolor = \"#4c9be8\"\nstretch = true\n",
    );
    saved(&folder, "tasks/pick.toml", "subtasks = [\"Reach\"]\n");
    let (_, body) = call(state(&folder), "GET", "/api/event-kinds", None).await;
    assert_eq!(kinds(&body), ["grasp file", "subtask.reach task"]);
    assert_eq!(
        body["kinds"][0],
        json!({"kind": "grasp", "name": "Grasp", "color": "#4c9be8", "stretch": true, "description": null, "from": "file", "task": null})
    );
    assert_eq!(body["kinds"][1]["task"], "pick");
    // A file with problems: the defaults stand in, and its problems say why.
    saved(&folder, "events.toml", "[kinds.grasp]\nshade = \"blue\"\n");
    let (_, body) = call(state(&folder), "GET", "/api/event-kinds", None).await;
    assert_eq!(kinds(&body)[0], "note default");
    assert_eq!(body["problems"][0]["code"], "events.unknown_key");
}
