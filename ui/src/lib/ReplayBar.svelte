<script lang="ts">
  import type { ReplayStatus } from "./api-types";
  import { duration, sentence } from "./format";
  import type { Playback } from "./viewer/replay";

  interface Props {
    /** The replay asked for or shown, and where it is. */
    replay: ReplayStatus;
    /** Which episode it is, in words: when it was recorded. */
    label: string;
    /** Where the replay is playing, once the viewer says. */
    playback?: Playback | null;
    /** Asked to play the replay again from its start. */
    onplayagain?: () => void;
  }

  let { replay, label, playback = null, onplayagain = () => {} }: Props = $props();
</script>

<div class="bar" role="status" aria-label="Replay">
  <div class="what">
    {#if replay.state === "ready"}
      <strong>Replay</strong> of the episode recorded {label}
      {#if playback?.over}
        <span class="playback"><strong>Episode over</strong> · {duration(playback.length)}</span>
        <button type="button" onclick={onplayagain}>Play again</button>
      {:else if playback}
        <span class="playback"
          >{playback.playing ? "Playing" : "Paused at"} {duration(playback.at)} of {duration(playback.length)}</span
        >
      {/if}
      {#each replay.lacks as lack (lack.code)}
        <p class="lacks">{sentence(lack.message)} <span class="remedy">{lack.remedy}</span></p>
      {/each}
    {:else if replay.state === "failed"}
      <strong>The replay of the episode recorded {label} couldn't be made.</strong>
      {#if replay.failure}
        <p class="lacks">
          {sentence(replay.failure.message)} <span class="remedy">{replay.failure.remedy}</span>
          <span class="code">{replay.failure.code}</span>
        </p>
      {/if}
    {:else}
      Preparing the replay of the episode recorded {label}…
    {/if}
  </div>
</div>

<style>
  .bar {
    padding: 0.4rem 0.75rem;
    margin-bottom: 0.5rem;
    border: 1px solid var(--line);
    border-radius: var(--radius);
  }
  .playback {
    margin-left: 0.75rem;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .playback strong {
    color: var(--text);
  }
  button {
    margin-left: 0.75rem;
    padding: 0.1rem 0.6rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .lacks {
    margin: 0.25rem 0 0;
    color: var(--warning);
    font-size: var(--text-sm);
  }
  .remedy,
  .code {
    color: var(--muted);
  }
  .code {
    font-family: var(--font-data);
  }
</style>
