# TOML for all configuration

## Context

Kapto is configured through files: its own settings, saved session profiles,
tasks, and the files in robot packages that describe streams, field mappings,
checks and relevance rules. People write these by hand, and the web UI
changes them too. When the UI saves a file, the comments and layout people
wrote must survive.

Error messages should point at the line a problem is on, and values should
never change type by accident.

## Approach

- Every configuration file Kapto reads or writes is **TOML**: `kapto.toml`,
  profiles, tasks, and robot package files.
- Files are read and written with the `toml_edit` library, which keeps the
  position of every value (for error messages) and preserves comments and
  layout when a file is changed.
- YAML from ROS packages (controller configurations, joint lists, camera
  calibrations) is accepted only when importing a robot, and converted to
  TOML. The TOML is what Kapto uses afterwards.

## Consequences

- One format and one library for every configuration file.
- Every error names the file and line.
- The UI can edit files without destroying people's comments.
- Every value has an explicit type: `on`, `yes` or `1.0` never silently
  become something else.
- People coming from ROS meet TOML instead of the YAML they know.

## Alternatives considered

- **YAML:** familiar to ROS users, but it guesses types (`no` becomes false),
  is sensitive to indentation, and Rust's YAML libraries can't edit a file
  while keeping its comments.
- **JSON:** no comments at all.
