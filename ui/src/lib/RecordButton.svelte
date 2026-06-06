<script lang="ts">
  import Icon from "./Icon.svelte";
  interface Props {
    recording: boolean;
    /** A request is on its way: the button waits for its answer. */
    busy: boolean;
    /** Can't be pressed for now, e.g. while pre-flight runs; it still says what it would do. */
    held?: boolean;
    onstart: () => void;
    onstop: () => void;
  }

  let { recording, busy, held = false, onstart, onstop }: Props = $props();
</script>

<button
  type="button"
  class:recording
  title={recording ? "Stop recording (R)" : "Record an episode (R)"}
  disabled={busy || held}
  onclick={() => (recording ? onstop() : onstart())}
>
  {#if busy}
    {recording ? "Finalising…" : "Starting…"}
  {:else}
    <Icon name={recording ? "stop" : "record"} />
    {recording ? "Stop" : "Record"}
  {/if}
</button>

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
  button:disabled {
    opacity: 0.6;
    cursor: progress;
  }
</style>
