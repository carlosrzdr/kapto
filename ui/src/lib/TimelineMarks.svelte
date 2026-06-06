<script lang="ts">
  import type { Item } from "./viewer/marks";
  import { SERIES, token } from "./theme";

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

  /** Colours for kinds without one, picked by the kind's name so each keeps its own. */
  const PALETTE = SERIES;
  /** Kapto's own: something went wrong with a stream. */
  const OWN = token("danger", "#f87171");
  const EPISODE = token("muted", "#9ca3a8");
  /** The strip's width in its own units; drawn stretched to the timeline's. */
  const WIDTH = 1000;
  /** The narrowest a mark is drawn, so a moment can be seen. */
  const NARROWEST = 4;
  /** The strip's height, and the least a mark takes to be hit: 24px. */
  const HEIGHT = 24;

  /** The strip's width on the page, to keep every mark 24px wide to the pointer. */
  let shownWidth = $state(WIDTH);
  const hitWidth = $derived(shownWidth > 0 ? Math.min((HEIGHT * WIDTH) / shownWidth, WIDTH) : NARROWEST);

  function color(kind: string): string {
    if (kind === "episode") {
      return EPISODE;
    }
    if (kind.startsWith("kapto.")) {
      return OWN;
    }
    const given = colors[kind];
    if (given) {
      return given;
    }
    let hash = 0;
    for (const character of kind) {
      hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
    }
    return PALETTE[hash % PALETTE.length] ?? EPISODE;
  }

  const x = (ms: number) => ((ms - fromMs) / Math.max(toMs - fromMs, 1)) * WIDTH;

  /** The items in view, placed. */
  const placed = $derived(
    items
      .filter((item) => (item.endMs ?? toMs) >= fromMs && item.startMs <= toMs)
      .map((item) => {
        const left = Math.max(0, x(item.startMs));
        const right = Math.min(WIDTH, x(item.endMs ?? toMs));
        const width = Math.max(right - left, NARROWEST);
        const shown = Math.min(left, WIDTH - width);
        // Centred on the mark, never narrower than a fingertip.
        const hit = Math.max(width, hitWidth);
        const hitLeft = Math.min(Math.max(shown + width / 2 - hit / 2, 0), WIDTH - hit);
        return { item, left: shown, width, hitLeft, hit, color: color(item.kind) };
      }),
  );
</script>

<div class="strip" bind:clientWidth={shownWidth}>
  <svg
    class="marks"
    viewBox="0 0 {WIDTH} {HEIGHT}"
    preserveAspectRatio="none"
    role="group"
    aria-label="What happened"
    data-marks={placed.length}
  >
    {#each placed as { item, left, width, hitLeft, hit, color: fill }, index (index)}
      <g
        role="button"
        tabindex="0"
        aria-label={item.label}
        class="mark"
        class:open={item.endMs === null}
        onclick={() => onseek(item.startMs)}
        onkeydown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onseek(item.startMs);
          }
        }}
      >
        <title>{item.label}</title>
        <rect class="shown" x={left} y={item.kind === "episode" ? 2 : 7} {width} height={item.kind === "episode" ? 20 : 10} {fill} />
        <rect class="hit" x={hitLeft} y="0" width={hit} height={HEIGHT} />
      </g>
    {/each}
  </svg>
</div>

<style>
  .strip {
    width: 100%;
  }
  .marks {
    display: block;
    width: 100%;
    height: 24px;
    /* So the focus ring round a mark's target isn't cut off at the edge. */
    overflow: visible;
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
