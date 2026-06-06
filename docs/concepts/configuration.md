# Configuration

Kapto's deployment settings say how it runs on this machine: where it
listens, where it stores data and how much it logs.

## Where settings come from

Each setting can come from four places. Each one overrides the one before:

1. **Built-in default.**
2. **The configuration file**, `kapto.toml`.
3. **An environment variable**: `KAPTO_` followed by the setting's name in
   capitals with dots replaced by underscores. `server.port` is
   `KAPTO_SERVER_PORT`.
4. **A command-line flag**, e.g. `--port 7701`.

Kapto remembers where every value came from. `kapto config show` lists them:

```
$ KAPTO_VIEWER_STREAM_PORT=9091 kapto config show --log-level debug
Configuration file: /home/me/.config/kapto/kapto.toml

SETTING             VALUE                                 SOURCE
server.bind         127.0.0.1                             default
server.port         7701                                  /home/me/.config/kapto/kapto.toml:2
viewer.stream_port  9091                                  environment variable KAPTO_VIEWER_STREAM_PORT
paths.data          /home/me/.local/share/kapto           default
paths.packages      /home/me/.local/share/kapto/packages  default
log.level           debug                                 flag --log-level
```

## The settings

They are grouped in sections: where Kapto listens (`[server]`), the live
view (`[viewer]`), recording (`[capture]`), restarting processes
(`[supervisor]`), the worker (`[worker]`), uploads (`[upload]`), where
things are stored (`[paths]`) and logging (`[log]`). The
[configuration reference](../reference/configuration.md) lists each one with
its accepted values, default, environment variable and flag.

Ports must be between 1 and 65535, and no two listeners may share a port.
Kapto never picks a different port on its own: the port in use is always the
one configured.

## The configuration file

Kapto looks for `kapto.toml` in this order:

1. The path given with `--config`.
2. The path in the `KAPTO_CONFIG` environment variable.
3. `$XDG_CONFIG_HOME/kapto/kapto.toml`, else `~/.config/kapto/kapto.toml`.

If the file is missing from the default location, Kapto runs on defaults. A
file named with `--config` must exist.

```toml
[server]
port = 7701

[log]
level = "debug"
```

## Checking a configuration

```sh
kapto config validate
```

This reports every problem at once, with the file and line, and exits with
status 1 if there are any. Misspelt settings get a suggestion
(`did you mean server.port?`).

## Upload destinations

Where episodes are uploaded is a list of `[[upload.destination]]` entries in
`kapto.toml`, none by default; without one, episodes stay on this machine.
Each names a bucket or container, the folder inside it the episodes go
under, and where its credentials come from, never the credentials
themselves:

```toml
[upload]
when = "finalised"
bandwidth = 20

[[upload.destination]]
name = "site_nas"
url = "s3://robot-data"
prefix = "fleet-a/robot-7"
endpoint = "http://nas.local:9000"
credentials = { env = "SITE_NAS" }
backoff_max = 30

[[upload.destination]]
name = "archive"
url = "gs://robot-archive"
```

- `name`: how statuses and the API call it, lowercase letters, digits and
  underscores.
- `url`: the bucket or container, `s3://`, `gs://` or `az://`, or a folder,
  `file:///srv/kapto`. S3-compatible storage other than AWS gives its server
  as `endpoint`, and a provider with regions takes `region`.
- `prefix`: the folder inside the bucket, so several robots or sites can
  share one bucket; the bucket's root if left out.
- `layout`: how each episode's key is made under the prefix, from
  `{robot}` (the robot package), when it was recorded, `{episode}` (its ID,
  required) and `{sha8}` (the start of its manifest's checksum), and plain
  names; `{robot}/{date}/{episode}` if left out. When it was recorded is
  `{date}` (`2026-03-23`) or its parts `{year}`, `{month}`, `{day}` and
  `{hour}`, in UTC and zero-padded so keys sort in time order:
  `{robot}/{year}/{month}/{day}/{episode}`, or
  `year={year}/month={month}/{episode}` as many data tools partition. An
  episode keeps the key it was uploaded under even if the layout changes
  later.
- `credentials`: where they are read from, when each upload starts, so
  a rotated secret is used from the next one on. Each kind of storage
  needs its own values: `access_key_id`, `secret_access_key` and, if the
  credentials are temporary, `session_token` for S3;
  `service_account_key` (the key's JSON) for Google Cloud Storage;
  `access_key` and, unless the environment gives it, `account` for Azure.
  `{ env = "SITE_NAS" }` reads them from environment variables named
  after it in upper case (`SITE_NAS_ACCESS_KEY_ID`,
  `SITE_NAS_SECRET_ACCESS_KEY`); `{ file = "/run/secrets/site_nas" }`
  from a mounted TOML file holding them under their names
  (`secret_access_key = "…"`). One missing is named with where it was
  looked for (`upload.credentials_missing`), and no message, log or page
  ever shows a value. Left out, the provider's own settings in Kapto's
  environment apply, as its tools read them (`AWS_ACCESS_KEY_ID` and
  `AWS_SECRET_ACCESS_KEY`, `GOOGLE_SERVICE_ACCOUNT`,
  `AZURE_STORAGE_ACCOUNT_NAME` and `AZURE_STORAGE_ACCOUNT_KEY`), or the
  machine's role. A key that holds a secret's value, such as
  `secret_access_key`, is refused in `kapto.toml` (`config.secret_inline`).

`kapto upload check` tests each destination, or the one it names, with
the settings and credentials an upload uses: it writes a small probe
object under the prefix, reads it back and removes it, and says what
failed with its cause code. Uploads never delete anything, so credentials
that may write but not delete pass, with the probe left in place.
`POST /api/uploads/destinations/{name}/test` runs the same test from a
running `kapto serve`, with serve's environment for the credentials, as
does each destination's *Test* button on the page's Uploads tab.

`kapto upload status` shows where uploads are, from `kapto.db`: each
destination's episodes uploaded, uploading and failed, how many requests it
throttled, and each upload not done, with its bytes or why it failed. A
failed upload is tried again by itself after `upload.retry_failed_after`;
`kapto upload retry <episode>` asks a running `kapto serve` to try it
again now, as do `POST /api/episodes/{id}/uploads/retry` and *Retry
now* on the Uploads tab, which shows the same, with how fast each upload
goes and how long it has left.

Every `[upload]` setting applies to every destination, and any of them
repeated in an entry applies to that destination alone: above, `site_nas`
waits at most 30 seconds before trying a request again. Environment
variables and flags set the `[upload]` values, not one destination's.
`kapto config validate` checks the entries, and warns about a destination
with `marker = false`, whose readers can't tell a whole episode from one
still arriving.

## Stream declarations

What a robot sends is described in its package, not in `kapto.toml`. The
file `streams.toml` declares each stream in its own `[streams.<name>]`
table. Names use lowercase letters, digits and underscores, e.g.
`arm_joints`.

```toml
[streams.arm_joints]
rate_hz = 2000            # messages per second, if the robot sends at a fixed rate
max_message_bytes = 512   # the largest message; longer ones are discarded and counted

[streams.arm_joints.udp]  # where the stream arrives
bind = "0.0.0.0:9000"     # local address and port
allowed_sources = ["10.0.0.5"]   # optional: only these robot addresses
# multicast_group = "239.255.0.1"  # optional: join this group

[streams.arm_joints.sequence]
offset = 0                # where each message carries a sequence number
type = "u32"              # u8, u16, u32 or u64
byte_order = "little"     # little (the default) or big

[streams.camera_info]     # neither: accepted, but its upstream loss can't be detected
```

`max_message_bytes` is 1472 by default, a full Ethernet frame's UDP
payload, and at most 65,507 for a UDP stream, the largest datagram. A TCP
or ROS 2 stream may declare up to 32 MiB, as a camera's frames need; above
65,507 its messages go through buffers of their own, a frame at a time,
sized for its rate (about 62 MiB for 1 MiB frames at 30 a second), so
the other streams' buffers don't grow, and a ROS 2 topic is received by a
thread and a ROS 2 node of its own, `kapto_<stream>`.

Each stream arrives on its own UDP port. Two streams can share a port only
if each joins a different multicast group, because otherwise their messages
couldn't be told apart.

A stream can arrive over TCP instead, in a `tcp` table. Kapto either
connects to the robot (`connect`) or listens for the robot to connect
(`listen`); a dropped connection is counted and re-established. Since TCP
carries bytes rather than messages, `framing` says how the robot marks where
each message ends:

```toml
[streams.base_odometry.tcp]
connect = "192.168.1.10:7000"    # or: listen = "0.0.0.0:7000"
# reconnect_initial_ms = 100     # first wait after a failed connection,
# reconnect_max_ms = 5000        #   doubling up to this

[streams.base_odometry.tcp.framing]
kind = "length_prefix"     # a header holding each message's length
length_bytes = 4           # 1, 2, 4 or 8
byte_order = "big"         # little (the default) or big
# length_offset = 0        # where the length is in the header
# header_bytes = 4         # the header's size; by default, up to the end of the length
# includes_header = false  # whether the length counts the header too

# Or one of:
# framing = { kind = "delimiter", delimiter = "\n" }   # each message ends with these bytes
# framing = { kind = "fixed", size = 68 }              # every message has this many bytes
```

A length-prefixed message is stored with its header, exactly as received; a
delimiter is not part of the message. Two streams can't listen on the same
TCP port.

A stream can be a ROS 2 topic instead, in a `ros2` table naming the topic and
the type of its messages (`kapto topics`, in the `kapto-ros2` image, lists
the topics your robot publishes, with their types):

```toml
[streams.joints]
rate_hz = 500
ros2 = { topic = "/joint_states", type = "sensor_msgs/msg/JointState" }

# Optional: how Kapto subscribes. These are the defaults, which match a
# reliable publisher as well as a best-effort one.
# [streams.joints.ros2.qos]
# reliability = "best_effort"   # or "reliable": only matches a reliable publisher
# durability = "volatile"       # or "transient_local": also the last messages
#                               #   published before Kapto subscribed
# depth = 10                    # messages kept for Kapto before it takes them
```

The type's definition is read when the file is: from the `.msg` files in
the package (see *Decoding messages* below), then from the ROS 2 installation
Kapto runs in, as ROS 2's setup script sets it (`AMENT_PREFIX_PATH`).

Only the `kapto-ros2` image's Kapto records ROS 2 topics; the others refuse
a file with one (`transport.ros2.not_built`). It subscribes with a node of
its own, on the robot's `ROS_DOMAIN_ID` and `RMW_IMPLEMENTATION`, and every
message type it subscribes with must be installed in the image, as for the
robot's own nodes. A topic is stored as rosbag2 stores one, so
`ros2 bag info` and Foxglove read the recording. Its channel is named after
the stream (`joints`), with the topic in the channel's metadata. `ros2 bag
play` doesn't replay it as it is (rosbag2 refuses a bag whose channels have
more than one encoding, and Kapto's own events channel is JSON), so export
its topics as a bag of their own, which replays on the robot's topics:

```sh
kapto export rosbag2 <episode folder> --output episode.mcap
ros2 bag play episode.mcap
```

`rate_hz` and `sequence` are optional, but they decide how Kapto can tell
that messages went missing before they reached it. With a sequence number, every skipped
number counts as lost. With only a rate, loss is estimated from silences
longer than three periods: once a second has passed, the periods since the
silence began, less every message that arrived since, count as lost when
more than three periods and the 2 ms a batch waits to fill: fewer can be a
message's delay on the way changing. A
middleware that holds messages back and delivers them in a burst, as ROS 2's
can, then makes up for the silence it caused, and loss shows up to that
second late. `loss_settle_ms = 250` waits a quarter of a second instead; it
is only for a stream with a rate and no sequence. With neither, upstream
loss can't be detected.
To be recorded, though, a stream needs a rate, which sizes its buffers;
only a ROS 2 topic subscribed to with `durability = "transient_local"`,
such as `/tf_static`, may leave it out. It may send its messages once and
nothing after, so its silences mean nothing: its buffers are sized for
as many messages as `depth` keeps, and pre-flight doesn't call it silent.

### Decoding messages

A stream whose messages have a fixed binary layout can declare it, so its
messages can be read as named values: joint positions in radians rather
than bytes 12 to 67. Each field gives its name, type and byte offset, and
optionally an array length, byte order, scale and unit:

```toml
[streams.arm_joints.decoder]
kind = "struct"

[[streams.arm_joints.decoder.fields]]
name = "sequence"
type = "u32"              # u8, i8, u16, i16, u32, i32, u64, i64, f32, f64 or bool
offset = 0                # byte where the field starts

[[streams.arm_joints.decoder.fields]]
name = "joint_positions"
type = "i32"
offset = 4
count = 7                 # optional: seven values one after another
byte_order = "big"        # optional: little (the default) or big
scale = 0.001             # optional: multiply each value, e.g. thousandths to units
unit = "rad"              # optional
```

Messages are still recorded exactly as received. The layout is stored in
every recording with them, so a recording can be decoded later without its
`streams.toml`:

```sh
kapto decode <episode folder>                          # the first 10 messages of each stream
kapto decode <episode folder> --stream arm_joints --format csv > arm_joints.csv
```

The table shows when each message was received, its index in the stream, and
its values; streams recorded without a layout are shown as raw bytes. With
`--pcap` and `--package`, `kapto decode` reads a packet capture of the
robot's traffic instead, decoded with the package's `streams.toml`, to check
a package before recording (see
[Check a robot package against your robot](../guides/check-a-package.md)). CSV
output has one stream per file, every message, with arrays as one column per
value (`joint_positions[0]`, …). A message too short for its layout is shown
as *not decoded* in the table and left out of CSV, with a count on standard
error. The [struct layout reference](../reference/struct-layout.md)
describes every key and type, and how recordings store the layout.

A stream whose messages are JSON or MessagePack declares `kind = "json"` or
`kind = "msgpack"` instead, and gives each field a `path` through the message
in place of `offset` and `byte_order`:

```toml
[streams.base.decoder]
kind = "json"

[[streams.base.decoder.fields]]
name = "wheel_speeds"
type = "f64"
path = "drive.wheels.speed"   # keys separated by dots, [n] for an array element
count = 4                     # the first four elements of that array
unit = "rad/s"
```

Each value is converted to the field's type: an integer field takes whole
numbers in range, a float field any number, a `bool` field `true` or `false`.
Text, `null`, or a path that isn't in the message leaves the message
undecoded, and it is still recorded as it arrived. The
[layouts by path reference](../reference/path-layout.md) has the
path syntax and every conversion rule.

A stream of ROS 2 messages declares `kind = "cdr"` and the messages' `type`,
and finds fields by path the same way; a ROS 2 topic's decoder needs no
`type`, as it has the topic's. The type's definition comes from `.msg` files
in a `msg/` folder beside `streams.toml`, laid out as ROS 2 installs them
(`msg/sensor_msgs/msg/JointState.msg`, and the file of every type it uses),
or else from the ROS 2 installation Kapto runs in. Copying them into the
package makes it work wherever Kapto runs:

```toml
[streams.joints.decoder]
kind = "cdr"
type = "sensor_msgs/msg/JointState"

[[streams.joints.decoder.fields]]
name = "stamp"
type = "i64"
path = "header.stamp.sec"
```

`kapto config validate` doesn't read `streams.toml`; mistakes in a layout
(an unknown type, a repeated name, a field past the largest message) are
reported, with their lines, when a recording starts.

Problems are reported the same way as for `kapto.toml`: all at once, each
with the file and line, and with a suggestion for a misspelt key.

## Mapping fields to joints

A robot package's `mapping.toml` says which decoded value drives which
joint of its robot description, so the robot can be drawn and checked from
what it sends. Each entry under `[joints]` names a joint and a decoded
field, as `<stream>.<field>`, with `[<index>]` for an element of an array
field:

```toml
[joints]
joint_1 = "arm_joints.joint_positions[0]"     # the joint's position
joint_2 = { position = "arm_joints.joint_positions[1]", velocity = "arm_joints.joint_velocities[1]" }
gripper = { position = "hand.width", effort = "hand.force" }
```

Mapping is checked against the robot and its `streams.toml`, and every
mistake is reported with its line: a joint the robot doesn't have (with the
closest name), a mimic joint (map the joint it copies instead), a fixed
joint, a stream, field or array element that isn't decoded, a true/false
field, and a field whose unit doesn't fit the joint. Positions are in `rad`
for turning joints and `m` for sliding ones; a robot that sends degrees
declares `scale` and `unit` on its decoder field. A moving joint without a
position is a warning, not an error: it is shown at 0 until it is mapped.
`packages/arm7/mapping.toml` maps the sample arm.

The same file says which value plays which **role**: what it is for,
whatever the robot calls it, so quality checks and statistics work the
same for every robot. Every mapped joint plays its own roles without
being written again: `joints.<joint>.position`, `.velocity` and
`.effort`, with the joint's name in lowercase and other characters than
letters, digits and underscores as `_`. Other values are mapped under
`[roles]`, each role two or more dot-separated names:

```toml
[roles]
"gripper.opening" = "arm_joints.gripper"
"wrench.left" = "ft_sensor.force"      # a whole array field: one value per element
"safety.enabled" = "status.enabled"    # true/false values play roles too
```

A role is checked like a joint's field: its stream and field must be
decoded, and an index must be within the array.
