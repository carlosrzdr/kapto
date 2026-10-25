<script lang="ts">
  import { onMount, tick } from "svelte";
  import uPlot from "uplot";
  import "uplot/dist/uPlot.min.css";

  import { AXIS_TIME_STEPS, axisTimes, axisTimesStep } from "./format";
  import {
    BANDED_AT_MOST,
    banded,
    type Drawn,
    drawAgain,
    type PlotPart,
    plotColumns,
    type PlotStream,
    resolvePlot,
    shortLabels,
    shownSpan,
    shownStretch,
    type ValuesAt,
    valuesAt,
  } from "./viewer/plots";
  import { seriesStyle, token } from "./theme";

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
  /** Where each stream's values start among the panel's, so each is drawn its own way across its charts. */
  const firsts = $derived(parts.map((_, index) => parts.slice(0, index).reduce((sum, part) => sum + part.labels.length, 0)));
  const count = $derived(parts.reduce((sum, part) => sum + part.labels.length, 0));
  /**
   * The key: each value's label, short, drawn as its line is. Its heading
   * says once what they share, and the stream when the panel draws only
   * one, rather than again as a title over its chart.
   */
  const key = $derived.by(() => {
    const short = shortLabels(parts.flatMap((part) => part.labels));
    const heading = parts.length === 1 ? [parts[0]!.stream.name, short.heading].filter(Boolean).join(" · ") : short.heading;
    return { heading, entries: short.labels.map((label, index) => ({ label, style: seriesStyle(index) })) };
  });
  /** The key's entries picked to show their range, by place. */
  let picked = $state(new Set<number>());
  /** The key's entry pointed at or focused, whose range shows while it is. */
  let pointedEntry = $state<number | null>(null);
  /** Each value's place among the panel's: whether it is drawn as its band from minimum to maximum. */
  const bands = $derived(banded(count, pointedEntry === null ? picked : new Set([...picked, pointedEntry])));
  /** Whether the key's entries can be picked: not while every value shows its band anyway. */
  const choosable = $derived(count > BANDED_AT_MOST);
  function pick(place: number) {
    const next = new Set(picked);
    if (!next.delete(place)) {
      next.add(place);
    }
    picked = next;
  }
  /**
   * What the charts are made for: each stream drawn, its values, their
   * labels and which show their band. New samples, as a replay or a look
   * back sends them, leave it alone, so the charts are only given them
   * rather than made again.
   */
  const shape = $derived(
    `${parts.map((part) => `${part.stream.name}:${part.values.join(",")}:${part.labels.join(",")}`).join("|")}/${bands.map(Number).join("")}`,
  );
  let holder = $state<HTMLDivElement | null>(null);
  let panel = $state<HTMLElement | null>(null);
  /** What the chart under the pointer says there, with its stream; none while it is elsewhere. */
  let hovered = $state<(ValuesAt & { stream: string; first: number }) | null>(null);
  let tip = $state<HTMLElement | null>(null);

  onMount(() => {
    /**
     * Each chart, in the element it is drawn in: its values' place among the
     * panel's and which show their band, what it was last drawn with, the
     * stretch shown (seconds), the samples it was given (as the stream keeps
     * them, for the tip) and how many points that was.
     */
    interface Chart extends Drawing {
      plot: uPlot;
    }
    /** What a chart's plot is made from and reads as it is drawn. */
    interface Drawing {
      part: PlotPart;
      element: HTMLElement;
      first: number;
      bands: boolean[];
      /** The middles of the values drawn as one line, made once. */
      middles: Float64Array[];
      drawn?: Drawn;
      stretch: [number, number];
      data: Float64Array[];
      points: number;
    }
    let charts: Chart[] = [];
    /**
     * The charts at least partly on screen. Only those are drawn: with many
     * streams most are scrolled out of sight, and drawing them all took the
     * page's main thread most of its time. Without the browser's observer,
     * every chart is drawn.
     */
    const inSight = new Set<Element>();
    const sight =
      typeof IntersectionObserver === "undefined"
        ? null
        : new IntersectionObserver((entries) => {
            for (const entry of entries) {
              if (entry.isIntersecting) {
                inSight.add(entry.target);
              } else {
                inSight.delete(entry.target);
              }
            }
          });
    let built: string | null = null;
    const line = token("line", "#2a2e31");
    // The page's own font, so the axes read as the rest of it and their labels can be measured.
    const font = `12px ${token("font-ui", "system-ui, sans-serif")}`;
    const axis = { stroke: token("muted", "#9ca3a8"), font, grid: { stroke: line }, ticks: { stroke: line } };
    const measuring = document.createElement("canvas").getContext("2d");
    if (measuring) {
      measuring.font = font;
    }
    // Each label measured once: the time axis asks for them as often as it is drawn.
    const widths = new Map<string, number>();
    const measure = (text: string) => {
      let width = widths.get(text);
      if (width === undefined) {
        width = measuring?.measureText(text).width ?? text.length * 8;
        widths.set(text, width);
      }
      return width;
    };
    // Room on the right for half the widest time label, centred on the last tick.
    const timeOverhang = Math.ceil(measure("00:00:00.000") / 2) + 6;

    const size = () => ({
      width: holder?.clientWidth ?? 300,
      height: Math.max(80, Math.floor((holder?.clientHeight ?? 160) / Math.max(charts.length, 1)) - 26),
    });

    /** The chart the pointer is over, whose values the tip shows. */
    let pointedAt: uPlot | null = null;

    /**
     * Shows what `plot` has under the pointer, beside it, and again as new
     * samples move the chart under a pointer that stays.
     */
    const point = (plot: uPlot, chart: Drawing) => {
      const { part } = chart;
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
      hovered = { stream: part.stream.name, first: chart.first, ...valuesAt(chart.data, index, shortLabels(part.labels).labels, spread) };
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

    /**
     * A chart per stream drawn, each of its values a line, or a band from
     * its minimum to its maximum where `bands` says so. Titled with its
     * stream only beside others, as the key names a lone one.
     */
    const build = (next: PlotPart[]) => {
      charts.forEach(({ plot }) => plot.destroy());
      sight?.disconnect();
      inSight.clear();
      holder?.replaceChildren();
      charts = [];
      built = shape;
      panel?.setAttribute("data-points", "0");
      next.forEach((part, index) => {
        const element = document.createElement("div");
        holder?.append(element);
        const first = firsts[index] ?? 0;
        const shown = part.labels.map((_, place) => bands[first + place] ?? true);
        const series: uPlot.Series[] = [{}];
        const filled: uPlot.Band[] = [];
        part.labels.forEach((label, place) => {
          const { color, dash } = seriesStyle(first + place);
          if (shown[place]) {
            series.push({ label: `${label} min`, stroke: color, width: 1, dash }, { label: `${label} max`, stroke: color, width: 1, dash });
            filled.push({ series: [series.length - 1, series.length - 2], fill: `${color}33` });
          } else {
            series.push({ label, stroke: color, width: 1.5, dash });
          }
        });
        const drawing: Drawing = {
          part,
          element,
          first,
          bands: shown,
          middles: part.labels.map(() => new Float64Array(part.stream.samples.capacity)),
          stretch: [0, spanMs / 1000],
          data: [],
          points: 0,
        };
        const plot = new uPlot(
          {
            ...(next.length > 1 ? { title: part.stream.name } : {}),
            ...size(),
            series,
            bands: filled,
            padding: [8, timeOverhang, 0, 0],
            legend: { show: false },
            // A line at the time pointed at, whose values the tip says.
            cursor: { x: true, y: false, points: { show: false }, drag: { x: false, y: false } },
            hooks: { setCursor: [(plot) => point(plot, drawing)] },
            // The stretch shown, rather than the samples' own, so the time
            // axis doesn't stretch and shrink with each sample.
            scales: { x: { time: true, range: () => drawing.stretch } },
            axes: [
              {
                ...axis,
                // Ticks as far apart as the labels written at their step
                // are wide, however narrow the panel, so a few seconds
                // without fractions still get more than one.
                incrs: AXIS_TIME_STEPS,
                space: (_, _axis, min, max, dim) => axisTimesStep(min, max, dim, measure).space,
                values: (_, ticks, _axis, _space, step) => axisTimes(ticks, step),
                // A line for the times, and one more only while the dates are written under them.
                size: (_, values) => (values?.some((value) => value.includes("\n")) ? 48 : 32),
              },
              { ...axis, size: 56 },
            ],
          },
          [[], ...series.slice(1).map(() => [])],
          element,
        );
        sight?.observe(element);
        charts.push(Object.assign(drawing, { plot }));
      });
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
        if (sight && !inSight.has(chart.element)) {
          continue;
        }
        const end = endMs ?? part.stream.samples.newestMs;
        if (end === null) {
          continue;
        }
        // Only as far back as the samples kept reach, so a chart just opened is filled.
        const shown = shownSpan(spanMs, end, part.stream.samples.oldestMs);
        const [from, to] = shownStretch(end, shown, plot.bbox.width);
        const next: Drawn = {
          toMs: to * 1000,
          widthPx: plot.bbox.width,
          spanMs: shown,
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
        chart.stretch = [from, to];
        const data = part.stream.samples.stretch(from * 1000, part.values);
        // Only up to the end shown: while looking back, nothing after it.
        const [times] = data;
        let last = times?.length ?? 0;
        while (last > 0 && (times?.[last - 1] ?? 0) * 1000 > end) {
          last--;
        }
        chart.data = data.map((column) => column.subarray(0, last));
        // A stream looked back at may keep more samples than the one the chart was made for.
        if ((chart.middles[0]?.length ?? last) < last) {
          chart.middles = chart.middles.map(() => new Float64Array(part.stream.samples.capacity));
        }
        plot.setData(plotColumns(chart.data, chart.bands, chart.middles) as uPlot.AlignedData);
        chart.points = last;
      }
      // For tests: whether the charts have something drawn.
      panel?.setAttribute("data-points", String(charts.reduce((sum, chart) => sum + chart.points, 0)));
    };
    frame = requestAnimationFrame(draw);

    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      sight?.disconnect();
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
          {@const style = seriesStyle(hovered.first + index)}
          <dt class={`swatch series-${style.hue} dash-${style.pattern}`}>{value.label}</dt>
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
      {@const swatch = `swatch series-${entry.style.hue} dash-${entry.style.pattern}`}
      <li>
        {#if choosable}
          <!-- Pointed at or focused, its range shows; picked, it stays. -->
          <button
            type="button"
            class={swatch}
            aria-pressed={picked.has(index)}
            title="Show its range, from minimum to maximum"
            onclick={() => pick(index)}
            onpointerenter={() => (pointedEntry = index)}
            onpointerleave={() => (pointedEntry = pointedEntry === index ? null : pointedEntry)}
            onfocus={() => (pointedEntry = index)}
            onblur={() => (pointedEntry = pointedEntry === index ? null : pointedEntry)}>{entry.label}</button
          >
        {:else}
          <span class={swatch}>{entry.label}</span>
        {/if}
      </li>
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
    max-height: calc(2 * 1.5rem + 0.15rem);
    overflow-y: auto;
    flex: none;
  }
  .labels li {
    display: flex;
    align-items: center;
    min-height: 1.5rem;
  }
  .labels .heading {
    color: var(--text);
  }
  /* An entry that shows its value's range while pointed at, focused or picked. */
  .labels button {
    min-height: 1.5rem;
    padding: 0 var(--space-1);
    border: 1px solid transparent;
    border-radius: var(--radius);
    background: transparent;
    color: inherit;
    font: inherit;
  }
  .labels button:hover {
    color: var(--text);
  }
  .labels button[aria-pressed="true"] {
    border-color: var(--control-line);
    color: var(--text);
  }
  /* Each value's line as the chart draws it: its colour and its pattern. */
  .swatch::before {
    content: "";
    display: inline-block;
    flex: none;
    width: 1.125rem;
    height: 2px;
    margin-right: 0.3rem;
    vertical-align: middle;
    background: var(--swatch);
  }
  .dash-1::before {
    background: repeating-linear-gradient(to right, var(--swatch) 0 6px, transparent 6px 10px);
  }
  .dash-2::before {
    background: repeating-linear-gradient(to right, var(--swatch) 0 2px, transparent 2px 5px);
  }
  .dash-3::before {
    background: repeating-linear-gradient(to right, var(--swatch) 0 10px, transparent 10px 13px, var(--swatch) 13px 15px, transparent 15px 18px);
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
