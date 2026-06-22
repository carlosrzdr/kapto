// SPDX-License-Identifier: Apache-2.0

//! Every cause code Kapto can report.
//!
//! To add one: pick an identifier `area.what_happened`, write a summary (a
//! capitalised phrase, no full stop) and a remedy (one or two sentences
//! telling the user what to do). Never rename or remove a released code.

error_codes! {
    /// The disk holding a recording ran out of space.
    CaptureDiskFull = "capture.disk_full" {
        summary: "The disk holding the recording is full",
        remedy: "Free space on the disk that holds the data directory, or move `paths.data` to a larger disk. Messages received while the disk was full are counted as dropped, not lost silently.",
    }

    /// The disk holding the recordings has less free space than
    /// `capture.disk_warn_below`.
    CaptureDiskLow = "capture.disk_low" {
        summary: "The disk holding the recordings is running low",
        remedy: "Free space on the disk that holds the data directory, or move `paths.data` to a larger disk. Recording stops once less than `capture.disk_stop_below` is free.",
    }

    /// The disk holding the recordings has less free space than
    /// `capture.disk_stop_below`, so the episode was finalised or not
    /// started.
    CaptureDiskNearlyFull = "capture.disk_nearly_full" {
        summary: "The disk holding the recordings is nearly full, so Kapto doesn't record",
        remedy: "Free space on the disk that holds the data directory, or move `paths.data` to a larger disk, then start the episode again. The episode being recorded was finalised with everything received until then; none is started while less than `capture.disk_stop_below` is free.",
    }

    /// An event to record was refused: its kind, ID or texts aren't as an
    /// event's must be.
    CaptureEventInvalid = "capture.event_invalid" {
        summary: "The event can't be recorded as it is",
        remedy: "The message says what is wrong. A kind is lowercase words joined by dots (e.g. `grasp` or `subtask.reach`), not beginning `kapto.`, which is Kapto's own; a beginning or an end needs an ID pairing them; a text is at most 1000 characters.",
    }

    /// Creating or writing a recording failed for a reason other than a
    /// full disk.
    CaptureWriteFailed = "capture.write_failed" {
        summary: "Writing the recording failed",
        remedy: "The message gives the operating system's reason. Check that the data directory exists, is writable by the user Kapto runs as, and is on a working disk.",
    }

    /// A recording thread couldn't be started, or stopped unexpectedly.
    CaptureThreadFailed = "capture.thread_failed" {
        summary: "A recording thread failed",
        remedy: "The system may be out of threads or memory: check free memory and the user's process limit (`ulimit -u`). If it happens again, report it with the full message.",
    }

    /// The capture process can't listen on its control socket.
    CaptureControlSocketFailed = "capture.control_socket_failed" {
        summary: "Can't listen for control requests",
        remedy: "Set capture.control_socket to a path of at most 107 bytes, in a folder Kapto may write, that isn't taken by another file.",
    }

    /// A control request couldn't be understood.
    CaptureControlInvalidRequest = "capture.control_invalid_request" {
        summary: "The control request couldn't be understood",
        remedy: "Each request is one line of JSON, and a connection starts with {\"command\":\"hello\",\"version\":1}. Use `kapto episode` or the API rather than writing to the socket by hand.",
    }

    /// The two ends of a control connection speak different versions.
    CaptureControlVersionMismatch = "capture.control_version_mismatch" {
        summary: "The capture process speaks another control protocol version",
        remedy: "Run the capture process and whatever controls it from the same Kapto release, and restart both after an upgrade.",
    }

    /// No capture process answers on the control socket.
    CaptureControlUnreachable = "capture.control_unreachable" {
        summary: "Can't reach the capture process",
        remedy: "Check that `kapto capture` is running and that both use the same capture.control_socket (by default `capture.sock` in the data directory).",
    }

    /// An episode was asked to start while one is being recorded.
    CaptureAlreadyRecording = "capture.already_recording" {
        summary: "An episode is already being recorded",
        remedy: "Stop the current episode first; one episode is recorded at a time.",
    }

    /// An episode was asked to stop while none is being recorded.
    CaptureNotRecording = "capture.not_recording" {
        summary: "No episode is being recorded",
        remedy: "Start an episode first. If it was started with a duration, it may already have ended.",
    }

    /// An episode was asked to start while a check of pre-flight's latest
    /// run had failed, and the start didn't waive it.
    CapturePreflightFailed = "capture.preflight_failed" {
        summary: "Pre-flight failed: this machine may not record what the robot sends",
        remedy: "Fix what each failed check says and run pre-flight again (`kapto episode preflight`), or record anyway by waiving each failed check (`kapto episode start --waive <code>`); the episode then says which were waived.",
    }

    /// The configuration file given explicitly doesn't exist.
    ConfigFileNotFound = "config.file_not_found" {
        summary: "Configuration file not found",
        remedy: "Check the path passed with --config, or remove the option to use the default location.",
    }

    /// The configuration file exists but can't be read.
    ConfigUnreadable = "config.unreadable" {
        summary: "Configuration file can't be read",
        remedy: "Check that the file is readable by the user Kapto runs as. In a container, check the /config mount.",
    }

    /// The configuration file isn't valid TOML.
    ConfigInvalidSyntax = "config.invalid_syntax" {
        summary: "Configuration file is not valid TOML",
        remedy: "Fix the syntax at the reported line. Strings need quotes, and each key is written `key = value`.",
    }

    /// The configuration contains a key Kapto doesn't know.
    ConfigUnknownKey = "config.unknown_key" {
        summary: "Unknown configuration key",
        remedy: "Remove the key or correct its spelling. `kapto config show` lists every valid key.",
    }

    /// A configuration value has the wrong type or is out of range.
    ConfigInvalidValue = "config.invalid_value" {
        summary: "Invalid configuration value",
        remedy: "Change the value to one of the accepted types or ranges given in the message.",
    }

    /// A credential's value is written in the configuration.
    ConfigSecretInline = "config.secret_inline" {
        summary: "A secret is written in the configuration",
        remedy: "Remove it from the file and say where it comes from instead: `credentials = { env = \"SITE_NAS\" }` reads it from environment variables starting with that name, `{ file = \"/run/secrets/site_nas\" }` from a mounted file. A secret that was in a file shared with anyone should be changed.",
    }

    /// An environment variable meant to override a setting can't be parsed.
    ConfigInvalidEnvVar = "config.invalid_env_var" {
        summary: "Environment variable has an invalid value",
        remedy: "Correct or unset the variable. It must hold a value of the same type as the setting it overrides.",
    }

    /// Two listeners are configured on the same port.
    ConfigPortConflict = "config.port_conflict" {
        summary: "Two listeners are configured on the same port",
        remedy: "Give each listener its own port. Kapto never picks a different port on its own.",
    }

    /// Text that should be an episode ID isn't one.
    EpisodeInvalidId = "episode.invalid_id" {
        summary: "Invalid episode ID",
        remedy: "Episode IDs look like 20260305T192345.123Z-1a2b3c4d: the UTC date and time, then 8 lowercase hex digits. Copy the ID exactly as Kapto printed it.",
    }

    /// An episode folder with this ID already exists, so it can't be created
    /// or finalised without overwriting it.
    EpisodeAlreadyExists = "episode.already_exists" {
        summary: "An episode with this ID already exists",
        remedy: "Kapto never overwrites an episode. IDs contain the creation time and a random suffix, so this usually means a folder was copied in or the clock was set back: move the existing folder out of the data directory, then record again.",
    }

    /// An episode's `episode.json` can't be written.
    EpisodeWriteFailed = "episode.write_failed" {
        summary: "Can't write the episode's episode.json",
        remedy: "The message gives the operating system's reason. Check that the data directory is on a working disk with free space.",
    }

    /// An episode's `episode.json` is missing or isn't one Kapto can read.
    EpisodeUnreadable = "episode.unreadable" {
        summary: "Can't read the episode's episode.json",
        remedy: "Check that the episode is finalised (it has a _FINALISED file) and was written by this or an older version of Kapto. A newer Kapto may add fields this version doesn't know.",
    }

    /// A field-to-joint mapping names a field its stream doesn't decode.
    MappingUnknownField = "mapping.unknown_field" {
        summary: "The mapping names a field that isn't decoded",
        remedy: "Name a stream from streams.toml and a field of its decoder, with an index for array fields: `<stream>.<field>` or `<stream>.<field>[<index>]`, e.g. `arm_joints.joint_positions[0]`.",
    }

    /// A field-to-joint mapping names a joint the robot can't take a value for.
    MappingUnknownJoint = "mapping.unknown_joint" {
        summary: "The mapping names a joint that can't be driven",
        remedy: "Use the name of a revolute, continuous or prismatic joint from the robot description. A mimic joint follows the joint it copies, so map that one instead.",
    }

    /// A key in mapping.toml that Kapto doesn't know.
    MappingUnknownKey = "mapping.unknown_key" {
        summary: "Unknown key in mapping.toml",
        remedy: "Remove the key or correct its spelling. The file holds a `[joints]` table, where each joint is a field reference or a table with `position`, `velocity` and `effort`, and a `[roles]` table, where each role is a field reference.",
    }

    /// A moving joint of the robot has no mapped position.
    MappingJointUnmapped = "mapping.joint_unmapped" {
        summary: "A joint of the robot has no position mapped",
        remedy: "Add the joint to the `[joints]` table of mapping.toml with the field that holds its position. Until then it is shown at 0.",
    }

    /// A mapped field's unit doesn't fit the joint.
    MappingUnitMismatch = "mapping.unit_mismatch" {
        summary: "The mapped field's unit doesn't fit the joint",
        remedy: "Joint positions are in rad (turning joints) or m (sliding joints), velocities per second, efforts in N·m or N. If the robot sends other units, set `scale` and `unit` on the decoder field, e.g. scale = 0.017453292519943295 and unit = \"rad\" for degrees.",
    }

    /// A folder that should be a robot package has no `package.toml`, or
    /// no package has the ID asked for.
    PackageNotFound = "package.not_found" {
        summary: "No such robot package",
        remedy: "A robot package is a folder with a `package.toml` in it. Check the folder, or list the packages Kapto finds with `kapto package list`; they are the folders in `paths.packages`.",
    }

    /// `package.toml` has a key Kapto doesn't know.
    PackageUnknownKey = "package.unknown_key" {
        summary: "Unknown key in package.toml",
        remedy: "Remove the key or correct its spelling. `package.toml` accepts `id`, `version`, `name`, `vendor`, `description`, `contact`, `urdf`, `streams`, `mapping` and `layout`.",
    }

    /// A layout has a key that isn't one of a layout's, a column's or a
    /// panel's.
    LayoutUnknownKey = "layout.unknown_key" {
        summary: "Unknown key in a layout",
        remedy: "Remove the key or correct its spelling. A layout holds `[[columns]]` tables, each with `width` and `panels`; a panel has `kind`, `height`, and `stream` for a camera or `fields` for a plot. The robot package format reference (docs/reference/package.md) describes them.",
    }

    /// A layout was to be saved as a package's default, but Kapto runs no
    /// robot package to save it for.
    LayoutWithoutPackage = "layout.without_package" {
        summary: "No robot package to save the layout for",
        remedy: "A default layout is saved for a robot package, in `layouts/<package id>.toml` of the configuration folder. Start `kapto serve` with a package's `streams.toml`, or arrange the view in `layout.toml` beside the streams.",
    }

    /// A default layout couldn't be written to, or removed from, the
    /// configuration folder.
    LayoutWriteFailed = "layout.write_failed" {
        summary: "The default layout couldn't be saved",
        remedy: "Check that the configuration folder exists and that Kapto may write in its `layouts` folder; the message says which file and why.",
    }

    /// A robot package's `qa.toml` has a key that isn't a setting.
    QaUnknownKey = "qa.unknown_key" {
        summary: "Unknown key in qa.toml",
        remedy: "Remove the key or correct its spelling. A check's settings are in the table named after its ID, e.g. `[streams.complete]`; the robot package format reference (docs/reference/package.md) lists every setting.",
    }

    /// A file `package.toml` names isn't in the package.
    PackageMissingFile = "package.missing_file" {
        summary: "A file of the robot package is missing",
        remedy: "Add the file to the package folder, or correct its path in `package.toml`. Paths are relative to the folder that holds `package.toml`.",
    }

    /// A package holds something that isn't a plain file or folder, such as
    /// a symbolic link, or a name that isn't UTF-8.
    PackageUnsafePath = "package.unsafe_path" {
        summary: "The robot package holds something Kapto won't read",
        remedy: "A package is plain files and folders with UTF-8 names. Replace symbolic links with the files they point to, and rename files whose names aren't UTF-8.",
    }

    /// Two packages in the packages folder have the same ID.
    PackageDuplicateId = "package.duplicate_id" {
        summary: "Two robot packages have the same ID",
        remedy: "Give each package in `paths.packages` its own `id` in its `package.toml`, or remove the one that isn't wanted.",
    }

    /// A package's files couldn't be read.
    PackageUnreadable = "package.unreadable" {
        summary: "Can't read the robot package",
        remedy: "The message gives the operating system's reason. Check that the package folder and its files exist and are readable by the user Kapto runs as.",
    }

    /// A package is imported into a folder that already exists.
    PackageImportExists = "package.import_exists" {
        summary: "There is already something where the package would be imported",
        remedy: "Import it under another ID with `--id`, or remove the folder first if it is an earlier import of the same robot.",
    }

    /// A file the imported robot description names can't be carried in a
    /// package: it is named by an absolute path.
    PackageImportNotPortable = "package.import_not_portable" {
        summary: "A file the robot description names can't go into the package",
        remedy: "The description names it by an absolute path, which means something else on every machine. Name it with `package://` or a path relative to the description, then import again.",
    }

    /// Pre-flight found the disk can't hold the minimum recording time at
    /// the streams' rate above `capture.disk_stop_below`.
    PreflightDiskTooSmall = "preflight.disk_too_small" {
        summary: "The disk doesn't have room for the minimum recording time",
        remedy: "Free space on the disk that holds the data directory, or move `paths.data` to a larger disk, then run pre-flight again. To record anyway, waive this check when starting the episode.",
    }

    /// Pre-flight's test file was written more slowly than the streams send
    /// data, or couldn't be written.
    PreflightDiskTooSlow = "preflight.disk_too_slow" {
        summary: "The disk is slower than the streams send data",
        remedy: "Move `paths.data` to a faster disk (an SSD rather than an SD card or a network share), or check what else is writing to it, then run pre-flight again. To record anyway, waive this check when starting the episode.",
    }

    /// A declared stream sent nothing while pre-flight watched it.
    PreflightStreamSilent = "preflight.stream_silent" {
        summary: "A declared stream sent nothing",
        remedy: "Check that the robot is on and sending to the address in `streams.toml`, and that no firewall is in the way, then run pre-flight again. To record without it, waive this check when starting the episode.",
    }

    /// A declared stream arrived well below its declared rate while
    /// pre-flight watched it.
    PreflightStreamSlow = "preflight.stream_slow" {
        summary: "A stream arrives well below its declared rate",
        remedy: "Check the robot's sending rate and the network between it and Kapto, or correct `rate_hz` in `streams.toml`, then run pre-flight again. To record anyway, waive this check when starting the episode.",
    }

    /// A joint that moves within limits has a lower limit at or above its
    /// upper one, so it can't move.
    RobotJointRangeEmpty = "robot.joint_range_empty" {
        summary: "A joint's limits leave it no room to move",
        remedy: "Set the joint's <limit lower=\"…\" upper=\"…\"> in the URDF to its real range, lower below upper, in radians (or metres for a sliding joint). A missing lower or upper counts as 0.",
    }

    /// A joint that moves within limits has no velocity or effort limit.
    RobotJointLimitsUnknown = "robot.joint_limits_unknown" {
        summary: "A joint has no velocity or effort limit",
        remedy: "Set the joint's <limit velocity=\"…\" effort=\"…\"> in the URDF to the robot's real limits, from its data sheet. With 0, nothing can check recordings against them.",
    }

    /// A mesh file says it was made with Y up; robot descriptions use Z up.
    RobotMeshYUp = "robot.mesh_y_up" {
        summary: "A mesh was made with Y pointing up",
        remedy: "Robot descriptions use Z up, so the part appears lying on its side. Export the mesh again with Z up, or rotate it in the URDF with the visual's <origin rpy=\"1.5708 0 0\">.",
    }

    /// Joint values given for a robot don't match its joints.
    RobotJointValuesMismatch = "robot.joint_values_mismatch" {
        summary: "The joint values don't match the robot's joints",
        remedy: "Give one value per moving joint of the robot, in the order its description declares them (mimic joints excluded), e.g. by mapping each stream field to a joint.",
    }

    /// A URDF's links and joints don't form one tree.
    RobotKinematicTreeInvalid = "robot.kinematic_tree_invalid" {
        summary: "The robot's links and joints don't form one tree",
        remedy: "Fix the URDF as the message says. Every joint names a declared parent and child link, names are unique, every link but one root has exactly one parent joint, and moving joints have an axis and limits with lower ≤ upper.",
    }

    /// A mesh file isn't a valid mesh of its format.
    RobotMeshInvalid = "robot.mesh_invalid" {
        summary: "The mesh file isn't valid",
        remedy: "The message says what is wrong. Re-export the mesh from its modelling tool, as triangles, and check that the file isn't cut short.",
    }

    /// A mesh file can't be read.
    RobotMeshUnreadable = "robot.mesh_unreadable" {
        summary: "Can't read the mesh file",
        remedy: "Check that the file (and, for glTF, its .bin buffer) exists next to where the robot description says, and that Kapto may read it.",
    }

    /// A robot description in xacro couldn't be expanded to URDF.
    RobotXacroFailed = "robot.xacro_failed" {
        summary: "Can't expand the robot description from xacro",
        remedy: "The message says which file and what went wrong in it. Give the arguments the description expects with `--arg name:=value`, and check that each package it finds with `$(find)` is beside it or in the packages folder.",
    }

    /// A mesh file has a format Kapto doesn't read.
    RobotMeshUnsupportedFormat = "robot.mesh_unsupported_format" {
        summary: "Kapto can't read this mesh format",
        remedy: "Convert the mesh to STL, OBJ, COLLADA (.dae) or glTF (.gltf or .glb), e.g. with Blender, and point the robot description at the converted file.",
    }

    /// A URDF file isn't valid URDF.
    RobotUrdfInvalid = "robot.urdf_invalid" {
        summary: "The robot description isn't valid URDF",
        remedy: "The message names what couldn't be read. Check that the file is plain URDF XML with a <robot> element; a .xacro file must first be expanded, e.g. `xacro robot.urdf.xacro > robot.urdf`.",
    }

    /// An API request's body isn't what the endpoint takes.
    ApiInvalidRequest = "api.invalid_request" {
        summary: "The request isn't what this endpoint takes",
        remedy: "Send a JSON body with `Content-Type: application/json`, as described in the API reference; an endpoint without fields takes `{}`.",
    }

    /// A request named a host the API doesn't answer to while it listens
    /// only on this machine: what a web page reaching it through DNS
    /// rebinding would send.
    ApiHostNotAllowed = "api.host_not_allowed" {
        summary: "The request was addressed to a host name this server doesn't answer to",
        remedy: "Open Kapto as http://localhost:<port> or http://127.0.0.1:<port>. While server.bind is a loopback address, Kapto only answers to those, so a web page can't reach it through a DNS name that points to this machine. To reach it under another name, set server.bind to an address on the network.",
    }

    /// A web page from another site tried to open the event stream.
    ApiOriginNotAllowed = "api.origin_not_allowed" {
        summary: "A page from another site may not open the event stream",
        remedy: "Open the event stream from Kapto's own web UI, or from a program that doesn't send an Origin header. Browsers send it, so other sites' pages are refused.",
    }

    /// No finalised episode has the ID asked for.
    ApiEpisodeNotFound = "api.episode_not_found" {
        summary: "No such episode",
        remedy: "List the episodes with GET /api/episodes and use one of their IDs. An episode still being recorded, aborted or never finalised isn't listed.",
    }

    /// The size of the file system holding the data directory can't be
    /// read.
    ServerStorageUnreadable = "server.storage_unreadable" {
        summary: "Can't tell how much room the data directory has",
        remedy: "The message gives the operating system's reason. Check that paths.data exists and is readable by the user Kapto runs as; it is created when the first episode is recorded.",
    }

    /// The server can't listen for API requests.
    ServerListenFailed = "server.listen_failed" {
        summary: "Can't listen for API requests",
        remedy: "Check that no other program (or other Kapto) uses server.port and that server.bind is an address of this machine. Ports below 1024 need extra permissions.",
    }

    /// The server couldn't start one of Kapto's processes, or it didn't
    /// come up.
    ServerChildFailed = "server.child_failed" {
        summary: "A Kapto process couldn't be started",
        remedy: "The message names the process and why. Its own messages above say more; fix what they report, or run that command (`kapto capture`, `kapto viz-bridge`) on its own to see it fail.",
    }

    /// A URDF file can't be read.
    RobotUrdfUnreadable = "robot.urdf_unreadable" {
        summary: "Can't read the robot description",
        remedy: "Check that the file exists and Kapto may read it. In a container, check that its folder is mounted.",
    }

    /// A stream is declared with a name that isn't a valid stream ID.
    StreamInvalidId = "stream.invalid_id" {
        summary: "Invalid stream ID",
        remedy: "Use lowercase letters, digits and underscores, starting with a letter, at most 64 characters, e.g. `arm_left_joints`.",
    }

    /// Accepting the robot's TCP connection failed for a reason that won't
    /// go away by trying again.
    TransportTcpAcceptFailed = "transport.tcp.accept_failed" {
        summary: "Can't accept the robot's TCP connection",
        remedy: "The message gives the operating system's reason. If it says too many open files, raise the limit (`ulimit -n`) and restart the recording.",
    }

    /// A TCP stream can't listen for the robot's connection.
    TransportTcpListenFailed = "transport.tcp.listen_failed" {
        summary: "Can't listen for the robot's TCP connection",
        remedy: "Check that the address belongs to this machine and the port is free and allowed: no other program (or other Kapto) listens on it, and ports below 1024 need extra permissions.",
    }

    /// A UDP socket can't be opened on its configured address.
    TransportUdpBindFailed = "transport.udp.bind_failed" {
        summary: "Can't listen for UDP on the configured address",
        remedy: "Check that no other program uses the port (`ss -ulpn` lists them) and that the address belongs to this machine. In a container, publish the port or use host networking.",
    }

    /// A UDP socket can't join its multicast group.
    TransportUdpMulticastJoinFailed = "transport.udp.multicast_join_failed" {
        summary: "Can't join the UDP multicast group",
        remedy: "Check that the group is a multicast address (224.0.0.0/4 or ff00::/8) and that a network interface with multicast enabled is up. In a container, use host networking.",
    }

    /// The operating system gave a UDP socket a smaller receive buffer than
    /// configured. Reported as a warning: receiving works, but a burst or a
    /// short stall can overflow the buffer and lose messages.
    TransportUdpReceiveBufferSmall = "transport.udp.receive_buffer_small" {
        summary: "UDP receive buffer is smaller than configured",
        remedy: "Raise the host's limit to at least the configured size, e.g. `sysctl -w net.core.rmem_max=8388608`, and add it to /etc/sysctl.d so it survives a reboot. A container uses the host's limit.",
    }

    /// Reading from a UDP socket failed for a reason other than having
    /// nothing to read.
    TransportUdpReceiveFailed = "transport.udp.receive_failed" {
        summary: "Receiving from a UDP socket failed",
        remedy: "The message gives the operating system's reason. Check that the network interface is still up, then restart the recording.",
    }

    /// Something needs ROS 2, and this Kapto was built without it.
    TransportRos2NotBuilt = "transport.ros2.not_built" {
        summary: "This Kapto was built without ROS 2",
        remedy: "Use the `kapto-ros2` image (`docker compose build kapto-ros2`), whose Kapto is built with ROS 2, on the robot's network with the same `ROS_DOMAIN_ID` and `RMW_IMPLEMENTATION` as the robot's nodes.",
    }

    /// Over Zenoh, nothing answers where the robot's Zenoh router should
    /// be, so no topic can be found or received.
    TransportRos2RouterUnreachable = "transport.ros2_router_unreachable" {
        summary: "No Zenoh router answers",
        remedy: "Start the robot's Zenoh router (`ros2 run rmw_zenoh_cpp rmw_zenohd`), or point Kapto at it with `ZENOH_CONFIG_OVERRIDE='connect/endpoints=[\"tcp/<host>:7447\"]'`, as the robot's nodes are. Recording carries on, and receives once a router is reachable.",
    }
    /// Kapto's ROS 2 node couldn't start.
    TransportRos2NodeFailed = "transport.ros2.node_failed" {
        summary: "Kapto can't join ROS 2",
        remedy: "The message gives ROS 2's reason. Check that ROS 2's environment is set as for the robot's nodes (the ROS 2 image sets it up), that `RMW_IMPLEMENTATION` names a middleware installed here, that `ROS_DOMAIN_ID` is between 0 and 232, and that ROS 2 can write its logs under `ROS_HOME` (or `~/.ros`).",
    }

    /// A ROS 2 topic's message type isn't installed where Kapto runs.
    TransportRos2TypeUnavailable = "transport.ros2.type_unavailable" {
        summary: "A ROS 2 message type isn't installed where Kapto runs",
        remedy: "Kapto subscribes to a topic with its type's support library from the ROS 2 installation, as the robot's nodes do. Install the package that defines it where Kapto runs (`ros-<distro>-<package>`, or the robot's own message package, built and sourced), or check the stream's `type` against `kapto topics`.",
    }

    /// Kapto's node can't subscribe to a ROS 2 topic.
    TransportRos2SubscribeFailed = "transport.ros2.subscribe_failed" {
        summary: "Kapto can't subscribe to a ROS 2 topic",
        remedy: "The message gives ROS 2's reason and the topic. Check the topic's name and the stream's `qos` (a depth of at least 1).",
    }

    /// Kapto's node stopped being able to wait for or take messages.
    TransportRos2ReceiveFailed = "transport.ros2.receive_failed" {
        summary: "Kapto can't receive from ROS 2 any more",
        remedy: "The message gives ROS 2's reason. The middleware Kapto's node uses may have stopped: check the machine's ROS 2 network and, for Zenoh, that its router is running.",
    }

    /// A stream declaration contains a key Kapto doesn't know.
    StreamUnknownKey = "stream.unknown_key" {
        summary: "Unknown key in a stream declaration",
        remedy: "Remove the key or correct its spelling. A stream accepts `rate_hz`, `sequence`, `loss_settle_ms`, `max_message_bytes`, `pre_roll_s`, `udp`, `tcp`, `ros2`, `decoder` and `camera`; a sequence accepts `offset`, `type` and `byte_order`; udp accepts `bind`, `multicast_group` and `allowed_sources`; tcp accepts `connect`, `listen`, `framing`, `reconnect_initial_ms` and `reconnect_max_ms`; a framing accepts `kind`, `length_bytes`, `length_offset`, `header_bytes`, `byte_order`, `includes_header`, `delimiter` and `size`; ros2 accepts `topic`, `type` and `qos`; a qos accepts `reliability`, `durability` and `depth`; a decoder accepts `kind`, `type` (cdr) and `fields`; a field accepts `name`, `type`, `offset` or `path`, `count`, `byte_order`, `scale` and `unit`; a camera accepts `format`, and for raw frames `width`, `height` and `encoding`.",
    }

    /// An episode's manifest can't be written.
    ManifestWriteFailed = "manifest.write_failed" {
        summary: "Can't write the episode's manifest",
        remedy: "The message gives the reason and the file. Check that the episode folder is on a working disk with free space, and that it holds only regular files and folders, no links.",
    }

    /// An episode's manifest is missing or isn't a manifest Kapto can read.
    ManifestUnreadable = "manifest.unreadable" {
        summary: "Can't read the episode's manifest",
        remedy: "An episode without a readable manifest.json is incomplete: it was interrupted before it was finalised, or the file was changed afterwards. Don't use it as a finished recording.",
    }

    /// An episode to upload can't be read from its folder.
    UploadEpisodeUnreadable = "upload.episode_unreadable" {
        summary: "Can't read the episode to upload",
        remedy: "The message names the file and the reason. An episode is uploaded from its folder as its manifest lists it: check the folder is still there, finalised and unchanged (`kapto episode` and the web UI show it).",
    }

    /// A destination's storage can't be set up from its settings.
    UploadDestinationUnusable = "upload.destination_unusable" {
        summary: "Can't set up the upload destination from its settings",
        remedy: "The message says what is wrong. Check the destination's `url` (`s3://bucket`, `gs://bucket`, `az://container` or `file:///folder`), `endpoint` and `region`, and the provider's own settings in Kapto's environment (e.g. `AZURE_STORAGE_ACCOUNT_NAME`). Nothing is uploaded there until it is fixed.",
    }

    /// No upload destination is configured by the name asked for.
    UploadNoDestination = "upload.no_destination" {
        summary: "No such upload destination",
        remedy: "Destinations are the `[[upload.destination]]` entries of `kapto.toml`, each with a `name`: `kapto config show` lists the file Kapto reads. Give one of their names, or add an entry (`name`, `url`, and for cloud storage its `credentials`).",
    }

    /// A destination's credentials aren't where its settings say.
    UploadCredentialsMissing = "upload.credentials_missing" {
        summary: "Can't read the upload destination's credentials",
        remedy: "The message names the value and where `credentials` says to find it: set the environment variable (e.g. `SITE_NAS_SECRET_ACCESS_KEY` for `{ env = \"SITE_NAS\" }`) in Kapto's environment, or give the file it names `name = \"value\"` lines (e.g. `secret_access_key`). S3 needs `access_key_id` and `secret_access_key`, Google Cloud Storage `service_account_key`, Azure `access_key`.",
    }

    /// The destination couldn't be reached, try after try.
    UploadUnreachable = "upload.unreachable" {
        summary: "Can't reach the upload destination",
        remedy: "Kapto tried as many times as `upload.max_attempts` allows, waiting longer each time, and tries the episode again after `upload.retry_failed_after`. Check the robot's network and the destination's `url` and `endpoint`; nothing uploaded so far is lost.",
    }

    /// The destination refused the credentials or the request.
    UploadDenied = "upload.denied" {
        summary: "The upload destination refused Kapto",
        remedy: "Check the destination's credentials (where `credentials` says they come from) and that they may write under its `prefix`. Trying again doesn't help until that is fixed.",
    }

    /// The destination answered with something Kapto can't act on.
    UploadFailed = "upload.failed" {
        summary: "The upload destination answered with an error",
        remedy: "The message gives the destination's answer and the key. Check the destination is the kind its `url` says, and its `url` and `prefix`.",
    }

    /// The destination has the episode already, with other files.
    UploadMarkerConflict = "upload.marker_conflict" {
        summary: "The episode is at the destination already, with other files",
        remedy: "Its completeness marker lists other files than the episode has here, so Kapto leaves it alone. Compare the two: the episode here may have been changed, or another one uploaded under the same key; change the destination's `layout` (e.g. add `{sha8}`) if keys collide.",
    }

    /// An uploaded file isn't the one sent.
    UploadVerifyFailed = "upload.verify_failed" {
        summary: "An uploaded file isn't the one sent",
        remedy: "The file at the destination has another size or SHA-256 than the episode's. Kapto writes no completeness marker for the episode, so nobody takes it for whole; it is uploaded again on the next try. If it keeps happening, check what else writes under the destination's `prefix`.",
    }

    /// An uploaded episode's completeness marker can't be read.
    UploadMarkerUnreadable = "upload.marker_unreadable" {
        summary: "Can't read an uploaded episode's completeness marker",
        remedy: "The `_SUCCESS` at the destination isn't a marker this Kapto reads: written by a newer Kapto, by another program, or changed since. Kapto never overwrites it; compare the episode there with its manifest, or upload it with a newer Kapto.",
    }

    /// An episode's files don't match its manifest.
    ManifestMismatch = "manifest.mismatch" {
        summary: "The episode's files don't match its manifest",
        remedy: "A file was changed, added or removed after the episode was finalised, or the disk returned wrong data. Restore the episode from a verified copy if there is one.",
    }

    /// A role name doesn't follow the role naming rule.
    RoleInvalidName = "role.invalid_name" {
        summary: "Invalid role name",
        remedy: "Write a role as two or more dot-separated parts of lowercase letters, digits and underscores, each starting with a letter, e.g. `arm.left.joints`.",
    }

    /// A recording's stored layout can't be read.
    DecoderInvalidSchema = "decoder.invalid_schema" {
        summary: "A stream's stored message layout can't be read",
        remedy: "The recording's layout for this stream was written by a different version of Kapto or was changed afterwards. Decode it with the Kapto version that recorded it, or with the layout from its streams.toml.",
    }

    /// A ROS 2 message type's `.msg` file, or that of a type it uses,
    /// wasn't found.
    DecoderMsgNotFound = "decoder.msg_not_found" {
        summary: "A ROS 2 message type's definition wasn't found",
        remedy: "The message names the missing `.msg` file and the type that uses it. For a ROS 2 topic, install the ROS 2 package that defines it where Kapto runs (`ros-<distro>-<package>`); for messages sent over UDP or TCP, put the `.msg` file in the robot package, as `msg/<package>/msg/<Type>.msg`.",
    }

    /// A struct layout can't be decoded as declared.
    DecoderInvalidLayout = "decoder.invalid_layout" {
        summary: "Invalid message layout",
        remedy: "Fix the field named in the message. Field names are lowercase letters, digits and underscores starting with a letter, and unique; an array has at least one element; a scale is a finite number other than 0; every field ends within 65,507 bytes, the largest UDP message.",
    }

    /// A recording file can't be read.
    RecordingUnreadable = "recording.unreadable" {
        summary: "The recording can't be read",
        remedy: "Check the path: give an episode folder or its recording.mcap. A recording cut off by a crash or power cut is read up to where it stops; one that isn't an MCAP file can't be read at all.",
    }

    /// A stream named on the command line isn't in the recording.
    RecordingUnknownStream = "recording.unknown_stream" {
        summary: "The recording has no stream with that name",
        remedy: "Use one of the stream names listed in the message, or leave out --stream to include every stream.",
    }

    /// An episode with no ROS 2 topic was to be exported as a bag.
    ExportNoRos2Topics = "export.no_ros2_topics" {
        summary: "The episode has no ROS 2 topics to export",
        remedy: "Only streams recorded from ROS 2 topics (declared with `ros2` in streams.toml) go into a bag. Other streams are read with `kapto decode`.",
    }

    /// The file an export would write already exists.
    ExportOutputExists = "export.output_exists" {
        summary: "The export's output file already exists",
        remedy: "Give a path that doesn't exist yet, or remove the file first: an export never overwrites one.",
    }

    /// Writing an export failed part-way; nothing of it is kept.
    ExportWriteFailed = "export.write_failed" {
        summary: "Writing the export failed",
        remedy: "The message gives the operating system's reason. Check the output folder exists and has room, then export again; the partial file was removed.",
    }

    /// CSV output was asked for without choosing one stream.
    DecodeCsvNeedsOneStream = "decode.csv_needs_one_stream" {
        summary: "CSV output needs exactly one stream",
        remedy: "Choose the stream with --stream; each stream has its own columns. The message lists the recording's streams.",
    }

    /// CSV output was asked for a stream recorded without a layout.
    DecodeNoLayout = "decode.no_layout" {
        summary: "The stream was recorded without a message layout",
        remedy: "Its messages can only be shown as raw bytes, with --format table. To decode future recordings, declare the stream's layout in its streams.toml decoder section.",
    }

    /// A file given as a packet capture isn't one, or is damaged.
    DecodeCaptureInvalid = "decode.capture_invalid" {
        summary: "Can't read the packet capture",
        remedy: "Give a pcap or pcapng file, as `tcpdump -w` or Wireshark save them. A capture cut off while being written can be repaired with Wireshark's `editcap`.",
    }

    /// The viewer bridge can't listen for the live view's pages.
    ViewerListenFailed = "viewer.listen_failed" {
        summary: "The viewer can't listen on its address",
        remedy: "Check that no other program (or other viewer bridge) uses viewer.stream_port, and that server.bind is an address of this machine. Ports below 1024 need extra permissions.",
    }

    /// The viewer bridge can't listen on the recording's live tap socket.
    ViewerTapSocketFailed = "viewer.tap_socket_failed" {
        summary: "The viewer can't listen for live samples",
        remedy: "Set viewer.tap_socket to a path of at most 107 bytes, in a folder Kapto may write, that isn't taken by another file. The recording and the viewer bridge must use the same path.",
    }

    /// The viewer bridge can't listen on the recording's frame socket.
    ViewerFrameSocketFailed = "viewer.frame_socket_failed" {
        summary: "The viewer can't listen for cameras' frames",
        remedy: "Set viewer.frame_socket to a path of at most 107 bytes, in a folder Kapto may write, that isn't taken by another file. The recording and the viewer bridge must use the same path.",
    }

    /// An episode has no such camera, or the camera no such frame.
    ViewerCameraNotFound = "viewer.camera_not_found" {
        summary: "The episode has no such camera or frame",
        remedy: "Ask for a camera the episode's replay opening lists, and a frame numbered from 0 below the count its frames give.",
    }

    /// A recorded camera's frame can't be shown in the web UI.
    ViewerPictureUnshown = "viewer.picture_unshown" {
        summary: "The camera's frame can't be shown",
        remedy: "The message says why: a picture neither JPEG nor PNG, a raw encoding other than rgb8, bgr8, rgba8, bgra8 or mono8, or a frame that isn't what the camera's stream declares. The frame is recorded as received all the same; export the episode to look at it with another tool.",
    }

    /// The viewer bridge stopped being able to hand data to the viewer.
    ViewerLoggingFailed = "viewer.logging_failed" {
        summary: "The viewer stopped taking data",
        remedy: "Restart the viewer bridge; recording isn't affected. If it happens again, report it with the full message.",
    }

    /// An episode's replay file can't be written.
    ViewerReplayWriteFailed = "viewer.replay_write_failed" {
        summary: "Can't write the episode's replay file",
        remedy: "Check that the folder exists, that Kapto may write to it and that the disk has room; the episode itself isn't affected.",
    }

    /// An episode's replay in the cache can't be read: damaged, cut off,
    /// or of a version this Kapto doesn't read.
    ViewerReplayUnreadable = "viewer.replay_unreadable" {
        summary: "Can't read the episode's replay",
        remedy: "Ask for the episode's replay again (POST /api/episodes/{id}/replay): it is made again from the recording, which isn't affected. If it happens again, report it with the full message.",
    }

    /// A replay shows the plots but not the robot: the recording carries
    /// no robot description.
    ViewerReplayWithoutRobot = "viewer.replay_without_robot" {
        summary: "The replay shows no robot",
        remedy: "Record with a robot package (a streams.toml beside a package.toml) for the robot to be shown; the plots are shown either way.",
    }

    /// The page asked for a mesh the robot's scene doesn't have.
    ViewerMeshNotFound = "viewer.mesh_not_found" {
        summary: "The robot's scene has no such mesh",
        remedy: "Ask `/api/robot/scene` again for the meshes it lists, numbered from 0; the robot may have changed since the page loaded it, so reload the page.",
    }

    /// A replay was asked for before it was made, or after it left the
    /// cache.
    ViewerReplayNotReady = "viewer.replay_not_ready" {
        summary: "The replay isn't made",
        remedy: "Ask for it with POST /api/episodes/{id}/replay, then load it once GET /api/episodes/{id}/replay says it is ready.",
    }

    /// A replay wasn't made: the disk holding the data directory has less
    /// free space than recording warns at, and replays are the first thing
    /// given up for a recording.
    ViewerReplayDiskLow = "viewer.replay_disk_low" {
        summary: "No room to make the replay",
        remedy: "Free space on the disk holding the data directory, above capture.disk_warn_below, then ask for the replay again.",
    }

    /// `kapto.db`, Kapto's runtime state, can't be opened or brought up to
    /// date.
    StoreOpenFailed = "store.open_failed" {
        summary: "Can't open Kapto's runtime state database",
        remedy: "The message gives SQLite's reason. Check that the data directory is writable by the user Kapto runs as. If kapto.db is damaged, move it aside: it holds only runtime state, never settings or recordings, and the episode index is rebuilt from the episode folders.",
    }

    /// Reading or writing `kapto.db` failed once it was open.
    StoreQueryFailed = "store.query_failed" {
        summary: "Reading or writing Kapto's runtime state database failed",
        remedy: "The message gives SQLite's reason. Check that the disk holding the data directory has room and works. If it happens again, report it with the full message.",
    }

    /// A job was interrupted, by its worker or serve stopping, as often as
    /// it may be started, so it isn't started again.
    WorkerJobInterrupted = "worker.job_interrupted" {
        summary: "The job was interrupted too often to start it again",
        remedy: "Something stops the worker while it does this job, e.g. it runs out of memory. Check the worker's output and the system log for why it stopped, fix that, and queue the job again.",
    }

    /// Serve can't listen on the socket the worker gets its jobs from.
    ServerJobSocketFailed = "server.job_socket_failed" {
        summary: "Can't listen for the worker",
        remedy: "The socket is `worker.sock` in the data directory. Check that the data directory is writable by the user Kapto runs as, that its path is at most 95 bytes long (a Unix socket path, here ending in `/worker.sock`, can have at most 107), and that no other file named worker.sock is in the way.",
    }

    /// A request on the job socket couldn't be understood.
    WorkerInvalidRequest = "worker.invalid_request" {
        summary: "Serve couldn't understand the worker's request",
        remedy: "Each request is one line of JSON, and a connection starts with {\"request\":\"hello\",\"version\":1}. Run the worker that comes with the same Kapto as serve rather than writing to the socket by hand.",
    }

    /// The worker and serve speak different job protocol versions.
    WorkerVersionMismatch = "worker.version_mismatch" {
        summary: "The worker speaks another job protocol version than serve",
        remedy: "Run the worker and serve from the same Kapto release, and restart both after an upgrade.",
    }

    /// The worker can't reach serve's job socket.
    WorkerServeUnreachable = "worker.serve_unreachable" {
        summary: "The worker can't reach serve",
        remedy: "The worker gets its jobs from `kapto serve`, through `worker.sock` in the data directory. Check that serve is running with the same paths.data; the worker keeps trying, so it carries on once serve is back.",
    }

    /// The worker was given a job of a kind it doesn't know.
    WorkerUnknownJob = "worker.unknown_job" {
        summary: "The worker doesn't know this kind of job",
        remedy: "The job was queued by a newer or different Kapto than the worker's. Run serve and the worker from the same Kapto release.",
    }

    /// The worker stopped a job part-way because it was itself stopping;
    /// the job is given back to be done again.
    WorkerJobStopped = "worker.job_stopped" {
        summary: "The job was stopped part-way because the worker is stopping",
        remedy: "Nothing to do: the job is given back to the queue and done again once the worker runs, without counting as an attempt.",
    }

    /// A lane of the worker ended by itself, so the worker stopped, to be
    /// started again by serve rather than run on without it.
    WorkerLaneEnded = "worker.lane_ended" {
        summary: "A lane of the worker ended by itself, so the worker stopped",
        remedy: "Serve starts the worker again by itself. The worker's output just before this says why the lane ended; if it keeps happening, report it with that output.",
    }

    /// Uploads were asked to be tried again for an episode none of whose
    /// uploads failed.
    UploadNothingToRetry = "upload.nothing_to_retry" {
        summary: "No upload of this episode failed, so there is nothing to try again",
        remedy: "Uploads going or waiting carry on by themselves, and those done are done. `kapto upload status` shows each episode's uploads; check the episode's ID.",
    }

    /// An episode's folder couldn't be set aside or removed while deleting
    /// it here.
    StoreDeleteFailed = "store.delete_failed" {
        summary: "Couldn't delete an episode's folder",
        remedy: "The message names the folder and why, usually its permissions or the disk. An episode set aside as `.deleting-<id>` in the data directory is finished when the worker next reaches serve; one not set aside is still there whole, and tried again as `upload.delete_local` says.",
    }

    /// Serve refused to record an episode as deleted here, so its folder
    /// stays.
    StoreDeletionRefused = "store.deletion_refused" {
        summary: "Serve refused to let an episode be deleted here",
        remedy: "The episode isn't verified on as many destinations as `upload.min_verified_destinations` asks, or `upload.delete_local` is `never`, so it is kept. Nothing to do: once it is verified on enough destinations it is deleted as `upload.delete_local` says.",
    }

    /// A profile in the configuration folder has a key Kapto doesn't know.
    ProfileUnknownKey = "profile.unknown_key" {
        summary: "Unknown key in a profile",
        remedy: "Remove the key or correct its spelling. The profile format reference (docs/reference/profiles.md) lists every key a profile can have.",
    }

    /// The event vocabulary, `events.toml`, has a key Kapto doesn't know.
    EventsUnknownKey = "events.unknown_key" {
        summary: "Unknown key in the event vocabulary",
        remedy: "Remove the key or correct its spelling. The reference (docs/reference/profiles.md, Event kinds) lists every key `events.toml` and its kinds can have.",
    }

    /// An event was sent of a kind that isn't in the event vocabulary.
    EventsUnknownKind = "events.unknown_kind" {
        summary: "The kind of event isn't one that may be sent",
        remedy: "Send one of the kinds the message lists (`GET /api/event-kinds` lists them too), or add it to `events.toml` in the configuration folder; a task's subtasks are `subtask.<name>`. The reference (docs/reference/profiles.md, Event kinds) shows the file.",
    }

    /// A task in the configuration folder has a key Kapto doesn't know.
    TaskUnknownKey = "task.unknown_key" {
        summary: "Unknown key in a task",
        remedy: "Remove the key or correct its spelling. The profile format reference (docs/reference/profiles.md) lists every key a task can have.",
    }

    /// No profile with the ID asked for is in the configuration folder.
    ProfileNotFound = "profile.not_found" {
        summary: "No such profile",
        remedy: "Use the ID of a profile in the configuration folder's profiles/ folder, its file's name without .toml; `kapto profile list` lists them.",
    }

    /// No task with the ID asked for is in the configuration folder.
    TaskNotFound = "task.not_found" {
        summary: "No such task",
        remedy: "Use the ID of a task in the configuration folder's tasks/ folder, its file's name without .toml; `kapto task list` lists them.",
    }

    /// A profile records a robot package that isn't among the packages.
    ProfileUnknownPackage = "profile.unknown_package" {
        summary: "The profile's robot package isn't among the packages",
        remedy: "Correct the profile's `package` to the ID of a package `kapto package list` shows, or add that package to the packages folder (`paths.packages`).",
    }

    /// An episode was asked to start with a profile for another robot
    /// package than the one capture records.
    ProfileUnavailable = "profile.unavailable" {
        summary: "The profile is for another robot package",
        remedy: "Choose a profile for the robot package Kapto runs (the web UI and /api/profiles mark which can be used), or start Kapto with the package the profile records.",
    }
}
