// SPDX-License-Identifier: Apache-2.0

//! `kapto serve`: the capture process and the viewer bridge started, kept
//! running and stopped by one command.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::root;
use common::serve::{Serve, streams};
use kapto_capture::control::{Request, Response};
use kapto_store::db::DATABASE_FILE;
use kapto_store::{Episode, JobState, Writer};
use kapto_testkit::scratch;

fn signal(signal: &str, pid: u32) {
    assert!(
        Command::new("kill")
            .args([signal, &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn serve_runs_capture_the_bridge_and_the_worker_and_stops_them_on_sigterm() {
    let folder = scratch("lifecycle");
    let mut serve = Serve::start(&folder, &root().join("packages/arm7/mapping.toml"));
    serve.wait_for("Kapto is running: ", 1);
    let (capture, bridge, worker) = (
        serve.child_pid("capture", 1),
        serve.child_pid("viz-bridge", 1),
        serve.child_pid("worker", 1),
    );

    let mut control = serve.control();
    assert!(matches!(
        control
            .ask(&Request::Start {
                duration_s: None,
                profile: None,
                task: None,
                waive: Vec::new(),
            })
            .unwrap(),
        Response::Started { .. }
    ));
    let Response::Stopped { episode, .. } = control.ask(&Request::Stop).unwrap() else {
        panic!("not stopped");
    };
    // Once finalised, the worker checks it against its manifest.
    serve.wait_for(&format!("[worker] verify {episode}: done"), 1);
    // Its streams aren't in a package, so it carries the mapping serve was
    // given, for the checks to know the joints.
    let folder_of_episode = folder.join("data").join(&episode);
    let recording = folder_of_episode.join(Episode::read(&folder_of_episode).unwrap().recording);
    let bytes = std::fs::read(recording).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    assert!(
        summary
            .attachment_indexes
            .iter()
            .any(|index| index.name == "mapping.toml")
    );

    signal("-TERM", serve.pid());
    let status = serve.child.take().unwrap().wait().unwrap();
    assert!(status.success());
    for child in ["capture", "viz-bridge", "worker"] {
        serve.wait_for(&format!("[serve] {child} stopped"), 1);
    }
    assert!(!alive(capture) && !alive(bridge) && !alive(worker));
}

#[test]
fn the_api_reports_status_and_starts_and_stops_episodes_while_serve_runs() {
    let folder = scratch("api");
    let serve = Serve::start(&folder, &root().join("packages/arm7/mapping.toml"));
    serve.wait_for("Kapto is running: ", 1);

    let (code, body) = serve.http("GET", "/api/version", None);
    assert_eq!(code, 200);
    assert!(body.contains("\"api\":1"), "{body}");
    let (code, body) = serve.http("GET", "/api/status", None);
    assert_eq!(code, 200);
    assert!(body.contains("\"episode\":null"), "{body}");
    assert!(body.contains("\"stream\":\"arm_joints\""), "{body}");

    let (code, body) = serve.http("POST", "/api/episodes/start", Some("{}"));
    assert_eq!(code, 201, "{body}");
    let (code, body) = serve.http("POST", "/api/episodes/stop", Some("{}"));
    assert_eq!(code, 200, "{body}");
    let (code, body) = serve.http("POST", "/api/episodes/stop", Some("{}"));
    assert_eq!(code, 409, "{body}");
    assert!(body.contains("capture.not_recording"), "{body}");
    let (code, body) = serve.http("GET", "/api/episodes", None);
    assert_eq!(code, 200);
    assert_eq!(body.matches("\"id\":").count(), 1, "{body}");
}

#[test]
fn the_api_describes_the_robot_its_joints_and_streams_as_serve_loaded_them() {
    let folder = scratch("robot");
    let serve = Serve::start(&folder, &root().join("packages/arm7/mapping.toml"));
    serve.wait_for("Kapto is running: ", 1);

    let (code, body) = serve.http("GET", "/api/robot", None);
    assert_eq!(code, 200, "{body}");
    let robot: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(robot["name"], "arm7");
    assert_eq!(robot["links"], 11);
    let joints = robot["joints"].as_array().unwrap();
    assert_eq!(joints.len(), 10);
    let first = joints
        .iter()
        .find(|joint| joint["name"] == "joint_1")
        .unwrap();
    assert_eq!(first["kind"], "revolute");
    assert_eq!(first["position_from"], "arm_joints.joint_positions[0]");
    assert!(first["lower"].as_f64().unwrap() < first["upper"].as_f64().unwrap());
    let mirror = joints
        .iter()
        .find(|joint| joint["name"] == "gripper_mirror")
        .unwrap();
    assert_eq!(mirror["mimics"], "gripper");
    assert_eq!(mirror["position_from"], serde_json::Value::Null);

    let stream = &robot["streams"][0];
    assert_eq!(stream["name"], "arm_joints");
    assert!(
        stream["input"]
            .as_str()
            .unwrap()
            .starts_with("udp 127.0.0.1:"),
        "{stream}"
    );
    assert_eq!(stream["decoder"], "struct");
    assert_eq!(stream["sequence"], true);
    let positions = stream["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["name"] == "joint_positions")
        .unwrap();
    assert_eq!(positions["count"], 7);
    assert_eq!(positions["unit"], "rad");
    assert_eq!(robot["warnings"], serde_json::json!([]));

    let roles = robot["roles"].as_array().unwrap();
    assert_eq!(
        roles[0],
        serde_json::json!({ "role": "gripper.opening", "field": "arm_joints.gripper" })
    );
    assert!(roles.contains(&serde_json::json!({
        "role": "joints.joint_1.position",
        "field": "arm_joints.joint_positions[0]"
    })));

    // Not in a package, so arranged by the layout made from its files.
    let layout = &robot["layout"];
    assert_eq!(layout["source"], "made");
    assert_eq!(layout["columns"][0]["panels"][0]["kind"], "robot");
    let side = layout["columns"][1]["panels"].as_array().unwrap();
    assert_eq!(side[0]["fields"][0], "role:joints.joint_1.position");
    assert_eq!(side.last().unwrap()["fields"][0], "role:gripper.opening");
    assert!(
        side.iter()
            .all(|panel| panel["missing"] == serde_json::json!([]))
    );
}

/// The sample arm as a package of its own in `folder`, with `layout` as its
/// `layout.toml`, served: its files are what serve is given.
fn serve_package(folder: &std::path::Path, layout: &str) -> Serve {
    let arm7 = root().join("packages/arm7");
    let streams = streams(folder);
    for file in ["mapping.toml", "urdf/arm7.urdf"] {
        let to = folder.join(file);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(arm7.join(file), to).unwrap();
    }
    std::fs::write(
        folder.join("package.toml"),
        "id = \"arm7\"\nversion = \"0.1.0\"\nurdf = \"urdf/arm7.urdf\"\n",
    )
    .unwrap();
    std::fs::write(folder.join("layout.toml"), layout).unwrap();
    Serve::start_robot(
        folder,
        &folder.join("urdf/arm7.urdf"),
        &streams,
        &folder.join("mapping.toml"),
        &[],
    )
}

fn robot_of(serve: &Serve) -> serde_json::Value {
    serve.wait_for("Kapto is running: ", 1);
    let (code, body) = serve.http("GET", "/api/robot", None);
    assert_eq!(code, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[test]
fn the_live_view_is_arranged_by_the_packages_own_layout_saying_what_it_lacks() {
    let folder = scratch("own-layout");
    let serve = serve_package(
        &folder,
        "[[columns]]\nwidth = 2\npanels = [\n  { kind = \"camera\", stream = \"wrist_camera\" },\n  \
         { kind = \"plot\", fields = [\"role:gripper.opening\"], height = 2 },\n]\n",
    );
    let robot = robot_of(&serve);
    assert_eq!(robot["package"], "arm7");
    let layout = &robot["layout"];
    assert_eq!(layout["source"], "package");
    let panels = layout["columns"][0]["panels"].as_array().unwrap();
    assert_eq!(layout["columns"][0]["width"], 2);
    assert_eq!(panels[0]["stream"], "wrist_camera");
    assert_eq!(
        panels[0]["missing"],
        serde_json::json!(["no stream `wrist_camera`"])
    );
    assert_eq!(panels[1]["height"], 2);
    assert_eq!(panels[1]["missing"], serde_json::json!([]));
}

#[test]
fn a_layout_with_a_mistake_is_a_warning_and_the_made_layout_is_used() {
    let folder = scratch("broken-layout");
    let serve = serve_package(
        &folder,
        "[[columns]]\npanels = [{ kind = \"robot\", heigth = 2 }]\n",
    );
    let robot = robot_of(&serve);
    assert_eq!(robot["layout"]["source"], "made");
    let codes: Vec<&str> = robot["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|warning| warning["code"].as_str())
        .collect();
    assert_eq!(codes, ["layout.unknown_key"]);
}

#[test]
fn the_api_lists_the_settings_serve_runs_with_and_where_each_came_from() {
    let folder = scratch("settings");
    let serve = Serve::start(&folder, &root().join("packages/arm7/mapping.toml"));
    serve.wait_for("Kapto is running: ", 1);

    let (code, body) = serve.http("GET", "/api/settings", None);
    assert_eq!(code, 200, "{body}");
    let settings: serde_json::Value = serde_json::from_str(&body).unwrap();
    let setting = |key: &str| {
        settings["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|setting| setting["key"] == key)
            .unwrap()
            .clone()
    };
    // Given as flags by the test.
    assert_eq!(setting("server.port")["source"], "flag --port");
    assert_eq!(setting("server.port")["default"], false);
    assert_eq!(setting("viewer.display_rate")["source"], "default");
    assert_eq!(setting("viewer.display_rate")["default"], true);
    assert_ne!(setting("viewer.display_rate")["help"], "");
}

#[test]
fn jobs_left_running_are_queued_again_when_serve_starts_and_failed_once_started_too_often() {
    let folder = scratch("requeued");
    let data = folder.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (twice, once) = (
        "20260310T120000.000Z-00000001",
        "20260310T120500.000Z-00000002",
    );
    {
        // A worker that was stopped while doing both, the first already
        // started a second time.
        let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
        store.enqueue("verify", twice).unwrap();
        store.enqueue("verify", once).unwrap();
        store.claim().unwrap();
        store.requeue_interrupted(10).unwrap();
        assert_eq!(store.claim().unwrap().unwrap().attempts, 2);
        assert_eq!(store.claim().unwrap().unwrap().attempts, 1);
    }
    let serve = Serve::start_with(
        &folder,
        &root().join("packages/arm7/mapping.toml"),
        &["--worker-max-attempts", "2"],
    );
    serve.wait_for(
        "[serve] 1 interrupted jobs queued again, 1 failed after 2 attempts",
        1,
    );
    let reader = kapto_store::Reader::open(&data.join(DATABASE_FILE)).unwrap();
    let states: Vec<_> = reader
        .jobs(10)
        .unwrap()
        .into_iter()
        .map(|job| (job.episode, job.state))
        .collect();
    assert_eq!(
        states,
        [
            (once.to_owned(), JobState::Queued),
            (twice.to_owned(), JobState::Failed)
        ]
    );
}

#[test]
fn an_api_port_in_use_is_reported_before_anything_starts() {
    let folder = scratch("busy");
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("serve")
        .arg("--robot")
        .arg(root().join("packages/arm7/urdf/arm7.urdf"))
        .arg("--streams")
        .arg(streams(&folder))
        .arg("--mapping")
        .arg(root().join("packages/arm7/mapping.toml"))
        .args(["--port", &port])
        .arg("--data-dir")
        .arg(folder.join("data"))
        .env_clear()
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr.starts_with("error[server.listen_failed]"),
        "{stderr}"
    );
    assert!(!stderr.contains("[serve] started"), "{stderr}");
}

#[test]
fn a_child_that_dies_is_started_again() {
    let folder = scratch("restart");
    let serve = Serve::start(&folder, &root().join("packages/arm7/mapping.toml"));
    serve.wait_for("Kapto is running: ", 1);
    let bridge = serve.child_pid("viz-bridge", 1);
    signal("-KILL", bridge);
    serve.wait_for(
        "[serve] viz-bridge ended (signal: 9 (SIGKILL)); starting it again in 1 s",
        1,
    );
    let again = serve.child_pid("viz-bridge", 2);
    assert_ne!(again, bridge);
}

#[test]
fn capture_keeps_recording_when_serve_is_killed_while_the_bridge_and_worker_end_with_it() {
    let folder = scratch("orphan");
    let mut serve = Serve::start_with(
        &folder,
        &root().join("packages/arm7/mapping.toml"),
        &["--control-timeout", "1"],
    );
    serve.wait_for("Kapto is running: ", 1);
    let (capture, bridge, worker) = (
        serve.child_pid("capture", 1),
        serve.child_pid("viz-bridge", 1),
        serve.child_pid("worker", 1),
    );
    let mut control = serve.control();
    let Response::Started { episode } = control
        .ask(&Request::Start {
            duration_s: None,
            profile: None,
            task: None,
            waive: Vec::new(),
        })
        .unwrap()
    else {
        panic!("not started");
    };
    // Serve's sign of life has reached capture.
    std::thread::sleep(Duration::from_millis(1500));

    signal("-KILL", serve.pid());
    serve.child.take().unwrap().wait().unwrap();
    // Of no use without serve, and holding its ports and socket: they end.
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(bridge) || alive(worker) {
        assert!(
            Instant::now() < deadline,
            "the bridge or the worker outlived serve"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(alive(capture), "capture died with serve");
    let Response::Status(status) = control.ask(&Request::Status).unwrap() else {
        panic!("not a status");
    };
    assert_eq!(
        status.episode.as_ref(),
        Some(&episode),
        "the episode is still being recorded"
    );
    let finalised = folder.join("data").join(&episode).join("_FINALISED");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !finalised.exists() {
        assert!(Instant::now() < deadline, "never finalised");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(alive(capture), "capture goes on receiving");
    let Response::Status(status) = control.ask(&Request::Status).unwrap() else {
        panic!("not a status");
    };
    assert_eq!(status.episode, None);

    signal("-TERM", capture);
}

#[test]
fn a_mistake_in_the_robots_files_is_reported_before_anything_starts() {
    let folder = scratch("mistake");
    let mapping = folder.join("mapping.toml");
    std::fs::write(
        &mapping,
        "[joints]\njoint_9 = \"arm_joints.joint_positions[0]\"\n",
    )
    .unwrap();
    let mut serve = Serve::start(&folder, &mapping);
    let status = serve.child.take().unwrap().wait().unwrap();
    assert_eq!(status.code(), Some(1));
    let first = serve.wait_for("error[", 1);
    assert!(first.starts_with("error[mapping.unknown_joint]"), "{first}");
    assert!(
        !serve
            .lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("[serve] started"))
    );
}
