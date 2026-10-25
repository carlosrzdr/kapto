// SPDX-License-Identifier: Apache-2.0

//! Where the `.msg` files a `streams.toml`'s ROS 2 messages are put together
//! from come from, and which of them a recording carries, so it reads back
//! where neither the package's `msg/` folder nor a ROS 2 installation is.
//!
//! A type's definition has one source. Where Kapto runs with a ROS 2
//! installation, it is the installation's, the build the robot's nodes use;
//! the package's `msg/` folder is read only where there is none. A package
//! copy differing from the installed file is refused, rather than one of
//! them silently deciding how the bytes are read.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use kapto_core::{ErrorCode, KaptoError};
use kapto_decode::DecoderDeclaration;
use kapto_decode::cdr::assemble;

use super::StreamDeclaration;
use crate::file::ConfigFile;

/// One `.msg` file a stream's messages use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MsgFile {
    /// Its text.
    pub text: String,
    /// Whether it came from the ROS 2 installation rather than the `msg/`
    /// folder beside `streams.toml`, and so isn't one of the package's files.
    pub installed: bool,
}

/// Where the `.msg` files of a `streams.toml` are found.
pub(super) struct MsgSources {
    /// The `msg/` folder beside it.
    package: PathBuf,
    /// The `share` folder of each ROS 2 installation Kapto runs in, in
    /// order; none where there is no installation.
    installed: Vec<PathBuf>,
}

impl MsgSources {
    /// Those of `file`: the `msg/` folder beside it, and the installation
    /// ROS 2's setup script describes (`AMENT_PREFIX_PATH`).
    pub(super) fn of(file: &ConfigFile) -> Self {
        let installed = installed_msg_folders(std::env::var_os("AMENT_PREFIX_PATH").as_deref());
        Self::new(file.path.with_file_name("msg"), installed)
    }

    /// The package's `msg/` folder `package`, and the installation's
    /// `share` folders `installed`.
    pub(super) const fn new(package: PathBuf, installed: Vec<PathBuf>) -> Self {
        Self { package, installed }
    }

    /// The folders a type's files are read from: the installation's alone
    /// where there is one, otherwise the package's.
    fn folders(&self) -> &[PathBuf] {
        if self.installed.is_empty() {
            std::slice::from_ref(&self.package)
        } else {
            &self.installed
        }
    }

    /// The path and text of `package/msg/name.msg`, from the first of the
    /// folders that has it.
    fn find(&self, package: &str, name: &str) -> Option<(PathBuf, String)> {
        find_in(self.folders(), package, name)
    }

    /// The text of `package/msg/name.msg`, for [`assemble`].
    pub(super) fn lookup(&self) -> impl FnMut(&str, &str) -> Option<String> + '_ {
        |package, name| self.find(package, name).map(|(_, text)| text)
    }

    /// A problem for each `.msg` file in the package's `msg/` folder that
    /// the installation also has, with other text: `config.ros2_msg_differs`,
    /// naming the type and both files. None where there is no installation.
    pub(super) fn differences(&self, file: &ConfigFile) -> Vec<KaptoError> {
        if self.installed.is_empty() {
            return Vec::new();
        }
        let mut problems = Vec::new();
        for (package, name, path) in msg_files_under(&self.package) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some((installed, installed_text)) = find_in(&self.installed, &package, &name)
            else {
                continue;
            };
            if text != installed_text {
                problems.push(
                    KaptoError::new(ErrorCode::ConfigRos2MsgDiffers)
                        .detail(format!(
                            "`{package}/msg/{name}` in the package isn't the installed one"
                        ))
                        .context("package's file", path.display().to_string())
                        .context("installed file", installed.display().to_string())
                        .context("file", file.path.display().to_string()),
                );
            }
        }
        problems
    }

    /// `errors` from reading the file with these sources, a missing `.msg`
    /// file saying where it was looked for where ROS 2 is installed: a
    /// package's own copy isn't read there.
    pub(super) fn explained(&self, errors: Vec<KaptoError>) -> Vec<KaptoError> {
        if self.installed.is_empty() {
            return errors;
        }
        errors
            .into_iter()
            .map(|error| {
                if error.code() == ErrorCode::DecoderMsgNotFound {
                    error.context(
                        "looked for",
                        "in the ROS 2 installation alone (AMENT_PREFIX_PATH): the package's \
                         msg/ folder is read only where ROS 2 isn't installed",
                    )
                } else {
                    error
                }
            })
            .collect()
    }
}

/// The `share` folder of each ROS 2 installation in `ament_prefix_path`,
/// which ROS 2's setup scripts set, in its order: where every package
/// installs its `.msg` files.
pub(super) fn installed_msg_folders(ament_prefix_path: Option<&OsStr>) -> Vec<PathBuf> {
    ament_prefix_path.map_or_else(Vec::new, |paths| {
        std::env::split_paths(paths)
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.join("share"))
            .collect()
    })
}

/// The path and text of `package/msg/name.msg` in the first of `folders`
/// that has it, laid out as ROS 2 installs them.
fn find_in(folders: &[PathBuf], package: &str, name: &str) -> Option<(PathBuf, String)> {
    folders.iter().find_map(|folder| {
        let path = folder.join(package).join("msg").join(format!("{name}.msg"));
        std::fs::read_to_string(&path).ok().map(|text| (path, text))
    })
}

/// Every `<package>/msg/<Type>.msg` file under `folder`, as package, type
/// and path, in order.
fn msg_files_under(folder: &Path) -> Vec<(String, String, PathBuf)> {
    let names = |folder: &Path| -> Vec<(String, PathBuf)> {
        let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
            .collect();
        names.sort();
        names
    };
    let mut found = Vec::new();
    for (package, path) in names(folder) {
        for (file, path) in names(&path.join("msg")) {
            if let Some(name) = file.strip_suffix(".msg") {
                found.push((package.clone(), name.to_owned(), path));
            }
        }
    }
    found
}

/// Every `.msg` file the ROS 2 messages of `streams`, declared in `file`,
/// are put together from, by where a package keeps it beside its
/// `streams.toml` (`msg/<package>/msg/<Type>.msg`), found as
/// [`super::parse_streams`] finds them: in the ROS 2 installation Kapto
/// runs in, or the `msg/` folder beside the file where there is none.
#[must_use]
pub fn msg_files(file: &ConfigFile, streams: &[StreamDeclaration]) -> BTreeMap<String, MsgFile> {
    msg_files_from(&MsgSources::of(file), streams)
}

/// [`msg_files`], from `sources`.
fn msg_files_from(
    sources: &MsgSources,
    streams: &[StreamDeclaration],
) -> BTreeMap<String, MsgFile> {
    let installed = !sources.installed.is_empty();
    let mut found = BTreeMap::new();
    let mut find = |package: &str, name: &str| {
        let (_, text) = sources.find(package, name)?;
        found.insert(
            format!("msg/{package}/msg/{name}.msg"),
            MsgFile {
                text: text.clone(),
                installed,
            },
        );
        Some(text)
    };
    for stream in streams {
        let types = stream
            .ros2
            .iter()
            .map(|topic| topic.message_type.as_str())
            .chain(stream.decoder.iter().filter_map(|decoder| match decoder {
                DecoderDeclaration::Cdr(layout) => Some(layout.type_name.as_str()),
                _ => None,
            }));
        for type_name in types {
            // Reading the file found them all already, so this can't fail;
            // if it did, there would be nothing more to carry.
            let _ = assemble(type_name, &mut find);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streams::parse_streams_from;

    /// A folder of its own for `test`, with `files` written in it.
    fn folder(test: &str, files: &[(&str, &str)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("kapto-msg-files-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    const TIME: &str = "int32 sec\nuint32 nanosec\n";
    const HEADER: &str = "builtin_interfaces/Time stamp\nstring frame_id\n";

    /// A topic and a CDR stream of `std_msgs/msg/Header`, and a stream
    /// without one.
    fn streams_file(root: &Path) -> ConfigFile {
        ConfigFile {
            path: root.join("package/streams.toml"),
            contents: "[streams.a]\nros2 = { topic = \"/a\", type = \"std_msgs/msg/Header\" }\n\
                       [streams.b]\nudp = { bind = \"0.0.0.0:9000\" }\n\
                       decoder = { kind = \"cdr\", type = \"std_msgs/msg/Header\", fields = [\n\
                         { name = \"s\", type = \"i32\", path = \"stamp.sec\" } ] }\n\
                       [streams.c]\nudp = { bind = \"0.0.0.0:9001\" }\n"
                .to_owned(),
        }
    }

    fn sources(root: &Path, installed: bool) -> MsgSources {
        let share = if installed {
            vec![root.join("ros/share")]
        } else {
            Vec::new()
        };
        MsgSources::new(root.join("package/msg"), share)
    }

    fn header_definition(streams: &[StreamDeclaration]) -> &str {
        &streams[0].ros2.as_ref().unwrap().definition
    }

    #[test]
    fn where_ros_2_is_installed_every_msg_file_is_the_installations() {
        let root = folder(
            "installed",
            &[
                ("ros/share/builtin_interfaces/msg/Time.msg", TIME),
                ("ros/share/std_msgs/msg/Header.msg", HEADER),
                // Only the package has it: not read where ROS 2 is installed.
                ("package/msg/acme_msgs/msg/Gripper.msg", "float64 opening\n"),
            ],
        );
        let file = streams_file(&root);
        let sources = sources(&root, true);
        let streams = parse_streams_from(&file, &sources).unwrap();
        assert!(header_definition(&streams).starts_with(HEADER));
        assert_eq!(
            msg_files_from(&sources, &streams),
            BTreeMap::from([
                (
                    "msg/builtin_interfaces/msg/Time.msg".to_owned(),
                    MsgFile {
                        text: TIME.to_owned(),
                        installed: true,
                    }
                ),
                (
                    "msg/std_msgs/msg/Header.msg".to_owned(),
                    MsgFile {
                        text: HEADER.to_owned(),
                        installed: true,
                    }
                ),
            ])
        );
        assert!(msg_files_from(&sources, &streams[2..]).is_empty());

        let gripper = ConfigFile {
            path: file.path.clone(),
            contents: "[streams.g]\nros2 = { topic = \"/g\", type = \"acme_msgs/msg/Gripper\" }\n"
                .to_owned(),
        };
        let errors = parse_streams_from(&gripper, &sources).unwrap_err();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].code(), ErrorCode::DecoderMsgNotFound);
        let report = errors[0].report();
        assert!(report.contains("acme_msgs/msg/Gripper.msg"), "{report}");
        assert!(
            report.contains("read only where ROS 2 isn't installed"),
            "{report}"
        );
    }

    #[test]
    fn a_package_copy_differing_from_the_installed_file_is_refused_naming_both() {
        let root = folder(
            "differs",
            &[
                ("ros/share/builtin_interfaces/msg/Time.msg", TIME),
                ("ros/share/std_msgs/msg/Header.msg", HEADER),
                // A copy gone stale: the robot's build has `frame_id`.
                (
                    "package/msg/std_msgs/msg/Header.msg",
                    "builtin_interfaces/Time stamp\n",
                ),
                // The same as installed: fine.
                ("package/msg/builtin_interfaces/msg/Time.msg", TIME),
            ],
        );
        let file = streams_file(&root);
        let errors = parse_streams_from(&file, &sources(&root, true)).unwrap_err();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].code(), ErrorCode::ConfigRos2MsgDiffers);
        let report = errors[0].report();
        for named in [
            "`std_msgs/msg/Header` in the package isn't the installed one",
            &root
                .join("package/msg/std_msgs/msg/Header.msg")
                .display()
                .to_string(),
            &root
                .join("ros/share/std_msgs/msg/Header.msg")
                .display()
                .to_string(),
            &file.path.display().to_string(),
        ] {
            assert!(report.contains(named), "{named} not in {report}");
        }
    }

    #[test]
    fn a_package_copy_identical_to_the_installed_file_is_accepted() {
        let root = folder(
            "identical",
            &[
                ("ros/share/builtin_interfaces/msg/Time.msg", TIME),
                ("ros/share/std_msgs/msg/Header.msg", HEADER),
                ("package/msg/std_msgs/msg/Header.msg", HEADER),
                ("package/msg/builtin_interfaces/msg/Time.msg", TIME),
            ],
        );
        let file = streams_file(&root);
        let sources = sources(&root, true);
        let streams = parse_streams_from(&file, &sources).unwrap();
        let found = msg_files_from(&sources, &streams);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(header_definition(&streams).starts_with(HEADER));
        assert!(found.values().all(|msg| msg.installed), "{found:?}");
    }

    #[test]
    fn without_ros_2_installed_the_packages_copies_decode() {
        let root = folder(
            "package",
            &[
                ("package/msg/std_msgs/msg/Header.msg", HEADER),
                ("package/msg/builtin_interfaces/msg/Time.msg", TIME),
                // Not an installation Kapto runs in: never read.
                ("ros/share/std_msgs/msg/Header.msg", "string other\n"),
            ],
        );
        let file = streams_file(&root);
        let sources = sources(&root, false);
        assert!(sources.differences(&file).is_empty());
        let streams = parse_streams_from(&file, &sources).unwrap();
        let found = msg_files_from(&sources, &streams);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(header_definition(&streams).starts_with(HEADER));
        let Some(DecoderDeclaration::Cdr(layout)) = &streams[1].decoder else {
            panic!("{:?}", streams[1].decoder);
        };
        assert_eq!(layout.fields[0].path, "stamp.sec");
        assert_eq!(found.len(), 2);
        assert!(found.values().all(|msg| !msg.installed), "{found:?}");
    }

    #[test]
    fn each_ros_2_installations_share_folder_is_looked_in_in_order() {
        let paths = std::env::join_paths(["/opt/ros/jazzy", "", "/home/me/ws/install"]).unwrap();
        assert_eq!(
            installed_msg_folders(Some(&paths)),
            [
                PathBuf::from("/opt/ros/jazzy/share"),
                PathBuf::from("/home/me/ws/install/share")
            ]
        );
        assert_eq!(installed_msg_folders(None), Vec::<PathBuf>::new());
        assert_eq!(
            installed_msg_folders(Some(OsStr::new(""))),
            Vec::<PathBuf>::new()
        );
    }
}
