<script lang="ts">
  import { onMount } from "svelte";
  import uPlot from "uplot";
  import "uplot/dist/uPlot.min.css";

  import type { PlotStream } from "./viewer/plots";
  import { SERIES, token } from "./theme";

  interface Props {
    /** Each stream's plots, its samples arriving as the page receives them. */
    streams: PlotStream[];
    /** How long a stretch the plots show, in milliseconds. */
    spanMs?: number;
    /** When the stretch shown ends, UTC milliseconds; its newest sample if not given. */
    endMs?: number | null;
  }

  let { streams, spanMs = 30_000, endMs = null }: Props = $props();
  /** The most times a second the plots are drawn again. */
  const DRAWS_PER_SECOND = 30;
  /** Each value's colour, by its place in its field, its band a fainter copy. */
  const PALETTE = SERIES;

  let selected = $state(0);
  let holder = $state<HTMLDivElement | null>(null);

  const stream = $derived(streams[Math.min(selected, streams.length - 1)] ?? null);

  onMount(() => {
    let plots: { plot: uPlot; values: number[] }[] = [];
    let shown: PlotStream | null = null;
    const line = token("line", "#2a2e31");
    const axis = { stroke: token("muted", "#9ca3a8"), grid: { stroke: line }, ticks: { stroke: line } };

    const size = () => {
      const width = holder?.clientWidth ?? 300;
      const height = Math.max(110, Math.floor((holder?.clientHeight ?? 300) / Math.max(plots.length, 1)) - 26);
      return { width, height };
    };

    /** Makes a plot per field of the shown stream, replacing the last stream's. */
    const build = (next: PlotStream | null) => {
      plots.forEach(({ plot }) => plot.destroy());
      plots = [];
      shown = next;
      if (!next || !holder) {
        return;
      }
      const count = next.fields.length;
      for (const field of next.fields) {
        const element = document.createElement("div");
        element.className = "plot";
        holder.append(element);
        const series: uPlot.Series[] = [{}];
        const bands: uPlot.Band[] = [];
        field.names.forEach((name, place) => {
          const color = PALETTE[place % PALETTE.length]!;
          series.push({ label: `${name} min`, stroke: color, width: 1 }, { label: `${name} max`, stroke: color, width: 1 });
          bands.push({ series: [series.length - 1, series.length - 2], fill: `${color}33` });
        });
        const { width } = size();
        const height = Math.max(110, Math.floor((holder.clientHeight || 300) / count) - 26);
        const plot = new uPlot(
          {
            title: field.name,
            width,
            height,
            series,
            bands,
            legend: { show: false },
            cursor: { show: false },
            scales: { x: { time: true } },
            axes: [axis, { ...axis, size: 56 }],
          },
          [[], ...field.values.flatMap(() => [[], []])],
          element,
        );
        plots.push({ plot, values: field.values });
      }
    };

    const observer = new ResizeObserver(() => {
      const { width, height } = size();
      plots.forEach(({ plot }) => plot.setSize({ width, height }));
    });
    if (holder) {
      observer.observe(holder);
    }

    let frame = 0;
    let drawn = 0;
    const draw = (now: number) => {
      frame = requestAnimationFrame(draw);
      // Made again only for another stream: the same stream's samples
      // looked back at are drawn in the same plots.
      if (stream?.name !== shown?.name || stream?.fields.length !== shown?.fields.length) {
        holder?.replaceChildren();
        build(stream);
      }
      if (!stream || !shown || now - drawn < 1000 / DRAWS_PER_SECOND) {
        return;
      }
      drawn = now;
      const end = endMs ?? stream.samples.newestMs;
      if (end === null) {
        return;
      }
      for (const { plot, values } of plots) {
        const data = stream.samples.stretch(end - spanMs, values);
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
      plots.forEach(({ plot }) => plot.destroy());
    };
  });
</script>

<section class="plots" aria-label="Live plots">
  {#if streams.length > 1}
    <div class="streams" role="tablist">
      {#each streams as each, index (each.name)}
        <button type="button" role="tab" aria-selected={index === selected} onclick={() => (selected = index)}>
          {each.name}
        </button>
      {/each}
    </div>
  {:else if stream}
    <p class="stream">{stream.name}</p>
  {/if}
  {#if streams.length === 0}
    <p class="empty">No decoded values to plot.</p>
  {/if}
  <div class="holder" bind:this={holder}></div>
</section>

<style>
  .plots {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    background: var(--background);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .streams {
    display: flex;
    gap: 0.25rem;
    padding: 0.4rem 0.5rem 0;
  }
  .streams button {
    padding: 0.15rem 0.6rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .streams button[aria-selected="true"] {
    color: var(--text);
    border-color: var(--accent);
  }
  .stream,
  .empty {
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
