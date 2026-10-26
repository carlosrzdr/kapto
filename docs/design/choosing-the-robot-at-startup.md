# The robot chosen when serve starts: a package by ID, or a URDF made into one

## Context

In a container, people bring their robot in one of three shapes: a bare
URDF with its meshes, a ROS `*_description` package, or a full Kapto robot
package. They mount it and set an environment variable. They don't write
command lines. `kapto serve` takes the robot as three files (`--robot`,
`--streams`, `--mapping`) and exits if any is wrong. In a container that
restarts on failure, that becomes a crash loop that shows nothing in the
web UI. Mounts are often read-only. A URDF says how the robot looks, not
what it sends, so a robot given only as a URDF needs streams from
somewhere.

## Approach

`kapto serve` takes its robot in one of three ways, and refuses more than
one with `config.robot_ambiguous`:

- **A package**, by ID or folder: `--package` or `KAPTO_ROBOT`. Every
  folder under the packages folder is found at startup. A folder that is a
  ROS description package, with a URDF but no `package.toml`, is made into
  a Kapto package on the data volume, as `kapto package import` makes one.
- **A URDF**: `--urdf` or `KAPTO_URDF`, with meshes found beside it or
  under `KAPTO_MESH_PATH`. It is made into a package on the data volume,
  `<data>/packages/<id>`, named after the URDF's robot.
- **The three files**, as before.

With none given, the one package in the packages folder is used if there
is exactly one.

A package made from a mount is never written into the mount. It is made
again only when what it was made from changes: the source's content hash
is kept beside it. Otherwise the one already made is used, with any edits
made to it there.

A package made from a URDF receives the robot's ROS 2 joint states: one
stream on `/joint_states` (`sensor_msgs/msg/JointState`), with each moving
joint driven by the position of the same name. This is the common case,
and it needs nothing more. Its rate, which sizes capture's buffers and is
what a slow stream is judged against, is the rate the topic is heard
publishing at over a few seconds while the package is made. A robot silent
then, or a build without ROS 2, gets 1000 a second, said as the package is
made; the made package's `streams.toml` can be corrected, and is kept until
the URDF changes. A robot that sends anything else gets a
`streams.toml` and a `mapping.toml`, which turns it into a package.

A robot that fails its lint doesn't stop serve. Serve logs the same report
as `kapto package lint` once, then runs with the robot marked invalid: the
API and web UI show the report, and recording is refused with
`capture.robot_invalid` until the robot is fixed and serve restarted. A
ROS 2 stream in a build without ROS 2 is one such problem. In the plain
image, a URDF-only robot is shown with the remedy: use the ROS 2 image, or
add a `streams.toml`.

## Consequences

Whichever shape it's mounted in, a robot reaches the web UI, either
working or with the reason it doesn't. The container never restarts over a
robot it can't use. Serve has a state without a working robot, which the
API, the web UI and capture's start all handle. The data volume holds
packages made from mounts, which a backup of `/data` includes. A changed
robot, or a different one, needs a restart: switching robots and picking
up edits while running come later.

## Alternatives considered

- **A `streams.toml` always required beside a URDF.** It's explicit, but
  the most common robot, a ROS 2 one publishing joint states, would need a
  file it doesn't need. A URDF wouldn't be enough on its own.
- **A UDP JSON message keyed by joint name as a URDF's default.** It works
  without ROS 2, but it's a format Kapto would invent, which no robot sends
  without being written for Kapto.
- **Exiting on a robot that fails lint.** It's simple, but it makes a
  crash loop in a container, and the report is seen only in the logs.
- **Packages made into the mount.** That fails on read-only mounts, and
  Kapto would write into files the user owns.
