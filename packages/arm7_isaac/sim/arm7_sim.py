# SPDX-License-Identifier: Apache-2.0
"""The sample arm in Isaac Sim, headless, publishing over its ROS 2 bridge.

Run with Isaac Sim's own Python (`./python.sh`), as
docs/guides/record-a-robot-in-isaac-sim.md shows. It imports the arm's URDF,
bends it over three blocks on the ground, its tool pointing down, sweeps it
across them, and publishes, once a simulated frame:

- /clock, the simulation's time;
- /joint_states, the arm's joints;
- /camera/image_raw, a camera beside the arm;
- /wrist_camera/image_raw, a camera on the arm's tool, looking past the
  fingers at the blocks.

Both cameras are raw rgb8 images, as Isaac Sim renders them.
"""

import argparse
import math
import time

from isaacsim import SimulationApp

parser = argparse.ArgumentParser()
parser.add_argument("--urdf", default="/packages/arm7_isaac/urdf/arm7.urdf")
parser.add_argument("--seconds", type=float, default=600.0, help="simulated time to run for")
parser.add_argument("--camera", default="960x540", help="the camera beside the arm, width x height")
parser.add_argument("--wrist-camera", default="640x480", help="the camera on the tool, width x height")
parser.add_argument("--camera-every", type=int, default=2, help="simulated frames between pictures")
parser.add_argument("--stiffness", type=float, default=1000.0, help="each joint drive's stiffness")
parser.add_argument("--damping", type=float, default=100.0, help="each joint drive's damping")
args, _ = parser.parse_known_args()

simulation_app = SimulationApp({"headless": True, "renderer": "RaytracedLighting"})

import isaacsim.core.experimental.utils.app as app_utils
import isaacsim.core.experimental.utils.stage as stage_utils
import omni.graph.core as og
import omni.usd
import usdrt.Sdf
from isaacsim.core.simulation_manager import SimulationManager
from pxr import Gf, UsdGeom, UsdLux, UsdPhysics

app_utils.enable_extension("isaacsim.ros2.bridge")
app_utils.enable_extension("isaacsim.asset.importer.urdf")
simulation_app.update()

from isaacsim.asset.importer.urdf import URDFImporter, URDFImporterConfig

DT = 1.0 / 60.0
ROBOT = "/World/arm7"
GRAPH = "/World/ROS"

stage_utils.set_stage_units(meters_per_unit=1.0)
stage = omni.usd.get_context().get_stage()
UsdGeom.Xform.Define(stage, "/World")

config = URDFImporterConfig()
config.urdf_path = args.urdf
config.usd_path = "/tmp/arm7_usd"
config.fix_base = True
config.joint_drive_type = "position"
# A URDF has no drive gains: without these each joint hangs limp.
config.override_joint_stiffness = args.stiffness
config.override_joint_damping = args.damping
stage_utils.add_reference_to_stage(URDFImporter(config).import_urdf(), ROBOT)
# The imported robot holds its physics in a variant, none chosen by default.
stage.GetPrimAtPath(ROBOT).GetVariantSets().GetVariantSet("Physics").SetVariantSelection("physx")


def path_ending(suffix, schema=None):
    """The first prim of the robot whose path ends with `suffix`."""
    for prim in stage.Traverse():
        path = prim.GetPath().pathString
        if path.startswith(ROBOT) and path.endswith(suffix) and (schema is None or prim.HasAPI(schema)):
            return path
    raise SystemExit(f"no prim ending in {suffix} under {ROBOT}")


def block(name, position, size, colour):
    """A coloured block on the ground, for the cameras to look at."""
    cube = UsdGeom.Cube.Define(stage, f"/World/{name}")
    cube.GetSizeAttr().Set(1.0)
    xform = UsdGeom.XformCommonAPI(cube)
    xform.SetTranslate(Gf.Vec3d(*position))
    xform.SetScale(Gf.Vec3f(*size))
    cube.GetDisplayColorAttr().Set([Gf.Vec3f(*colour)])
    UsdPhysics.CollisionAPI.Apply(cube.GetPrim())


block("Ground", (0, 0, -0.01), (4, 4, 0.02), (0.35, 0.36, 0.38))
block("Red", (0.45, 0.25, 0.05), (0.1, 0.1, 0.1), (0.8, 0.15, 0.1))
block("Green", (0.4, -0.3, 0.04), (0.08, 0.16, 0.08), (0.1, 0.65, 0.3))
block("Blue", (-0.35, 0.4, 0.06), (0.12, 0.12, 0.12), (0.15, 0.3, 0.8))
UsdLux.DomeLight.Define(stage, "/World/Light").GetIntensityAttr().Set(1000)

# The camera beside the arm, looking at it from the front.
front = UsdGeom.Camera.Define(stage, "/World/Camera")
front.GetFocalLengthAttr().Set(18)
UsdGeom.XformCommonAPI(front).SetTranslate(Gf.Vec3d(2.2, 0, 1.0))
UsdGeom.XformCommonAPI(front).SetRotate((75, 0, 90), UsdGeom.XformCommonAPI.RotationOrderXYZ)
simulation_app.update()

# The wrist camera, a child of the tool so it moves with it: on the tool's
# side, looking along the fingers (a USD camera looks down its -Z, so it is
# turned half round), tilted a little towards them.
tool = path_ending("/tool")
wrist = UsdGeom.Camera.Define(stage, f"{tool}/WristCamera")
wrist.GetFocalLengthAttr().Set(12)
wrist.GetClippingRangeAttr().Set(Gf.Vec2f(0.01, 100))
UsdGeom.XformCommonAPI(wrist).SetTranslate(Gf.Vec3d(0, 0.055, 0.03))
UsdGeom.XformCommonAPI(wrist).SetRotate((-12, 180, 0), UsdGeom.XformCommonAPI.RotationOrderXYZ)
root = path_ending("", UsdPhysics.ArticulationRootAPI)
print(f"[arm7] articulation {root}, wrist camera on {tool}", flush=True)


def size(text):
    width, height = text.lower().split("x")
    return int(width), int(height)


def camera_nodes(name, prim, topic, resolution):
    """The nodes rendering camera `prim` and publishing it on `topic`."""
    width, height = size(resolution)
    return (
        [(f"{name}Render", "isaacsim.core.nodes.IsaacCreateRenderProduct"),
         (name, "isaacsim.ros2.bridge.ROS2CameraHelper")],
        [("Tick.outputs:tick", f"{name}Render.inputs:execIn"),
         (f"{name}Render.outputs:execOut", f"{name}.inputs:execIn"),
         (f"{name}Render.outputs:renderProductPath", f"{name}.inputs:renderProductPath"),
         ("Context.outputs:context", f"{name}.inputs:context")],
        [(f"{name}Render.inputs:cameraPrim", [usdrt.Sdf.Path(prim)]),
         (f"{name}Render.inputs:width", width),
         (f"{name}Render.inputs:height", height),
         (f"{name}.inputs:topicName", topic),
         (f"{name}.inputs:frameId", name.lower()),
         (f"{name}.inputs:type", "rgb"),
         (f"{name}.inputs:frameSkipCount", max(args.camera_every - 1, 0))],
    )


cameras = [
    camera_nodes("Camera", "/World/Camera", "camera/image_raw", args.camera),
    camera_nodes("WristCamera", f"{tool}/WristCamera", "wrist_camera/image_raw", args.wrist_camera),
]
keys = og.Controller.Keys
og.Controller.edit(
    {"graph_path": GRAPH, "evaluator_name": "execution"},
    {
        keys.CREATE_NODES: [
            ("Tick", "omni.graph.action.OnPlaybackTick"),
            ("Context", "isaacsim.ros2.bridge.ROS2Context"),
            ("SimTime", "isaacsim.core.nodes.IsaacReadSimulationTime"),
            ("Clock", "isaacsim.ros2.bridge.ROS2PublishClock"),
            ("ReadJoints", "isaacsim.sensors.physics.IsaacReadJointState"),
            ("Joints", "isaacsim.ros2.bridge.ROS2PublishJointState"),
            ("Drive", "isaacsim.core.nodes.IsaacArticulationController"),
        ] + [node for nodes, _, _ in cameras for node in nodes],
        keys.CONNECT: [
            ("Tick.outputs:tick", "Clock.inputs:execIn"),
            ("SimTime.outputs:simulationTime", "Clock.inputs:timeStamp"),
            ("Context.outputs:context", "Clock.inputs:context"),
            ("Tick.outputs:tick", "ReadJoints.inputs:execIn"),
            ("ReadJoints.outputs:execOut", "Joints.inputs:execIn"),
            ("ReadJoints.outputs:jointNames", "Joints.inputs:jointNames"),
            ("ReadJoints.outputs:jointPositions", "Joints.inputs:jointPositions"),
            ("ReadJoints.outputs:jointVelocities", "Joints.inputs:jointVelocities"),
            ("ReadJoints.outputs:jointEfforts", "Joints.inputs:jointEfforts"),
            ("ReadJoints.outputs:jointDofTypes", "Joints.inputs:jointDofTypes"),
            ("ReadJoints.outputs:stageMetersPerUnit", "Joints.inputs:stageMetersPerUnit"),
            ("ReadJoints.outputs:sensorTime", "Joints.inputs:sensorTime"),
            ("Context.outputs:context", "Joints.inputs:context"),
            ("Tick.outputs:tick", "Drive.inputs:execIn"),
        ] + [edge for _, edges, _ in cameras for edge in edges],
        keys.SET_VALUES: [
            ("ReadJoints.inputs:prim", [usdrt.Sdf.Path(root)]),
            ("Joints.inputs:topicName", "joint_states"),
            ("Drive.inputs:robotPath", root),
        ] + [value for _, _, values in cameras for value in values],
    },
)
simulation_app.update()

SimulationManager.setup_simulation(dt=DT, device="cpu")
app_utils.play()
simulation_app.update()

joints = [f"joint_{i}" for i in range(1, 8)] + ["gripper"]
og.Controller.attribute(f"{GRAPH}/Drive.inputs:jointNames").set(joints)
command = og.Controller.attribute(f"{GRAPH}/Drive.inputs:positionCommand")

# Bent over the blocks, the tool pointing straight down 45 cm out from the
# base and 25 cm above the ground: joints 2, 4 and 6 bend the arm, and add
# up to half a turn.
OVER = (0.30, 1.86, 0.98)


def reaching(t):
    """Each joint's position at `t` s: over the blocks, the base sweeping
    across the red and green ones, the arm dipping as if to reach, the tool
    still pointing down, while the wrist turns a little and the gripper
    opens and closes."""
    settled = min(t / 3.0, 1.0)
    dip = 0.12 * math.sin(0.5 * t)
    bend = [OVER[0] + dip, OVER[1] - 0.5 * dip, OVER[2] - 0.5 * dip]
    sweep = 0.9 * math.sin(0.15 * t)
    wrist = 0.4 * math.sin(0.3 * t)
    arm = [sweep, bend[0], 0.0, bend[1], 0.0, bend[2], wrist]
    return [settled * q for q in arm] + [0.0175 + 0.0175 * math.sin(t)]


# At the wall clock's pace, as a robot would move.
started = time.monotonic()
for frame in range(int(args.seconds / DT)):
    command.set(reaching(frame * DT))
    t = frame * DT
    simulation_app.update()
    ahead = (frame + 1) * DT - (time.monotonic() - started)
    if ahead > 0:
        time.sleep(ahead)
    elif ahead < -0.1:
        # Fallen behind, as while the first frames are rendered: carried on
        # from now, rather than racing through what was missed.
        started -= ahead
    if frame % 600 == 0:
        print(f"[arm7] simulated {t:.0f} s", flush=True)

app_utils.stop()
simulation_app.close()
