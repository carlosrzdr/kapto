<script lang="ts">
  import type { PreflightRun } from "./api-types";
  import { time } from "./format";

  interface Props {
    /** Pre-flight's latest run; while recording, the run the episode keeps. */
    run: PreflightRun | null;
    recording: boolean;
    /** Pre-flight was asked to run again and hasn't answered yet. */
    running: boolean;
    /** Another order is on its way: nothing more is asked meanwhile. */
    busy: boolean;
    onrun: () => void;
    /** Starts an episode, waiving the failed checks whose codes it is given. */
    onrecordanyway: (waive: string[]) => void;
  }

  let { run, recording, running, busy, onrun, onrecordanyway }: Props = $props();

  const failed = $derived(run?.checks.filter((check) => !check.passed) ?? []);
  /** Exactly what failed, each code once: what recording anyway waives. */
  const waive = $derived([...new Set(failed.map((check) => check.code))]);
</script>

{#if run === null}
  <p class="unknown">Not run yet: it runs when capture starts, for a few seconds.</p>
{:else}
  <p class="verdict" class:problem={failed.length > 0}>
    {#if failed.length === 0}
      All {run.checks.length} checks passed
    {:else if recording}
      Recording despite {failed.length} failed {failed.length === 1 ? "check" : "checks"}
    {:else}
      {failed.length} of {run.checks.length} checks failed
    {/if}
    <span class="when">· {time(Date.parse(run.ran_at) * 1e6)}</span>
  </p>
  {#if failed.length > 0}
    <ul>
      {#each failed as check (`${check.code}/${check.stream ?? ""}`)}
        <li>
          <strong>{check.summary || check.code}</strong>{#if check.stream}<span class="stream"
            >{` · ${check.stream}`}</span
          >{/if}
          {#if check.waived}<span class="waived">waived</span>{/if}
          <p>Measured {check.measured}; needs {check.needed}.</p>
          {#if !recording && check.remedy}<p class="remedy">{check.remedy}</p>{/if}
          <p class="code">{check.code}</p>
        </li>
      {/each}
    </ul>
  {/if}
{/if}
{#if !recording}
  <div class="actions">
    <button type="button" disabled={running || busy} aria-busy={running} onclick={onrun}>
      {running ? "Running…" : "Run again"}
    </button>
    {#if failed.length > 0}
      <button
        type="button"
        class="anyway"
        disabled={running || busy}
        title="Start an episode, waiving {waive.join(', ')}: the episode says so"
        onclick={() => onrecordanyway(waive)}
      >
        Record anyway
      </button>
    {/if}
  </div>
{/if}

<style>
  p {
    margin: 0;
  }
  .verdict {
    font-weight: 600;
  }
  .when,
  .stream,
  .unknown,
  .remedy,
  .code {
    color: var(--muted);
    font-weight: normal;
  }
  .problem {
    color: var(--warning);
  }
  ul {
    margin: 0.5rem 0 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 0.6rem;
  }
  li p {
    font-size: var(--text-sm);
  }
  .code {
    font-family: var(--font-data);
    font-size: var(--text-xs);
  }
  .waived {
    margin-left: 0.4rem;
    padding: 0 0.35rem;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.6rem;
  }
  button {
    padding: 0.3rem 0.8rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  button.anyway {
    border-color: var(--warning);
  }
</style>
