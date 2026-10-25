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
  one click away. An operator keeps several, named, and picks one; a
  layout made on one machine is used on another.
- **Nothing replaces an operator's layout without asking**: a package
  updated with another layout, or an episode recorded with one, is offered,
  never applied in its place.
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
  3D view, from the `view` it is given: `front`, `side`, `top` or, by
  default, the angle that frames the robot, or the place an operator left
  it while arranging, as `{ from = [x, y, z], at = [x, y, z] }` in the
  robot's frame; up to four), `camera` (one camera stream), `plot` (fields, by stream and
  name or by role, each with its min/max band), `health` (the streams'
  counts), `events` (the timeline's marks as a list). Any panel may have a
  `title`, shown in place of the one made from what it shows. A layout
  saved by the UI starts with its `name`; a package's own needs none. The
  timeline and the mark bar are always there, under everything. The side
  panel (episodes, live, system, settings) folds to a strip of its tabs'
  icons, by a button on its edge or a key, remembered per browser, so the
  view takes the page's width; the header, with the health chip and
  Record, stays.

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
    { kind = "plot", fields = ["role:gripper.opening"], title = "Gripper" },
    { kind = "health" },
  ]
  ```

  At most 24 panels, and a plot at most 16 fields: beyond, the page can't
  keep its frame rate. Below 70rem the columns stack, in order.
- **Health in one place, said once.** The header carries one chip, beside
  Record: "All well" in the quiet style while every stream receives at its
  rate with nothing lost or dropped, pre-flight passed, the disk has room,
  and every process and destination answers; otherwise the one thing most
  in need of attention ("arm_joints 12 lost", "Disk 4 % free", "Capture not
  reachable"), with "+2" when there are more, in the colour of the worst.
  Clicking it opens the console's tab that shows that problem (Live for
  streams, pre-flight and the disk, System for processes, Uploads for
  destinations) at its row. The console stops repeating what the chip says while all is well, so the side panel
  opens on Episodes by default and Live becomes the place for details. The
  chip is computed in one function over the status the page already
  receives, so it adds no request and no field to the API.
- **A package's layouts.** The page offers, for the package being shown,
  in one list beside *Arrange* at the view's top right:
  1. **The named ones**, `layouts/<package id>/<file>.toml` in the config
     folder, each starting with its `name` (unique for the package, at most
     64 characters; the file is named from it). Written by the server
     through the API, read by every browser: an operator's own layouts are
     configuration like the rest (invariant 13), a file to review, back up
     and copy to another machine. `default.toml` among them is *Default*,
     which a browser that hasn't picked one opens on.
  2. **The package's own**, `layout.toml` beside `package.toml` (a
     `layout` key may name another file), shown as *Package's*.
  3. **Made from what the package declares**, shown as *Made from roles*:
     the robot if its URDF moves; each camera; a plot per role group,
     joints first (`joints.*.position`), then the package's other roles,
     then each decoded stream's other fields. Not the streams' health,
     which the header sums up and the console shows in full. The same
     package always gives the same layout.

  Each browser remembers only which one it shows, per package; the page
  opens on that one, else *Default*, else *Package's*, else *Made from
  roles*.
- **Reading one.** A layout that doesn't parse is reported with its file
  and line (`config.*` cause codes) and the next one is used. A panel
  naming what the package doesn't have is shown as a placeholder saying
  what is missing, with the rest of the layout around it.
- **Editing one**: with *Arrange*, on the view itself, panels are added
  from the package's streams, cameras and roles (a stream without a
  decoder is listed as recorded only, nothing to plot), removed, moved,
  resized and given a title by clicking their name. A named layout is
  saved as it is arranged. *Package's* and *Made from roles* follow the
  package, so arranging either first saves it as a named layout of its own
  ("Package's (edited)"), which is then the one shown. The list renames,
  duplicates and deletes a named layout, saves the one shown as *Default*,
  exports it as a `.toml` file and imports one as a new named layout (asked
  for another name if its own is taken).
- **Offered, never applied.** The page keeps, per package, the package's
  own layout as it last saw it. When the package's layout differs, compared
  as parsed (spacing, order of keys and comments don't count), the view
  says so above the panels: *Use the new one*, *Save the old one as…*
  (kept under a name, then the new one is shown) or *Keep this one*. A
  replay whose recording carries a layout other than the one shown says
  the same: *Use the episode's* (shown for this replay, nothing saved),
  *Save it as…* or *Keep this one*; asked once per episode. A recording
  carries its package's `layout.toml` as one of its files (invariant 14),
  but a replay is arranged by the layout chosen, like the live view.
- **Checked the same on both sides.** The server checks a layout when it is
  saved; the page when it loads one, with the same rules, from
  `kapto-wasm` once it exists and by hand until then.

## Consequences

- A package author ships a view of their robot with the package; most
  packages need none, as the generated one follows its roles.
- An operator's layouts are files in the config folder, kept across
  restarts and shared by every browser on the machine; one moves to
  another machine as a file, by *Export* and *Import* or by copying it.
- Replays are arranged like the live view; an episode recorded with
  another layout offers it rather than taking the operator's place.
- A new file kind (`layout.toml`, `layouts/<id>/<file>.toml`) with its
  reference page, cause codes for its mistakes (a name taken or too long
  among them), and API endpoints to list, write, rename and delete a
  package's named layouts.
- The page keeps a copy of each package's own layout as last seen, in the
  browser, to tell when it changed.
- The live view's fixed arrangement gives way to the generated layout:
  the sample arm's view is arranged by its roles (its joints' positions
  and its gripper), no longer by one stream's fields picked from a list.
- An operator sees whether anything is wrong without reading the side
  panel, and the side panel can stay on Episodes during a session.
- One ranking of problems to keep in step with what the status can say: a
  new kind of problem must be given a rank, checked by a test that every
  cause code the status can carry has one.

## Alternatives considered

- **Free placement on a grid** (x, y, width, height per panel, as dashboard
  tools do): any arrangement, but one made on a wide screen breaks on a
  tablet, and moving one panel means moving others.
- **One default for every package**: simpler, but robots differ, and a
  layout naming another robot's streams shows mostly placeholders.
- **The operator's layouts in their browser only**: no API to write
  them, but they are lost with the browser's storage, each browser has its
  own, and they move to another machine only by export.
- **The operator's layouts in SQLite**, with the episodes and jobs:
  as lasting as files, but `kapto.db` holds what Kapto writes for itself
  and no settings; a layout there would be the only copy of what someone
  made, lost with the database and out of reach of review, hand edits and
  backups.
- **Replays arranged by the layout their recording carries**, as before:
  an episode shows as it was recorded, but the operator's view changes
  under them from one replay to the next, and a recording without one is
  arranged by another rule.
- **Layouts only in the package**: no editor to build, but an operator
  could change their view only by editing the package's files, which
  changes its hash.
- **Foxglove's layout format**: an existing format, but tied to its panels
  and their options, not Kapto's, and changing with Foxglove.
- **A banner per problem**: louder, but it shifts the layout and stacks
  when several things go wrong, when the operator most needs a steady
  screen.
- **Health only in the Live tab**: no new element, but the Live tab must
  then stay open to notice a problem, which is what fills the screen today.
