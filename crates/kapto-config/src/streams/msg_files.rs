// SPDX-License-Identifier: Apache-2.0

//! The `.msg` files a `streams.toml`'s ROS 2 messages are put together
//! from, for a recording to carry, so it reads back where neither the
//! package's `msg/` folder nor a ROS 2 installation is.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kapto_decode::DecoderDeclaration;
use kapto_decode::cdr::{assemble, msg_files_in};

use super::{StreamDeclaration, installed_msg_folders};
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

/// Every `.msg` file the ROS 2 messages of `streams`, declared in `file`,
/// are put together from, by where a package keeps it beside its
/// `streams.toml` (`msg/<package>/msg/<Type>.msg`), found as
/// [`super::parse_streams`] finds them: in the `msg/` folder beside the
/// file, then in the ROS 2 installation Kapto runs in.
#[must_use]
pub fn msg_files(file: &ConfigFile, streams: &[StreamDeclaration]) -> BTreeMap<String, MsgFile> {
    let installed = installed_msg_folders(std::env::var_os("AMENT_PREFIX_PATH").as_deref());
    msg_files_from(file.path.with_file_name("msg"), &installed, streams)
}

/// [`msg_files`], with the package's `msg/` folder and the installation's
/// `share` folders given.
fn msg_files_from(
    package: PathBuf,
    installed: &[PathBuf],
    streams: &[StreamDeclaration],
) -> BTreeMap<String, MsgFile> {
    let package = [package];
    let mut in_package = msg_files_in(&package);
    let mut in_installation = msg_files_in(installed);
    let mut found = BTreeMap::new();
    let mut find = |package: &str, name: &str| {
        let (text, installed) = match in_package(package, name) {
            Some(text) => (text, false),
            None => (in_installation(package, name)?, true),
        };
        let path = format!("msg/{package}/msg/{name}.msg");
        found.insert(
            path,
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
    use crate::streams::parse_streams_with;

    #[test]
    fn each_msg_file_used_is_found_once_in_the_package_or_else_the_installation() {
        let root = std::env::temp_dir().join(format!("kapto-msg-files-{}", std::process::id()));
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        // The package has its own Header; Time only the installation has.
        write(
            "package/msg/std_msgs/msg/Header.msg",
            "builtin_interfaces/Time stamp\n",
        );
        write(
            "ros/share/builtin_interfaces/msg/Time.msg",
            "int32 sec\nuint32 nanosec\n",
        );
        write("ros/share/std_msgs/msg/Header.msg", "string ignored\n");
        let file = ConfigFile {
            path: root.join("package/streams.toml"),
            contents: "[streams.a]\nros2 = { topic = \"/a\", type = \"std_msgs/msg/Header\" }\n\
                       [streams.b]\nudp = { bind = \"0.0.0.0:9000\" }\n\
                       decoder = { kind = \"cdr\", type = \"std_msgs/msg/Header\", fields = [\n\
                         { name = \"s\", type = \"i32\", path = \"stamp.sec\" } ] }\n\
                       [streams.c]\nudp = { bind = \"0.0.0.0:9001\" }\n"
                .to_owned(),
        };
        let folders = [root.join("package/msg"), root.join("ros/share")];
        let streams = parse_streams_with(&file, &mut msg_files_in(&folders)).unwrap();

        let found = msg_files_from(
            root.join("package/msg"),
            &[root.join("ros/share")],
            &streams,
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            found,
            BTreeMap::from([
                (
                    "msg/builtin_interfaces/msg/Time.msg".to_owned(),
                    MsgFile {
                        text: "int32 sec\nuint32 nanosec\n".to_owned(),
                        installed: true,
                    }
                ),
                (
                    "msg/std_msgs/msg/Header.msg".to_owned(),
                    MsgFile {
                        text: "builtin_interfaces/Time stamp\n".to_owned(),
                        installed: false,
                    }
                ),
            ])
        );
        assert!(msg_files_from(root.join("none"), &[], &streams[2..]).is_empty());
    }
}
