<script lang="ts">
  import type { Shown } from "./viewer/cameras";

  interface Props {
    /** Each camera with its newest picture. */
    cameras: Shown[];
  }

  let { cameras }: Props = $props();

  /** Draws `bitmap` on the canvas, sized to it, and again as it changes. */
  function draw(canvas: HTMLCanvasElement, bitmap: ImageBitmap) {
    const paint = (next: ImageBitmap) => {
      canvas.width = next.width;
      canvas.height = next.height;
      canvas.getContext("2d")?.drawImage(next, 0, 0);
    };
    paint(bitmap);
    return { update: paint };
  }
</script>

<section class="cameras" aria-label="Cameras">
  {#each cameras as camera (camera.stream)}
    <figure data-pictures={camera.pictures}>
      {#if camera.unshown}
        <p class="unshown" role="status">{camera.unshown}</p>
      {:else if camera.bitmap}
        <div class="picture" role="img" aria-label={`${camera.name}, its newest picture`}>
          <canvas use:draw={camera.bitmap}></canvas>
        </div>
      {:else}
        <p class="waiting">Waiting for pictures…</p>
      {/if}
      <figcaption>
        <span class="name">{camera.name}</span>
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
    gap: 0.5rem;
    min-width: 0;
  }
  figure {
    position: relative;
    margin: 0;
    aspect-ratio: 4 / 3;
    max-height: 18rem;
    border-radius: var(--radius);
    overflow: hidden;
    background: #0d1011;
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
</style>
