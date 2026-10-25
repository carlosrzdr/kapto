<script lang="ts">
  import { type QualityFilter, retryUploads } from "./api";
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
  import { AlertDialog, Toggle, ToggleGroup } from "bits-ui";

  import Choice from "./controls/Choice.svelte";
  import EpisodeDetails from "./EpisodeDetails.svelte";
  import Icon from "./Icon.svelte";
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
    /** Tries an episode's failed uploads again now; the next refresh shows them queued. */
    onretry?: (id: string) => Promise<void>;
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
    onretry = async (id: string) => {
      await retryUploads(id);
    },
    inBin = false,
    onbin = () => {},
    keepDays = 7,
    ondelete = () => {},
    onrestore = () => {},
    onempty = () => {},
  }: Props = $props();

  /** Whether Empty the bin was pressed once, and waits to be confirmed. */
  let emptying = $state(false);

  const total = (episode: EpisodeSummary, field: "dropped_by_us" | "lost_upstream") =>
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
  <ToggleGroup.Root
    type="single"
    class="segments"
    aria-label="Quality"
    value={filter}
    onValueChange={(value) => value && onfilter(value as QualityFilter)}
  >
    <ToggleGroup.Item class="segment" value="all">All</ToggleGroup.Item>
    <ToggleGroup.Item class="segment" value="accepted">Accepted</ToggleGroup.Item>
    <ToggleGroup.Item class="segment" value="rejected">Rejected</ToggleGroup.Item>
  </ToggleGroup.Root>
  <Choice
    label="With"
    showLabel={false}
    name="With an event of kind"
    value={eventFilter ?? ""}
    options={[{ value: "", label: "Any events" }, ...findable.map((kind) => ({ value: kind, label: names[kind] ?? kind }))]}
    onchange={(value) => onevent(value === "" ? null : value)}
  />
  <Toggle.Root
    class="bin-toggle"
    pressed={inBin}
    title="Episodes deleted, until the bin is emptied or they are restored"
    onPressedChange={(pressed) => onbin(pressed)}><Icon name="bin" /> In the bin</Toggle.Root
  >
  {#if inBin && episodes.length > 0}
    <AlertDialog.Root bind:open={emptying}>
      <AlertDialog.Trigger class="delete">Empty the bin…</AlertDialog.Trigger>
      <AlertDialog.Portal>
        <AlertDialog.Overlay class="overlay" />
        <AlertDialog.Content class="dialog">
          <AlertDialog.Title class="dialog-title">Empty the bin</AlertDialog.Title>
          <AlertDialog.Description class="dialog-text">
            Delete all {episodes.length} {episodes.length === 1 ? "episode" : "episodes"} in the bin for good? Their
            recordings are removed from this machine; copies already uploaded stay where they are.
          </AlertDialog.Description>
          <div class="dialog-actions">
            <AlertDialog.Cancel class="secondary">Cancel</AlertDialog.Cancel>
            <AlertDialog.Action class="delete" onclick={() => onempty()}>Empty the bin</AlertDialog.Action>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
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
        <!-- Nothing missing is the normal case, left unsaid; anything else is said. -->
        {#if episode.recovered}
          <span class="warning" title="Only finishing a recording counts what it lost">loss unknown</span>
        {:else if lost > 0}
          <span class="warning">{count(lost)} missing</span>
        {:else}
          <!-- Its place kept, so the verdicts line up. -->
          <span></span>
        {/if}
        {#if episode.accepted === false}<span
            class="badge rejected"
            title="A quality gate failed: the episode isn't accepted">rejected</span
          >{:else if episode.accepted === true}<span class="badge accepted" title="Every quality gate passed"
            >accepted</span
          >{:else}<span class="badge pending" title="The worker checks each episode after it is recorded"
            >not checked yet</span
          >{/if}
        {#if session(episode) || happened(episode)}
          <span class="session"
            >{session(episode)}{#if happened(episode)}<span class="happened" title="Its events, by kind"
                >{`${session(episode) ? " · " : ""}${happened(episode)}`}</span
              >{/if}</span
          >
        {/if}
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
          onretry={() => onretry(episode.id)}
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
    align-items: center;
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
  /* One scale: danger for rejected or at risk, warning for degraded but
     usable; each said in a word, never by colour alone. */
  .warning {
    color: var(--warning);
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: center;
    margin-bottom: var(--space-2);
  }
  /* The filters' controls are Bits UI's elements, so their rules are global
     under the filters. */
  .filters :global(.segments) {
    display: inline-flex;
    box-sizing: border-box;
    height: var(--control-height);
    align-items: center;
    padding: 2px;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: var(--panel);
  }
  .filters :global(.segment) {
    height: 100%;
    padding: 0 var(--space-2);
    border: none;
    border-radius: calc(var(--radius) - 1px);
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .filters :global(.segment:hover) {
    color: var(--text);
  }
  .filters :global(.segment[data-state="on"]) {
    background: var(--raised);
    color: var(--text);
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .filters :global(.bin-toggle),
  .filters :global(.delete) {
    display: inline-flex;
    gap: var(--space-1);
    align-items: center;
    height: var(--control-height);
    padding: 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .filters :global(.bin-toggle[data-state="on"]) {
    border-color: var(--accent);
    color: var(--accent);
  }
  .filters :global(.delete) {
    border-color: var(--danger);
    color: var(--danger);
  }
  .filters :global(:is(.segment, .bin-toggle, .delete):focus-visible) {
    outline: 2px solid var(--ring);
    outline-offset: 2px;
  }
  /* The dialog is drawn over the page, outside the list. */
  :global(.overlay) {
    position: fixed;
    inset: 0;
    z-index: 60;
    background: rgb(0 0 0 / 60%);
  }
  :global(.dialog) {
    position: fixed;
    top: 50%;
    left: 50%;
    z-index: 61;
    width: min(28rem, calc(100vw - 2rem));
    padding: var(--space-6);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--raised);
    color: var(--text);
    font-family: var(--font-ui);
    transform: translate(-50%, -50%);
    box-shadow: 0 16px 48px rgb(0 0 0 / 60%);
  }
  :global(.dialog-title) {
    margin: 0 0 var(--space-2);
    font-size: var(--text-lg);
    font-weight: 600;
  }
  :global(.dialog-text) {
    margin: 0 0 var(--space-6);
    color: var(--muted);
    line-height: 1.5;
  }
  :global(.dialog-actions) {
    display: flex;
    gap: var(--space-2);
    justify-content: flex-end;
  }
  :global(.dialog :is(.secondary, .delete)) {
    height: 2rem;
    padding: 0 var(--space-4);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  :global(.dialog .delete) {
    border-color: var(--danger-fill);
    background: var(--danger-fill);
    color: var(--on-danger);
    font-weight: 600;
  }
  :global(.dialog :is(.secondary, .delete):focus-visible) {
    outline: 2px solid var(--ring);
    outline-offset: 2px;
  }
  /* The verdict at a glance, in a word as well as a colour. */
  .badge {
    justify-self: end;
    min-width: 7rem;
    box-sizing: border-box;
    padding: 0 var(--space-2);
    border: 1px solid currentColor;
    border-radius: var(--radius);
    text-align: center;
    font-size: var(--text-xs);
    line-height: 1.5;
  }
  .badge.accepted {
    color: var(--ok);
  }
  .badge.rejected {
    border-color: var(--danger);
    background: var(--danger-background);
    color: var(--danger);
    font-weight: 600;
  }
  .badge.pending {
    color: var(--muted);
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
    border-color: var(--danger);
    color: var(--danger);
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
