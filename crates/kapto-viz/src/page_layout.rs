// SPDX-License-Identifier: Apache-2.0

//! How the page arranges the live view or a replay: the layout chosen for
//! a package, or for a recording from the files it carries, each panel with
//! what it names that they lack, and the roles its plots may name.

use kapto_config::{Layout, MappingDeclaration, PanelKind};
use kapto_core::KaptoError;
use kapto_robot::{Declared, made_layout, missing};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A role and the decoded value that plays it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RobotRole {
    /// The role, e.g. `gripper.opening` or `joints.joint_1.position`.
    pub role: String,
    /// The value that plays it, e.g. `arm_joints.gripper` or
    /// `arm_joints.joint_positions[0]`.
    pub field: String,
}

/// How the live view or a replay is arranged: columns side by side, each a
/// stack of panels, as `docs/reference/package.md` describes `layout.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ViewLayout {
    /// Where it comes from: `package` (the package's `layout.toml`) or
    /// `made` (made from what the package declares).
    pub source: String,
    /// Its columns, left to right.
    pub columns: Vec<ViewColumn>,
}

/// A column of panels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ViewColumn {
    /// Its width relative to the other columns'.
    pub width: u8,
    /// Its panels, top to bottom.
    pub panels: Vec<ViewPanel>,
}

/// One panel of a column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ViewPanel {
    /// What it shows: `robot`, `camera`, `plot`, `health` or `events`.
    pub kind: String,
    /// Its height relative to the other panels' in its column.
    pub height: u8,
    /// A camera panel's stream.
    pub stream: Option<String>,
    /// A plot's fields, each `stream.field`, `stream.field[3]` or
    /// `role:gripper.opening`.
    pub fields: Vec<String>,
    /// What it names that the package doesn't have, each a phrase; shown in
    /// its place when not empty.
    pub missing: Vec<String>,
}

/// The layout to arrange by: `own` if it reads, else the one made from
/// `declared`; with the problems of an own layout that doesn't read.
#[must_use]
pub fn arranged(
    own: Option<Result<Option<Layout>, Vec<KaptoError>>>,
    declared: &Declared,
) -> (ViewLayout, Vec<KaptoError>) {
    let (own, problems) = match own {
        Some(Ok(own)) => (own, Vec::new()),
        Some(Err(problems)) => (None, problems),
        None => (None, Vec::new()),
    };
    let source = if own.is_some() { "package" } else { "made" };
    let layout = own.unwrap_or_else(|| made_layout(declared));
    (view_of(&layout, declared, source), problems)
}

/// Every role `mapping` gives a value, written roles first.
#[must_use]
pub fn roles_of(mapping: &MappingDeclaration) -> Vec<RobotRole> {
    mapping
        .all_roles()
        .into_iter()
        .map(|(role, field)| RobotRole {
            role: role.as_str().to_owned(),
            field: field.to_string(),
        })
        .collect()
}

/// `layout` as the page reads it, each panel with what it lacks of `declared`.
pub fn view_of(layout: &Layout, declared: &Declared, source: &str) -> ViewLayout {
    let lacking = missing(layout, declared);
    let columns = layout
        .columns
        .iter()
        .enumerate()
        .map(|(column, each)| ViewColumn {
            width: each.width,
            panels: each
                .panels
                .iter()
                .enumerate()
                .map(|(place, panel)| {
                    let (kind, stream, fields) = match &panel.kind {
                        PanelKind::Robot => ("robot", None, Vec::new()),
                        PanelKind::Camera { stream } => {
                            ("camera", Some(stream.clone()), Vec::new())
                        }
                        PanelKind::Plot { fields } => (
                            "plot",
                            None,
                            fields.iter().map(ToString::to_string).collect(),
                        ),
                        PanelKind::Health => ("health", None, Vec::new()),
                        PanelKind::Events => ("events", None, Vec::new()),
                    };
                    ViewPanel {
                        kind: kind.to_owned(),
                        height: panel.height,
                        stream,
                        fields,
                        missing: lacking
                            .iter()
                            .find(|found| found.column == column && found.panel == place)
                            .map_or_else(Vec::new, |found| found.what.clone()),
                    }
                })
                .collect(),
        })
        .collect();
    ViewLayout {
        source: source.to_owned(),
        columns,
    }
}
