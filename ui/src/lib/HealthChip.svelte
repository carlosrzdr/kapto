<script lang="ts">
  import Hint from "./controls/Hint.svelte";
  import type { Health } from "./health";

  interface Props {
    health: Health;
    /** Opens where the problem said is shown in full, or the Live tab while all is well. */
    onopen: () => void;
  }

  let { health, onopen }: Props = $props();

  const said = $derived(health.more > 0 ? `${health.text} +${health.more}` : health.text);
  /** Every problem, for a screen reader and on hover: the chip says only the worst. */
  const described = $derived(
    health.problems.length === 0
      ? "Every stream, the disk, pre-flight, every process and destination: nothing needs attention. Opens the Live tab."
      : `${health.problems.map((problem) => problem.text).join("; ")}. Opens where it is shown in full.`,
  );
</script>

<!-- Every problem, shown on hover and focus; said to screen readers always. -->
<Hint text={described}>
  {#snippet children(props)}
    <button
      {...props}
      type="button"
      class="chip {health.level}"
      aria-label="Health: {said}"
      aria-describedby="health-detail"
      onclick={onopen}
    >
      <span class="dot" aria-hidden="true"></span>
      <span class="text">{health.text}</span>
      {#if health.more > 0}<span class="more">+{health.more}</span>{/if}
    </button>
  {/snippet}
</Hint>
<span id="health-detail" class="visually-hidden">{described}</span>

<style>
  /* As wide as what it says, so "All well" isn't a long empty box; a long
     problem is cut short at a limit, said whole on hover. */
  .chip {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: var(--space-2);
    width: max-content;
    max-width: 18rem;
    padding: var(--space-1) var(--space-3);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
  }
  .text {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .more {
    flex: none;
    font-variant-numeric: tabular-nums;
  }
  .dot {
    flex: none;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--ok);
  }
  .warning {
    border-color: var(--warning);
    color: var(--warning);
    font-weight: 600;
  }
  .danger {
    border-color: var(--danger);
    color: var(--danger);
    font-weight: 600;
  }
  /* A ring, not a filled dot, when something is wrong: told apart by
     shape as well as colour, as in the stream table. */
  .warning .dot,
  .danger .dot {
    width: 0.35rem;
    height: 0.35rem;
    border: 2px solid currentColor;
    background: none;
  }
</style>
