// SPDX-License-Identifier: Apache-2.0

//! How the live view and replays are arranged: columns side by side, each a
//! stack of panels, from a package's `layout.toml`.
//!
//! ```toml
//! [[columns]]
//! width = 3
//! panels = [
//!   { kind = "robot", height = 3 },
//!   { kind = "camera", stream = "wrist_camera" },
//! ]
//! ```
//!
//! This module only reads the file; whether the streams, fields and roles
//! it names are the package's is checked against the package, and a panel
//! naming what the package lacks is shown as a placeholder rather than
//! refused.

use std::fmt::{self, Write as _};
use std::str::FromStr;

use kapto_core::{ErrorCode, KaptoError, Role};
use toml_edit::{Document, InlineTable, TableLike};

use crate::file::ConfigFile;
use crate::located::{at_key, syntax_error};
use crate::mapping::FieldReference;
use crate::reader::{FileKey, Reader};

/// A package's layout, beside its `package.toml` unless that names another.
pub const LAYOUT_FILE: &str = "layout.toml";

/// The most panels a layout may have: beyond, the page can't keep its frame
/// rate.
pub const MAX_PANELS: usize = 24;

/// The most fields one plot may draw, for the same reason.
pub const MAX_PLOT_FIELDS: usize = 16;

/// The largest relative width or height.
const MAX_SHARE: i64 = 12;

/// An example of the file, as the reference shows it.
pub const LAYOUT_EXAMPLE: &str = "[[columns]]
width = 3
panels = [
  { kind = \"robot\", height = 3 },
  { kind = \"camera\", stream = \"wrist_camera\" },
]

[[columns]]
width = 2
panels = [
  { kind = \"plot\", fields = [\"arm_joints.joint_positions\"], height = 2 },
  { kind = \"plot\", fields = [\"role:gripper.opening\"] },
  { kind = \"health\" },
]
";

/// Every key of a column, in the order they are documented.
pub const LAYOUT_COLUMN_KEYS: [FileKey; 2] = [
    FileKey {
        name: "width",
        required: false,
        help: "Its width relative to the other columns', a whole number from 1 to 12. 1 if not given.",
    },
    FileKey {
        name: "panels",
        required: true,
        help: "Its panels from top to bottom, each an inline table with a `kind`.",
    },
];

/// Every key of a panel, in the order they are documented.
pub const LAYOUT_PANEL_KEYS: [FileKey; 4] = [
    FileKey {
        name: "kind",
        required: true,
        help: "What it shows: `robot` (the 3D view), `camera` (one camera stream), `plot` (fields over time), `health` (the streams' counts) or `events` (the timeline's marks as a list).",
    },
    FileKey {
        name: "height",
        required: false,
        help: "Its height relative to the other panels' in its column, a whole number from 1 to 12. 1 if not given.",
    },
    FileKey {
        name: "stream",
        required: false,
        help: "A `camera` panel's stream, by its ID. Only for `camera`, which needs it.",
    },
    FileKey {
        name: "fields",
        required: false,
        help: "A `plot` panel's fields, 1 to 16: each a stream's field, `stream.field` or `stream.field[3]`, or a role, `role:gripper.opening`. Only for `plot`, which needs them.",
    },
];

/// A whole layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Its columns, left to right.
    pub columns: Vec<Column>,
}

/// A column of panels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    /// Its width relative to the other columns'.
    pub width: u8,
    /// Its panels, top to bottom.
    pub panels: Vec<Panel>,
}

/// One panel of a column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panel {
    /// What it shows.
    pub kind: PanelKind,
    /// Its height relative to the other panels' in its column.
    pub height: u8,
}

/// What a panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelKind {
    /// The robot in 3D.
    Robot,
    /// One camera stream.
    Camera {
        /// The stream's ID.
        stream: String,
    },
    /// Fields over time.
    Plot {
        /// What it draws.
        fields: Vec<PlotField>,
    },
    /// The streams' counts.
    Health,
    /// The timeline's marks as a list.
    Events,
}

/// One thing a plot draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlotField {
    /// A stream's field, as the package's decoder names it.
    Field(FieldReference),
    /// Whatever field the package's mapping gives the role.
    Role(Role),
}

impl fmt::Display for PlotField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field(field) => write!(f, "{field}"),
            Self::Role(role) => write!(f, "role:{}", role.as_str()),
        }
    }
}

impl Layout {
    /// The layout as `layout.toml` holds it, which reads back the same.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut text = String::new();
        for (index, column) in self.columns.iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            let _ = write!(text, "[[columns]]\nwidth = {}\npanels = [\n", column.width);
            for panel in &column.panels {
                let what = match &panel.kind {
                    PanelKind::Robot => "kind = \"robot\"".to_owned(),
                    PanelKind::Camera { stream } => {
                        format!("kind = \"camera\", stream = \"{stream}\"")
                    }
                    PanelKind::Plot { fields } => {
                        let fields: Vec<String> =
                            fields.iter().map(|field| format!("\"{field}\"")).collect();
                        format!("kind = \"plot\", fields = [{}]", fields.join(", "))
                    }
                    PanelKind::Health => "kind = \"health\"".to_owned(),
                    PanelKind::Events => "kind = \"events\"".to_owned(),
                };
                let _ = writeln!(text, "  {{ {what}, height = {} }},", panel.height);
            }
            text.push_str("]\n");
        }
        text
    }
}

impl FromStr for PlotField {
    type Err = ();

    /// `stream.field`, `stream.field[3]` or `role:gripper.opening`.
    fn from_str(text: &str) -> Result<Self, ()> {
        match text.strip_prefix("role:") {
            Some(role) => role.parse().map(Self::Role).map_err(|_| ()),
            None => text.parse().map(Self::Field),
        }
    }
}

/// Reads the layout in `file`.
///
/// # Errors
///
/// Every problem found, each with the file and line: invalid TOML
/// (`config.invalid_syntax`), unknown keys (`layout.unknown_key`), and
/// missing, invalid or too many values (`config.invalid_value`).
pub fn parse_layout(file: &ConfigFile) -> Result<Layout, Vec<KaptoError>> {
    let document = Document::parse(file.contents.as_str())
        .map_err(|error| vec![syntax_error(file, &error)])?;
    let root = document.as_table();
    let mut top = Reader::new(file, root);
    top.check_keys(
        &[FileKey {
            name: "columns",
            required: true,
            help: "",
        }],
        ErrorCode::LayoutUnknownKey,
        &format!("`{LAYOUT_FILE}`"),
    );
    let mut errors = top.errors;
    let tables = root.get("columns").map(|item| item.as_array_of_tables());
    let columns = match tables {
        Some(Some(tables)) if !tables.is_empty() => tables
            .iter()
            .filter_map(|table| column(file, table, &mut errors))
            .collect(),
        Some(_) => {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
                .detail("`columns` must be one or more `[[columns]]` tables".to_owned());
            errors.push(at_key(error, file, root, "columns"));
            Vec::new()
        }
        None => Vec::new(),
    };
    let layout = Layout { columns };
    let panels: usize = layout.columns.iter().map(|each| each.panels.len()).sum();
    if panels > MAX_PANELS {
        let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
            "the layout has {panels} panels, more than the {MAX_PANELS} a page can draw"
        ));
        errors.push(at_key(error, file, root, "columns"));
    }
    if errors.is_empty() {
        Ok(layout)
    } else {
        Err(errors)
    }
}

/// One `[[columns]]` table, or `None` with its problems added to `errors`.
fn column(
    file: &ConfigFile,
    table: &dyn TableLike,
    errors: &mut Vec<KaptoError>,
) -> Option<Column> {
    let mut reader = Reader::new(file, table);
    reader.check_keys(&LAYOUT_COLUMN_KEYS, ErrorCode::LayoutUnknownKey, "a column");
    let width = reader.whole("width", 1..=MAX_SHARE);
    let mut panels = Vec::new();
    match table.get("panels").map(|item| item.as_array()) {
        Some(Some(list)) if !list.is_empty() => {
            for value in list {
                if let Some(inline) = value.as_inline_table() {
                    panels.extend(panel(file, inline, &mut reader.errors));
                } else {
                    let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(
                        "each of `panels` must be an inline table, e.g. `{ kind = \"robot\" }`"
                            .to_owned(),
                    );
                    reader.located(error, "panels");
                }
            }
        }
        Some(_) => {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
                .detail("`panels` must be a list of one or more panels".to_owned());
            reader.located(error, "panels");
        }
        None => {}
    }
    let clean = reader.errors.is_empty();
    errors.append(&mut reader.errors);
    clean.then(|| Column {
        width: share(width),
        panels,
    })
}

/// One panel's inline table, or `None` with its problems added to `errors`.
fn panel(file: &ConfigFile, table: &InlineTable, errors: &mut Vec<KaptoError>) -> Option<Panel> {
    let mut reader = Reader::new(file, table);
    reader.check_keys(&LAYOUT_PANEL_KEYS, ErrorCode::LayoutUnknownKey, "a panel");
    let height = reader.whole("height", 1..=MAX_SHARE);
    let kind = reader.text(
        "kind",
        "one of \"robot\", \"camera\", \"plot\", \"health\" or \"events\"",
        |kind| ["robot", "camera", "plot", "health", "events"].contains(&kind),
    );
    let stream = reader.text("stream", "a stream's ID", kapto_core::name::is_name);
    let fields = reader.texts(
        "fields",
        "fields, `stream.field` or `role:name.of.role`",
        |text| text.parse::<PlotField>().is_ok(),
    );
    let only_for = |key: &str, kind: &str| {
        KaptoError::new(ErrorCode::ConfigInvalidValue)
            .detail(format!("`{key}` is only for a `{kind}` panel"))
    };
    let needs = |key: &str, kind: &str| {
        KaptoError::new(ErrorCode::ConfigInvalidValue)
            .detail(format!("a `{kind}` panel needs `{key}`"))
    };
    let named = kind.as_deref();
    for (key, belongs) in [("stream", "camera"), ("fields", "plot")] {
        if named.is_some_and(|kind| kind != belongs) && table.get(key).is_some() {
            reader.located(only_for(key, belongs), key);
        }
    }
    let kind = match named {
        Some("camera") => match stream {
            Some(stream) => Some(PanelKind::Camera { stream }),
            None if table.get("stream").is_none() => {
                reader.located(needs("stream", "camera"), "kind");
                None
            }
            None => None,
        },
        Some("plot") if fields.len() > MAX_PLOT_FIELDS => {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                "a plot draws at most {MAX_PLOT_FIELDS} fields, not {}",
                fields.len()
            ));
            reader.located(error, "fields");
            None
        }
        Some("plot") if fields.is_empty() => {
            if table.get("fields").is_none() {
                reader.located(needs("fields", "plot"), "kind");
            }
            None
        }
        Some("plot") => Some(PanelKind::Plot {
            fields: fields.iter().filter_map(|text| text.parse().ok()).collect(),
        }),
        Some("robot") => Some(PanelKind::Robot),
        Some("health") => Some(PanelKind::Health),
        Some("events") => Some(PanelKind::Events),
        _ => None,
    };
    let clean = reader.errors.is_empty();
    errors.append(&mut reader.errors);
    kind.filter(|_| clean).map(|kind| Panel {
        kind,
        height: share(height),
    })
}

/// A relative width or height read, 1 if absent.
fn share(read: Option<i64>) -> u8 {
    read.and_then(|whole| u8::try_from(whole).ok()).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn file(contents: &str) -> ConfigFile {
        ConfigFile {
            path: PathBuf::from("packages/arm7/layout.toml"),
            contents: contents.to_owned(),
        }
    }

    /// Each problem as `code line: detail`.
    fn problems(contents: &str) -> Vec<String> {
        parse_layout(&file(contents))
            .unwrap_err()
            .iter()
            .map(|error| {
                let line = error
                    .context_entries()
                    .iter()
                    .find(|(key, _)| *key == "line")
                    .map(|(_, line)| line.clone())
                    .unwrap_or_default();
                let message = error.message();
                let detail = message
                    .split_once(": ")
                    .map_or(message.as_str(), |(_, detail)| detail);
                format!("{} {line}: {detail}", error.code().id())
            })
            .collect()
    }

    #[test]
    fn the_documented_example_reads_as_two_columns_of_panels() {
        let layout = parse_layout(&file(LAYOUT_EXAMPLE)).unwrap();
        assert_eq!(layout.columns.len(), 2);
        assert_eq!(layout.columns[0].width, 3);
        assert_eq!(
            layout.columns[0].panels,
            [
                Panel {
                    kind: PanelKind::Robot,
                    height: 3
                },
                Panel {
                    kind: PanelKind::Camera {
                        stream: "wrist_camera".to_owned()
                    },
                    height: 1
                },
            ]
        );
        let plots = &layout.columns[1].panels;
        assert_eq!(
            plots[0].kind,
            PanelKind::Plot {
                fields: vec![PlotField::Field(
                    "arm_joints.joint_positions".parse().unwrap()
                )]
            }
        );
        assert_eq!(
            plots[1].kind,
            PanelKind::Plot {
                fields: vec![PlotField::Role("gripper.opening".parse().unwrap())]
            }
        );
        assert_eq!(plots[2].kind, PanelKind::Health);
    }

    #[test]
    fn each_mistake_is_reported_at_its_line() {
        let text = "[[columns]]
widht = 2
panels = [
  { kind = \"robot\" },
  { kind = \"camera\" },
  { kind = \"plot\", fields = [\"not a field\"] },
  { kind = \"robot\", height = 40 },
  { kind = \"map\" },
  { kind = \"health\", stream = \"arm_joints\" },
]
";
        assert_eq!(
            problems(text),
            [
                "layout.unknown_key 2: `widht` (did you mean `width`?)",
                "config.invalid_value 5: a `camera` panel needs `stream`",
                "config.invalid_value 6: `fields` must be a list of fields, `stream.field` or `role:name.of.role`",
                "config.invalid_value 7: `height` must be a whole number from 1 to 12",
                "config.invalid_value 8: `kind` must be one of \"robot\", \"camera\", \"plot\", \"health\" or \"events\"",
                "config.invalid_value 9: `stream` is only for a `camera` panel",
            ]
        );
    }

    #[test]
    fn a_layout_without_columns_or_panels_says_so() {
        assert_eq!(
            problems("# nothing yet\n"),
            ["config.invalid_value 1: `layout.toml` needs `columns`"]
        );
        assert_eq!(
            problems("[[columns]]\nwidth = 1\npanels = []\n"),
            ["config.invalid_value 3: `panels` must be a list of one or more panels"]
        );
        assert_eq!(
            problems("columns = 3\n"),
            ["config.invalid_value 1: `columns` must be one or more `[[columns]]` tables"]
        );
    }

    #[test]
    fn more_than_a_page_can_draw_is_refused() {
        let health = "{ kind = \"health\" }, ".repeat(MAX_PANELS + 1);
        assert_eq!(
            problems(&format!("[[columns]]\npanels = [{health}]\n")),
            ["config.invalid_value 1: the layout has 25 panels, more than the 24 a page can draw"]
        );
        let fields = "\"arm.f\", ".repeat(MAX_PLOT_FIELDS + 1);
        assert_eq!(
            problems(&format!(
                "[[columns]]\npanels = [{{ kind = \"plot\", fields = [{fields}] }}]\n"
            )),
            ["config.invalid_value 2: a plot draws at most 16 fields, not 17"]
        );
    }

    #[test]
    fn a_layout_written_out_reads_back_the_same() {
        let layout = parse_layout(&file(LAYOUT_EXAMPLE)).unwrap();
        assert_eq!(parse_layout(&file(&layout.to_toml())).unwrap(), layout);
    }

    #[test]
    fn a_plot_field_is_a_streams_field_or_a_role() {
        assert_eq!(
            "arm_joints.joint_positions[2]".parse::<PlotField>(),
            Ok(PlotField::Field(
                "arm_joints.joint_positions[2]".parse().unwrap()
            ))
        );
        assert!(matches!(
            "role:joints.joint_1.position".parse(),
            Ok(PlotField::Role(_))
        ));
        for wrong in ["role:gripper", "arm_joints", "role:", "arm joints.x"] {
            assert_eq!(wrong.parse::<PlotField>(), Err(()), "{wrong}");
        }
    }
}
