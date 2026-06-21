# Universal Robots UR5e in xacro

- **Files:** `ur_description/`, unchanged, a part of Universal Robots'
  ROS 2 description: `urdf/ur.urdf.xacro`, `urdf/ur_macro.xacro`,
  `urdf/inc/ur_common.xacro`, `config/ur5e/*.yaml` and `package.xml`.
- **Source:** <https://github.com/UniversalRobots/Universal_Robots_ROS2_Description>,
  version 3.5.1, as installed by `ros-jazzy-ur-description` from the ROS 2
  Jazzy apt repository.
- **Licence:** BSD-3-Clause, see `ur_description/LICENSE`, taken from the
  repository's `jazzy` branch.
- **Why it is here:** a real robot description in xacro: arguments, macros,
  properties, Python expressions, includes found with `$(find)`, and its
  dimensions read with `load_yaml`.
- **`ur5e.urdf`:** what ROS's own xacro makes of it, the reference the
  tests compare Kapto's expansion with. Made in the ROS 2 Jazzy image with
  `ros-jazzy-ur-description` installed:
  `xacro /opt/ros/jazzy/share/ur_description/urdf/ur.urdf.xacro ur_type:=ur5e name:=ur5e > ur5e.urdf`.
  Never edit it by hand; regenerate it.
- **Not included:** the meshes, under Universal Robots' own terms. The URDF
  names them; tests don't need them.
