<script lang="ts">
  import Icon from "./Icon.svelte";
  import { duration } from "./format";
  import TimelineMarks from "./TimelineMarks.svelte";
  import type { Item } from "./viewer/marks";
  import { SPEEDS } from "./viewer/play";

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
  }: Props = $props();

  /** The stretches the plots can show. */
  const SPANS = [
    { ms: 10_000, label: "10 s" },
    { ms: 30_000, label: "30 s" },
    { ms: 120_000, label: "2 min" },
    { ms: 600_000, label: "10 min" },
  ];

  const oldest = $derived(keptMs?.[0] ?? 0);
  const newest = $derived(keptMs?.[1] ?? 0);
  const at = $derived(paused && cursorMs !== null ? Math.min(Math.max(cursorMs, oldest), newest) : newest);
  const behind = $derived(
    replaying ? duration((at - oldest) / 1000) : paused ? `−${duration((newest - at) / 1000)}` : "now",
  );
  /** How far back can be looked now. */
  const kept = $derived(keptMs === null ? null : duration((newest - oldest) / 1000));
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
    <select aria-label="Speed" value={speed} onchange={(event) => onspeed(Number(event.currentTarget.value))}>
      {#each SPEEDS as each (each)}
        <option value={each}>{each}×</option>
      {/each}
    </select>
    <button type="button" title={replaying ? "Back to the live robot (Esc)" : "Back to now (Esc)"} onclick={onlive}
      >{replaying ? "Back to live" : "Back to now"}</button
    >
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
  <span class="behind" class:paused>{behind}</span>
  {#if kept !== null}
    <span class="kept" title={limit}>{paused ? `of ${kept}` : `${kept} kept`}</span>
  {/if}
  <select aria-label="Span" value={spanMs} onchange={(event) => onspan(Number(event.currentTarget.value))}>
    {#each SPANS as span (span.ms)}
      <option value={span.ms}>{span.label}</option>
    {/each}
  </select>
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
    padding: 0.2rem 0.7rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .track {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  input {
    width: 100%;
    margin: 0;
    accent-color: var(--accent);
  }
  .behind {
    min-width: 3.5rem;
    color: var(--muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .behind.paused {
    color: var(--warning);
  }
  .kept {
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    cursor: help;
  }
  select {
    padding: 0.15rem 0.3rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
  }
</style>
