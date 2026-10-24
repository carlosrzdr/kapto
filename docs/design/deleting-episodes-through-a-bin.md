# Episodes deleted by hand go to a bin, emptied by the retention executor

## Context

An operator records episodes that aren't worth keeping: a false start, the
robot set up wrong, a test of the setup. Until now Kapto removed an
episode's folder only once it was verified at enough upload destinations
(`upload.delete_local`), through the worker's retention executor: the
folder is renamed aside, reported to serve, which records it as deleted
here and keeps its summary, and only then removed. There was no way to
delete an episode by hand.

What has to hold:

- **Raw data goes away only through the retention executor** (invariant 6):
  nothing else removes an episode's files, and a crash part way never
  leaves half an episode looking whole.
- **A mistake can be undone.** A click on the wrong episode, or on the
  right one too soon, shouldn't lose a recording at once; the data an
  operator deletes is often the data someone else wanted.
- **The disk is what runs out.** An episode deleted to make room has to
  make room in the end, without anyone emptying anything by hand.
- **What was deleted, when and how is kept**, as for an episode deleted
  after its upload: its summary stays listed, marked as no longer here.

## Approach

**Deleting an episode by hand moves its folder into a bin; the bin is
emptied by the retention executor after `bin.keep_days`, sooner when the
disk runs low, or at once on request; until then the episode can be put
back.**

- **Deleting.** `DELETE /api/episodes/{id}`, the episode's *Delete* button
  in the web UI after a confirmation, or `kapto episode delete <id>`. Serve
  renames the episode's folder to `.bin/<id>` in the data directory, in one
  rename on the same disk, and records in `kapto.db` when it was put in the
  bin and from where (the web UI, the API, the command line). Nothing in
  the folder changes. An episode still being recorded, or one the
  retention executor is deleting, can't be; an upload of it in progress is
  stopped and not retried, and its replay is dropped from the cache.
- **In the bin.** The episode is listed only under the list's *In the bin*
  filter, with when it goes for good; its details, events, quality and
  replay stay available, read from the bin.
- **Putting back.** `POST /api/episodes/{id}/restore`, *Restore* in the web
  UI, or `kapto episode restore <id>`: the folder is renamed back, and the
  episode is listed again as it was. An upload the deletion stopped is
  queued again if the episode would have been uploaded.
- **Emptying.** Serve hands the worker a `delete` job for each episode in
  the bin longer than `bin.keep_days` (7 by default); for the oldest in the
  bin first whenever the disk's free space falls below
  `capture.disk_warn_below`, until it no longer is; and for every one when
  the bin is emptied by hand (`POST /api/bin/empty`, *Empty the bin*). The
  retention executor deletes each as it deletes an uploaded one: renamed
  aside, reported, removed. Serve then records it as deleted here, by hand,
  keeping its summary, which the list shows as it shows an episode deleted
  after its upload.
- **A crash** between renaming into the bin and recording it leaves a
  folder in `.bin` that `kapto.db` doesn't know: serve records it as put in
  the bin when it next starts, so nothing in `.bin` is ever forgotten.

## Consequences

- An operator deletes an episode in one click and a confirmation, and has
  `bin.keep_days` to change their mind; the disk gets the room back by
  itself, at once if it needs it.
- Invariant 6 holds as it is: the bin is a rename, and the only code that
  removes an episode's files is still the retention executor.
- `kapto.db` keeps, for each episode deleted by hand, when it went into the
  bin and from where, and when it was emptied and why; the episode's
  summary outlives its files, as for an uploaded one.
- One more setting (`bin.keep_days`), two endpoints beside the delete, a
  filter in the list, and a command with two subcommands.
- An episode in the bin still takes its space until it is emptied: the
  storage summary counts the bin apart, so the room it holds is seen.

## Alternatives considered

- **Deleting at once**, recorded as a one-episode discard plan: the
  simplest, and the room comes back immediately, but a mistake is final.
- **Only once uploaded and verified**: nothing is ever lost, but an
  episode that is never to be uploaded, a false start, can never be
  deleted, and a robot with no destination fills its disk.
- **A bin kept in `kapto.db` alone, the folder left in place**: no rename,
  but every reader of the data directory would have to ask the database
  which folders are episodes, and a lost `kapto.db` would bring every
  deleted episode back.
