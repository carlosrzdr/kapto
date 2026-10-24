<script lang="ts">
  import { onMount, tick } from "svelte";
  import uPlot from "uplot";
  import "uplot/dist/uPlot.min.css";

  import { axisTimes, axisTimesSpace } from "./format";
  import { type Drawn, drawAgain, type PlotPart, type PlotStream, resolvePlot, shortLabels, shownStretch, type ValuesAt, valuesAt } from "./viewer/plots";
  import { SERIES, token } from "./theme";

  interface Props {
    /** What it draws: `stream.field`, `stream.field[3]` or `role:gripper.opening`. */
    fields: string[];
    /** Each role's field, e.g. `gripper.opening` to `arm_joints.gripper`. */
    roles: Record<string, string>;
    /** Every stream's plots, their samples arriving as the page receives them. */
    streams: PlotStream[];
    /** How long a stretch it shows, in milliseconds. */
    spanMs?: number;
    /** When the stretch shown ends, UTC milliseconds; its newest sample if not given. */
    endMs?: number | null;
  }

  let { fields, roles, streams, spanMs = 30_000, endMs = null }: Props = $props();
  /** The most times a second it is drawn again. */
  const DRAWS_PER_SECOND = 30;

  const parts = $derived(resolvePlot(fields, roles, streams));
  /** The key: each value's label, short, with its colour's place in its stream's chart. */
  const key = $derived.by(() => {
    const short = shortLabels(parts.flatMap((part) => part.labels));
    const places = parts.flatMap((part) => part.labels.map((_, place) => place % SERIES.length));
    return { heading: short.heading, entries: short.labels.map((label, index) => ({ label, place: places[index] ?? 0 })) };
  });
  /**
   * What the charts are made for: each stream drawn, its values and their
   * labels. New samples, as a replay or a look back sends them, leave it
   * alone, so the charts are only given them rather than made again.
   */
  const shape = $derived(parts.map((part) => `${part.stream.name}:${part.values.join(",")}:${part.labels.join(",")}`).join("|"));
  let holder = $state<HTMLDivElement | null>(null);
  let panel = $state<HTMLElement | null>(null);
  /** What the chart under the pointer says there, with its stream; none while it is elsewhere. */
  let hovered = $state<(ValuesAt & { stream: string }) | null>(null);
  let tip = $state<HTMLElement | null>(null);

  onMount(() => {
    /** Each chart, with what it was last drawn with and how many points that was. */
    let charts: { plot: uPlot; part: PlotPart; drawn?: Drawn; points: number }[] = [];
    let built: string | null = null;
    const line = token("line", "#2a2e31");
    // The page's own font, so the axes read as the rest of it and their labels can be measured.
    const font = `12px ${token("font-ui", "system-ui, sans-serif")}`;
    const axis = { stroke: token("muted", "#9ca3a8"), font, grid: { stroke: line }, ticks: { stroke: line } };
    const measuring = document.createElement("canvas").getContext("2d");
    if (measuring) {
      measuring.font = font;
    }
    // Ticks no closer than their labels are wide, however narrow the panel.
    const timeSpace = axisTimesSpace((text) => measuring?.measureText(text).width ?? text.length * 8);

    const size = () => ({
      width: holder?.clientWidth ?? 300,
      height: Math.max(80, Math.floor((holder?.clientHeight ?? 160) / Math.max(charts.length, 1)) - 26),
    });

    /** Where the stretch shown ends, UTC milliseconds, as last drawn. */
    let ending = 0;
    /** The chart the pointer is over, whose values the tip shows. */
    let pointedAt: uPlot | null = null;

    /**
     * Shows what `plot` has under the pointer, beside it, and again as new
     * samples move the chart under a pointer that stays.
     */
    const point = (plot: uPlot, part: PlotPart) => {
      const index = plot.cursor.idx;
      const left = plot.cursor.left ?? -1;
      if (index == null || left < 0) {
        if (pointedAt === plot) {
          pointedAt = null;
          hovered = null;
        }
        return;
      }
      pointedAt = plot;
      const spread = (low: number, high: number) => Math.abs(plot.valToPos(high, "y") - plot.valToPos(low, "y"));
      hovered = { stream: part.stream.name, ...valuesAt(plot.data, index, shortLabels(part.labels).labels, spread) };
      const over = plot.over.getBoundingClientRect();
      const top = plot.cursor.top ?? 0;
      // Placed once what it says is drawn, so its size is known.
      void tick().then(() => place(over, left, top));
    };

    /**
     * Places the tip beside the pointer at `left` and `top` over the chart
     * whose area is `over`, on whichever side has room. Placed from here,
     * as the page allows no style written into it.
     */
    const place = (over: DOMRect, left: number, top: number) => {
      if (!tip || !panel) {
        return;
      }
      const box = panel.getBoundingClientRect();
      const x = over.left - box.left + left;
      const y = over.top - box.top + top;
      const gap = 12;
      const across = x + gap + tip.offsetWidth > box.width ? x - gap - tip.offsetWidth : x + gap;
      const down = Math.min(y + gap, box.height - tip.offsetHeight);
      tip.style.translate = `${Math.max(0, across)}px ${Math.max(0, down)}px`;
    };

    /** A chart per stream drawn, each of its values a band from its minimum to its maximum. */
    const build = (next: PlotPart[]) => {
      charts.forEach(({ plot }) => plot.destroy());
      holder?.replaceChildren();
      charts = [];
      built = shape;
      panel?.setAttribute("data-points", "0");
      for (const part of next) {
        const element = document.createElement("div");
        holder?.append(element);
        const series: uPlot.Series[] = [{}];
        const bands: uPlot.Band[] = [];
        part.labels.forEach((label, place) => {
          const color = SERIES[place % SERIES.length]!;
          series.push({ label: `${label} min`, stroke: color, width: 1 }, { label: `${label} max`, stroke: color, width: 1 });
          bands.push({ series: [series.length - 1, series.length - 2], fill: `${color}33` });
        });
        const plot = new uPlot(
          {
            title: part.stream.name,
            ...size(),
            series,
            bands,
            // Room on the right for half the time axis's last label, centred on its tick.
            padding: [8, Math.ceil(timeSpace / 2), 0, 0],
            legend: { show: false },
            // A line at the time pointed at, whose values the tip says.
            cursor: { x: true, y: false, points: { show: false }, drag: { x: false, y: false } },
            hooks: { setCursor: [(plot) => point(plot, part)] },
            // The span shown, rather than the samples' own, so the time axis
            // doesn't stretch and shrink as they arrive.
            scales: { x: { time: true, range: (chart) => shownStretch(ending, spanMs, chart.bbox.width) } },
            axes: [
              {
                ...axis,
                space: timeSpace,
                values: (_, ticks, _axis, _space, step) => axisTimes(ticks, step),
              },
              { ...axis, size: 56 },
            ],
          },
          [[], ...part.values.flatMap(() => [[], []])],
          element,
        );
        charts.push({ plot, part, points: 0 });
      }
      const { width, height } = size();
      charts.forEach(({ plot }) => plot.setSize({ width, height }));
    };

    const observer = new ResizeObserver(() => {
      const { width, height } = size();
      charts.forEach(({ plot }) => plot.setSize({ width, height }));
    });
    if (holder) {
      observer.observe(holder);
    }

    let frame = 0;
    let drawn = 0;
    const draw = (now: number) => {
      frame = requestAnimationFrame(draw);
      // Made again only when what it draws changes, as a stream opens;
      // otherwise each chart draws the newest samples of its stream.
      if (shape !== built) {
        build(parts);
      } else {
        charts.forEach((chart, place) => {
          chart.part = parts[place] ?? chart.part;
        });
      }
      if (now - drawn < 1000 / DRAWS_PER_SECOND) {
        return;
      }
      drawn = now;
      for (const chart of charts) {
        const { plot, part } = chart;
        const end = endMs ?? part.stream.samples.newestMs;
        if (end === null) {
          continue;
        }
        const [from, to] = shownStretch(end, spanMs, plot.bbox.width);
        const next: Drawn = {
          toMs: to * 1000,
          widthPx: plot.bbox.width,
          spanMs,
          stream: part.stream,
          newestMs: part.stream.samples.newestMs,
          atMs: now,
        };
        // Only when it would look different: a chart drawn costs the page
        // more than anything else it does, many times a second.
        if (!drawAgain(chart.drawn, next)) {
          continue;
        }
        chart.drawn = next;
        ending = end;
        const data = part.stream.samples.stretch(from * 1000, part.values);
        // Only up to the end shown: while looking back, nothing after it.
        const [times] = data;
        let last = times?.length ?? 0;
        while (last > 0 && (times?.[last - 1] ?? 0) * 1000 > end) {
          last--;
        }
        plot.setData(data.map((column) => column.subarray(0, last)) as uPlot.AlignedData);
        chart.points = last;
      }
      // For tests: whether the charts have something drawn.
      panel?.setAttribute("data-points", String(charts.reduce((sum, chart) => sum + chart.points, 0)));
    };
    frame = requestAnimationFrame(draw);

    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      charts.forEach(({ plot }) => plot.destroy());
    };
  });
</script>

<section class="plot" aria-label={`Plot of ${fields.join(", ")}`} bind:this={panel}>
  <div class="tip" role="tooltip" hidden={hovered === null} bind:this={tip}>
    {#if hovered}
      <p class="when">{hovered.stream} · {hovered.time}</p>
      <dl>
        {#each hovered.values as value, index (index)}
          <dt class={`series-${index % SERIES.length}`}>{value.label}</dt>
          <dd>{value.value}</dd>
        {/each}
      </dl>
    {/if}
  </div>
  <ul class="labels">
    {#if key.heading}
      <li class="heading">{key.heading}</li>
    {/if}
    {#each key.entries as entry, index (index)}
      <li class={`series-${entry.place}`}>{entry.label}</li>
    {/each}
  </ul>
  {#if parts.length === 0}
    <p class="waiting">Waiting for {fields.join(", ")}</p>
  {/if}
  <div class="holder" bind:this={holder}></div>
</section>

<style>
  .plot {
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    background: var(--background);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .labels {
    display: flex;
    flex-wrap: wrap;
    gap: 0.15rem 0.75rem;
    margin: 0;
    padding: 0.4rem 0.6rem 0;
    list-style: none;
    color: var(--muted);
    font-size: var(--text-xs);
    /* Two lines at most, the rest scrolled to, so the chart keeps the panel. */
    max-height: 3.4em;
    overflow-y: auto;
    flex: none;
  }
  .labels .heading {
    color: var(--text);
  }
  .labels li:not(.heading)::before {
    content: "";
    display: inline-block;
    width: 0.6rem;
    height: 2px;
    margin-right: 0.3rem;
    vertical-align: middle;
    background: var(--swatch);
  }
  .series-0 {
    --swatch: var(--series-0);
  }
  .series-1 {
    --swatch: var(--series-1);
  }
  .series-2 {
    --swatch: var(--series-2);
  }
  .series-3 {
    --swatch: var(--series-3);
  }
  .series-4 {
    --swatch: var(--series-4);
  }
  .series-5 {
    --swatch: var(--series-5);
  }
  .series-6 {
    --swatch: var(--series-6);
  }
  .series-7 {
    --swatch: var(--series-7);
  }
  .waiting {
    margin: 0;
    padding: 0.4rem 0.6rem 0;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  /* What the chart under the pointer says there, above the charts. */
  .tip {
    position: absolute;
    top: 0;
    left: 0;
    z-index: 1;
    max-width: 18rem;
    padding: var(--space-1) var(--space-2);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    color: var(--text);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    pointer-events: none;
  }
  .tip[hidden] {
    display: none;
  }
  .tip .when {
    margin: 0 0 var(--space-1);
    color: var(--muted);
  }
  .tip dl {
    display: grid;
    grid-template-columns: auto auto;
    gap: 0 var(--space-3);
    margin: 0;
  }
  .tip dt::before {
    content: "";
    display: inline-block;
    width: 0.6rem;
    height: 2px;
    margin-right: 0.3rem;
    vertical-align: middle;
    background: var(--swatch);
  }
  .tip dd {
    margin: 0;
    text-align: right;
  }
  .holder {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .holder :global(.u-title) {
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 500;
  }
</style>
