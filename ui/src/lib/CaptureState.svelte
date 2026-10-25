<script lang="ts">
  import type { Status } from "./api-types";
  import { duration } from "./format";

  interface Props {
    /** Capture's latest status; `null` until the first comes. */
    status: Status | null;
    /** Whether the last try to reach capture failed. */
    unreachable: boolean;
    /**
     * The replay asked for, while the viewer isn't back to live: when its
     * episode was recorded, and whether the viewer shows it yet.
     */
    replay?: { label: string; shown: boolean } | null;
    /** Leaves the replay for the live robot. */
    onlive?: () => void;
  }

  let { status, unreachable, replay = null, onlive = () => {} }: Props = $props();
  const recording = $derived(status?.capture.episode != null);
  const closing = $derived(status?.capture.closing ?? null);
  /** What the viewer shows is a replay: said instead of "Live", which it isn't. */
  const replaying = $derived(replay?.shown === true);
</script>

<span class="state" class:live={recording}>
  {#if status === null}
    <!-- Unreachable, the health chip beside Record says so: said once. -->
    {unreachable ? "" : "Connecting…"}
  {:else if recording}
    Recording · {duration(status.capture.seconds)}
  {:else if !replaying}
    Live, not recording
  {/if}
  {#if replay && replaying}
    <span class="replaying">{recording ? `· replaying ${replay.label}` : `Replaying · ${replay.label}`}</span>
  {/if}
  {#if closing}
    <span
      class="closing"
      title={`Episode ${closing} is being finalised: written to disk and checked. Its stop is answered, and it is listed, once it is.`}
      >· finalising the last episode</span
    >
  {/if}
  {#if replay}
    <!-- The one way back, here whatever the page shows while the viewer isn't live. -->
    <button type="button" class="back" title="Back to the live robot (Esc)" onclick={onlive}>Back to live</button>
  {/if}
</span>

<style>
  .state {
    display: inline-flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0 var(--space-2);
    margin-left: auto;
    color: var(--muted);
  }
  .state.live {
    color: var(--danger);
    font-weight: 600;
  }
  /* Not live, and not a problem: the info colour. */
  .replaying {
    color: var(--info);
    font-weight: 600;
  }
  .closing {
    color: var(--muted);
    font-weight: 400;
  }
  .back {
    height: var(--control-height);
    padding: 0 var(--space-3);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    font-weight: 400;
    cursor: pointer;
  }
</style>
