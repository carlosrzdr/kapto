# Check a robot package against your robot

Before recording with a new robot package, check that it describes the
robot correctly: that its files are complete and consistent, and that its
decoders read what the robot really sends. Both checks run on any machine,
without the robot connected to Kapto.

## Start from the robot's own description

A robot's URDF and meshes usually come from its vendor, in a repository
laid out for ROS. Make a package of them in Kapto's packages folder:

```sh
kapto package import ~/src/unitree_ros/robots/h1_description/urdf/h1.urdf --id h1
```

The URDF is copied as it is, with every mesh and texture it names, placed
so each is found as the URDF names it: a `package://h1_description/...`
file under `h1_description/` in the package, a relative one where it was
relative to the URDF. Only the files the URDF uses are copied. A file it
names by an absolute path can't travel with the package, and one that
isn't there can't be copied: each is said, with its cause code, and the
rest is imported. The package gets a `package.toml`, a `streams.toml` to
fill in with what the robot sends, and a `mapping.toml` listing each of
its moving joints, to give the field that drives it. A description in
xacro is expanded to URDF first: with ROS 2, `xacro robot.urdf.xacro >
robot.urdf`.

## Check the package itself

```sh
kapto package lint packages/my_robot
```

The linter loads the package the way Kapto does to run it and reports every
problem at once, each with its cause code, its file and line where it has
them, and what to do. Errors, such as a mapped joint the robot doesn't have
or a field in the wrong unit, stop Kapto from using the package and make the
command fail. Warnings, such as a missing mesh or a joint without a velocity
limit, don't. `--format json` gives the same report for scripts. The
[package format](../reference/package.md) lists every rule.

## Check the decoders against real traffic

Capture what the robot sends, on the machine that receives it or anywhere
on the same network, with `tcpdump` (or Wireshark), for a few seconds:

```sh
sudo tcpdump -i any -s 0 -w robot.pcap udp port 9000
```

Then decode the capture with the package:

```sh
kapto decode --pcap robot.pcap --package packages/my_robot
```

Each UDP datagram goes to the stream whose `udp.bind` port (and address,
multicast group and allowed sources, if declared) it matches, as the capture
process would receive it, and is decoded with that stream's decoder. The
table shows the first messages of each stream with their values and units;
values that look wrong (a joint at 3000 radians, a counter that jumps)
usually mean a wrong offset, type or byte order in `streams.toml`. A message
that can't be decoded says why. For every message of one stream as CSV:

```sh
kapto decode --pcap robot.pcap --package packages/my_robot --stream arm_joints --format csv > arm_joints.csv
```

Captures in pcap and pcapng format are read, of Ethernet (with VLAN tags),
Linux cooked (`-i any`), loopback and raw IP links, over IPv4 and IPv6.
Streams that arrive over TCP aren't looked for in a capture yet.
Datagrams split into fragments because they were larger than one packet
can't be read on their own and are counted, as are packets cut short when
captured; capture with `-s 0` to keep them whole.
