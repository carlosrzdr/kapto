<script lang="ts">
  import { onMount } from "svelte";
  import uPlot from "uplot";
  import "uplot/dist/uPlot.min.css";

  import { axisTimes } from "./format";
  import { type PlotPart, type PlotStream, resolvePlot, shortLabels } from "./viewer/plots";
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
  let holder = $state<HTMLDivElement | null>(null);

  onMount(() => {
    let charts: { plot: uPlot; part: PlotPart }[] = [];
    let built: PlotPart[] = [];
    const line = token("line", "#2a2e31");
    const axis = { stroke: token("muted", "#9ca3a8"), grid: { stroke: line }, ticks: { stroke: line } };

    const size = () => ({
      width: holder?.clientWidth ?? 300,
      height: Math.max(80, Math.floor((holder?.clientHeight ?? 160) / Math.max(charts.length, 1)) - 26),
    });

    /** A chart per stream drawn, each of its values a band from its minimum to its maximum. */
    const build = (next: PlotPart[]) => {
      charts.forEach(({ plot }) => plot.destroy());
      holder?.replaceChildren();
      charts = [];
      built = next;
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
            // Room on the right for the time axis's last label, however narrow.
            padding: [8, 24, 0, 0],
            legend: { show: false },
            cursor: { show: false },
            scales: { x: { time: true } },
            axes: [{ ...axis, values: (_, ticks, _axis, _space, step) => axisTimes(ticks, step) }, { ...axis, size: 56 }],
          },
          [[], ...part.values.flatMap(() => [[], []])],
          element,
        );
        charts.push({ plot, part });
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
      // Made again only when what it draws changes, as a stream opens.
      if (parts !== built) {
        build(parts);
      }
      if (now - drawn < 1000 / DRAWS_PER_SECOND) {
        return;
      }
      drawn = now;
      for (const { plot, part } of charts) {
        const end = endMs ?? part.stream.samples.newestMs;
        if (end === null) {
          continue;
        }
        const data = part.stream.samples.stretch(end - spanMs, part.values);
        // Only up to the end shown: while looking back, nothing after it.
        const [times] = data;
        let last = times?.length ?? 0;
        while (last > 0 && (times?.[last - 1] ?? 0) * 1000 > end) {
          last--;
        }
        plot.setData(data.map((column) => column.subarray(0, last)) as uPlot.AlignedData);
      }
    };
    frame = requestAnimationFrame(draw);

    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      charts.forEach(({ plot }) => plot.destroy());
    };
  });
</script>

<section class="plot" aria-label={`Plot of ${fields.join(", ")}`}>
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
