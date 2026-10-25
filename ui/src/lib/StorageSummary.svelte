<script lang="ts">
  import type { Storage } from "./api-types";
  import { bytes, duration } from "./format";

  interface Props {
    storage: Storage | null;
    /** Bytes written per second while an episode is recorded, if known. */
    writing: number | null;
  }

  let { storage, writing }: Props = $props();

  // As capture judges it: low below `capture.disk_warn_below`, and no
  // recording below `capture.disk_stop_below`.
  const low = $derived(storage !== null && storage.free_bytes < storage.warn_below_bytes);
  /** Seconds until recording stops for lack of room, at the current rate. */
  const left = $derived(
    storage !== null && writing !== null && writing > 0
      ? Math.max(0, storage.free_bytes - storage.stop_below_bytes) / writing
      : null,
  );
</script>

{#if storage}
  <dl>
    <dt>Disk</dt>
    <dd class:problem={low}>{bytes(storage.free_bytes)} free of {bytes(storage.total_bytes)}</dd>
    {#if low}
      <dt>Limit</dt>
      <dd class="problem">Recording stops below {bytes(storage.stop_below_bytes)} free</dd>
    {/if}
    {#if writing !== null}
      <dt>Writing</dt>
      <dd>
        {bytes(writing)}/s{#if left !== null}<span
            class:problem={low}
            title="Until less than {bytes(storage.stop_below_bytes)} is free and recording stops">{` · ${duration(left)} left at this rate`}</span
          >{/if}
      </dd>
    {/if}
  </dl>
  <!-- What takes the room and where: wanted when it runs short, so open then. -->
  <details open={low}>
    <summary>Episodes and folder</summary>
    <dl>
      <dt>Episodes</dt>
      <dd>{storage.episodes} · {bytes(storage.episodes_bytes)}</dd>
      <dt>Folder</dt>
      <dd class="folder">{storage.data_dir}</dd>
    </dl>
  </details>
{:else}
  <p class="unknown">Not known yet.</p>
{/if}

<style>
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0;
    font-variant-numeric: tabular-nums;
  }
  dt {
    color: var(--muted);
  }
  details {
    margin-top: var(--space-2);
  }
  summary {
    color: var(--muted);
    font-size: var(--text-sm);
    cursor: pointer;
  }
  details dl {
    margin-top: var(--space-1);
  }
  dd {
    margin: 0;
  }
  .folder {
    font-family: var(--font-data);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
    color: var(--muted);
  }
  .problem {
    color: var(--warning);
    font-weight: 600;
  }
  .unknown {
    color: var(--muted);
    margin: 0;
  }
</style>
