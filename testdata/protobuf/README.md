# Protobuf messages

`robot.proto` is a robot's joint state with every kind of Protobuf field:
nested messages, repeated scalars and messages, an enum, a map, bytes,
`sint32`, `fixed64`, a `oneof` and 64-bit integers beyond 2^53. The
messages in `cases/*.json` are written by hand in Protobuf's JSON form.

`robot.binpb` (the descriptor set) and `cases/*.binpb` (each message as
Protobuf binary) are made from them by buf, not by Kapto or prost, with
`cargo xtask golden protobuf` (Docker). Never edit them by hand; edit the
`.proto` or `.json` files and regenerate them.
