## What

One paragraph describing the change.

## Planning reference

Optional, while planning documents exist: the requirements and plan row this
covers.

## Why this way

The design choice and the alternative not taken. Link its design document
in `docs/design/` if one exists.

## Read in this order

1. `path/to/file.rs`: what to look at first and why
2. …

## How to try it

```
commands that show the change working
```

## Screenshots

For a change to what the web UI shows: every screen or state it adds or
changes, taken against `kapto demo` (`cargo xtask ui screenshots` takes
them), before and after for one that already existed. Remove this section
otherwise.

## Tests added

What each new test proves, including failure paths.

## Checklist

- [ ] Tests for every new behaviour, including the failure paths
- [ ] Docs page or reference updated in this PR
- [ ] Generated reference pages regenerated (`cargo xtask gen`)
- [ ] New error paths have a descriptive cause code, message and remedy
- [ ] No requirement IDs or plan references in code, tests, comments, config, docs or commit messages
- [ ] Hot-path change: benchmark before/after attached
- [ ] UI change: screenshots of every screen or state it adds or changes
- [ ] AGENTS.md updated if a command, invariant or rule changed
- [ ] New dependency justified (maintenance, licence, size)
