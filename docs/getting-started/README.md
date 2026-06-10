# Getting started

## Try the demo

`kapto demo` runs Kapto with a built-in sample robot: a seven-joint arm, and
a synthetic robot inside Kapto that sends its joint positions 2000 times a
second over UDP on the same machine, with a wrist camera's 640x480 pictures
30 times a second over TCP beside them (`--no-camera` leaves it out). Open <http://localhost:7700> to see the
arm move, each stream's counts, and a Record button; recorded episodes are
listed beside it and stored in the data directory, and each one's **Replay**
plays it again in the same view, on its own timeline, until **Back to live**.

With Docker:

```sh
docker compose build
docker run --rm -p 127.0.0.1:7700:7700 -p 127.0.0.1:9877:9877 kapto:dev demo
```

From source (the web UI is built in Docker, so Docker is still needed once):

```sh
cargo xtask ui build
cargo run --release --bin kapto -- demo
```

Ctrl-C stops it, finalising an episode that is being recorded. To see
episodes uploaded too, without a cloud account, follow
[Try uploads on this machine](../guides/try-uploads.md).

## Show it to people

`kapto showcase` is the demo, narrated, for showing Kapto to someone in a few
minutes. It prints each step and waits for Enter, so you can talk over it:

1. The arm live in the page; add a sample cell (a table, a red cube, a bin)
   from **Environment**.
2. A dozen ordinary episodes of the task *Pick and place*, each checked as it
   ends, so the anomaly model learns what usual looks like.
3. One episode in which the robot misbehaves (its shoulder freezes and jumps
   back, its wrist vibrates, messages are lost on the way) with an
   intervention marked around it, and what Kapto found in it.
4. Where to find it in the page and watch each moment found.

It keeps its profile, task and episodes in `showcase/` in the data directory
and never reads or writes your configuration. `--unattended` runs every step
on by itself; `--episodes` and `--seconds` make the first part shorter or
longer.

```sh
docker run --rm -it -p 127.0.0.1:7700:7700 -p 127.0.0.1:9877:9877 kapto:dev showcase
```

or, from source:

```sh
cargo run --release --bin kapto -- showcase
```

## Run with Docker

Docker (with BuildKit, the default) is the only prerequisite. From a clone of
the repository:

```sh
docker compose build                      # builds kapto:dev and kapto-ros2:dev
docker run --rm kapto:dev                 # shows the configuration
```

The images keep Kapto's files on three mounts: recordings in `/data`, robot
packages in `/packages`, and `kapto.toml` in `/config`. To use your own
configuration file:

```sh
docker run --rm -v ./kapto.toml:/config/kapto.toml:ro kapto:dev
```

`kapto-ros2:dev` is Kapto built with ROS 2, on a ROS 2 Jazzy base image, for
robots that publish ROS 2 topics. On the robot's network, it lists the
topics it sees and their message types:

```sh
docker run --rm --network host kapto-ros2:dev topics
```

Set `ROS_DOMAIN_ID` and `RMW_IMPLEMENTATION` (`-e`) as the robot's nodes
have them, if they aren't the defaults. The image has the three middlewares
ROS 2 robots use: Fast DDS (`rmw_fastrtps_cpp`, the default), Cyclone DDS
(`rmw_cyclonedds_cpp`) and Zenoh (`rmw_zenoh_cpp`). With Zenoh, nodes find
each other through a Zenoh router, which must be running and reachable, as
for the robot's own nodes (`ros2 run rmw_zenoh_cpp rmw_zenohd`; the image
has it too):

```sh
docker run --rm --network host -e RMW_IMPLEMENTATION=rmw_zenoh_cpp kapto-ros2:dev topics
```

Kapto runs as an unprivileged user (UID and GID 10001) inside both images.

## Build from source

You need the Rust toolchain ([rustup](https://rustup.rs)); the repository
pins the exact version in `rust-toolchain.toml`, and rustup installs it on
first use.

```sh
git clone https://github.com/carlosrzdr/kapto.git
cd kapto
cargo build --release
```

The `kapto` binary is in `target/release/`.

## First run

```sh
target/release/kapto config show
```

This lists every setting with its value and where the value came from. On a
fresh machine there is no configuration file, so every value is a default.
See [Configuration](../concepts/configuration.md) for how to change them.

## First recording

Without a robot, the synthetic one in `tools/kapto-loadgen` stands in: it
sends streams of joint positions over UDP, stream `i` to port 9000 + `i`,
each message starting with a 4-byte little-endian sequence number. Describe
two of its streams in a `streams.toml`:

```toml
[streams.arm_left]
rate_hz = 2000
udp = { bind = "127.0.0.1:9000" }
sequence = { offset = 0, type = "u32" }

[streams.arm_right]
rate_hz = 2000
udp = { bind = "127.0.0.1:9001" }
sequence = { offset = 0, type = "u32" }
```

Start recording for 10 seconds, then start the synthetic robot in a second
terminal:

```sh
target/release/kapto record --streams streams.toml --duration 10 --data-dir /tmp/kapto
target/release/kapto-loadgen --streams 2 --duration 5
```

While it records, Kapto prints a line per second with how many messages it
received, wrote, dropped, and how many were lost before reaching it. At the
end it prints the folder of the finished episode and each stream's counts.
[Episodes on disk](../concepts/episodes.md) explains what the folder holds.

If Kapto warns that the UDP receive buffer is smaller than configured,
recording still works; the warning says how to raise the host's limit.
