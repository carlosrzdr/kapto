// SPDX-License-Identifier: Apache-2.0

//! `kapto viz-bridge`: streams the robot moving live to the web UI's live
//! view, from the samples a running capture process (or `kapto record`)
//! sends to the live tap. The robot is checked here; streaming it is in
//! `live_view`.

use std::path::{Path, PathBuf};

use clap::Args;
use kapto_config::{
    ConfigFile, MappingDeclaration, StreamDeclaration, parse_mapping, parse_streams,
    read_config_file,
};
use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::{JointMap, Kinematics, RobotModel, load_urdf};

/// Arguments of `kapto viz-bridge`.
#[derive(Args)]
pub(crate) struct VizBridgeArgs {
    /// The robot's URDF file.
    #[arg(long, value_name = "FILE")]
    pub(crate) robot: PathBuf,
    /// The streams.toml the recording uses.
    #[arg(long, value_name = "FILE")]
    pub(crate) streams: PathBuf,
    /// The mapping.toml saying which decoded value drives which joint.
    #[arg(long, value_name = "FILE")]
    pub(crate) mapping: PathBuf,
    /// Stop after this many seconds; without it, runs until interrupted.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) duration: Option<u32>,
    /// End when process PID ends, even if it is killed: serve passes its
    /// own, so this ends with it.
    #[arg(long, value_name = "PID")]
    pub(crate) parent: Option<u32>,
}

/// The robot, checked against its streams and mapping, and the problems
/// that don't stop it being shown.
pub(crate) struct Prepared {
    pub(crate) robot: RobotModel,
    pub(crate) kinematics: Kinematics,
    pub(crate) map: JointMap,
    /// The streams, as declared.
    pub(crate) streams: Vec<StreamDeclaration>,
    /// Which decoded value drives which joint, as declared.
    pub(crate) mapping: MappingDeclaration,
    /// Meshes that aren't there; [`Prepared::warnings`] has them all.
    missing_meshes: Vec<KaptoError>,
}

impl Prepared {
    /// Problems that don't stop the robot being shown: meshes that aren't
    /// there, joints nothing drives.
    pub(crate) fn warnings(&self) -> impl Iterator<Item = &KaptoError> {
        self.missing_meshes.iter().chain(self.map.warnings())
    }
}

/// Loads the robot, its streams and its mapping, and checks them against
/// each other.
///
/// # Errors
///
/// Every problem that stops the robot being shown.
pub(crate) fn prepare(
    robot: &Path,
    streams: &Path,
    mapping: &Path,
    packages: &Path,
) -> Result<Prepared, Vec<KaptoError>> {
    let model = load_urdf(robot, &[packages.to_path_buf()])?;
    let missing_meshes: Vec<KaptoError> = model
        .missing_files()
        .map(|(link, file)| {
            KaptoError::new(ErrorCode::RobotMeshUnreadable)
                .detail(format!("{} isn't there, so it isn't shown", file.uri))
                .context("link", link.name.clone())
        })
        .collect();
    let kinematics = Kinematics::new(&model).map_err(|error| vec![error])?;
    let streams = parse_streams(&read(streams)?)?;
    let declarations = read_mapping(mapping)?;
    let map = JointMap::new(&model, &kinematics, &streams, &declarations, mapping)?;
    Ok(Prepared {
        robot: model,
        kinematics,
        map,
        streams,
        mapping: declarations,
        missing_meshes,
    })
}

fn read(path: &Path) -> Result<ConfigFile, Vec<KaptoError>> {
    read_config_file(path, true)
        .map_err(|error| vec![error])?
        .ok_or_else(|| {
            vec![
                KaptoError::new(ErrorCode::ConfigFileNotFound)
                    .context("file", path.display().to_string()),
            ]
        })
}

fn read_mapping(path: &Path) -> Result<MappingDeclaration, Vec<KaptoError>> {
    parse_mapping(&read(path)?)
}

/// A problem's report, as a warning.
pub(crate) fn warning(problem: &KaptoError) -> String {
    problem.warning_report()
}
