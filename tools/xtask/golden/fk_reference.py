# SPDX-License-Identifier: Apache-2.0
"""Link poses computed by ROS 2's robot_state_publisher, as reference values
for Kapto's forward kinematics.

Run inside a ROS 2 container by `cargo xtask golden fk`. Reads a URDF on
stdin, starts robot_state_publisher with it, publishes joint values, reads
every link's pose back from tf, and prints JSON on stdout. The first
argument names the generator, recorded in the output.
"""

import json
import math
import random
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET

import rclpy
from rclpy.duration import Duration
from rclpy.node import Node
from sensor_msgs.msg import JointState
from tf2_ros import Buffer, TransformListener

ONE_VALUE = {"revolute", "continuous", "prismatic"}


def describe(urdf):
    robot = ET.fromstring(urdf)
    links = [link.get("name") for link in robot.findall("link")]
    children = {joint.find("child").get("link") for joint in robot.findall("joint")}
    root = next(link for link in links if link not in children)
    joints = []
    for joint in robot.findall("joint"):
        if joint.get("type") not in ONE_VALUE or joint.find("mimic") is not None:
            continue
        limit = joint.find("limit")
        if joint.get("type") == "continuous" or limit is None:
            low, high = -math.pi, math.pi
        else:
            low, high = float(limit.get("lower", 0)), float(limit.get("upper", 0))
        joints.append((joint.get("name"), low, high))
    return root, links, joints


def cases(joints):
    rng = random.Random(20261010)
    yield {name: 0.0 for name, _, _ in joints}
    yield {name: low for name, low, _ in joints}
    yield {name: high for name, _, high in joints}
    for _ in range(5):
        yield {name: rng.uniform(low, high) for name, low, high in joints}


def attempt(node, states, buffer, root, links, values):
    """Publishes `values` once and reads every link's pose at that time."""
    message = JointState()
    message.header.stamp = node.get_clock().now().to_msg()
    message.name = list(values)
    message.position = list(values.values())
    stamp = rclpy.time.Time.from_msg(message.header.stamp)
    states.publish(message)
    poses = {}
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        rclpy.spin_once(node, timeout_sec=0.05)
        for link in links:
            if link in poses:
                continue
            try:
                t = buffer.lookup_transform(root, link, stamp, Duration(seconds=0)).transform
            except Exception:
                continue
            poses[link] = {
                "translation": [t.translation.x, t.translation.y, t.translation.z],
                "rotation_xyzw": [t.rotation.x, t.rotation.y, t.rotation.z, t.rotation.w],
            }
        if len(poses) == len(links):
            return poses
    return None


def main():
    urdf = sys.stdin.read()
    root, links, joints = describe(urdf)
    with tempfile.NamedTemporaryFile("w", suffix=".yaml", delete=False) as params:
        # JSON is valid YAML, and quotes the URDF safely.
        json.dump({"robot_state_publisher": {"ros__parameters": {"robot_description": urdf}}}, params)
    publisher = subprocess.Popen(
        ["ros2", "run", "robot_state_publisher", "robot_state_publisher",
         "--ros-args", "--params-file", params.name],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    rclpy.init()
    node = Node("kapto_fk_reference")
    states = node.create_publisher(JointState, "joint_states", 10)
    buffer = Buffer()
    TransformListener(buffer, node)
    results = []
    try:
        # Wait until robot_state_publisher listens, so no message is lost.
        deadline = time.monotonic() + 30
        while states.get_subscription_count() == 0 and time.monotonic() < deadline:
            rclpy.spin_once(node, timeout_sec=0.1)
        for values in cases(joints):
            poses = None
            # robot_state_publisher ignores a message no newer than the last,
            # so each attempt carries a new time.
            for _attempt in range(5):
                poses = attempt(node, states, buffer, root, links, values)
                if poses is not None:
                    break
            if poses is None:
                sys.exit(f"robot_state_publisher gave no poses for {values}")
            results.append({"joints": values, "links": poses})
    finally:
        publisher.terminate()
        rclpy.shutdown()
    generator = sys.argv[1] if len(sys.argv) > 1 else "robot_state_publisher"
    json.dump({"generator": generator, "root": root, "cases": results}, sys.stdout, indent=1)
    sys.stdout.write("\n")


main()
