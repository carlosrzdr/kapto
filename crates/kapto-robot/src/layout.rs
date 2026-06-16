// SPDX-License-Identifier: Apache-2.0

//! The layout a package is shown with when it has none of its own, made
//! from what it declares, and what a layout names that its package lacks.

use kapto_config::{
    Column, FieldReference, Layout, MAX_PANELS, MAX_PLOT_FIELDS, MappingDeclaration, Panel,
    PanelKind, PlotField, StreamDeclaration,
};
use kapto_core::Role;

use crate::model::{JointKind, RobotModel};

/// What a layout can show of a package.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declared {
    /// Whether the robot has a joint that moves, so its 3D view changes.
    pub robot_moves: bool,
    /// Its streams, in the order declared.
    pub streams: Vec<DeclaredStream>,
    /// Every role the mapping gives a field, written roles first.
    pub roles: Vec<(Role, FieldReference)>,
}

/// One stream, as a layout sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredStream {
    /// Its ID.
    pub id: String,
    /// Whether its messages are a camera's frames.
    pub camera: bool,
    /// Its decoded fields, each with how many values it holds; none without
    /// a decoder.
    pub fields: Vec<(String, usize)>,
    /// Those of its fields that aren't plotted: counters and clocks.
    pub unplotted: Vec<String>,
}

impl Declared {
    /// What `robot`, `streams` and `mapping` declare; a robot that couldn't
    /// be read is taken not to move.
    #[must_use]
    pub fn new(
        robot: Option<&RobotModel>,
        streams: &[StreamDeclaration],
        mapping: &MappingDeclaration,
    ) -> Self {
        Self {
            robot_moves: robot.is_some_and(|robot| {
                robot
                    .joints
                    .iter()
                    .any(|joint| joint.kind != JointKind::Fixed)
            }),
            streams: streams
                .iter()
                .map(|stream| DeclaredStream {
                    id: stream.id.as_str().to_owned(),
                    camera: stream.is_camera(),
                    fields: stream.decoder.as_ref().map_or_else(Vec::new, |decoder| {
                        decoder
                            .fields()
                            .into_iter()
                            .map(|field| (field.name, field.count))
                            .collect()
                    }),
                    unplotted: stream.unplotted_fields(),
                })
                .collect(),
            roles: mapping.all_roles(),
        }
    }

    fn stream(&self, id: &str) -> Option<&DeclaredStream> {
        self.streams.iter().find(|stream| stream.id == id)
    }
}

/// The layout made from what a package declares: the robot on the left if
/// it moves; on the right each camera, a plot per group of roles (the
/// joints' positions first), a plot per stream of its fields no role
/// covers, and the streams' health. The same declarations always give the
/// same layout, and never more panels than a page can draw.
#[must_use]
pub fn made_layout(declared: &Declared) -> Layout {
    let robot = usize::from(declared.robot_moves);
    let mut side: Vec<Panel> = declared
        .streams
        .iter()
        .filter(|stream| stream.camera)
        .map(|stream| PanelKind::Camera {
            stream: stream.id.clone(),
        })
        .chain(role_plots(&declared.roles))
        .chain(field_plots(declared))
        .map(panel)
        .collect();
    // Room for the health panel, which comes last.
    side.truncate(MAX_PANELS - robot - 1);
    side.push(panel(PanelKind::Health));
    let mut columns = Vec::new();
    if declared.robot_moves {
        columns.push(Column {
            width: 3,
            panels: vec![panel(PanelKind::Robot)],
        });
    }
    columns.push(Column {
        width: 2,
        panels: side,
    });
    Layout { columns }
}

fn panel(kind: PanelKind) -> Panel {
    Panel { kind, height: 1 }
}

/// A plot per group of roles: the joints' positions, velocities and
/// efforts, then the other roles by their first segment, in the order they
/// first appear; each split in plots of at most [`MAX_PLOT_FIELDS`].
fn role_plots(roles: &[(Role, FieldReference)]) -> Vec<PanelKind> {
    let group = |role: &Role| -> String {
        let segments: Vec<&str> = role.segments().collect();
        match segments.as_slice() {
            ["joints", .., quantity] => format!("joints.*.{quantity}"),
            [first, ..] => (*first).to_owned(),
            [] => String::new(),
        }
    };
    let mut groups: Vec<String> = ["position", "velocity", "effort"]
        .map(|quantity| format!("joints.*.{quantity}"))
        .to_vec();
    for (role, _) in roles {
        let name = group(role);
        if !groups.contains(&name) {
            groups.push(name);
        }
    }
    groups
        .iter()
        .flat_map(|name| {
            let fields: Vec<PlotField> = roles
                .iter()
                .filter(|(role, _)| group(role) == *name)
                .map(|(role, _)| PlotField::Role(role.clone()))
                .collect();
            plots_of(&fields)
        })
        .collect()
}

/// A plot per decoded stream that isn't a camera, of its plotted fields no
/// role covers.
fn field_plots(declared: &Declared) -> Vec<PanelKind> {
    declared
        .streams
        .iter()
        .filter(|stream| !stream.camera)
        .flat_map(|stream| {
            let fields: Vec<PlotField> = stream
                .fields
                .iter()
                .filter(|(field, _)| {
                    !stream.unplotted.contains(field)
                        && !declared
                            .roles
                            .iter()
                            .any(|(_, source)| source.stream == stream.id && source.field == *field)
                })
                .map(|(field, _)| {
                    PlotField::Field(FieldReference {
                        stream: stream.id.clone(),
                        field: field.clone(),
                        index: None,
                    })
                })
                .collect();
            plots_of(&fields)
        })
        .collect()
}

fn plots_of(fields: &[PlotField]) -> Vec<PanelKind> {
    fields
        .chunks(MAX_PLOT_FIELDS)
        .map(|fields| PanelKind::Plot {
            fields: fields.to_vec(),
        })
        .collect()
}

/// A panel of a layout naming what its package doesn't have, to be shown
/// as a placeholder saying so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    /// Its column, counting from 0.
    pub column: usize,
    /// Its place in the column, counting from 0.
    pub panel: usize,
    /// What it names that the package lacks, each as a phrase.
    pub what: Vec<String>,
}

/// Every panel of `layout` naming a stream, field or role `declared`
/// doesn't have.
#[must_use]
pub fn missing(layout: &Layout, declared: &Declared) -> Vec<Missing> {
    let mut found = Vec::new();
    for (column, each) in layout.columns.iter().enumerate() {
        for (place, panel) in each.panels.iter().enumerate() {
            let what = lacking(&panel.kind, declared);
            if !what.is_empty() {
                found.push(Missing {
                    column,
                    panel: place,
                    what,
                });
            }
        }
    }
    found
}

/// What one panel names that `declared` doesn't have.
fn lacking(kind: &PanelKind, declared: &Declared) -> Vec<String> {
    match kind {
        PanelKind::Camera { stream } => match declared.stream(stream) {
            None => vec![format!("no stream `{stream}`")],
            Some(found) if !found.camera => vec![format!("`{stream}` isn't a camera")],
            Some(_) => Vec::new(),
        },
        PanelKind::Plot { fields } => fields
            .iter()
            .filter_map(|field| match field {
                PlotField::Role(role) => (!declared.roles.iter().any(|(known, _)| known == role))
                    .then(|| format!("no field plays `{}`", role.as_str())),
                PlotField::Field(field) => lacking_field(field, declared),
            })
            .collect(),
        PanelKind::Robot | PanelKind::Health | PanelKind::Events => Vec::new(),
    }
}

fn lacking_field(field: &FieldReference, declared: &Declared) -> Option<String> {
    let Some(stream) = declared.stream(&field.stream) else {
        return Some(format!("no stream `{}`", field.stream));
    };
    let Some((_, count)) = stream.fields.iter().find(|(name, _)| *name == field.field) else {
        return Some(format!(
            "`{}` decodes no field `{}`",
            field.stream, field.field
        ));
    };
    if stream.unplotted.contains(&field.field) {
        return Some(format!(
            "`{}.{}` only ever climbs, so isn't plotted",
            field.stream, field.field
        ));
    }
    match field.index {
        Some(index) if index >= *count => Some(format!(
            "`{}.{}` holds {count} values, so no `[{index}]`",
            field.stream, field.field
        )),
        _ => None,
    }
}
