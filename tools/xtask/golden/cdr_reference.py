# SPDX-License-Identifier: Apache-2.0
"""Messages serialised by ROS 2 itself, as reference data for Kapto's CDR
reader and `.msg` parser.

Run inside a ROS 2 container by `cargo xtask golden cdr`. For each message
it prints, as one JSON document on stdout: its type, its definition as
rosbag2 stores it in MCAP (written by rosbag2 itself, then read back from
the file; for a type rosbag2 stores as IDL, because its package lists it
as an `.idl` interface, the `.msg` file put together the same way, with
`rosbag2_encoding` saying so), the CDR bytes `rclpy.serialization.serialize_message` gives, and
its field values; and every `.msg` file those definitions are made of, as
installed, so Kapto putting a definition together from them can be checked
against rosbag2's. The first argument names the generator, recorded in the
output.
"""

import array
import glob
import importlib
import json
import os
import struct
import sys
import tempfile

import rosbag2_py

from ament_index_python.packages import get_package_share_directory
from rclpy.serialization import serialize_message

PRIMITIVES = {
    "bool", "byte", "char", "float32", "float64", "int8", "uint8", "int16",
    "uint16", "int32", "uint32", "int64", "uint64", "string", "wstring",
}


def msg_text(package, name):
    path = os.path.join(get_package_share_directory(package), "msg", name + ".msg")
    with open(path, encoding="utf-8") as file:
        return file.read()


def used_types(package, text):
    """The message types a definition's fields use, as (package, name)."""
    used = []
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        type_name, _, rest = line.partition(" ")
        if "=" in rest:
            continue  # a constant: TYPE NAME=VALUE
        base = type_name.split("[", 1)[0].split("<=", 1)[0]
        if base in PRIMITIVES:
            continue
        if "/" in base:
            parts = base.split("/")
            used.append((parts[0], parts[-1]))
        else:
            used.append((package, base))
    return used


def assembled(package, name):
    """The definition as rosbag2 puts it together from `.msg` files: the
    type's own, then each type it uses once, depth first, the types each
    uses in alphabetical order, after a line of 80 `=` and `MSG:
    package/Type`."""
    seen = set()
    sections = []

    def visit(pkg, typ):
        for dependency in sorted(set(used_types(pkg, msg_text(pkg, typ)))):
            if dependency in seen:
                continue
            seen.add(dependency)
            sections.append(
                "\n" + "=" * 80 + "\nMSG: " + dependency[0] + "/" + dependency[1] + "\n"
                + msg_text(*dependency)
            )
            visit(*dependency)

    visit(package, name)
    return msg_text(package, name) + "".join(sections)


def msg_files(package, name, files):
    """Adds the `.msg` file of the type and of every type it uses to `files`,
    by `package/Type`."""
    key = package + "/" + name
    if key in files:
        return
    files[key] = msg_text(package, name)
    for dependency in used_types(package, files[key]):
        msg_files(*dependency, files)


def rosbag2_definitions(type_names):
    """Each type's definition as rosbag2 writes it into an MCAP file: a topic
    of each type is created with rosbag2's writer, and the schema records are
    read back from the file."""
    folder = os.path.join(tempfile.mkdtemp(), "bag")
    writer = rosbag2_py.SequentialWriter()
    writer.open(
        rosbag2_py.StorageOptions(uri=folder, storage_id="mcap"),
        rosbag2_py.ConverterOptions("cdr", "cdr"),
    )
    for number, type_name in enumerate(type_names):
        writer.create_topic(rosbag2_py.TopicMetadata(
            id=number, name="/t%d" % number, type=type_name, serialization_format="cdr"))
    del writer
    data = open(glob.glob(os.path.join(folder, "*.mcap"))[0], "rb").read()
    definitions = {}
    at = 8  # after the magic
    while at < len(data):
        opcode = data[at]
        length = struct.unpack_from("<Q", data, at + 1)[0]
        body = data[at + 9:at + 9 + length]
        if opcode == 0x02:  # footer
            break
        if opcode == 0x03:  # schema: id, name, encoding, data
            texts = []
            offset = 2
            for _ in range(3):
                size = struct.unpack_from("<I", body, offset)[0]
                texts.append(body[offset + 4:offset + 4 + size])
                offset += 4 + size
            name, encoding, text = texts
            definitions[name.decode()] = (encoding.decode(), text.decode())
        at += 9 + length
    return definitions


def plain(value):
    """A field's value as JSON: messages as objects in field order, arrays
    as lists, bytes as numbers."""
    if hasattr(value, "get_fields_and_field_types"):
        return {
            field: plain(getattr(value, field))
            for field in value.get_fields_and_field_types()
        }
    if isinstance(value, (bytes, bytearray)):
        return value[0] if len(value) == 1 else list(value)
    if isinstance(value, (list, tuple, array.array)) or type(value).__name__ == "ndarray":
        return [plain(item) for item in value]
    if type(value).__module__ == "numpy":
        return value.item()
    return value


def make(type_name, **fields):
    package, _, name = type_name.split("/")
    module = importlib.import_module(package + ".msg")
    message = getattr(module, name)()
    for field, value in fields.items():
        setattr(message, field, value)
    return message


def fill(message, values):
    for field, value in values.items():
        current = getattr(message, field)
        if isinstance(value, dict):
            fill(current, value)
        else:
            setattr(message, field, value)
    return message


def cases():
    from geometry_msgs.msg import Point, Quaternion
    from std_msgs.msg import Header
    from builtin_interfaces.msg import Time

    header = Header(stamp=Time(sec=1773066812, nanosec=500), frame_id="base_link")
    yield "joint_state", make(
        "sensor_msgs/msg/JointState",
        header=header,
        name=["joint_1", "joint_2", "gripper"],
        position=[0.5, -1.25, 0.04],
        velocity=[],
        effort=[1.5, 2.0, 0.25],
    )
    yield "pose", make(
        "geometry_msgs/msg/Pose",
        position=Point(x=0.1, y=-0.2, z=1.3),
        orientation=Quaternion(x=0.0, y=0.0, z=0.7071067811865476, w=0.7071067811865476),
    )
    # Types whose fields use types in other than alphabetical order: rosbag2
    # lists the types a definition uses alphabetically.
    yield "pose_stamped", make(
        "geometry_msgs/msg/PoseStamped",
        header=header,
        pose=make("geometry_msgs/msg/Pose", position=Point(x=1.0, y=2.0, z=3.0)),
    )
    yield "twist_with_covariance_stamped", make(
        "geometry_msgs/msg/TwistWithCovarianceStamped",
        header=header,
    )
    yield "image", make(
        "sensor_msgs/msg/Image",
        header=header,
        height=2,
        width=3,
        encoding="mono8",
        is_bigendian=0,
        step=3,
        data=[0, 1, 2, 253, 254, 255],
    )
    # ROS 2's own test messages: every primitive, every kind of array,
    # bounded strings, nesting.
    yield "basic_types", make(
        "test_msgs/msg/BasicTypes",
        bool_value=True, byte_value=b"\xfe", char_value=200,
        float32_value=1.5, float64_value=-2.25,
        int8_value=-8, uint8_value=200, int16_value=-1600, uint16_value=60000,
        int32_value=-320000, uint32_value=4000000000,
        int64_value=-9000000000000, uint64_value=18000000000000000000,
    )
    yield "strings", make(
        "test_msgs/msg/Strings",
        string_value="hello", bounded_string_value="bounded",
        string_value_default1="", string_value_default2="",
        string_value_default3="", string_value_default4="", string_value_default5="",
        bounded_string_value_default1="", bounded_string_value_default2="",
        bounded_string_value_default3="", bounded_string_value_default4="",
        bounded_string_value_default5="",
    )
    yield "arrays", make(
        "test_msgs/msg/Arrays",
        bool_values=[True, False, True], int16_values=[-1, 0, 1],
        float64_values=[0.5, 1.5, 2.5], string_values=["a", "", "ccc"],
    )
    yield "bounded_sequences", make(
        "test_msgs/msg/BoundedSequences",
        int32_values=[7, -7], string_values=["x"], uint8_values=[1, 2, 3],
        float64_values=[3.25],
    )
    yield "unbounded_sequences", make(
        "test_msgs/msg/UnboundedSequences",
        int64_values=[1, -2, 3], float32_values=[0.25], bool_values=[],
        string_values=["one", "two"],
    )
    yield "wstrings", make(
        "test_msgs/msg/WStrings",
        wstring_value="hé", array_of_wstrings=["a", "", "bc"],
        unbounded_sequence_of_wstrings=["xyz"],
    )
    yield "multi_nested", make("test_msgs/msg/MultiNested")
    yield "nested", make("test_msgs/msg/Nested")


def main():
    generator = sys.argv[1]
    out = {"generator": generator, "messages": {}, "msg_files": {}}
    messages = [
        (case, type(message).__module__.split(".")[0] + "/msg/" + type(message).__name__, message)
        for case, message in cases()
    ]
    definitions = rosbag2_definitions(sorted({type_name for _, type_name, _ in messages}))
    files = {}
    for case, type_name, message in messages:
        package, _, name = type_name.split("/")
        msg_files(package, name, files)
        encoding, text = definitions[type_name]
        out["messages"][case] = {
            "type": type_name,
            "rosbag2_encoding": encoding,
            "definition": text if encoding == "ros2msg" else assembled(package, name),
            "cdr": serialize_message(message).hex(),
            "values": plain(message),
        }
    out["msg_files"] = dict(sorted(files.items()))
    json.dump(out, sys.stdout, indent=1, sort_keys=False)
    sys.stdout.write("\n")


main()
