# Record a robot in Isaac Sim

Isaac Sim publishes a simulated robot on ROS 2 through its bridge, as a
real robot's nodes would: its joint states, its cameras and the
simulation's clock. Kapto records that as it records any ROS 2 robot, with
nothing of its own to add to the simulation. This guide sets a robot up in
Isaac Sim to publish, declares what it publishes in a robot package, and
records and replays it. The sample package
[`packages/arm7_isaac`](../../packages/arm7_isaac/README.md) is the
example throughout: the sample seven-joint arm, with a camera beside it
and one on its wrist, and a script that runs the whole scene without Isaac
Sim's window.

Read [Record a ROS 2 robot](record-a-ros2-robot.md) first: everything it
says about the `kapto-ros2` image, the network and declaring topics holds
here too. This guide says what is particular to a simulation.

## Before you start

- **Isaac Sim 6.2**, with its ROS 2 bridge, on a machine with an NVIDIA
  GPU: installed, or its container image `nvcr.io/nvidia/isaac-sim:6.2.0`.
  The bridge brings its own ROS 2 Jazzy libraries, so ROS 2 needn't be
  installed beside it. Start Isaac Sim with them chosen, as its
  documentation says: `ROS_DISTRO=jazzy`,
  `RMW_IMPLEMENTATION=rmw_fastrtps_cpp` and, on Linux,
  `LD_LIBRARY_PATH` including `exts/isaacsim.ros2.core/jazzy/lib` under
  Isaac Sim's folder.
- **Kapto's `kapto-ros2` image**, built from the repository:

  ```sh
  docker compose build kapto-ros2
  ```

- **One network for both.** On the same machine, run Kapto's container
  with `--network host`, and Isaac Sim's too if it runs in one. Both use
  Fast DDS and the default domain unless told otherwise; if Isaac Sim is
  given a `ROS_DOMAIN_ID`, give Kapto the same.

## Set the robot up in Isaac Sim

In Isaac Sim's window, from the robot's URDF, the one its package is made
from:

1. **Import it**: *File > Import*, choose the URDF, and set *Base Type*
   to *Fixed* for an arm bolted down.
2. **Give its joints drive gains.** A URDF has none, and without them each
   joint hangs limp when the simulation plays. Set each joint drive's
   *Stiffness* and *Damping* in the *Property* panel, or with *Tools >
   Robotics > Asset Editors > Gain Tuner*; the sample uses 1000 and 100.
   If the imported robot doesn't move at all when played, check that its
   physics is chosen: in the *Property* panel of the robot's top prim,
   its *Physics* variant set to *physx*.
3. **Publish its joints and the clock**: *Tools > Robotics > ROS 2
   OmniGraphs > Joint States*, with the robot's articulation root as
   *Articulation Root* and the publisher on `/joint_states`; then *Clock*,
   which publishes `/clock`. With *Subscriber* on, *Joint States* also
   moves the robot as `/joint_command` asks; otherwise drive it as it is
   meant to be driven, by its controller or a policy.
4. **Publish its cameras**: add a camera where it should be (under the
   tool's prim for one on the wrist, so it moves with it), then *Tools >
   Robotics > ROS 2 OmniGraphs > Camera*, with that camera as *Camera
   Prim* and its *RGB Topic*, e.g. `/camera/image_raw`. The form also
   publishes depth unless *Depth* is turned off.
5. **Press Play**, and save the scene (*File > Save As*) to open it again
   ready.

The sample's script does all of that without the window, which is how it
runs in Isaac Sim's container, on a machine without a display, or to try
Kapto with Isaac Sim without building a scene. With the container, from
the repository's folder:

```sh
docker run --rm --name kapto-isaac --gpus all --network host \
  -e ACCEPT_EULA=Y -e PRIVACY_CONSENT=N \
  -e ROS_DISTRO=jazzy -e RMW_IMPLEMENTATION=rmw_fastrtps_cpp \
  -e LD_LIBRARY_PATH=/isaac-sim/exts/isaacsim.ros2.core/jazzy/lib \
  -v "$PWD/packages:/packages:ro" \
  --entrypoint ./python.sh nvcr.io/nvidia/isaac-sim:6.2.0 \
  /packages/arm7_isaac/sim/arm7_sim.py
```

With an installed Isaac Sim, run the same script with its `python.sh`,
giving the URDF's path with `--urdf`. It runs for ten minutes
(`--seconds`), saying every ten simulated seconds how far it has got
(`[arm7] simulated 10 s`). The first time, Isaac Sim prepares its renderer
before the first frame, which took three minutes on a laptop; Isaac Sim's
documentation adds folders kept between runs for its caches, which make
later starts take about one. Its container needs the NVIDIA Container
Toolkit; with Podman, `--device nvidia.com/gpu=all` can replace
`--gpus all`.

## Declare what it publishes

Once the simulation plays, list its topics as the declarations of a
package's `streams.toml`, with the rates they arrived at:

```sh
docker run --rm --network host kapto-ros2:dev topics --streams --wait 5
```

The sample's [`streams.toml`](../../packages/arm7_isaac/streams.toml) is
what that gave, completed. Three things differ from a real robot's:

- **The joints' order.** Isaac Sim publishes the joints in its
  articulation's order, which needn't be the URDF's, so find each by its
  name rather than by its position:
  `path = "position[name=joint_1]"`.
- **The cameras' pictures are raw**: `sensor_msgs/msg/Image` in `rgb8`,
  1.5 MiB for 960 by 540, where a real camera often sends JPEG. Give each
  camera's stream room for its largest message, `max_message_bytes`; `kapto
  topics --streams` does it for you.
- **Fewer pictures arrive than asked for.** How many Isaac Sim publishes
  depends on the machine: on a laptop, the sample's two cameras came to
  about 40 a second together, shared unevenly between them, of the 60 the
  script asks for. Declare each camera's `rate_hz` under the least it
  delivers, so pre-flight doesn't fail on how that varies. Kapto counts the
  pictures that never arrived as lost before Kapto, as it counts any
  robot's.

Check the package, as for any robot:

```sh
cargo run --bin kapto -- package lint packages/arm7_isaac
```

## Record it

Run `serve` in the image on the host's network, with the package:

```sh
docker run --rm --network host -v kapto-data:/data -v "$PWD/packages:/packages:ro" \
  -e KAPTO_SERVER_BIND=127.0.0.1 kapto-ros2:dev serve \
  --robot /packages/arm7_isaac/urdf/arm7.urdf \
  --streams /packages/arm7_isaac/streams.toml \
  --mapping /packages/arm7_isaac/mapping.toml
```

Open <http://127.0.0.1:7700>: the arm moves as Isaac Sim moves it, and the
camera panels show what its cameras render. Pre-flight runs as `serve`
starts; if that was before the simulation played, it found the streams
silent, so run it again once it plays, with *Run again* in the page or
`POST /api/preflight`. Then record with the Record button, or from the
simulation's own script through the API, as
[Control recording from your own programs](control-recording.md) shows,
and replay the episode as any other.

## What is particular to a simulation

- **Time.** Each message is stored as received, its header's stamp in
  simulated time as Isaac Sim gives it, beside the time Kapto received it
  on the machine's clock, and the `/clock` stream records the simulated
  time as it went. A simulation that runs slower or faster than real time
  is recorded as it ran, and replayed at the machine's pace, not the
  simulation's.
- **Raw pictures cost more to show.** Kapto stores them as received; to
  show one live, the viewer bridge encodes it for the page, which takes
  more of a core than passing a camera's JPEG on.
