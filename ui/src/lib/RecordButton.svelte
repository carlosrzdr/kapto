<script lang="ts">
  import Hint from "./controls/Hint.svelte";
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

<!-- What it does, and why it can't for now, on hover; a disabled button
     takes no pointer, so the hint is on what holds it. -->
<Hint text={held && !busy ? `${does}: ${held}` : does}>
  {#snippet children(props)}
    <span {...props} class="holder">
      <button
        type="button"
        class:recording
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
    </span>
  {/snippet}
</Hint>
{#if held && !busy}
  <span id="record-held" class="visually-hidden">{held}</span>
{/if}

<style>
  .holder {
    display: inline-flex;
  }
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
