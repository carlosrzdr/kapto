# syntax=docker/dockerfile:1
#
# Kapto container images. Two targets:
#   kapto       slim runtime for robots that speak UDP/TCP
#   kapto-ros2  Kapto built with ROS 2 (feature `ros2`), on a ROS 2 base image
#
# Build with Docker (BuildKit) only; nothing else is needed on the host:
#   docker compose build

# Base images are pinned by digest so builds are reproducible. Update them
# deliberately, in their own pull request.
ARG RUST_IMAGE=rust:1.99-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa
ARG RUNTIME_IMAGE=debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f
ARG NODE_IMAGE=node:24-bookworm-slim@sha256:d6aa754f16b3197301076f047b5def2f02ea1dbbc2ca920407d46d7ec7f87b20
ARG ROS2_IMAGE=ros:jazzy-ros-core@sha256:8bbec5839a5258d43c8ff8d44c79c87a5c5a929ad419b22d0fa6cd5f196c7818

# --- web UI ----------------------------------------------------------------
# Checked, tested and built with the pinned Node.js; only its output goes
# on, into the binary.
FROM ${NODE_IMAGE} AS ui
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run check && npm test && npm run build

# --- build -----------------------------------------------------------------
# Built on Debian bookworm: its older C library means the binary also runs on
# the newer bases below (Debian trixie, Ubuntu 24.04 under ROS 2 Jazzy).
FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY . .
# Embedded in the binary by kapto-server's build script.
COPY --from=ui /ui/dist ui/dist
# The cargo registry and build directory are cache mounts, so a rebuild after
# a code change recompiles only what changed. The binary is copied out of the
# cache in the same step, because cache mounts aren't part of the image.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --package kapto-cli \
    && install -D target/release/kapto /out/kapto

# --- build with ROS 2 ------------------------------------------------------
# Kapto with the `ros2` feature, built against the pinned ROS 2 installation
# it then runs on: ROS 2's client library is linked, not bundled. Rust comes
# from the pinned Rust image; libclang generates the bindings to ROS 2's C
# library. `cargo xtask ros2` checks and tests the feature in `ros2-dev`.
FROM ${RUST_IMAGE} AS rust

# ROS 2 with the middlewares robots use besides the default, Fast DDS:
# Cyclone DDS and Zenoh. Kapto uses whichever RMW_IMPLEMENTATION names, as
# the robot's nodes do; Zenoh's nodes also need its router (rmw_zenohd)
# reachable.
FROM ${ROS2_IMAGE} AS ros2-base
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates ros-${ROS_DISTRO}-rmw-cyclonedds-cpp ros-${ROS_DISTRO}-rmw-zenoh-cpp \
       ros-${ROS_DISTRO}-tf2-msgs \
    && rm -rf /var/lib/apt/lists/*

FROM ros2-base AS ros2-dev
# rosbag2 with MCAP storage only for the checks: ROS 2's own tool reads
# what Kapto records.
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential ca-certificates libclang-dev \
       ros-${ROS_DISTRO}-ros2bag ros-${ROS_DISTRO}-rosbag2-storage-mcap \
    && rm -rf /var/lib/apt/lists/*
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
# The components rust-toolchain.toml names, which the Rust image lacks.
RUN rustup component add clippy rustfmt
# Rust types are made for few packages of messages only: Kapto takes what
# it records still serialised, needing none, and its tests publish from
# `ros2 topic pub`, as a robot's node would, in a process of its own. The
# synthetic robot publishes sensor_msgs' JointState, which needs
# geometry_msgs and std_msgs too (r2r generates no dependencies itself).
ENV IDL_PACKAGE_FILTER="std_msgs;geometry_msgs;sensor_msgs"

FROM ros2-dev AS build-ros2
WORKDIR /src
COPY . .
COPY --from=ui /ui/dist ui/dist
# Its own build directory: the slim build's is for another C library and
# other features. The cargo caches are shared with the slim build, one build
# at a time, as compose builds both images at once.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=kapto-target-ros2,target=/src/target \
    . /opt/ros/$ROS_DISTRO/setup.sh \
    && cargo build --release --locked --package kapto-cli --features ros2 \
    && install -D target/release/kapto /out/kapto

# --- runtimes ----------------------------------------------------------------
# Both runtimes are set up the same way: an unprivileged user with fixed IDs
# (10001), and data, robot packages and configuration on explicit mounts. The
# KAPTO_* variables point Kapto at the mounts; each can be overridden at
# `docker run`.
FROM ${RUNTIME_IMAGE} AS kapto
# Uploads to cloud storage trust the certificates the image has.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN groupadd --system --gid 10001 kapto \
    && useradd --system --uid 10001 --gid kapto --no-create-home --shell /usr/sbin/nologin kapto \
    && install -d -o kapto -g kapto /data /packages /config
# Inside a container Kapto listens on every address, or published ports
# couldn't reach it; which of the host's addresses it is reachable on is
# decided by `docker run -p` (e.g. `-p 127.0.0.1:7700:7700`).
ENV KAPTO_CONFIG=/config/kapto.toml \
    KAPTO_PATHS_DATA=/data \
    KAPTO_PATHS_PACKAGES=/packages \
    KAPTO_SERVER_BIND=0.0.0.0
# Web UI and API, and the live view's stream.
EXPOSE 7700 9877
COPY --from=build /out/kapto /usr/local/bin/kapto
USER kapto
VOLUME ["/data", "/packages", "/config"]
ENTRYPOINT ["kapto"]
CMD ["config", "show"]

FROM ros2-base AS kapto-ros2
RUN groupadd --system --gid 10001 kapto \
    && useradd --system --uid 10001 --gid kapto --no-create-home --shell /usr/sbin/nologin kapto \
    && install -d -o kapto -g kapto /data /packages /config
# Inside a container Kapto listens on every address, or published ports
# couldn't reach it; which of the host's addresses it is reachable on is
# decided by `docker run -p` (e.g. `-p 127.0.0.1:7700:7700`). ROS 2 writes
# its logs under ROS_HOME, which the kapto user, without a home, can't
# otherwise: ROS 2 wouldn't start. Fast DDS sends to a node on the same
# machine through shared memory, which a node outside the container, or of
# another user, never sees: it would find Kapto and send it nothing. Over
# UDP alone it reaches Kapto wherever it runs; Kapto's side is enough, and
# the other middlewares ignore it. Shared memory still works with
# FASTDDS_BUILTIN_TRANSPORTS=DEFAULT, --ipc host and the robot nodes' user
# (`docker run --user`); the ROS 2 guide says how.
ENV KAPTO_CONFIG=/config/kapto.toml \
    KAPTO_PATHS_DATA=/data \
    KAPTO_PATHS_PACKAGES=/packages \
    KAPTO_SERVER_BIND=0.0.0.0 \
    ROS_HOME=/tmp/ros \
    FASTDDS_BUILTIN_TRANSPORTS=UDPv4
# Web UI and API, and the live view's stream.
EXPOSE 7700 9877
COPY --from=build-ros2 /out/kapto /usr/local/bin/kapto
USER kapto
VOLUME ["/data", "/packages", "/config"]
# ROS 2's environment set up first, so Kapto finds its libraries and the
# middleware, as the robot's nodes do.
ENTRYPOINT ["/ros_entrypoint.sh", "kapto"]
CMD ["config", "show"]
