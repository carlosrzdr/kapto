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
        "/api/layout",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"], "default");
    assert_eq!(body["columns"][0]["panels"][0]["height"], 3);
    assert_eq!(
        body["columns"][1]["panels"][0]["missing"],
        json!(["no stream `wrist_camera`"])
    );
    let file = folder.join("config/layouts/arm7.toml");
    let written = std::fs::read_to_string(&file).unwrap();
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
    let (status, body) = call(api.clone(), "DELETE", "/api/layout", None).await;
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
            "/api/layout",
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
        "/api/layout",
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
async fn a_layout_cant_be_saved_without_a_package_or_a_folder_to_write_it_in() {
    let folder = scratch("layout-nowhere");
    let (status, error) = call(
        arranged(&folder, None),
        "PUT",
        "/api/layout",
        Some(("application/json", SAVED)),
    )
    .await;
    assert_eq!(
        (status, &error["code"]),
        (StatusCode::CONFLICT, &json!("layout.without_package"))
    );
    let (status, error) = call(arranged(&folder, None), "DELETE", "/api/layout", None).await;
    assert_eq!(
        (status, &error["code"]),
        (StatusCode::CONFLICT, &json!("layout.without_package"))
    );

    // The configuration folder is a file: nothing can be written under it.
    std::fs::write(folder.join("config"), "").unwrap();
    let (status, error) = call(
        arranged(&folder, Some("arm7")),
        "PUT",
        "/api/layout",
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
    std::fs::create_dir_all(folder.join("config/layouts")).unwrap();
    std::fs::write(
        folder.join("config/layouts/arm7.toml"),
        "[[columns]]\npanels = [{ kind = \"robot\", heigth = 2 }]\n",
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
