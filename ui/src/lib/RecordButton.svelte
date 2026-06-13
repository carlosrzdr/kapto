<script lang="ts">
  import Icon from "./Icon.svelte";
  interface Props {
    recording: boolean;
    /** A request is on its way: the button waits for its answer. */
    busy: boolean;
    /** Why it can't be pressed for now, if it can't, e.g. while pre-flight runs; it still says what it would do. */
    held?: string | null;
    onstart: () => void;
    onstop: () => void;
  }

  let { recording, busy, held = null, onstart, onstop }: Props = $props();
  const does = $derived(recording ? "Stop recording (R)" : "Record an episode (R)");
</script>

<button
  type="button"
  class:recording
  title={held && !busy ? `${does}: ${held}` : does}
  disabled={busy || held !== null}
  aria-busy={busy}
  aria-describedby={held && !busy ? "record-held" : undefined}
  onclick={() => (recording ? onstop() : onstart())}
>
  {#if busy}
    {recording ? "Finalising…" : "Starting…"}
  {:else}
    <Icon name={recording ? "stop" : "record"} />
    {recording ? "Stop" : "Record"}
  {/if}
</button>
{#if held && !busy}
  <span id="record-held" class="visually-hidden">{held}</span>
{/if}

<style>
  button {
    min-width: 7rem;
    padding: var(--space-2) var(--space-4);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: var(--accent);
    color: var(--on-accent);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  button:not(:disabled):hover {
    filter: brightness(1.08);
  }
  button.recording {
    background: var(--danger-fill);
    border-color: var(--danger-fill);
    color: var(--on-danger);
  }
</style>
