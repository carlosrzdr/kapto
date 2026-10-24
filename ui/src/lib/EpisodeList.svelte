<script lang="ts">
  import type { QualityFilter } from "./api";
  import type {
    EpisodeDetail,
    EpisodeEvents,
    EpisodeQuality,
    EpisodeStatistics,
    EpisodeSummary,
    EpisodeUploads,
    CheckRange,
    ReplayStatus,
    UploadDestination,
    VocabularyKind,
  } from "./api-types";
  import EpisodeDetails from "./EpisodeDetails.svelte";
  import { bytes, count, duration, time } from "./format";

  interface Props {
    episodes: EpisodeSummary[];
    /** The episode shown in full, if any. */
    open: EpisodeDetail | null;
    /** What the quality checks found on the open episode, if known. */
    quality?: EpisodeQuality | null;
    /** The open episode's statistics, if known. */
    statistics?: EpisodeStatistics | null;
    /** What happened in the open episode, if known. */
    events?: EpisodeEvents | null;
    /** Where the open episode was uploaded, if known. */
    uploads?: EpisodeUploads | null;
    /** The destinations configured, for where each is. */
    destinations?: UploadDestination[];
    /** The kinds of event that may be sent, to find episodes by. */
    kinds?: VocabularyKind[];
    /** Only episodes with an event of this kind are listed, if given. */
    eventFilter?: string | null;
    /** Asked to list episodes with an event of a kind, or `null` for all. */
    onevent?: (kind: string | null) => void;
    /** Which episodes are listed, by what the quality checks decided. */
    filter?: QualityFilter;
    /** Asked to show an episode in full, or `null` to close it. */
    onselect: (id: string | null) => void;
    /** Asked to list other episodes. */
    onfilter?: (filter: QualityFilter) => void;
    /** The replay asked for, if the viewer hasn't gone back to live since. */
    replay?: ReplayStatus | null;
    /** Asked to watch an episode again in the viewer, at a moment if given. */
    onreplay?: (id: string, moment?: CheckRange | null) => void;
    /** Whether the episodes listed are those in the bin. */
    inBin?: boolean;
    /** Asked to list those in the bin, or the others. */
    onbin?: (inBin: boolean) => void;
    /** How many days an episode deleted stays in the bin (`bin.keep_days`). */
    keepDays?: number;
    /** Asked to delete an episode, into the bin. */
    ondelete?: (id: string) => void;
    /** Asked to put an episode back from the bin. */
    onrestore?: (id: string) => void;
    /** Asked to empty the bin, once confirmed. */
    onempty?: () => void;
  }

  let {
    episodes,
    open,
    quality = null,
    statistics = null,
    events = null,
    uploads = null,
    destinations = [],
    kinds = [],
    eventFilter = null,
    onevent = () => {},
    filter = "all",
    onselect,
    onfilter = () => {},
    replay = null,
    onreplay = () => {},
    inBin = false,
    onbin = () => {},
    keepDays = 7,
    ondelete = () => {},
    onrestore = () => {},
    onempty = () => {},
  }: Props = $props();

  /** Whether Empty the bin was pressed once, and waits to be confirmed. */
  let emptying = $state(false);

  const total = (episode: EpisodeSummary, field: "written" | "dropped_by_us" | "lost_upstream") =>
    episode.streams.reduce((sum, stream) => sum + stream[field], 0);

  /** Why a recovered episode is marked, in a sentence. */
  const recovery = (episode: EpisodeSummary) =>
    episode.recovered
      ? `Recording stopped without finishing it: a crash, a kill or a power cut. Recovered ${time(episode.recovered.at_utc_ns)}, ${bytes(episode.recovered.bytes_cut)} cut off its end; what it lost isn't known`
      : "";

  /** What people call each kind, from the vocabulary; Kapto's own and unknown ones by their kind. */
  const names = $derived(Object.fromEntries(kinds.map((each) => [each.kind, each.name])));

  /** The kinds to find episodes by: the vocabulary's, then any other a listed episode holds. */
  const findable = $derived([
    ...new Set([
      ...kinds.map((each) => each.kind),
      ...episodes.flatMap((episode) => Object.keys(episode.events ?? {})),
      ...(eventFilter ? [eventFilter] : []),
    ]),
  ]);

  /** Its events by kind, in a few words: `2 intervention · 1 note`. */
  const happened = (episode: EpisodeSummary) =>
    Object.entries(episode.events ?? {})
      .map(([kind, n]) => `${n} ${names[kind] ?? kind}`)
      .join(" · ");

  /** What it was recorded for and with: the task, then the profile, if chosen. */
  const session = (episode: EpisodeSummary) =>
    [episode.task_name ?? episode.task, episode.profile].filter((part) => part != null).join(" · ");
</script>

<div class="filters">
  <label class="filter">
    Quality
    <select value={filter} onchange={(event) => onfilter(event.currentTarget.value as QualityFilter)}>
      <option value="all">All episodes</option>
      <option value="accepted">Accepted</option>
      <option value="rejected">Rejected</option>
    </select>
  </label>
  <label class="filter">
    With
    <select
      aria-label="With an event of kind"
      value={eventFilter ?? ""}
      onchange={(event) => onevent(event.currentTarget.value === "" ? null : event.currentTarget.value)}
    >
      <option value="">Any events</option>
      {#each findable as kind (kind)}
        <option value={kind}>{names[kind] ?? kind}</option>
      {/each}
    </select>
  </label>
  <button
    type="button"
    class="bin-toggle"
    aria-pressed={inBin}
    title="Episodes deleted, until the bin is emptied or they are restored"
    onclick={() => {
      emptying = false;
      onbin(!inBin);
    }}>In the bin</button
  >
  {#if inBin && episodes.length > 0}
    {#if emptying}
      <span class="confirm" role="group" aria-label="Empty the bin">
        Delete all {episodes.length} for good?
        <button
          type="button"
          class="delete"
          onclick={() => {
            emptying = false;
            onempty();
          }}>Empty the bin</button
        >
        <button type="button" onclick={() => (emptying = false)}>Cancel</button>
      </span>
    {:else}
      <button type="button" class="delete" onclick={() => (emptying = true)}>Empty the bin…</button>
    {/if}
  {/if}
</div>

<ul>
  {#each episodes as episode (episode.id)}
    {@const lost = total(episode, "dropped_by_us") + total(episode, "lost_upstream")}
    {@const expanded = open?.id === episode.id}
    <li>
      <button
        type="button"
        aria-expanded={expanded}
        title="Show this episode in full"
        onclick={() => onselect(expanded ? null : episode.id)}
      >
        <span class="when">
          {time(episode.started_utc_ns)}{#if !episode.package}<span
              class="unchecked"
              title="Recorded without a robot package: nothing to check it against">unchecked</span
            >{/if}{#if episode.recovered}<span class="recovered" title={recovery(episode)}>recovered</span
            >{/if}{#if !episode.stored_here}<span
              class="remote"
              title="Deleted here once verified elsewhere: its uploads hold it, and what was found in it is kept">remote only</span
            >{/if}
        </span>
        <span>{duration((episode.ended_utc_ns - episode.started_utc_ns) / 1e9)}</span>
        <span>{count(total(episode, "written"))} messages</span>
        {#if episode.recovered}
          <span class="problem" title="Only finishing a recording counts what it lost">loss unknown</span>
        {:else}
          <span class:problem={lost > 0}>{lost > 0 ? `${count(lost)} missing` : "complete"}</span>
        {/if}
        <span class="session">
          {#if episode.accepted === false}<span
              class="rejected"
              title="A quality gate failed: the episode isn't accepted">rejected</span
            >{:else if episode.accepted === true}<span title="Every quality gate passed">accepted</span
            >{:else}<span title="The worker checks each episode after it is recorded">not checked yet</span
            >{/if}{#if session(episode)}{` · ${session(episode)}`}{/if}{#if happened(episode)}<span
              class="happened"
              title="Its events, by kind">{` · ${happened(episode)}`}</span
            >{/if}
        </span>
      </button>
      {#if expanded && open}
        <EpisodeDetails
          episode={open}
          {quality}
          {statistics}
          {events}
          {uploads}
          {destinations}
          {names}
          replay={replay?.episode === episode.id ? replay.state : null}
          onreplay={(moment) => onreplay(episode.id, moment)}
          {keepDays}
          ondelete={() => ondelete(episode.id)}
          onrestore={() => onrestore(episode.id)}
        />
      {/if}
    </li>
  {:else}
    <li class="empty">
      {inBin
        ? "The bin is empty."
        : filter === "all" && eventFilter === null
        ? "No episodes recorded yet."
        : `No ${filter === "all" ? "" : `${filter} `}episodes${eventFilter === null ? "" : ` with ${names[eventFilter] ?? eventFilter}`}.`}
    </li>
  {/each}
</ul>

<style>
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    border-bottom: 1px solid var(--line);
  }
  button {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto auto;
    gap: 0.75rem;
    width: 100%;
    /* Its highlight reaches the panel's edges; its text stays off them. */
    padding: 0.35rem var(--space-2);
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    font-variant-numeric: tabular-nums;
    cursor: pointer;
  }
  button:hover,
  button:hover {
    background: var(--panel);
  }
  button[aria-expanded="true"] {
    background: var(--raised);
  }
  .when {
    color: var(--muted);
  }
  .problem {
    color: var(--warning);
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 1rem;
    margin-bottom: 0.4rem;
  }
  .bin-toggle,
  .filters .delete,
  .confirm button {
    /* Not the rows' full-width grid. */
    display: inline-block;
    width: auto;
    padding: 0.2rem 0.6rem;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .confirm {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    align-items: center;
  }
  .bin-toggle[aria-pressed="true"] {
    border-color: var(--accent);
  }
  .filters .delete {
    border-color: var(--danger);
    color: var(--danger);
  }
  .filter {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .rejected {
    color: var(--warning);
    font-weight: 600;
  }
  .unchecked,
  .recovered,
  .remote {
    margin-left: 0.5rem;
    padding: 0 0.3rem;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    font-size: var(--text-xs);
  }
  .recovered {
    border-color: var(--warning);
    color: var(--warning);
  }
  .remote {
    color: var(--muted);
  }
  /* Its own row under the times and counts. */
  .session {
    grid-column: 1 / -1;
    margin-top: -0.4rem;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .empty {
    padding: 0.35rem 0;
    color: var(--muted);
  }
</style>
