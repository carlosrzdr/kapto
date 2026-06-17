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

/// A layout as the page sends it to be saved: what [`ViewLayout`] gives,
/// without what the API works out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LayoutInput {
    /// Its columns, left to right.
    pub columns: Vec<ColumnInput>,
}

/// A column of a layout to be saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ColumnInput {
    /// Its width relative to the other columns', from 1 to 12.
    pub width: u8,
    /// Its panels, top to bottom.
    pub panels: Vec<PanelInput>,
}

/// A panel of a layout to be saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PanelInput {
    /// `robot`, `camera`, `plot`, `health` or `events`.
    pub kind: String,
    /// Its height relative to the other panels' in its column, from 1 to 12.
    pub height: u8,
    /// A camera panel's stream.
    #[serde(default)]
    pub stream: Option<String>,
    /// A plot's fields: `stream.field`, `stream.field[3]` or
    /// `role:gripper.opening`.
    #[serde(default)]
    pub fields: Vec<String>,
}

impl LayoutInput {
    /// The layout as `layout.toml` would hold it, checked by the same rules
    /// as `file` would be.
    ///
    /// # Errors
    ///
    /// Every problem, as reading the file would give it.
    pub fn to_layout(&self, file: &std::path::Path) -> Result<Layout, Vec<KaptoError>> {
        let mut problems = Vec::new();
        let columns = self
            .columns
            .iter()
            .map(|column| kapto_config::Column {
                width: column.width,
                panels: column
                    .panels
                    .iter()
                    .filter_map(|panel| {
                        let kind = match panel.kind.as_str() {
                            "robot" => PanelKind::Robot,
                            "camera" => PanelKind::Camera {
                                stream: panel.stream.clone().unwrap_or_default(),
                            },
                            "plot" => PanelKind::Plot {
                                fields: panel
                                    .fields
                                    .iter()
                                    .filter_map(|field| {
                                        let parsed = field.parse().ok();
                                        if parsed.is_none() {
                                            problems.push(invalid(format!(
                                                "`{field}` isn't a field, `stream.field` or `role:name.of.role`"
                                            )));
                                        }
                                        parsed
                                    })
                                    .collect(),
                            },
                            "health" => PanelKind::Health,
                            "events" => PanelKind::Events,
                            other => {
                                problems.push(invalid(format!(
                                    "`{other}` isn't a kind of panel: robot, camera, plot, health or events"
                                )));
                                return None;
                            }
                        };
                        Some(kapto_config::Panel {
                            kind,
                            height: panel.height,
                        })
                    })
                    .collect(),
            })
            .collect();
        if !problems.is_empty() {
            return Err(problems);
        }
        // Written out and read back, so a layout saved is held to every rule
        // a layout.toml is.
        let text = Layout { columns }.to_toml();
        kapto_config::parse_layout(&kapto_config::ConfigFile {
            path: file.to_owned(),
            contents: text,
        })
    }
}

fn invalid(detail: String) -> KaptoError {
    KaptoError::new(kapto_core::ErrorCode::ConfigInvalidValue).detail(detail)
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
