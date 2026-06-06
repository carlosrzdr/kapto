# arm7: a sample seven-joint arm

A generic arm for trying Kapto without a real robot, and a template for
describing your own. It stands about 1.3 m tall when straight; its joints
turn about z, y, z, y, z, y, z from the base up, like most collaborative
arms, and it ends in a two-finger gripper. It is made of simple shapes
(hinge drums at the pitch joints, collars at the roll joints), so it needs
no mesh files.

| File | What it says |
|---|---|
| `package.toml` | What the package is: its ID `arm7`, version and name, and where the URDF is |
| `urdf/arm7.urdf` | The arm: links, joints (`joint_1` to `joint_7`, and `gripper`, which `gripper_mirror` follows) and their limits |
| `streams.toml` | What the robot sends: one stream, `arm_joints`, with the message layout of the synthetic robot in `tools/kapto-loadgen` |
| `mapping.toml` | Which decoded value drives which joint: `arm_joints.joint_positions[0]` to `[6]` for `joint_1` to `joint_7`, `arm_joints.gripper` for `gripper` |

The synthetic robot sends exactly what `streams.toml` declares: a sequence
number, the time it sent the message, the seven joint positions in radians
and the gripper opening in metres. The arm leans forward with its gripper
pointing down, as if over a table, and sweeps across it every 12 seconds,
reaching in and out twice per sweep; the gripper closes as it reaches in
and opens as it reaches out.
Everything stays well within the joint limits.

## Try it

```sh
cargo run --release --bin kapto -- record --streams packages/arm7/streams.toml --duration 10 --data-dir /tmp/kapto
cargo run --release -p kapto-loadgen -- --streams 1 --duration 5
```

The recording stores the message layout with the stream, so
`kapto decode <episode folder>` shows the joint positions by name.

To watch the arm move, run Kapto, send the synthetic robot's stream, and
open <http://127.0.0.1:7700>: the arm moves all the time, and the Record
button records an episode. Build the UI into Kapto first with
`cargo xtask ui build`, or use the container image. From the command
line:

```sh
cargo run --release --bin kapto -- serve --robot packages/arm7/urdf/arm7.urdf --streams packages/arm7/streams.toml --mapping packages/arm7/mapping.toml --data-dir /tmp/kapto &
cargo run --release -p kapto-loadgen -- --streams 1 --duration 60 &
cargo run --bin kapto -- episode start --data-dir /tmp/kapto
cargo run --bin kapto -- episode stop --data-dir /tmp/kapto
```

The same through the API, as the web UI will do it:

```sh
curl -s -X POST -H 'Content-Type: application/json' -d '{}' http://127.0.0.1:7700/api/episodes/start
curl -s -X POST -H 'Content-Type: application/json' -d '{}' http://127.0.0.1:7700/api/episodes/stop
```

The commands find each other through sockets in the data directory
(`--data-dir`), so give them all the same one.
