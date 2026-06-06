# Troubleshooting

## Reading an error report

Every problem Kapto reports has the same shape:

```
error[config.unknown_key]: Unknown configuration key: `server.prot` (did you mean `server.port`?)
  file: /home/me/.config/kapto/kapto.toml
  line: 2
  remedy: Remove the key or correct its spelling. `kapto config show` lists every valid key.
```

- **The code** in brackets (`config.unknown_key`) names the kind of problem.
  It never changes between versions, so it is the thing to search for.
- **The message** says what went wrong, followed by what is specific to this
  case.
- **Context lines** point at where the problem is: a file and line, a setting,
  an environment variable or a flag.
- **`caused by`** lines, when present, show the underlying error, for example
  from the operating system.
- **The remedy** says what to do about it.

When several things are wrong, Kapto reports all of them at once, so they can
be fixed in one pass.

The [error code reference](../reference/error-codes.md) lists every code with
what to do about it.
