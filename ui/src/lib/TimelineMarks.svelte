<script lang="ts">
  import { type Item, LANE_WIDTH as WIDTH, laneMarks, markTip } from "./viewer/marks";

  interface Props {
    /** What happened, oldest first. */
    items: Item[];
    /** The stretch the timeline spans, UTC milliseconds. */
    fromMs: number;
    toMs: number;
    /** Each kind's colour, `#rrggbb`; others get one of their own. */
    colors: Record<string, string>;
    /** Moves the view to `ms`. */
    onseek: (ms: number) => void;
  }

  let { items, fromMs, toMs, colors, onseek }: Props = $props();

  /** The strip's height, and the least a mark takes to be hit: 24px. */
  const HEIGHT = 24;

  /** The mark pointed at or focused, whose tip shows; none otherwise. */
  let pointed = $state<number | null>(null);
  let tip = $state<HTMLElement | null>(null);

  /** The strip's width on the page, to keep every mark 24px wide to the pointer. */
  let shownWidth = $state(WIDTH);
  const hitWidth = $derived(shownWidth > 0 ? Math.min((HEIGHT * WIDTH) / shownWidth, WIDTH) : 0);

  /** The items in view, placed on the lane. */
  const placed = $derived(laneMarks(items, fromMs, toMs, colors, hitWidth));
  const told = $derived(pointed === null ? null : (placed[pointed] ?? null));

  // Over the mark it is about, kept inside the strip. Placed from here, as
  // the page allows no style written into it.
  $effect(() => {
    if (!tip || !told) {
      return;
    }
    const centre = ((told.left + told.width / 2) / WIDTH) * shownWidth;
    const left = Math.min(Math.max(centre - tip.offsetWidth / 2, 0), Math.max(shownWidth - tip.offsetWidth, 0));
    tip.style.translate = `${left}px 0`;
  });
</script>

<div class="strip" bind:clientWidth={shownWidth}>
  <div class="tip" role="tooltip" id="mark-tip" hidden={told === null} bind:this={tip}>
    {#if told}
      {@const said = markTip(told.item)}
      <p class="what">{said.what}</p>
      <p class="when">{said.when}{said.lasting ? ` · ${said.lasting}` : ""}</p>
      <p class="hint">Click to go there</p>
    {/if}
  </div>
  <svg
    class="marks"
    viewBox="0 0 {WIDTH} {HEIGHT}"
    preserveAspectRatio="none"
    role="group"
    aria-label="What happened"
    data-marks={placed.length}
  >
    <!-- The lane the marks run along, above the track and as long as the thumb travels. -->
    <rect class="lane" x="0" y="10" width={WIDTH} height="12" />
    {#each placed as { item, left, width, hitLeft, hit, color: fill }, index (index)}
      <g
        role="button"
        tabindex="0"
        aria-label={item.label}
        aria-describedby={pointed === index ? "mark-tip" : undefined}
        class="mark"
        class:open={item.endMs === null}
        onclick={() => onseek(item.startMs)}
        onpointerenter={() => (pointed = index)}
        onpointerleave={() => (pointed = pointed === index ? null : pointed)}
        onfocus={() => (pointed = index)}
        onblur={() => (pointed = pointed === index ? null : pointed)}
        onkeydown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onseek(item.startMs);
          }
        }}
      >
        <!-- An episode's start or end crosses the lane; what happened within it stays on it. -->
        <rect class="shown" x={left} y={item.kind === "episode" ? 4 : 12} {width} height={item.kind === "episode" ? 20 : 8} {fill} />
        <rect class="hit" x={hitLeft} y="0" width={hit} height={HEIGHT} />
      </g>
    {/each}
  </svg>
</div>

<style>
  /* Inset by half the slider's thumb, which travels that much short of
     each end: a mark starts where the thumb stands at its moment. */
  .strip {
    position: relative;
    margin-inline: calc(var(--thumb, 0.875rem) / 2);
  }
  /* Above the strip, over the mark it is about. */
  .tip {
    position: absolute;
    bottom: calc(100% + var(--space-1));
    left: 0;
    z-index: 2;
    max-width: 20rem;
    padding: var(--space-1) var(--space-2);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    pointer-events: none;
    white-space: nowrap;
  }
  .tip[hidden] {
    display: none;
  }
  .tip p {
    margin: 0;
  }
  .tip .when,
  .tip .hint {
    color: var(--muted);
  }
  .marks {
    display: block;
    width: 100%;
    height: 24px;
    /* So the focus ring round a mark's target isn't cut off at the edge. */
    overflow: visible;
  }
  .lane {
    fill: var(--line);
  }
  .mark {
    cursor: pointer;
    outline: none;
  }
  .shown {
    opacity: 0.85;
  }
  .hit {
    fill: transparent;
  }
  .mark:hover .shown {
    opacity: 1;
  }
  .mark.open .shown {
    opacity: 0.55;
  }
  /* The page's focus ring, drawn round the mark's whole target. */
  .mark:focus-visible .hit {
    stroke: var(--ring);
    stroke-width: 2px;
    vector-effect: non-scaling-stroke;
  }
</style>
