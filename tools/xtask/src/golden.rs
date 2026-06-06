// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask golden`: reference values computed by independent tools, so
//! tests compare Kapto's results against something other than itself.

use std::fs::File;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

/// The ROS 2 image `robot_state_publisher` runs in, pinned so the reference
/// values can be regenerated exactly.
const ROS_IMAGE: &str =
    "ros@sha256:066420e07f60aa18262f2479981def87ebcfcec42eefb0c0c57c4a46098348ca";

/// The robots whose link poses are checked against `robot_state_publisher`:
/// (folder in `testdata/robots/` for the reference, URDF from the repository
/// root).
const FK_ROBOTS: &[(&str, &str)] = &[
    ("panda", "testdata/robots/panda/panda.urdf"),
    ("humanoid", "testdata/robots/humanoid/humanoid.urdf"),
    ("arm7", "packages/arm7/urdf/arm7.urdf"),
];

/// buf v1.47.2, which compiles `.proto` files and converts JSON messages
/// to Protobuf binary, independently of Kapto and of prost.
const BUF_IMAGE: &str =
    "bufbuild/buf@sha256:c14957e613e9b95c4bf462fa73afdee48e13a67b8014ed1f951e5615492d457d";

/// Writes, in `testdata/protobuf/`, the descriptor set of `robot.proto`
/// (`robot.binpb`) and each message in `cases/*.json` as Protobuf binary
/// (`cases/*.binpb`), with buf. Needs Docker; no network once the image is
/// there.
pub(crate) fn protobuf(root: &Path) -> ExitCode {
    let folder = root.join("testdata/protobuf");
    let user = crate::ui::user();
    let buf = |args: &[&str]| {
        Command::new("docker")
            .args([
                "run",
                "--rm",
                "--network",
                "none",
                "-e",
                "XDG_CACHE_HOME=/tmp",
            ])
            .args(["--user", &user, "-w", "/work", "-v"])
            .arg(format!("{}:/work", folder.display()))
            .arg(BUF_IMAGE)
            .args(args)
            .status()
            .is_ok_and(|status| status.success())
    };
    eprintln!("Compiling robot.proto with buf ({BUF_IMAGE})");
    if !buf(&["build", "-o", "robot.binpb"]) {
        eprintln!("error: buf couldn't compile robot.proto");
        return ExitCode::FAILURE;
    }
    let Ok(cases) = std::fs::read_dir(folder.join("cases")) else {
        eprintln!("error: no cases folder in {}", folder.display());
        return ExitCode::FAILURE;
    };
    let mut names: Vec<String> = cases
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
        .collect();
    names.sort();
    for name in names {
        let (from, to) = (format!("cases/{name}.json"), format!("cases/{name}.binpb"));
        let args = [
            "convert",
            "robot.binpb",
            "--type",
            "kapto.test.v1.JointState",
        ];
        if !buf(&[&args[..], &["--from", &from, "--to", &to]].concat()) {
            eprintln!("error: buf couldn't convert {from}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// Writes `testdata/cdr/reference.json`: messages serialised by ROS 2 itself
/// (`rclpy.serialization`), with their definitions as rosbag2 stores them
/// and their values. Needs Docker and network access to pull the image and
/// install ROS 2's `test_msgs`.
pub(crate) fn cdr(root: &Path) -> ExitCode {
    let script = root.join("tools/xtask/golden/cdr_reference.py");
    let generator = format!("rclpy.serialization, ROS 2 Jazzy ({ROS_IMAGE})");
    let dir = root.join("testdata/cdr");
    let (Ok(()), Ok(output)) = (
        std::fs::create_dir_all(&dir),
        File::create(dir.join("reference.json")),
    ) else {
        eprintln!("error: can't write reference.json in {}", dir.display());
        return ExitCode::FAILURE;
    };
    eprintln!("Serialising the reference messages with ROS 2");
    let status = Command::new("docker")
        .args(["run", "--rm", "-v"])
        .arg(format!("{}:/cdr_reference.py:ro", script.display()))
        .args([
            ROS_IMAGE,
            "bash",
            "-c",
            "apt-get update -qq >/dev/null \
             && apt-get install -y -qq ros-jazzy-test-msgs ros-jazzy-rosbag2-py \
                ros-jazzy-rosbag2-storage-mcap >/dev/null 2>&1 \
             && . /opt/ros/jazzy/setup.sh \
             && python3 /cdr_reference.py \"$0\"",
            &generator,
        ])
        .stdout(Stdio::from(output))
        .status();
    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("error: the reference run failed ({status})");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("error: can't run docker: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Writes each robot's `fk_reference.json` in `testdata/robots/`: link poses from
/// ROS 2's `robot_state_publisher` for a set of joint values. Needs Docker and
/// network access to pull the image and install `robot_state_publisher`.
pub(crate) fn fk(root: &Path) -> ExitCode {
    let script = root.join("tools/xtask/golden/fk_reference.py");
    let generator = format!("robot_state_publisher, ROS 2 Jazzy ({ROS_IMAGE})");
    for (folder, urdf) in FK_ROBOTS {
        let dir = root.join("testdata/robots").join(folder);
        eprintln!("{folder}: computing link poses with robot_state_publisher");
        let created = std::fs::create_dir_all(&dir);
        let (Ok(()), Ok(input), Ok(output)) = (
            created,
            File::open(root.join(urdf)),
            File::create(dir.join("fk_reference.json")),
        ) else {
            eprintln!(
                "error: can't read {urdf} or write fk_reference.json in {}",
                dir.display()
            );
            return ExitCode::FAILURE;
        };
        let status = Command::new("docker")
            .args(["run", "--rm", "-i", "-v"])
            .arg(format!("{}:/fk_reference.py:ro", script.display()))
            .args([
                ROS_IMAGE,
                "bash",
                "-c",
                "apt-get update -qq >/dev/null \
                 && apt-get install -y -qq ros-jazzy-robot-state-publisher >/dev/null 2>&1 \
                 && . /opt/ros/jazzy/setup.sh \
                 && python3 /fk_reference.py \"$0\"",
                &generator,
            ])
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .status();
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("error: the reference run for {folder} failed ({status})");
                return ExitCode::FAILURE;
            }
            Err(error) => {
                eprintln!("error: can't run docker: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
