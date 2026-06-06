// SPDX-License-Identifier: Apache-2.0

//! Loading a URDF file into a [`RobotModel`].
//!
//! The XML is parsed by `roxmltree`, which refuses document type
//! declarations, so a hostile file can't expand entities. This module reads
//! the URDF elements Kapto uses (links, their shapes and materials, joints),
//! ignores the rest (`<gazebo>`, `<transmission>`, inertia, …), checks that
//! the joints form one tree, and finds the mesh and texture files. Every
//! problem is reported at once, each with its cause code and line.

mod files;
mod tree;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use kapto_core::{ErrorCode, KaptoError};
use roxmltree::{Document, Node};

use crate::model::{Geometry, JointKind, Link, Material, Pose, RobotModel, Shape};
use files::Finder;

/// Reads the URDF file at `path`. Mesh and texture files are looked up as
/// [`parse_urdf`] describes.
///
/// # Errors
///
/// `robot.urdf_unreadable` if the file can't be read; otherwise as
/// [`parse_urdf`].
pub fn load_urdf(path: &Path, search_paths: &[PathBuf]) -> Result<RobotModel, Vec<KaptoError>> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        vec![
            KaptoError::new(ErrorCode::RobotUrdfUnreadable)
                .detail(error.to_string())
                .context("file", path.display().to_string())
                .source(error),
        ]
    })?;
    parse_urdf(&text, path, search_paths)
}

/// Builds the model from URDF `text`, as if read from the file at `path`.
///
/// Files are found the way ROS finds them. A relative path is relative to
/// the URDF's folder. `package://<name>/<rest>` is looked up in each of
/// `search_paths`, then in each folder above the URDF: as `<folder>/<name>/
/// <rest>`, or as `<folder>/<rest>` if the folder is itself called `<name>`.
/// A file that isn't found is kept with no path, so the model still loads;
/// [`RobotModel::missing_files`] lists them.
///
/// ```
/// use std::path::Path;
///
/// use kapto_robot::{JointKind, parse_urdf};
///
/// let urdf = r#"
///     <robot name="pendulum">
///       <link name="base"/>
///       <link name="arm"><visual><geometry><box size="0.1 0.1 1"/></geometry></visual></link>
///       <joint name="swing" type="revolute">
///         <parent link="base"/><child link="arm"/>
///         <axis xyz="0 1 0"/>
///         <limit lower="-1.5" upper="1.5" effort="10" velocity="2"/>
///       </joint>
///     </robot>"#;
/// let robot = parse_urdf(urdf, Path::new("pendulum.urdf"), &[]).unwrap();
/// assert_eq!(robot.links[robot.root].name, "base");
/// assert_eq!(robot.joints[0].kind, JointKind::Revolute);
/// ```
///
/// # Errors
///
/// `robot.urdf_invalid` for XML that isn't a URDF document or a value that
/// can't be read, and `robot.kinematic_tree_invalid` for each way the links
/// and joints fail to form one tree: unknown or repeated names, a link with
/// two parents, several roots or a loop, a moving joint without an axis,
/// limits the wrong way round, or a mimic of an unknown joint. Each error
/// gives the file and, where it applies, the line.
pub fn parse_urdf(
    text: &str,
    path: &Path,
    search_paths: &[PathBuf],
) -> Result<RobotModel, Vec<KaptoError>> {
    let file = path.display().to_string();
    let in_file = |error: KaptoError| error.context("file", file.clone());
    let declared: String;
    let document = match Document::parse(text) {
        Err(roxmltree::Error::UnknownNamespace(..)) => {
            declared = with_prefixes_declared(text);
            Document::parse(&declared)
        }
        parsed => parsed,
    }
    .map_err(|error| {
        let line = error.pos().row.to_string();
        vec![in_file(
            KaptoError::new(ErrorCode::RobotUrdfInvalid)
                .detail(error.to_string())
                .context("line", line),
        )]
    })?;
    let mut reader = Reader {
        document: &document,
        finder: Finder::new(path, search_paths),
        materials: HashMap::new(),
        errors: Vec::new(),
    };
    let model = reader.robot();
    match model {
        Some(model) if reader.errors.is_empty() => Ok(model),
        _ => Err(reader.errors.into_iter().map(in_file).collect()),
    }
}

/// The most undeclared prefixes declared before giving up.
const MAX_UNDECLARED_PREFIXES: usize = 32;

/// `text` with every namespace prefix it uses but never declares declared
/// on its `<robot>` element. Descriptions carry Gazebo extensions written
/// for parsers that ignore namespaces (e.g. `<sensor:camera>`), which
/// urdfdom reads; XML proper refuses them. Kapto never reads those
/// elements, so any namespace will do. Lines stay where they were.
fn with_prefixes_declared(text: &str) -> String {
    let mut declared = text.to_owned();
    for _ in 0..MAX_UNDECLARED_PREFIXES {
        let Err(roxmltree::Error::UnknownNamespace(prefix, _)) = Document::parse(&declared) else {
            break;
        };
        let Some(robot) = declared
            .match_indices("<robot")
            .map(|(at, tag)| at + tag.len())
            .find(|end| {
                declared[*end..]
                    .chars()
                    .next()
                    .is_some_and(|next| next.is_whitespace() || next == '>' || next == '/')
            })
        else {
            break;
        };
        declared.insert_str(
            robot,
            &format!(" xmlns:{prefix}=\"urn:kapto:undeclared:{prefix}\""),
        );
    }
    declared
}

/// A joint as written, before its links and mimicked joint are looked up.
struct RawJoint {
    name: String,
    kind: JointKind,
    parent: String,
    child: String,
    origin: Pose,
    axis: [f64; 3],
    /// Lower, upper, velocity, effort.
    limits: Option<[f64; 4]>,
    /// The mimicked joint's name, multiplier and offset.
    mimic: Option<(String, f64, f64)>,
    line: u32,
}

struct Reader<'a, 'input> {
    document: &'a Document<'input>,
    finder: Finder<'a>,
    /// Materials declared at the top level, which shapes may refer to by name.
    materials: HashMap<String, Material>,
    errors: Vec<KaptoError>,
}

impl<'a, 'input> Reader<'a, 'input> {
    fn robot(&mut self) -> Option<RobotModel> {
        let robot = self.document.root_element();
        if robot.tag_name().name() != "robot" {
            let detail = format!(
                "the top element is <{}>, but a URDF starts with <robot>",
                robot.tag_name().name()
            );
            self.invalid(robot, detail);
            return None;
        }
        // Materials first: shapes may name one declared further down.
        for node in elements(robot, "material") {
            if let Some(name) = self.required(node, "name") {
                let material = self.material(node);
                self.materials.insert(name.to_owned(), material);
            }
        }
        let links: Vec<(Link, u32)> = elements(robot, "link")
            .filter_map(|node| Some((self.link(node)?, self.line(node))))
            .collect();
        let joints: Vec<RawJoint> = elements(robot, "joint")
            .filter_map(|node| self.joint(node))
            .collect();
        let name = robot.attribute("name").unwrap_or_default().to_owned();
        let complete = self.errors.is_empty();
        tree::build(name, links, &joints, complete, &mut self.errors)
    }

    fn link(&mut self, node: Node<'a, 'input>) -> Option<Link> {
        let name = self.required(node, "name")?.to_owned();
        let mut visuals = Vec::new();
        for visual in elements(node, "visual") {
            visuals.extend(self.shape(visual, true));
        }
        let mut collisions = Vec::new();
        for collision in elements(node, "collision") {
            collisions.extend(self.shape(collision, false));
        }
        Some(Link {
            name,
            visuals,
            collisions,
        })
    }

    fn shape(&mut self, node: Node<'a, 'input>, with_material: bool) -> Option<Shape> {
        let origin = self.origin(node);
        let material = if with_material {
            child(node, "material").map(|material| self.shape_material(material))
        } else {
            None
        };
        let Some(geometry) = child(node, "geometry") else {
            let detail = format!("<{}> needs a <geometry>", node.tag_name().name());
            self.invalid(node, detail);
            return None;
        };
        Some(Shape {
            name: node.attribute("name").map(str::to_owned),
            origin: origin?,
            geometry: self.geometry(geometry)?,
            material,
        })
    }

    fn geometry(&mut self, node: Node<'a, 'input>) -> Option<Geometry> {
        let Some(shape) = node.children().find(Node::is_element) else {
            self.invalid(
                node,
                "<geometry> needs a <box>, <cylinder>, <capsule>, <sphere> or <mesh>".to_owned(),
            );
            return None;
        };
        let geometry = match shape.tag_name().name() {
            "box" => Geometry::Box {
                size: self.numbers(shape, "size")?,
            },
            "cylinder" => Geometry::Cylinder {
                radius: self.number(shape, "radius")?,
                length: self.number(shape, "length")?,
            },
            "capsule" => Geometry::Capsule {
                radius: self.number(shape, "radius")?,
                length: self.number(shape, "length")?,
            },
            "sphere" => Geometry::Sphere {
                radius: self.number(shape, "radius")?,
            },
            "mesh" => {
                let filename = self.required(shape, "filename")?;
                Geometry::Mesh {
                    file: self.finder.resource(filename),
                    scale: match shape.attribute("scale") {
                        Some(_) => self.numbers(shape, "scale")?,
                        None => [1.0; 3],
                    },
                }
            }
            other => {
                let detail = format!(
                    "<{other}> isn't a shape: use <box>, <cylinder>, <capsule>, <sphere> or <mesh>"
                );
                self.invalid(shape, detail);
                return None;
            }
        };
        Some(geometry)
    }

    /// A shape's material. One given only by name is the top-level material
    /// of that name.
    fn shape_material(&mut self, node: Node<'a, 'input>) -> Material {
        let own = self.material(node);
        if own.rgba.is_none()
            && own.texture.is_none()
            && let Some(declared) = own.name.as_ref().and_then(|name| self.materials.get(name))
        {
            return Material {
                name: own.name.clone(),
                ..declared.clone()
            };
        }
        own
    }

    fn material(&mut self, node: Node<'a, 'input>) -> Material {
        let rgba = child(node, "color").and_then(|color| self.numbers(color, "rgba"));
        let texture = child(node, "texture")
            .and_then(|texture| self.required(texture, "filename"))
            .map(|file| self.finder.resource(file));
        Material {
            name: node
                .attribute("name")
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            rgba,
            texture,
        }
    }

    fn joint(&mut self, node: Node<'a, 'input>) -> Option<RawJoint> {
        let name = self.required(node, "name");
        let kind = self.required(node, "type").and_then(|kind| {
            let kind = match kind {
                "revolute" => JointKind::Revolute,
                "continuous" => JointKind::Continuous,
                "prismatic" => JointKind::Prismatic,
                "fixed" => JointKind::Fixed,
                "floating" => JointKind::Floating,
                "planar" => JointKind::Planar,
                "spherical" => JointKind::Spherical,
                other => {
                    let detail = format!(
                        "joint type `{other}` isn't one of revolute, continuous, prismatic, fixed, floating, planar or spherical"
                    );
                    self.invalid(node, detail);
                    return None;
                }
            };
            Some(kind)
        });
        let parent = self.link_name(node, "parent");
        let child_link = self.link_name(node, "child");
        let origin = self.origin(node);
        let axis = match child(node, "axis") {
            Some(axis) => self.numbers(axis, "xyz"),
            // The URDF default.
            None => Some([1.0, 0.0, 0.0]),
        };
        let limits = match kind? {
            JointKind::Revolute | JointKind::Prismatic => Some(self.limits(node, name?)?),
            _ => None,
        };
        let mimic = match child(node, "mimic") {
            Some(mimic) => Some((
                self.required(mimic, "joint")?.to_owned(),
                self.optional_number(mimic, "multiplier", 1.0)?,
                self.optional_number(mimic, "offset", 0.0)?,
            )),
            None => None,
        };
        Some(RawJoint {
            name: name?.to_owned(),
            kind: kind?,
            parent: parent?,
            child: child_link?,
            origin: origin?,
            axis: axis?,
            limits,
            mimic,
            line: self.line(node),
        })
    }

    /// Lower, upper, velocity and effort of a revolute or prismatic joint,
    /// which needs a `<limit>`.
    fn limits(&mut self, node: Node<'a, 'input>, name: &str) -> Option<[f64; 4]> {
        let Some(limit) = child(node, "limit") else {
            self.invalid(
                node,
                format!("joint `{name}` moves within limits, so it needs a <limit>"),
            );
            return None;
        };
        Some([
            self.optional_number(limit, "lower", 0.0)?,
            self.optional_number(limit, "upper", 0.0)?,
            self.optional_number(limit, "velocity", 0.0)?,
            self.optional_number(limit, "effort", 0.0)?,
        ])
    }

    /// The `link` of a joint's `<parent>` or `<child>`.
    fn link_name(&mut self, node: Node<'a, 'input>, role: &'static str) -> Option<String> {
        let Some(element) = child(node, role) else {
            self.invalid(node, format!("<joint> needs a <{role}>"));
            return None;
        };
        self.required(element, "link").map(str::to_owned)
    }

    /// The pose in a `<origin>` child; the identity if there is none.
    fn origin(&mut self, node: Node<'a, 'input>) -> Option<Pose> {
        let Some(origin) = child(node, "origin") else {
            return Some(Pose::default());
        };
        let xyz = match origin.attribute("xyz") {
            Some(_) => self.numbers(origin, "xyz"),
            None => Some([0.0; 3]),
        };
        let rpy = match origin.attribute("rpy") {
            Some(_) => self.numbers(origin, "rpy"),
            None => Some([0.0; 3]),
        };
        Some(Pose {
            xyz: xyz?,
            rpy: rpy?,
        })
    }

    /// `N` numbers separated by spaces, e.g. `xyz="0 0 0.1"`.
    fn numbers<const N: usize>(&mut self, node: Node<'a, 'input>, name: &str) -> Option<[f64; N]> {
        let text = self.required(node, name)?;
        let values: Option<Vec<f64>> = text.split_whitespace().map(finite).collect();
        let values = values.and_then(|values| <[f64; N]>::try_from(values).ok());
        if values.is_none() {
            let detail = format!(
                "`{name}` of <{}> must be {N} numbers separated by spaces, not \"{text}\"",
                node.tag_name().name()
            );
            self.invalid(node, detail);
        }
        values
    }

    fn number(&mut self, node: Node<'a, 'input>, name: &str) -> Option<f64> {
        let text = self.required(node, name)?;
        self.parse_number(node, name, text)
    }

    fn optional_number(&mut self, node: Node<'a, 'input>, name: &str, default: f64) -> Option<f64> {
        match node.attribute(name) {
            Some(text) => self.parse_number(node, name, text),
            None => Some(default),
        }
    }

    fn parse_number(&mut self, node: Node<'a, 'input>, name: &str, text: &str) -> Option<f64> {
        let value = finite(text);
        if value.is_none() {
            let detail = format!(
                "`{name}` of <{}> must be a number, not \"{text}\"",
                node.tag_name().name()
            );
            self.invalid(node, detail);
        }
        value
    }

    fn required(&mut self, node: Node<'a, 'input>, name: &str) -> Option<&'a str> {
        let value = node.attribute(name);
        if value.is_none() {
            let detail = format!("<{}> needs `{name}`", node.tag_name().name());
            self.invalid(node, detail);
        }
        value
    }

    fn invalid(&mut self, node: Node<'a, 'input>, detail: String) {
        let error = KaptoError::new(ErrorCode::RobotUrdfInvalid)
            .detail(detail)
            .context("line", self.line(node).to_string());
        self.errors.push(error);
    }

    fn line(&self, node: Node<'a, 'input>) -> u32 {
        self.document.text_pos_at(node.range().start).row
    }
}

/// A number that is neither infinite nor "not a number".
fn finite(text: &str) -> Option<f64> {
    f64::from_str(text.trim())
        .ok()
        .filter(|value| value.is_finite())
}

/// The element children of `node` called `name`.
fn elements<'a, 'input>(
    node: Node<'a, 'input>,
    name: &'static str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

/// The first element child of `node` called `name`.
fn child<'a, 'input>(node: Node<'a, 'input>, name: &'static str) -> Option<Node<'a, 'input>> {
    elements(node, name).next()
}
