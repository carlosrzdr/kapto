// SPDX-License-Identifier: Apache-2.0

//! The robot as serve loaded it, its scene and meshes, and the layouts
//! saved for it.

use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;
use kapto_core::{ErrorCode, KaptoError};
use kapto_server::api::{ApiError, ApiState};
use kapto_server::layouts::Layouts;
use kapto_server::robot::RobotOverview;
use kapto_server::scene::{MESH_MAGIC, Scene};
use kapto_testkit::scratch;
use serde_json::{Value, json};

use crate::common::{call, get_bytes, state};

#[tokio::test]
async fn the_robot_is_what_serve_loaded_even_while_capture_is_down() {
    let folder = scratch("robot");
    let state = ApiState {
        robot: Arc::new(RobotOverview {
            name: "arm7".into(),
            links: 11,
            warnings: vec![ApiError::from(&KaptoError::new(
                ErrorCode::MappingJointUnmapped,
            ))],
            ..RobotOverview::default()
        }),
        ..state(&folder)
    };
    let (status, body) = call(state, "GET", "/api/robot", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "arm7");
    assert_eq!(body["links"], 11);
    assert_eq!(body["warnings"][0]["code"], "mapping.joint_unmapped");
}

/// A mesh file of one triangle.
const TRIANGLE_STL: &str = "solid t\nfacet normal 0 0 1\n outer loop\n  vertex 0 0 0\n  vertex 1 0 0\n  vertex 0 1 0\n endloop\nendfacet\nendsolid t\n";

/// A robot in `folder`: a red box on its base, a mesh on its arm, the same
/// mesh, a sphere and a mesh file that isn't there on its hand.
fn cell(folder: &Path) -> kapto_robot::RobotModel {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("part.stl"), TRIANGLE_STL).unwrap();
    let urdf = folder.join("cell.urdf");
    std::fs::write(
        &urdf,
        r#"<robot name="cell">
  <link name="base"><visual><geometry><box size="0.1 0.2 0.3"/></geometry>
    <material name="red"><color rgba="1 0 0 1"/></material></visual></link>
  <link name="arm"><visual><origin xyz="0 0 0.5"/>
    <geometry><mesh filename="part.stl" scale="0.001 0.001 0.001"/></geometry></visual></link>
  <link name="hand">
    <visual><geometry><mesh filename="part.stl"/></geometry></visual>
    <visual><geometry><sphere radius="0.05"/></geometry></visual>
    <visual><geometry><mesh filename="missing.stl"/></geometry></visual>
  </link>
  <joint name="lift" type="fixed"><parent link="base"/><child link="arm"/></joint>
  <joint name="turn" type="revolute"><parent link="arm"/><child link="hand"/>
    <axis xyz="0 0 1"/><limit lower="-1" upper="1" effort="1" velocity="1"/></joint>
</robot>"#,
    )
    .unwrap();
    kapto_robot::load_urdf(&urdf, &[]).unwrap()
}

#[tokio::test]
async fn the_robot_s_scene_lists_each_link_in_order_with_its_shapes_and_each_mesh_file_once() {
    let folder = scratch("scene");
    let state = ApiState {
        scene: Arc::new(Scene::of(&cell(&folder.join("robot")))),
        layouts: Layouts::default(),
        ..state(&folder)
    };
    let (status, scene) = call(state, "GET", "/api/robot/scene", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&scene["name"], &scene["root"]),
        (&json!("cell"), &json!("base"))
    );
    let links: Vec<&str> = scene["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link["name"].as_str().unwrap())
        .collect();
    assert_eq!(links, ["base", "arm", "hand"]);
    assert_eq!(
        scene["links"][0]["visuals"][0],
        json!({
            "origin": {"translation": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0]},
            "color": [1.0, 0.0, 0.0, 1.0],
            "shape": {"kind": "box", "size": [0.1, 0.2, 0.3]}
        })
    );
    let arm = &scene["links"][1]["visuals"][0];
    assert_eq!(arm["origin"]["translation"], json!([0.0, 0.0, 0.5]));
    assert_eq!(
        arm["shape"],
        json!({"kind": "mesh", "mesh": 0, "scale": [0.001, 0.001, 0.001]})
    );
    // The same file is the same mesh; the missing one is left out.
    let hand: Vec<&Value> = scene["links"][2]["visuals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|visual| &visual["shape"])
        .collect();
    assert_eq!(
        hand,
        [
            &json!({"kind": "mesh", "mesh": 0, "scale": [1.0, 1.0, 1.0]}),
            &json!({"kind": "sphere", "radius": 0.05})
        ]
    );
    assert_eq!(scene["meshes"], 1);
    // How far it reaches: around the base's box at least.
    let center: Vec<f64> = scene["reach"]["center"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_f64().unwrap())
        .collect();
    let radius = scene["reach"]["radius"].as_f64().unwrap();
    for corner in [[0.05, 0.1, 0.15], [-0.05, -0.1, -0.15]] {
        let away: f64 = corner
            .iter()
            .zip(&center)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!(away <= radius, "{corner:?} outside {center:?} {radius}");
    }
}

#[tokio::test]
async fn a_mesh_is_sent_as_the_triangles_the_mesh_loader_reads() {
    let folder = scratch("mesh");
    let state = ApiState {
        scene: Arc::new(Scene::of(&cell(&folder.join("robot")))),
        layouts: Layouts::default(),
        ..state(&folder)
    };
    let (status, kind, bytes) = get_bytes(state, "/api/robot/meshes/0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind, "application/octet-stream");
    let read = kapto_robot::load_mesh(&folder.join("robot/part.stl")).unwrap();
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    assert_eq!(&bytes[..4], MESH_MAGIC);
    assert_eq!(word(8) as usize, read.meshes.len());
    let part = &read.meshes[0];
    assert_eq!(
        (word(12) as usize, word(16) as usize),
        (part.positions.len(), part.triangles.len())
    );
    let positions: Vec<f32> = bytes[40..40 + part.positions.len() * 12]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|value| f32::from_le_bytes(*value))
        .collect();
    assert_eq!(positions, part.positions.concat());
    let triangles = &bytes[bytes.len() - part.triangles.len() * 12..];
    let indices: Vec<u32> = triangles
        .as_chunks::<4>()
        .0
        .iter()
        .map(|value| u32::from_le_bytes(*value))
        .collect();
    assert_eq!(indices, part.triangles.concat());
}

#[tokio::test]
async fn a_mesh_the_scene_lacks_or_can_t_read_is_refused_with_its_cause() {
    let folder = scratch("mesh-refused");
    let scene = Arc::new(Scene::of(&cell(&folder.join("robot"))));
    let state = || ApiState {
        scene: Arc::clone(&scene),
        layouts: Layouts::default(),
        ..state(&folder)
    };
    for (uri, expected, code) in [
        (
            "/api/robot/meshes/1",
            StatusCode::NOT_FOUND,
            "viewer.mesh_not_found",
        ),
        (
            "/api/robot/meshes/first",
            StatusCode::BAD_REQUEST,
            "api.invalid_request",
        ),
    ] {
        let (status, body) = call(state(), "GET", uri, None).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (expected, Some(code)),
            "{uri}"
        );
    }
    // Gone since serve loaded the robot.
    std::fs::remove_file(folder.join("robot/part.stl")).unwrap();
    let (status, body) = call(state(), "GET", "/api/robot/meshes/0", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "robot.mesh_unreadable");
}

/// The API of a serve running the package `arm7`, which has one stream of
/// joints, its configuration folder in `folder`, arranged by the robot
/// alone unless a default is saved.
fn arranged(folder: &Path, package: Option<&str>) -> ApiState {
    let declared = kapto_robot::Declared {
        robot_moves: true,
        streams: vec![kapto_robot::DeclaredStream {
            id: "arm_joints".to_owned(),
            camera: false,
            fields: vec![("gripper".to_owned(), 1)],
            unplotted: Vec::new(),
        }],
        roles: Vec::new(),
    };
    let own: kapto_server::robot::ViewLayout = serde_json::from_value(
        json!({"source": "made", "columns": [{"width": 1, "panels": [
            {"kind": "robot", "height": 1, "stream": null, "fields": [], "missing": []}
        ]}]}),
    )
    .unwrap();
    let (layouts, problems) = Layouts::load(&folder.join("config"), package, declared, own);
    assert_eq!(problems.len(), 0);
    ApiState {
        layouts,
        ..state(folder)
    }
}

const SAVED: &str = r#"{"columns": [
    {"width": 2, "panels": [{"kind": "plot", "height": 3, "fields": ["arm_joints.gripper"]}]},
    {"width": 1, "panels": [{"kind": "camera", "height": 1, "stream": "wrist_camera"}, {"kind": "robot", "height": 1}]}
]}"#;

#[tokio::test]
async fn a_layout_saved_as_default_is_written_for_the_package_and_given_from_then_on() {
    let folder = scratch("layout-saved");
    let api = arranged(&folder, Some("arm7"));
    let (status, body) = call(
        api.clone(),
        "PUT",
        "/api/layouts/default",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (&body["id"], &body["name"]),
        (&json!("default"), &json!("Default"))
    );
    let body = body["layout"].clone();
    assert_eq!(body["source"], "default");
    assert_eq!(body["columns"][0]["panels"][0]["height"], 3);
    assert_eq!(
        body["columns"][1]["panels"][0]["missing"],
        json!(["no stream `wrist_camera`"])
    );
    let file = folder.join("config/layouts/arm7/default.toml");
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.starts_with("name = \"Default\"\n"), "{written}");
    assert!(
        written.contains("fields = [\"arm_joints.gripper\"], height = 3"),
        "{written}"
    );
    let (_, robot) = call(api.clone(), "GET", "/api/robot", None).await;
    assert_eq!(robot["layout"], body);

    // Read again when serve starts: it comes before the package's own.
    let again = arranged(&folder, Some("arm7"));
    assert_eq!(again.layouts.current().unwrap().source, "default");

    // Removed, the package's own layout is back.
    let (status, body) = call(api.clone(), "DELETE", "/api/layouts/default", None).await;
    assert_eq!((status, &body["source"]), (StatusCode::OK, &json!("made")));
    assert!(!file.exists());
    let (_, robot) = call(api, "GET", "/api/robot", None).await;
    assert_eq!(robot["layout"]["columns"][0]["panels"][0]["kind"], "robot");
}

#[tokio::test]
async fn a_layout_checked_says_what_each_panel_lacks_and_nothing_is_saved() {
    let folder = scratch("layout-checked");
    let api = arranged(&folder, Some("arm7"));
    let (status, body) = call(
        api.clone(),
        "POST",
        "/api/layout/check",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The operator's own, by the rules every other layout is held to.
    assert_eq!(body["source"], "own");
    assert_eq!(body["columns"][0]["panels"][0]["missing"], json!([]));
    assert_eq!(
        body["columns"][1]["panels"][0]["missing"],
        json!(["no stream `wrist_camera`"])
    );
    assert!(!folder.join("config/layouts").exists());
    let (_, robot) = call(api.clone(), "GET", "/api/robot", None).await;
    assert_eq!(robot["layout"]["source"], "made");

    let (status, error) = call(
        api,
        "POST",
        "/api/layout/check",
        Some((
            "application/json",
            r#"{"columns": [{"width": 1, "panels": [{"kind": "map", "height": 1}]}]}"#,
        )),
    )
    .await;
    assert_eq!(
        (status, &error["code"]),
        (StatusCode::CONFLICT, &json!("config.invalid_value"))
    );
}

#[tokio::test]
async fn a_layout_breaking_a_rule_of_layout_toml_is_refused_and_nothing_is_written() {
    let folder = scratch("layout-refused");
    let api = arranged(&folder, Some("arm7"));
    for (body, detail) in [
        (
            r#"{"columns": [{"width": 1, "panels": [{"kind": "map", "height": 1}]}]}"#,
            "`map` isn't a kind of panel",
        ),
        (
            r#"{"columns": [{"width": 40, "panels": [{"kind": "robot", "height": 1}]}]}"#,
            "`width` must be a whole number from 1 to 12",
        ),
        (
            r#"{"columns": [{"width": 1, "panels": [{"kind": "camera", "height": 1}]}]}"#,
            "`stream` must be a stream's ID",
        ),
        (r#"{"columns": []}"#, "needs `columns`"),
    ] {
        let (status, error) = call(
            api.clone(),
            "PUT",
            "/api/layouts/default",
            Some(("application/json", body)),
        )
        .await;
        assert_eq!(
            (status, &error["code"]),
            (StatusCode::CONFLICT, &json!("config.invalid_value")),
            "{body}"
        );
        assert!(
            error["message"].as_str().unwrap().contains(detail),
            "{error}"
        );
    }
    let (status, error) = call(
        api,
        "PUT",
        "/api/layouts/default",
        Some(("application/json", "{\"rows\": 2}")),
    )
    .await;
    assert_eq!(
        (status, &error["code"]),
        (StatusCode::BAD_REQUEST, &json!("api.invalid_request"))
    );
    assert!(!folder.join("config/layouts").exists());
}

#[tokio::test]
async fn a_panels_own_title_is_saved_given_back_and_refused_when_too_long() {
    let folder = scratch("layout-titled");
    let api = arranged(&folder, Some("arm7"));
    let titled = r#"{"columns": [{"width": 1, "panels": [
        {"kind": "robot", "height": 1, "title": "The \"arm\""},
        {"kind": "health", "height": 1}
    ]}]}"#;
    let (status, body) = call(
        api.clone(),
        "PUT",
        "/api/layouts/default",
        Some(("application/json", titled)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let panels = &body["layout"]["columns"][0]["panels"];
    assert_eq!(
        (&panels[0]["title"], &panels[1]["title"]),
        (&json!("The \"arm\""), &Value::Null)
    );
    let written = std::fs::read_to_string(folder.join("config/layouts/arm7/default.toml")).unwrap();
    assert!(
        written.contains("{ kind = \"robot\", height = 1, title = \"The \\\"arm\\\"\" }"),
        "{written}"
    );

    let long = format!(
        r#"{{"columns": [{{"width": 1, "panels": [{{"kind": "robot", "height": 1, "title": "{}"}}]}}]}}"#,
        "x".repeat(65)
    );
    for (method, uri) in [
        ("POST", "/api/layout/check"),
        ("PUT", "/api/layouts/default"),
    ] {
        let (status, error) =
            call(api.clone(), method, uri, Some(("application/json", &long))).await;
        assert_eq!(
            (status, &error["code"]),
            (StatusCode::CONFLICT, &json!("layout.title_too_long")),
            "{method} {uri}"
        );
    }
    // Refused, the one saved before stays.
    let written_after =
        std::fs::read_to_string(folder.join("config/layouts/arm7/default.toml")).unwrap();
    assert_eq!(written_after, written);
}

#[tokio::test]
async fn a_robot_panels_view_is_saved_given_back_and_a_fifth_view_refused() {
    let folder = scratch("layout-views");
    let api = arranged(&folder, Some("arm7"));
    let viewed = r#"{"columns": [{"width": 1, "panels": [
        {"kind": "robot", "height": 1, "view": "top"},
        {"kind": "robot", "height": 1, "view": {"from": [1.5, -1, 0.8], "at": [0, 0, 0.4]}},
        {"kind": "robot", "height": 1}
    ]}]}"#;
    let (status, body) = call(
        api.clone(),
        "PUT",
        "/api/layouts/default",
        Some(("application/json", viewed)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let panels = &body["layout"]["columns"][0]["panels"];
    assert_eq!(
        (&panels[0]["view"], &panels[1]["view"], &panels[2]["view"]),
        (
            &json!("top"),
            &json!({"from": [1.5, -1.0, 0.8], "at": [0.0, 0.0, 0.4]}),
            &Value::Null
        )
    );
    let written = std::fs::read_to_string(folder.join("config/layouts/arm7/default.toml")).unwrap();
    assert!(
        written.contains(r#"{ kind = "robot", view = { from = [1.5, -1.0, 0.8], at = [0.0, 0.0, 0.4] }, height = 1 }"#),
        "{written}"
    );

    let robot = r#"{"kind": "robot", "height": 1, "view": "side"}"#;
    let five = format!(
        r#"{{"columns": [{{"width": 1, "panels": [{}]}}]}}"#,
        [robot; 5].join(", ")
    );
    let on_a_plot = r#"{"columns": [{"width": 1, "panels": [
        {"kind": "plot", "height": 1, "fields": ["arm_joints.joint_positions"], "view": "top"}
    ]}]}"#;
    for (layout, code) in [
        (five.as_str(), "layout.too_many_views"),
        (on_a_plot, "config.invalid_value"),
    ] {
        for (method, uri) in [
            ("POST", "/api/layout/check"),
            ("PUT", "/api/layouts/default"),
        ] {
            let (status, error) =
                call(api.clone(), method, uri, Some(("application/json", layout))).await;
            assert_eq!(
                (status, &error["code"]),
                (StatusCode::CONFLICT, &json!(code)),
                "{method} {uri}: {error}"
            );
        }
    }
    // Refused, the one saved before stays.
    let written_after =
        std::fs::read_to_string(folder.join("config/layouts/arm7/default.toml")).unwrap();
    assert_eq!(written_after, written);
}

#[tokio::test]
async fn a_layout_cant_be_saved_without_a_package_or_a_folder_to_write_it_in() {
    let folder = scratch("layout-nowhere");
    let (status, error) = call(
        arranged(&folder, None),
        "PUT",
        "/api/layouts/default",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(
        (status, &error["code"]),
        (StatusCode::CONFLICT, &json!("layout.without_package"))
    );
    for (method, uri) in [("DELETE", "/api/layouts/default"), ("GET", "/api/layouts")] {
        let (status, error) = call(arranged(&folder, None), method, uri, None).await;
        assert_eq!(
            (status, &error["code"]),
            (StatusCode::CONFLICT, &json!("layout.without_package")),
            "{method} {uri}"
        );
    }

    // The configuration folder is a file: nothing can be written under it.
    let api = arranged(&folder, Some("arm7"));
    std::fs::write(folder.join("config"), "").unwrap();
    let (status, error) = call(
        api,
        "PUT",
        "/api/layouts/default",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(
        (status, &error["code"]),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!("layout.write_failed")
        )
    );
}

#[test]
fn a_saved_default_that_doesnt_read_is_a_problem_and_the_packages_layout_is_used() {
    let folder = scratch("layout-unread");
    std::fs::create_dir_all(folder.join("config/layouts/arm7")).unwrap();
    std::fs::write(
        folder.join("config/layouts/arm7/default.toml"),
        "name = \"Default\"\n[[columns]]\npanels = [{ kind = \"robot\", heigth = 2 }]\n",
    )
    .unwrap();
    let own = kapto_server::robot::ViewLayout::default();
    let (layouts, problems) = Layouts::load(
        &folder.join("config"),
        Some("arm7"),
        kapto_robot::Declared::default(),
        own.clone(),
    );
    let codes: Vec<&str> = problems.iter().map(|problem| problem.code().id()).collect();
    assert_eq!(codes, ["layout.unknown_key"]);
    assert_eq!(layouts.current(), Some(own));
}

#[tokio::test]
async fn hand_edited_layouts_are_listed_as_written_saved_over_and_deleted() {
    let folder = scratch("layouts-by-hand");
    let api = arranged(&folder, Some("arm7"));
    let layouts = folder.join("config/layouts/arm7");
    std::fs::create_dir_all(&layouts).unwrap();
    let night = "# kept for the night shift\nname = \"Night (by hand)\"\n\n[[columns]]\npanels = [{ kind = \"health\" }]\n";
    std::fs::write(layouts.join("Night.toml"), night).unwrap();
    std::fs::write(
        layouts.join("broken.toml"),
        "name = \"Broken\"\n[[columns]]\n",
    )
    .unwrap();
    std::fs::write(layouts.join("notes.txt"), "not a layout").unwrap();
    std::fs::write(layouts.join("arm.toml"), format!("name = \"arm\"\n{night}")).unwrap();

    let (status, list) = call(api.clone(), "GET", "/api/layouts", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let listed: Vec<(&str, &str)> = list["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|each| (each["id"].as_str().unwrap(), each["name"].as_str().unwrap()))
        .collect();
    assert_eq!(listed, [("Night", "Night (by hand)")]);
    let shown = &list["layouts"][0]["layout"];
    assert_eq!(shown["source"], "named");
    assert_eq!(shown["columns"][0]["panels"][0]["kind"], "health");
    // Each file that doesn't read, by its file and line.
    let problems: Vec<&str> = list["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|problem| problem["message"].as_str().unwrap())
        .collect();
    assert_eq!(problems.len(), 2, "{list}");
    assert!(
        problems
            .iter()
            .any(|message| message.starts_with("`broken.toml` line 1: ")),
        "{list}"
    );

    // Saved over, it keeps its name; the view isn't arranged by it.
    let (status, body) = call(
        api.clone(),
        "PUT",
        "/api/layouts/Night",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(
        (status, &body["name"]),
        (StatusCode::OK, &json!("Night (by hand)"))
    );
    let written = std::fs::read_to_string(layouts.join("Night.toml")).unwrap();
    assert!(
        written.starts_with("name = \"Night (by hand)\"\n"),
        "{written}"
    );
    assert_eq!(api.layouts.current().unwrap().source, "made");

    let (status, body) = call(api.clone(), "DELETE", "/api/layouts/Night", None).await;
    assert_eq!((status, &body["source"]), (StatusCode::OK, &json!("made")));
    assert!(!layouts.join("Night.toml").exists());
}

#[tokio::test]
async fn a_layout_not_there_is_not_found() {
    let folder = scratch("layouts-not-found");
    let api = arranged(&folder, Some("arm7"));
    for uri in ["/api/layouts/elbow", "/api/layouts/..%2Fdefault"] {
        let (status, error) =
            call(api.clone(), "PUT", uri, Some(("application/json", SAVED))).await;
        assert_eq!(
            (status, &error["code"]),
            (StatusCode::NOT_FOUND, &json!("layout.not_found")),
            "{uri}"
        );
    }
    assert!(!folder.join("config/layouts").exists());
}

/// `SAVED` as a named layout's body, named `name`.
fn named(name: &str) -> String {
    let columns: Value = serde_json::from_str(SAVED).unwrap();
    json!({"name": name, "columns": columns["columns"]}).to_string()
}

#[tokio::test]
async fn named_layouts_are_saved_listed_renamed_and_deleted_each_in_a_file_named_from_it() {
    let folder = scratch("layouts-named");
    let api = arranged(&folder, Some("arm7"));
    let layouts = folder.join("config/layouts/arm7");
    for name in ["Wrist view", "  arm, close up "] {
        let (status, body) = call(
            api.clone(),
            "POST",
            "/api/layouts",
            Some(("application/json", &named(name))),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["layout"]["source"], "named");
    }
    assert!(layouts.join("wrist-view.toml").exists());
    let (status, list) = call(api.clone(), "GET", "/api/layouts", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let ids: Vec<(&str, &str)> = list["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|each| (each["id"].as_str().unwrap(), each["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        ids,
        [
            ("arm-close-up", "arm, close up"),
            ("wrist-view", "Wrist view")
        ]
    );
    // Saving over one keeps its name; the view isn't arranged by it.
    let (status, body) = call(
        api.clone(),
        "PUT",
        "/api/layouts/wrist-view",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(
        (status, &body["name"]),
        (StatusCode::OK, &json!("Wrist view"))
    );
    assert_eq!(api.layouts.current().unwrap().source, "made");

    let (status, body) = call(
        api.clone(),
        "PATCH",
        "/api/layouts/wrist-view",
        Some(("application/json", r#"{"name": "Gripper"}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (&body["id"], &body["name"]),
        (&json!("gripper"), &json!("Gripper"))
    );
    assert!(!layouts.join("wrist-view.toml").exists());
    // Renamed to Default, it is the one the view is arranged by.
    let (status, _) = call(
        api.clone(),
        "PATCH",
        "/api/layouts/gripper",
        Some(("application/json", r#"{"name": "Default"}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api.layouts.current().unwrap().source, "default");

    let (status, body) = call(api.clone(), "DELETE", "/api/layouts/default", None).await;
    assert_eq!((status, &body["source"]), (StatusCode::OK, &json!("made")));
    let (_, body) = call(api.clone(), "DELETE", "/api/layouts/arm-close-up", None).await;
    assert_eq!(body["source"], "made");
    let (_, list) = call(api, "GET", "/api/layouts", None).await;
    assert_eq!(
        (&list["layouts"], &list["problems"]),
        (&json!([]), &json!([]))
    );
}

#[tokio::test]
async fn a_name_taken_too_long_or_empty_or_a_layout_not_there_is_refused_with_its_cause() {
    let folder = scratch("layouts-refused");
    let api = arranged(&folder, Some("arm7"));
    let first = named("Wrist view");
    let (status, _) = call(
        api.clone(),
        "POST",
        "/api/layouts",
        Some(("application/json", &first)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let long = "x".repeat(65);
    for (method, uri, body, status, code) in [
        (
            "POST",
            "/api/layouts",
            named("Wrist view"),
            StatusCode::CONFLICT,
            "layout.name_taken",
        ),
        (
            "POST",
            "/api/layouts",
            named("WRIST  view!"),
            StatusCode::CONFLICT,
            "layout.name_taken",
        ),
        (
            "POST",
            "/api/layouts",
            named(&long),
            StatusCode::CONFLICT,
            "layout.name_too_long",
        ),
        (
            "POST",
            "/api/layouts",
            named(" ?! "),
            StatusCode::CONFLICT,
            "layout.name_empty",
        ),
        (
            "POST",
            "/api/layouts",
            r#"{"name": 3}"#.to_owned(),
            StatusCode::BAD_REQUEST,
            "api.invalid_request",
        ),
        (
            "PATCH",
            "/api/layouts/elbow",
            r#"{"name": "Elbow"}"#.to_owned(),
            StatusCode::NOT_FOUND,
            "layout.not_found",
        ),
        (
            "PATCH",
            "/api/layouts/wrist-view",
            format!(r#"{{"name": "{long}"}}"#),
            StatusCode::CONFLICT,
            "layout.name_too_long",
        ),
    ] {
        let (got, error) = call(api.clone(), method, uri, Some(("application/json", &body))).await;
        assert_eq!(
            (got, error["code"].as_str()),
            (status, Some(code)),
            "{method} {uri} {body}"
        );
    }
}

#[tokio::test]
async fn default_cant_be_made_while_another_file_holds_a_layout_named_so() {
    let folder = scratch("layouts-default-taken");
    let api = arranged(&folder, Some("arm7"));
    let (status, _) = call(
        api.clone(),
        "POST",
        "/api/layouts",
        Some(("application/json", &named("default"))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    std::fs::rename(
        folder.join("config/layouts/arm7/default.toml"),
        folder.join("config/layouts/arm7/mine.toml"),
    )
    .unwrap();
    let (status, error) = call(
        api,
        "PUT",
        "/api/layouts/default",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(
        (status, &error["code"]),
        (StatusCode::CONFLICT, &json!("layout.name_taken"))
    );
}

#[tokio::test]
async fn a_hand_edited_layout_renamed_keeps_the_rest_as_written() {
    let folder = scratch("layouts-renamed-by-hand");
    let api = arranged(&folder, Some("arm7"));
    let layouts = folder.join("config/layouts/arm7");
    std::fs::create_dir_all(&layouts).unwrap();
    let text = "# kept for the night shift\nname = \"Night (by hand)\"\n\n[[columns]]\npanels = [{ kind = \"health\" }]\n";
    std::fs::write(layouts.join("Night.toml"), text).unwrap();
    let (status, body) = call(
        api,
        "PATCH",
        "/api/layouts/Night",
        Some(("application/json", r#"{"name": "Night shift"}"#)),
    )
    .await;
    assert_eq!(
        (status, &body["id"]),
        (StatusCode::OK, &json!("night-shift"))
    );
    assert_eq!(
        std::fs::read_to_string(layouts.join("night-shift.toml")).unwrap(),
        text.replace("\"Night (by hand)\"", "\"Night shift\"")
    );
    assert!(!layouts.join("Night.toml").exists());
}

#[tokio::test]
async fn the_list_offers_the_package_s_own_layout_if_it_has_one_and_the_one_made_from_it() {
    let folder = scratch("layouts-offered");
    let (_, list) = call(arranged(&folder, Some("arm7")), "GET", "/api/layouts", None).await;
    // Without a layout.toml of its own, the view is made from the package.
    assert_eq!(list["package"], Value::Null);
    assert_eq!(list["made"]["source"], "made");
    let kinds: Vec<&Value> = list["made"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|column| column["panels"].as_array().unwrap())
        .map(|panel| &panel["kind"])
        .collect();
    assert_eq!(kinds, [&json!("robot"), &json!("plot")]);

    let mut api = arranged(&folder, Some("arm7"));
    let own: kapto_server::robot::ViewLayout = serde_json::from_value(json!({
        "source": "package",
        "columns": [{"width": 1, "panels": [{"kind": "health", "height": 1, "stream": null, "fields": [], "missing": []}]}]
    }))
    .unwrap();
    let declared = kapto_robot::Declared::default();
    api.layouts = Layouts::load(&folder.join("config"), Some("arm7"), declared, own.clone()).0;
    let (_, list) = call(api, "GET", "/api/layouts", None).await;
    assert_eq!(list["package"], serde_json::to_value(&own).unwrap());
}

#[tokio::test]
async fn a_layout_exported_is_a_file_named_from_it_that_imports_as_the_same_layout() {
    let folder = scratch("layouts-exported");
    let api = arranged(&folder, Some("arm7"));
    let (status, file) = call(
        api.clone(),
        "POST",
        "/api/layout/export",
        Some(("application/json", &named("Wrist view"))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{file}");
    assert_eq!(file["file"], "wrist-view.toml");
    let text = file["text"].as_str().unwrap();
    assert!(
        text.starts_with("name = \"Wrist view\"\n\n[[columns]]\nwidth = 2\n"),
        "{text}"
    );

    let (status, input) = call(
        api.clone(),
        "POST",
        "/api/layout/import",
        Some(("application/json", &file.to_string())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{input}");
    assert_eq!(input["name"], "Wrist view");
    let (status, again) = call(
        api,
        "POST",
        "/api/layout/export",
        Some(("application/json", &input.to_string())),
    )
    .await;
    assert_eq!((status, &again), (StatusCode::OK, &file));
    // Nothing is saved either way.
    assert!(!folder.join("config/layouts").exists());
}

#[tokio::test]
async fn a_file_without_a_name_imports_named_from_the_file_and_one_that_isnt_a_layout_says_where() {
    let folder = scratch("layouts-imported");
    let api = arranged(&folder, Some("arm7"));
    let file = json!({
        "file": "layout.toml",
        "text": "# the package's own\n[[columns]]\npanels = [{ kind = \"health\", title = \"Streams\" }]\n",
    });
    let (status, input) = call(
        api.clone(),
        "POST",
        "/api/layout/import",
        Some(("application/json", &file.to_string())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{input}");
    assert_eq!(
        input,
        json!({"name": "layout", "columns": [{"width": 1, "panels": [
            {"kind": "health", "height": 1, "stream": null, "fields": [], "title": "Streams"}
        ]}]})
    );

    for (text, code) in [
        ("name = \"Night\"\n[[columns]\n", "config.invalid_syntax"),
        (
            "name = \"Night\"\n[[columns]]\npanels = [{ kind = \"radar\" }]\n",
            "config.invalid_value",
        ),
    ] {
        let file = json!({"file": "night.toml", "text": text});
        let (status, error) = call(
            api.clone(),
            "POST",
            "/api/layout/import",
            Some(("application/json", &file.to_string())),
        )
        .await;
        assert_eq!(
            (status, &error["code"]),
            (StatusCode::CONFLICT, &json!(code)),
            "{error}"
        );
        let message = error["message"].as_str().unwrap();
        assert!(message.starts_with("`night.toml` line "), "{message}");
        assert_ne!(error["remedy"], json!(""));
    }

    let (status, error) = call(
        api,
        "POST",
        "/api/layout/export",
        Some(("application/json", &named("  ,  "))),
    )
    .await;
    assert_eq!(
        (status, &error["code"]),
        (StatusCode::CONFLICT, &json!("layout.name_empty"))
    );
}
