<script lang="ts">
  import Choice from "./controls/Choice.svelte";
  import Icon from "./Icon.svelte";
  import { duration } from "./format";
  import TimelineMarks from "./TimelineMarks.svelte";
  import type { Item } from "./viewer/marks";
  import { ALL_MS } from "./viewer/plots";
  import { SPEEDS } from "./viewer/play";
  import { type SimulatedTime, simulatedLabel } from "./viewer/simulated";

  interface Props {
    /** Whether the view is held at `cursorMs` rather than following now. */
    paused: boolean;
    /** The oldest and newest moments the bridge keeps, UTC milliseconds, if known. */
    keptMs: [number, number] | null;
    /** The moment shown while paused. */
    cursorMs: number | null;
    /** How long a stretch the plots show, in milliseconds. */
    spanMs: number;
    /** How far back the bridge keeps at most, in seconds (`viewer.history_age`); 0 if not known. */
    historySeconds?: number;
    /** What happened, drawn along the slider; clicking one shows it. */
    items?: Item[];
    /** Each kind of event's colour. */
    colors?: Record<string, string>;
    onpause: () => void;
    onlive: () => void;
    /** Shows `ms`, pausing if live. */
    onseek: (ms: number) => void;
    onspan: (ms: number) => void;
    /** Whether the moment shown is moving forward, and how fast. */
    playing?: boolean;
    speed?: number;
    onplay?: () => void;
    onhold?: () => void;
    onspeed?: (speed: number) => void;
    /** Opens what happened at `ms`, playing on; seeks there if not given. */
    onwatch?: (ms: number) => void;
    /** Whether an episode is replayed: its span is `keptMs`, and leaving goes back to live. */
    replaying?: boolean;
    /** The simulation's time a replay plays in, if it does: how far in is counted in it. */
    simulatedTime?: SimulatedTime | null;
    /** The simulation's time now, in words, while the live view follows one. */
    simulatedNow?: string | null;
  }

  let {
    paused,
    keptMs,
    cursorMs,
    spanMs,
    historySeconds = 0,
    items = [],
    colors = {},
    onpause,
    onlive,
    onseek,
    onspan,
    playing = false,
    speed = 1,
    onplay = () => {},
    onhold = () => {},
    onspeed = () => {},
    onwatch,
    replaying = false,
    simulatedTime = null,
    simulatedNow = null,
  }: Props = $props();

  /** The stretches the plots can show. */
  const SPANS = [
    { ms: 10_000, label: "10 s" },
    { ms: 30_000, label: "30 s" },
    { ms: 120_000, label: "2 min" },
    { ms: 600_000, label: "10 min" },
    { ms: ALL_MS, label: "All" },
  ];

  const oldest = $derived(keptMs?.[0] ?? 0);
  const newest = $derived(keptMs?.[1] ?? 0);
  const at = $derived(paused && cursorMs !== null ? Math.min(Math.max(cursorMs, oldest), newest) : newest);
  const behind = $derived(
    replaying && simulatedTime
      ? `${duration(simulatedTime.played(at) / 1000)} · ${simulatedLabel(simulatedTime.at(at))}`
      : replaying
        ? duration((at - oldest) / 1000)
        : paused
          ? `−${duration((newest - at) / 1000)}`
          : simulatedNow
            ? `now · ${simulatedNow}`
            : "now",
  );
  /** How far back can be looked now. */
  const kept = $derived(
    keptMs === null
      ? null
      : replaying && simulatedTime
        ? duration(simulatedTime.lengthMs / 1000)
        : duration((newest - oldest) / 1000),
  );
  const limit = $derived(
    replaying
      ? "How long the episode replayed is."
      : historySeconds > 0
      ? `The viewer bridge keeps up to the last ${duration(historySeconds)} (viewer.history_age), less if viewer.history (MiB) fills first. It is kept in memory: restarting Kapto starts it again.`
      : "How far back the viewer bridge keeps what it streamed.",
  );
</script>

<div class="timeline">
  {#if paused}
    {#if playing}
      <button type="button" title="Pause here (Space)" onclick={onhold}><Icon name="pause" /> Pause</button>
    {:else}
      <button type="button" title="Play back from here (Space)" onclick={onplay}><Icon name="play" /> Play</button>
    {/if}
    <Choice
      label="Speed"
      showLabel={false}
      width="compact"
      value={String(speed)}
      options={SPEEDS.map((each) => ({ value: String(each), label: `${each}×` }))}
      onchange={(value) => onspeed(Number(value))}
    />
    <!-- Replaying, the header's Back to live is the one way back: said once. -->
    {#if !replaying}
      <button type="button" title="Back to now (Esc)" onclick={onlive}>Back to now</button>
    {/if}
  {:else}
    <button type="button" title="Pause and look back (Space)" onclick={onpause} disabled={keptMs === null}
      ><Icon name="pause" /> Pause</button
    >
  {/if}
  <div class="track">
    {#if keptMs !== null}
      <TimelineMarks {items} fromMs={oldest} toMs={newest} {colors} onseek={onwatch ?? onseek} />
    {/if}
    <input
      type="range"
      aria-label="Moment shown"
      aria-valuetext={behind}
      min={oldest}
      max={newest}
      step="any"
      value={at}
      disabled={keptMs === null}
      oninput={(event) => onseek(Number(event.currentTarget.value))}
    />
  </div>
  <span class="behind">{behind}</span>
  {#if kept !== null}
    <span class="kept" title={limit}>{paused ? `of ${kept}` : `${kept} kept`}</span>
  {/if}
  <Choice
    label="Span"
    showLabel={false}
    width="compact"
    value={String(spanMs)}
    options={SPANS.map((span) => ({ value: String(span.ms), label: span.label }))}
    onchange={(value) => onspan(Number(value))}
  />
</div>

<style>
  .timeline {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.4rem 0.2rem 0;
  }
  button {
    min-width: 6.5rem;
    height: var(--control-height);
    padding: 0 var(--space-3);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  /* The marks' lane right on the track, so each is read against it. */
  .track {
    --thumb: 0.875rem;
    flex: 1;
    display: flex;
    flex-direction: column;
  }
  /* A quiet line to slide along, as wide as the timeline, with only the
     moment shown in the accent: drawn here, as the browser's own fills the
     track up to the thumb, all of it while live. */
  input {
    width: 100%;
    height: 1rem;
    margin: 0;
    background: transparent;
    appearance: none;
    cursor: pointer;
  }
  input:disabled {
    cursor: not-allowed;
  }
  input::-webkit-slider-runnable-track {
    height: 2px;
    border-radius: 1px;
    background: var(--control-line);
  }
  input::-moz-range-track {
    height: 2px;
    border-radius: 1px;
    background: var(--control-line);
  }
  input::-webkit-slider-thumb {
    width: var(--thumb);
    height: var(--thumb);
    margin-top: calc(1px - var(--thumb) / 2);
    border: none;
    border-radius: 50%;
    background: var(--accent);
    appearance: none;
  }
  input::-moz-range-thumb {
    width: var(--thumb);
    height: var(--thumb);
    border: none;
    border-radius: 50%;
    background: var(--accent);
  }
  input:disabled::-webkit-slider-thumb {
    background: var(--muted);
  }
  input:disabled::-moz-range-thumb {
    background: var(--muted);
  }
  /* The moment shown, in the text's colour: where the view is, not a state. */
  .behind {
    min-width: 3.5rem;
    color: var(--text);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .kept {
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    cursor: help;
  }
</style>
