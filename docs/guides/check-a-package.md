# Check a robot package against your robot

Before recording with a new robot package, check that it describes the
robot correctly: that its files are complete and consistent, and that its
decoders read what the robot really sends. Both checks run on any machine,
without the robot connected to Kapto.

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
