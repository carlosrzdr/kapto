<script lang="ts">
  import type { Status } from "./api-types";
  import { duration } from "./format";

  interface Props {
    /** Capture's latest status; `null` until the first comes. */
    status: Status | null;
    /** Whether the last try to reach capture failed. */
    unreachable: boolean;
  }

  let { status, unreachable }: Props = $props();
  const recording = $derived(status?.capture.episode != null);
  const closing = $derived(status?.capture.closing ?? null);
</script>

<span class="state" class:live={recording}>
  {#if status === null}
    <!-- Unreachable, the health chip beside Record says so: said once. -->
    {unreachable ? "" : "Connecting…"}
  {:else if recording}
    Recording · {duration(status.capture.seconds)}
  {:else}
    Live, not recording
  {/if}
  {#if closing}
    <span
      class="closing"
      title={`Episode ${closing} is being finalised: written to disk and checked. Its stop is answered, and it is listed, once it is.`}
      >· finalising the last episode</span
    >
  {/if}
</span>

<style>
  .state {
    margin-left: auto;
    color: var(--muted);
  }
  .state.live {
    color: var(--danger);
    font-weight: 600;
  }
  .closing {
    color: var(--muted);
    font-weight: 400;
  }
</style>
