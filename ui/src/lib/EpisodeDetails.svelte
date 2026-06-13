<script lang="ts">
  import EpisodeUploadsList from "./EpisodeUploads.svelte";
  import Icon from "./Icon.svelte";
  import type {
    CheckRange,
    EpisodeDetail,
    EpisodeEvents,
    EpisodeQuality,
    EpisodeStatistics,
    EpisodeUploads,
    ReplayState,
    SentEvent,
    UploadDestination,
  } from "./api-types";
  import { breakable, bytes, count, duration, figure, milliseconds, time } from "./format";

  interface Props {
    episode: EpisodeDetail;
    /** What the quality checks found on it, if known. */
    quality?: EpisodeQuality | null;
    /** Its statistics, if known. */
    statistics?: EpisodeStatistics | null;
    /** What happened in it, if known. */
    events?: EpisodeEvents | null;
    /** What people call each kind of event, by kind. */
    names?: Record<string, string>;
    /**
     * Where its replay is, if it was asked for and the viewer hasn't gone
     * back to live since.
     */
    replay?: ReplayState | null;
    /** Asked to watch it again in the viewer, at a moment a check found if given. */
    onreplay?: (moment?: CheckRange | null) => void;
    /** Where it was uploaded, if known. */
    uploads?: EpisodeUploads | null;
    /** The destinations configured, for where each is. */
    destinations?: UploadDestination[];
  }

  let {
    episode,
    quality = null,
    statistics = null,
    events = null,
    names = {},
    replay = null,
    onreplay = () => {},
    uploads = null,
    destinations = [],
  }: Props = $props();

  /** How many of a check's moments are listed; the rest are counted. */
  const MOMENTS_SHOWN = 5;

  /** Where in the episode a moment is, from its start. */
  const within = (range: CheckRange) =>
    `${duration((range.start_utc_ns - episode.started_utc_ns) / 1e9)}–${duration(
      (range.end_utc_ns - episode.started_utc_ns) / 1e9,
    )}`;

  /** Where an event is in the episode: a moment, or a whole stretch, from its start. */
  const at = (event: SentEvent) =>
    event.until_utc_ns == null
      ? duration((event.at_utc_ns - episode.started_utc_ns) / 1e9)
      : within({ start_utc_ns: event.at_utc_ns, end_utc_ns: event.until_utc_ns });

  /** An event in words: its kind's name, and which end of a stretch. */
  const phases: Record<string, string> = { moment: "", begin: "began", end: "ended", stretch: "stretch" };

  /** Whether `event` is a beginning nothing ended. */
  const isUnended = (event: SentEvent) =>
    events?.unended.some((open) => open.kind === event.kind && open.id === event.id && open.at_utc_ns === event.at_utc_ns) ??
    false;

  /** Being prepared or shown, it isn't asked for again; failed, it can be. */
  const asked = $derived(replay !== null && replay !== "failed");
  /** A replay is made from its recording, which a copy elsewhere doesn't give. */
  const notHere = "Deleted here: a replay is made from the recording on this machine";

  /** A percentile or extreme, or a dash where the statistic has none. */
  const shown = (value: number | null | undefined) => (value == null ? "–" : figure(value));

  /** What the quality checks decided, in a few words. */
  const decided = $derived.by(() => {
    if (!quality || quality.accepted == null) {
      return "not checked yet";
    }
    const settings = quality.params_version ? `settings ${quality.params_version}` : "default settings";
    if (quality.checks.length === 0) {
      return `accepted: no check turned on · ${settings}`;
    }
    return `${quality.accepted ? "accepted" : "rejected: a gate failed"} · ${settings}`;
  });
  const verdicts: Record<string, string> = {
    passed: "passed",
    failed: "failed",
    not_applicable: "not applicable",
  };

  const size = $derived(episode.files.reduce((sum, file) => sum + file.bytes, 0));
  const detection: Record<string, string> = {
    sequence: "sequence numbers",
    rate_gap: "silences (estimate)",
    undetectable: "not detectable",
  };
</script>

<section aria-label="Episode {episode.id}">
  <button
    type="button"
    class="replay"
    title={!episode.stored_here
      ? notHere
      : asked
        ? "The viewer shows this episode; Back to live shows the robot again"
        : "Watch this episode again in the viewer"}
    disabled={asked || !episode.stored_here}
    onclick={() => onreplay(null)}
  >
    {#if replay === "ready"}Replaying{:else if asked}Preparing…{:else}Replay{/if}
  </button>
  <dl>
    <dt>Recorded</dt>
    <dd>
      {time(episode.started_utc_ns)} · {duration((episode.ended_utc_ns - episode.started_utc_ns) / 1e9)} · Kapto {episode.kapto_version}
    </dd>
    {#if episode.recovered}
      <dt>Recovered</dt>
      <dd class="problem" title="Recording stopped without finishing it: {episode.recovered.reason}">
        {[
          time(episode.recovered.at_utc_ns),
          `${bytes(episode.recovered.bytes_cut)} cut off its end`,
          episode.recovered.last_message_utc_ns == null
            ? "no message kept"
            : `last message at ${duration((episode.recovered.last_message_utc_ns - episode.started_utc_ns) / 1e9)}`,
          "what it lost isn't known",
        ].join(" · ")}
      </dd>
    {/if}
    <dt>Robot</dt>
    {#if episode.package}
      <dd title="Content hash {episode.package.hash}">
        {episode.package.id} {episode.package.version} · {episode.package.hash.slice(0, 12)}
      </dd>
    {:else}
      <dd class="problem">unchecked: recorded without a robot package</dd>
    {/if}
    {#if episode.task}
      <dt>Task</dt>
      <dd title={episode.task.hash ? `Content hash ${episode.task.hash}` : undefined}>
        {[
          episode.task.name ?? episode.task.id,
          episode.task.prompt ? `“${episode.task.prompt}”` : null,
          episode.task.subtasks.length > 0 ? episode.task.subtasks.join(" → ") : null,
        ]
          .filter((part) => part !== null)
          .join(" · ")}
      </dd>
    {/if}
    {#if episode.profile}
      <dt>Profile</dt>
      <dd title={episode.profile.hash ? `Content hash ${episode.profile.hash}` : undefined}>
        {[
          episode.profile.id,
          episode.profile.operator,
          episode.profile.mode,
          episode.profile.environment,
          episode.profile.tags.length > 0 ? episode.profile.tags.join(", ") : null,
        ]
          .filter((part) => part != null)
          .join(" · ")}
      </dd>
    {/if}
    <dt>Quality</dt>
    <dd class:problem={quality?.accepted === false} class:pending={quality?.accepted == null}>{decided}</dd>
    <dt>Size</dt>
    <dd>{bytes(size)} in {episode.files.length} files</dd>
    {#if episode.recovered}
      <!-- Only finishing a recording measures these. -->
      <dt>On disk</dt>
      <dd class="pending">not known for a recovered episode, nor its buffers and clock</dd>
    {:else}
      <dt>On disk</dt>
      <dd>
        99 % within {milliseconds(episode.durability.p99_unsynced_ns)}, all within {milliseconds(episode.durability.max_unsynced_ns)}
      </dd>
      <dt>Buffers</dt>
      <dd>
        peak {episode.buffers.peak_batches} of {episode.buffers.batches} batches · {bytes(episode.buffers.memory_bytes)}
      </dd>
      <dt>Clock</dt>
      <dd class:problem={episode.clock.steps > 0}>
        {episode.clock.steps === 0 ? "steady" : `stepped ${episode.clock.steps} times`} · uncertainty up to
        {milliseconds(episode.clock.max_uncertainty_ns)}
      </dd>
    {/if}
    <dt>Folder</dt>
    {#if episode.stored_here}
      <dd class="folder">{episode.folder}</dd>
    {:else}
      <dd class="pending" title="Was {episode.folder}">deleted here, stored only remotely</dd>
    {/if}
  </dl>

  {#if uploads}
    <EpisodeUploadsList {uploads} {destinations} />
  {/if}

  {#if events && !events.read}
    <p class="pending">Events: not read yet. The worker reads them after the episode is recorded.</p>
  {:else if events && events.events.length > 0}
    <h3>Events</h3>
    <table aria-label="Events" class="events">
      <thead>
        <tr>
          <th scope="col">At</th>
          <th scope="col">What happened</th>
          <th scope="col">From</th>
        </tr>
      </thead>
      <tbody>
        {#each events.events as event, index (index)}
          {@const open = isUnended(event)}
          <tr>
            <td>{at(event)}</td>
            <th scope="row">
              {names[event.kind] ?? event.kind}{#if phases[event.phase]}{` ${phases[event.phase]}`}{/if}{#if open}<span
                  class="problem"
                  title="Nothing ended this stretch before the recording ended">{" · never ended"}</span
                >{/if}{#if event.text}<span class="text">{` · ${event.text}`}</span>{/if}
            </th>
            <td class="source">
              {event.source}
              <button
                type="button"
                class="watch"
                title={episode.stored_here ? "Replay the episode from a few seconds before this moment" : notHere}
                disabled={!episode.stored_here}
                onclick={() => onreplay({ start_utc_ns: event.at_utc_ns, end_utc_ns: event.until_utc_ns ?? event.at_utc_ns })}
                ><Icon name="play" /> Watch</button
              >
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if quality && quality.checks.length > 0}
    <h3>Quality checks</h3>
    <table aria-label="Quality checks" class="checks">
      <thead>
        <tr>
          <th scope="col">Check</th>
          <th scope="col">Verdict</th>
          <th scope="col">What it found</th>
        </tr>
      </thead>
      <tbody>
        {#each quality.checks as check (check.check)}
          <tr>
            <th scope="row" title="Version {check.version}">
              {#each breakable(check.check) as piece, index (index)}{piece}<wbr />{/each}
              <span class="kind">{check.kind}</span>
            </th>
            <td class:problem={check.verdict === "failed"}>{verdicts[check.verdict] ?? check.verdict}</td>
            <td class="found">
              {check.message}
              {#if check.ranges.length > 0}
                <ul class="moments" aria-label="Moments {check.check} found">
                  {#each check.ranges.slice(0, MOMENTS_SHOWN) as range (range.start_utc_ns)}
                    <li>
                      at {within(range)}
                      <button
                        type="button"
                        class="watch"
                        title={episode.stored_here ? "Replay the episode from a few seconds before this moment" : notHere}
                        disabled={!episode.stored_here}
                        onclick={() => onreplay(range)}
                        ><Icon name="play" /> Watch</button
                      >
                    </li>
                  {/each}
                  {#if check.ranges.length > MOMENTS_SHOWN}
                    <li>and {check.ranges.length - MOMENTS_SHOWN} more</li>
                  {/if}
                </ul>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if statistics && statistics.statistics.length > 0}
    <h3>Statistics</h3>
    <table aria-label="Statistics" class="statistics">
      <thead>
        <tr>
          <th scope="col">Statistic</th>
          <th scope="col">Median</th>
          <th scope="col">90 %</th>
          <th scope="col">99 %</th>
          <th scope="col">Max</th>
        </tr>
      </thead>
      <tbody>
        {#each statistics.statistics as statistic (statistic.name)}
          <tr>
            <th scope="row">
              {#each breakable(statistic.name) as piece, index (index)}{piece}<wbr />{/each}
            </th>
            {#if statistic.value != null}
              <td colspan="4">{figure(statistic.value)}</td>
            {:else}
              <td>{shown(statistic.p50)}</td>
              <td>{shown(statistic.p90)}</td>
              <td>{shown(statistic.p99)}</td>
              <td>{shown(statistic.max)}</td>
            {/if}
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  <h3>Streams</h3>
  <table>
    <thead>
      <tr>
        <th scope="col">Stream</th>
        <th scope="col">Written</th>
        <th scope="col">Decoded</th>
        <th scope="col">Dropped</th>
        <th scope="col">Lost</th>
        <th scope="col">Loss seen by</th>
      </tr>
    </thead>
    <tbody>
      {#each episode.streams as stream (stream.stream)}
        <tr>
          <th scope="row">{#each breakable(stream.stream) as piece, index (index)}{piece}<wbr />{/each}</th>
          <td title={stream.carried ? "kept from before the episode, written at its start" : undefined}>
            {count(stream.written)}{stream.carried ? ` + ${count(stream.carried)} kept` : ""}
          </td>
          <td class:problem={stream.decoded < stream.received}>{count(stream.decoded)}</td>
          <td class:problem={stream.dropped_by_us > 0}>{count(stream.dropped_by_us)}</td>
          <td class:problem={stream.lost_upstream > 0}>
            {count(stream.lost_upstream)}{stream.disconnects ? ` · ${stream.disconnects} disconnects` : ""}
          </td>
          <td class="detection">{detection[stream.loss_detection] ?? stream.loss_detection}</td>
        </tr>
      {/each}
    </tbody>
  </table>
</section>

<style>
  /* In line with its row's text above. */
  section {
    padding: 0.5rem var(--space-2) 0.75rem;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0 0 0.5rem;
    font-variant-numeric: tabular-nums;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
  .replay {
    float: right;
    margin: 0.4rem 0 0.4rem 0.5rem;
    padding: 0.25rem 0.75rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .moments {
    list-style: none;
    margin: 0.2rem 0 0;
    padding: 0;
  }
  .events .source {
    color: var(--muted);
  }
  .events .text {
    color: var(--muted);
    font-weight: normal;
  }
  .watch {
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    font: inherit;
    cursor: pointer;
  }
  .folder {
    font-family: var(--font-data);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
    color: var(--muted);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-variant-numeric: tabular-nums;
  }
  th,
  td {
    padding: 0.25rem 0.5rem;
    text-align: right;
    border-bottom: 1px solid var(--line);
  }
  th[scope="row"],
  thead th:first-child {
    text-align: left;
    font-weight: 500;
  }
  thead th {
    color: var(--muted);
    font-weight: 500;
    font-size: var(--text-xs);
  }
  .detection {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .problem {
    color: var(--warning);
    font-weight: 600;
  }
  .pending {
    color: var(--muted);
  }
  .checks,
  .statistics {
    margin-bottom: 0.5rem;
  }
  .statistics th[scope="row"] {
    font-size: var(--text-sm);
  }
  .statistics td[colspan] {
    text-align: left;
  }
  .checks td,
  .checks th {
    text-align: left;
  }
  .kind,
  .found {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  /* Prose wraps between words; a word too long for a narrow panel breaks
     rather than pushing the table past it. */
  .found {
    overflow-wrap: anywhere;
  }
</style>
