<script lang="ts">
  import { untrack } from "svelte";

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

  /**
   * What the replay lacks, each said once: every missing mesh has the same
   * code, and a file a link draws and collides with is missing twice.
   */
  const lacks = $derived([...new Map(replay.lacks.map((lack) => [`${lack.code} ${lack.message}`, lack])).values()]);
  /** What it lacks in one line, the rest folded under it: one remedy said once if they share it. */
  const lacking = $derived.by(() => {
    if (lacks.length === 0) {
      return null;
    }
    const meshes = lacks.every((lack) => lack.code === "robot.mesh_unreadable");
    const remedies = new Set(lacks.map((lack) => lack.remedy));
    const line = meshes
      ? lacks.length === 1
        ? "The robot is drawn without one of its meshes, which couldn't be read."
        : `The robot is drawn without ${lacks.length} of its meshes, which couldn't be read.`
      : lacks.length === 1
        ? sentence(lacks[0]!.message)
        : `The replay lacks ${lacks.length} things it was recorded with.`;
    return { line, remedy: remedies.size === 1 ? lacks[0]!.remedy : null };
  });

  /** Whether the replay is still being made. */
  const preparing = $derived(replay.state !== "ready" && replay.state !== "failed");
  /** When the page first saw this episode's replay being prepared, and the time now, each second. */
  let since = $state<{ episode: string; at: number } | null>(null);
  let now = $state(Date.now());
  $effect(() => {
    if (!preparing) {
      return;
    }
    const episode = replay.episode;
    untrack(() => {
      if (since?.episode !== episode) {
        since = { episode, at: Date.now() };
      }
    });
    now = Date.now();
    const ticking = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(ticking);
  });
  /** How long it has taken so far. */
  const taken = $derived(duration(since === null ? 0 : (now - since.at) / 1000));
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
      {#if lacking}
        <!-- One line however much it lacks: each file, and what to do, folded under it. -->
        <details class="lacks">
          <summary>{lacking.line}</summary>
          {#if lacking.remedy}
            <p class="remedy">{lacking.remedy}</p>
          {/if}
          <ul>
            {#each lacks as lack (`${lack.code} ${lack.message}`)}
              <li>
                {sentence(lack.message)}
                {#if !lacking.remedy}<span class="remedy">{lack.remedy}</span>{/if}
                <span class="code">{lack.code}</span>
              </li>
            {/each}
          </ul>
        </details>
      {/if}
    {:else if replay.state === "failed"}
      <strong>The replay of the episode recorded {label} couldn't be made.</strong>
      {#if replay.failure}
        <p class="lacks">
          {sentence(replay.failure.message)} <span class="remedy">{replay.failure.remedy}</span>
          <span class="code">{replay.failure.code}</span>
        </p>
      {/if}
    {:else}
      Preparing the replay of the episode recorded {label}… <span class="taken">{taken}</span>
      <!-- How long it will take isn't known: the bar, with no value, only says it is being made; the time says how long so far. -->
      <progress aria-label="Preparing the replay" aria-valuetext="{taken} so far"></progress>
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
    height: var(--control-height);
    margin-left: 0.75rem;
    padding: 0 var(--space-3);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .lacks {
    margin: 0.25rem 0 0;
    font-size: var(--text-sm);
  }
  p.lacks {
    color: var(--warning);
  }
  /* The line in one row, cut short rather than wrapped: the rest is under it. */
  .lacks summary {
    overflow: hidden;
    color: var(--warning);
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
  }
  .lacks[open] summary {
    white-space: normal;
  }
  .lacks p {
    margin: 0.25rem 0 0;
  }
  .lacks ul {
    margin: 0.25rem 0 0;
    padding-left: 1.25rem;
  }
  .lacks li + li {
    margin-top: 0.15rem;
  }
  .taken {
    margin-left: 0.5rem;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  progress {
    display: block;
    width: 100%;
    height: 0.375rem;
    margin-top: var(--space-2);
    accent-color: var(--accent);
  }
  .remedy,
  .code {
    color: var(--muted);
  }
  .code {
    font-family: var(--font-data);
  }
</style>
