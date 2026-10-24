<script lang="ts">
  import { clock } from "./format";
  import type { Shown } from "./viewer/cameras";

  interface Props {
    /** Each camera with its newest picture. */
    cameras: Shown[];
    /** In a replay, moves to camera `stream`'s frame `step` frames on (negative: back). */
    onstep?: (stream: number, step: number) => void;
    /** Told how wide camera `stream`'s picture is shown, in device pixels, as that changes. */
    onwidth?: (stream: number, pixels: number) => void;
  }

  let { cameras, onstep, onwidth }: Props = $props();

  /**
   * Draws `bitmap` on the canvas, sized to it, and again as it changes.
   * Sized again only when the picture's size changes: sizing a canvas
   * clears it and makes its pixels afresh.
   */
  function draw(canvas: HTMLCanvasElement, bitmap: ImageBitmap) {
    const paint = (next: ImageBitmap) => {
      if (canvas.width !== next.width || canvas.height !== next.height) {
        canvas.width = next.width;
        canvas.height = next.height;
      }
      canvas.getContext("2d")?.drawImage(next, 0, 0);
    };
    paint(bitmap);
    return { update: paint };
  }

  /** Tells `onwidth` how wide camera `stream`'s picture is shown, whenever that changes. */
  function measure(element: HTMLElement, stream: number) {
    const observer = new ResizeObserver(([entry]) => {
      const width = entry?.contentRect.width ?? 0;
      if (width > 0) {
        onwidth?.(stream, Math.ceil(width * window.devicePixelRatio));
      }
    });
    observer.observe(element);
    return { destroy: () => observer.disconnect() };
  }
</script>

<section class="cameras" aria-label="Cameras">
  {#each cameras as camera (camera.stream)}
    <figure data-pictures={camera.pictures}>
      {#if camera.unshown}
        <p class="unshown" role="status">{camera.unshown}</p>
      {:else if camera.bitmap}
        <div
          use:measure={camera.stream}
          class="picture"
          role="img"
          aria-label={camera.frame
            ? `${camera.name}, frame ${camera.frame.n + 1}`
            : camera.keptAtMs
              ? `${camera.name}, its picture at ${clock(camera.keptAtMs)}`
              : `${camera.name}, its newest picture`}
        >
          <canvas use:draw={camera.bitmap}></canvas>
        </div>
      {:else}
        <p class="waiting">Waiting for pictures…</p>
      {/if}
      {#if onstep}
        <div class="steps">
          <button type="button" title="The frame before" aria-label={`${camera.name}: the frame before`} onclick={() => onstep(camera.stream, -1)}>‹</button>
          <button type="button" title="The frame after" aria-label={`${camera.name}: the frame after`} onclick={() => onstep(camera.stream, 1)}>›</button>
        </div>
      {/if}
      <figcaption>
        <span class="name">{camera.name}</span>
        {#if camera.frame}
          <span class="delay">frame {camera.frame.n + 1} of {camera.frame.of}</span>
        {/if}
        {#if camera.keptAtMs}
          <span class="delay" title="A picture the live view kept of this camera, the nearest to the moment shown">at {clock(camera.keptAtMs)}</span>
        {/if}
        {#if camera.delayMs !== null && !camera.unshown}
          <span
            class="delay"
            title="From Kapto receiving the frame to this page receiving its picture, on this computer's clock"
            >{Math.max(0, Math.round(camera.delayMs))} ms</span
          >
        {/if}
      </figcaption>
    </figure>
  {/each}
</section>

<style>
  .cameras {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
    align-content: start;
    gap: 0.5rem;
    min-width: 0;
    /* Within the space it is given, when that is bounded. */
    max-height: 100%;
    overflow: hidden;
  }
  figure {
    position: relative;
    margin: 0;
    aspect-ratio: 4 / 3;
    max-height: min(18rem, 100%);
    border-radius: var(--radius);
    overflow: hidden;
    background: var(--viewer);
  }
  .picture {
    width: 100%;
    height: 100%;
  }
  canvas {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .waiting,
  .unshown {
    margin: 0;
    padding: 0.6rem;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .unshown {
    color: var(--warning);
  }
  figcaption {
    position: absolute;
    left: 0.4rem;
    bottom: 0.4rem;
    display: flex;
    gap: 0.4rem;
    padding: 0.1rem 0.45rem;
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--background) 70%, transparent);
    color: var(--text);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .delay {
    color: var(--muted);
  }
  /* At the top, clear of the caption however narrow the camera is. */
  .steps {
    position: absolute;
    top: 0.4rem;
    right: 0.4rem;
    display: flex;
    gap: 0.25rem;
  }
  .steps button {
    min-width: 1.6rem;
    padding: 0.05rem 0.4rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--background) 70%, transparent);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
</style>
