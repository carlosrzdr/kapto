# Layouts

## Context

The live view and replays are arranged the same way for every robot: the
3D view on the left, the cameras and one stream's plots on the right, the
timeline below ([viewer](a-viewer-of-our-own.md)). Robots differ: a mobile
base has no arm worth a large 3D view, a bimanual robot has two grippers
and four cameras, a test rig has forty streams and wants six fields side by
side. And operators differ: one watches a wrist camera and the gripper's
force, another the joints' tracking error.

What has to hold:

- **A robot package says how its robot is shown** (VIS-5), and a package
  that says nothing still gets a sensible view, made from what it declares.
- **An operator arranges their own** (VIS-11) in the web UI, without
  editing files, and finds it again after a reload; the package's view is
  one click away.
- **User configuration lives in files** (invariant 13): a layout someone
  chose for every browser is configuration, reviewed and backed up like the
  rest. SQLite holds runtime state only.
- **Nothing advisory rejects data** (invariant 9): a layout naming a stream
  or field the package no longer has shows the rest and says what is
  missing; it never stops the view, recording or a replay.
- **It works at a tablet's width**: whatever is arranged reflows rather than
  overflows.
- **It stays cheap**: plots stay decimated to the display's rate, and a
  layout can't ask the page for more than it can draw.

## Approach

**A layout is a small TOML document of columns, each a stack of panels; it
comes from the operator's browser, else the config folder's default for the
package, else the package, else it is made from what the package
declares.**

- **The document.** Columns side by side, each with a relative width, each
  a stack of panels with relative heights. A panel is one of: `robot` (the
  3D view), `camera` (one camera stream), `plot` (fields, by stream and
  name or by role, each with its min/max band), `health` (the streams'
  counts), `events` (the timeline's marks as a list). The timeline and the
  mark bar are always there, under everything.

  ```toml
  # layout.toml
  [[columns]]
  width = 3
  panels = [
    { kind = "robot", height = 3 },
    { kind = "camera", stream = "wrist_camera" },
  ]

  [[columns]]
  width = 2
  panels = [
    { kind = "plot", fields = ["arm_joints.joint_positions"], height = 2 },
    { kind = "plot", fields = ["role:gripper.opening"] },
    { kind = "health" },
  ]
  ```

  At most 24 panels, and a plot at most 16 fields: beyond, the page can't
  keep its frame rate. Below 70rem the columns stack, in order.
- **Where a layout comes from**, the first found:
  1. **The operator's own**, kept in their browser per package ID, as they
     left it.
  2. **The config folder's default** for the package,
     `layouts/<package id>.toml`, written by *Save as default* in the UI
     (through the API) and read by every browser. One per package: robots
     differ, and a default for one would name streams another doesn't have.
  3. **The package's own**, `layout.toml` beside `package.toml` (a
     `layout` key may name another file), carried in every recording like
     the package's other files, so a replay is arranged as its episode was
     recorded.
  4. **Made from what the package declares**: the robot if its URDF moves;
     each camera; a plot per role group, joints first
     (`joints.*.position`), then the package's other roles, then each
     decoded stream's other fields; the streams' health. The same package
     always gives the same layout.
- **Reading one.** A layout that doesn't parse is reported with its file
  and line (`config.*` cause codes) and the next one is used. A panel
  naming what the package doesn't have is shown as a placeholder saying
  what is missing, with the rest of the layout around it.
- **Editing one** (the operator's own): in the web UI, panels are added
  from the package's streams, cameras and roles (a stream without a
  decoder is listed as recorded only, nothing to plot), removed, moved
  between columns and resized. *Save as default* writes the config
  folder's file; *Reset* goes back to the package's (or the generated) one.
- **Checked the same on both sides.** The server checks a layout when it is
  saved; the page when it loads one, with the same rules, from
  `kapto-wasm` once it exists and by hand until then.

## Consequences

- A package author ships a view of their robot with the package; most
  packages need none, as the generated one follows its roles.
- An operator's layout survives reloads in their browser, and a team shares
  one by saving it as the default, which is a file to review and copy.
- Replays are arranged as their episode was recorded, unless the operator's
  own layout is in use.
- A new file kind (`layout.toml`, `layouts/<id>.toml`) with its reference
  page, cause codes for its mistakes, and an API endpoint to save the
  default.
- The live view's fixed arrangement becomes the generated layout of the
  sample arm, so nothing changes for it.

## Alternatives considered

- **Free placement on a grid** (x, y, width, height per panel, as dashboard
  tools do): any arrangement, but one made on a wide screen breaks on a
  tablet, and moving one panel means moving others.
- **The default in SQLite**, with the operator's other state: one place for
  everything, but a layout chosen for every browser is configuration, which
  lives in files (invariant 13), and it would be lost with the database.
- **One default for every package**: simpler, but robots differ, and a
  layout naming another robot's streams shows mostly placeholders.
- **Layouts only in the package**: no editor to build, but an operator
  could change their view only by editing the package's files, which
  changes its hash.
- **Foxglove's layout format**: an existing format, but tied to its panels
  and their options, not Kapto's, and changing with Foxglove.
