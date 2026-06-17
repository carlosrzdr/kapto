// SPDX-License-Identifier: Apache-2.0

//! The robot and its streams as the API shows them, worked out once from
//! what `kapto serve` loaded and checked when it started.

use std::path::Path;

use kapto_config::{
    DecoderDeclaration, FieldReference, MappingDeclaration, StreamDeclaration,
    TcpEndpointDeclaration,
};
use kapto_core::KaptoError;
use kapto_decode::PathFormat;
use kapto_robot::{Declared, JointKind, Package, RobotModel};
use kapto_server::api::ApiError;
use kapto_server::robot::{
    RobotField, RobotFiles, RobotJoint, RobotOverview, RobotStream, ViewLayout, arranged, roles_of,
};

use crate::viz_bridge::Prepared;

/// The overview of `prepared`, loaded from `files` (URDF, streams, mapping),
/// arranged by `package`'s layout if it is in one that has its own.
pub(crate) fn overview(
    prepared: &Prepared,
    [urdf, streams, mapping]: [&Path; 3],
    package: Option<&Package>,
) -> RobotOverview {
    let (layout, layout_problems) = view_layout(prepared, package);
    RobotOverview {
        name: prepared.robot.name.clone(),
        files: RobotFiles {
            urdf: urdf.display().to_string(),
            streams: streams.display().to_string(),
            mapping: mapping.display().to_string(),
        },
        links: prepared.robot.links.len(),
        joints: joints(&prepared.robot, &prepared.mapping),
        streams: prepared.streams.iter().map(stream).collect(),
        roles: roles_of(&prepared.mapping),
        layout,
        warnings: prepared
            .warnings()
            .chain(&layout_problems)
            .map(ApiError::from)
            .collect(),
    }
}

/// The live view's layout: the package's own if it has one that reads,
/// else one made from what is declared; and the problems of one that
/// doesn't read.
fn view_layout(prepared: &Prepared, package: Option<&Package>) -> (ViewLayout, Vec<KaptoError>) {
    let declared = Declared::new(Some(&prepared.robot), &prepared.streams, &prepared.mapping);
    arranged(package.map(Package::own_layout), &declared)
}

fn joints(robot: &RobotModel, mapping: &MappingDeclaration) -> Vec<RobotJoint> {
    let source = |reference: Option<&FieldReference>| reference.map(ToString::to_string);
    robot
        .joints
        .iter()
        .map(|joint| {
            let mapped = mapping
                .joints
                .iter()
                .find(|entry| entry.joint == joint.name);
            let finite = |value: f64| value.is_finite().then_some(value);
            // The description says 0 for a limit it doesn't know.
            let known = |value: f64| (value.is_finite() && value > 0.0).then_some(value);
            RobotJoint {
                name: joint.name.clone(),
                kind: kind(joint.kind).to_owned(),
                lower: joint.limits.and_then(|limits| finite(limits.lower)),
                upper: joint.limits.and_then(|limits| finite(limits.upper)),
                velocity: joint.limits.and_then(|limits| known(limits.velocity)),
                effort: joint.limits.and_then(|limits| known(limits.effort)),
                mimics: joint
                    .mimic
                    .and_then(|mimic| robot.joints.get(mimic.joint))
                    .map(|copied| copied.name.clone()),
                position_from: source(mapped.and_then(|entry| entry.position.as_ref())),
                velocity_from: source(mapped.and_then(|entry| entry.velocity.as_ref())),
                effort_from: source(mapped.and_then(|entry| entry.effort.as_ref())),
            }
        })
        .collect()
}

fn kind(kind: JointKind) -> &'static str {
    match kind {
        JointKind::Revolute => "revolute",
        JointKind::Continuous => "continuous",
        JointKind::Prismatic => "prismatic",
        JointKind::Fixed => "fixed",
        JointKind::Floating => "floating",
        JointKind::Planar => "planar",
        JointKind::Spherical => "spherical",
    }
}

fn stream(declaration: &StreamDeclaration) -> RobotStream {
    let input = match (&declaration.udp, &declaration.tcp, &declaration.ros2) {
        (Some(udp), _, _) => format!("udp {}", udp.bind),
        (None, Some(tcp), _) => match tcp.endpoint {
            TcpEndpointDeclaration::Listen(address) => format!("tcp listen {address}"),
            TcpEndpointDeclaration::Connect(address) => format!("tcp connect {address}"),
        },
        (None, None, Some(topic)) => format!("ros2 {}", topic.topic),
        (None, None, None) => "none".to_owned(),
    };
    let decoder = declaration.decoder.as_ref();
    RobotStream {
        name: declaration.id.to_string(),
        input,
        rate_hz: declaration.rate_hz,
        kept: declaration.kept_for_late_subscribers(),
        sequence: declaration.sequence.is_some(),
        decoder: decoder.map(|decoder| {
            match decoder {
                DecoderDeclaration::Struct(_) => "struct",
                DecoderDeclaration::Path(layout) => match layout.format {
                    PathFormat::Json => "json",
                    PathFormat::MsgPack => "msgpack",
                },
                DecoderDeclaration::Cdr(_) => "cdr",
            }
            .to_owned()
        }),
        fields: decoder
            .map(|decoder| {
                decoder
                    .fields()
                    .into_iter()
                    .map(|field| RobotField {
                        name: field.name,
                        ty: field.ty.name().to_owned(),
                        count: field.count,
                        unit: field.unit,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}
