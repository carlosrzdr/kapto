<script lang="ts">
  import type { ApiFailure } from "./api";
  import type { DestinationTested, UploadsOverview, UploadStatus } from "./api-types";
  import { bytes, count, duration, plain, sentence, time } from "./format";
  import { type Measured, timeLeft, uploadKey } from "./uploads";

  interface Props {
    overview: UploadsOverview | null;
    /** How fast each upload goes, by `uploadKey`. */
    measured: Map<string, Measured>;
    /** Whether `_SUCCESS` is written last in each episode, as `upload.marker` says. */
    marker: boolean;
    /** Tries the episode's failed uploads again now. */
    onretry: (episode: string) => Promise<void>;
    /** Writes, reads back and removes a probe at the destination named. */
    ontest: (name: string) => Promise<DestinationTested>;
  }

  let { overview, measured, marker, onretry, ontest }: Props = $props();

  /** How each destination's last test went, by name; `"testing"` while it runs. */
  let tests = $state<Record<string, "testing" | DestinationTested | ApiFailure>>({});
  /** The episodes being retried, and why a retry was refused, by episode. */
  let retrying = $state<Record<string, true | ApiFailure>>({});

  const going = $derived(overview?.uploads.filter((upload) => upload.state === "uploading") ?? []);
  const failed = $derived(overview?.uploads.filter((upload) => upload.state === "failed") ?? []);

  async function test(name: string) {
    tests[name] = "testing";
    try {
      tests[name] = await ontest(name);
    } catch (error) {
      tests[name] = error as ApiFailure;
    }
  }

  async function retry(episode: string) {
    retrying[episode] = true;
    try {
      await onretry(episode);
      delete retrying[episode];
    } catch (error) {
      retrying[episode] = error as ApiFailure;
    }
  }

  function progress(upload: UploadStatus): string {
    const rate = measured.get(uploadKey(upload))?.rate ?? null;
    const left = timeLeft(upload, rate);
    const sent = `${bytes(upload.bytes_sent)} of ${bytes(upload.bytes_total)}`;
    return rate === null ? sent : `${sent} · ${bytes(rate)}/s${left === null ? "" : ` · ${duration(left)} left`}`;
  }
</script>

{#if overview === null}
  <p class="unknown">Not known yet.</p>
{:else if overview.destinations.length === 0}
  <p class="unknown">
    No destinations: episodes stay on this machine. Add an <code>[[upload.destination]]</code> to
    <code>kapto.toml</code> to upload them.
  </p>
{:else}
  {#if !marker}
    <p class="warning" role="note">
      <code>upload.marker</code> is off: no <code>_SUCCESS</code> is written, so whoever reads a destination can't tell
      a complete episode from one still going up.
    </p>
  {/if}
  <p class="waiting" class:quiet={overview.waiting === 0}>
    {overview.waiting === 0
      ? "No episode waiting"
      : `${count(overview.waiting)} ${overview.waiting === 1 ? "episode" : "episodes"} waiting`}
  </p>
  <ul class="destinations">
    {#each overview.destinations as destination (destination.name)}
      {@const tested = tests[destination.name]}
      <li aria-label={destination.name}>
        <div class="head">
          <strong>{destination.name}</strong>
          <button type="button" disabled={tested === "testing"} aria-busy={tested === "testing"} onclick={() => void test(destination.name)}>
            {tested === "testing" ? "Testing…" : "Test"}
          </button>
        </div>
        <p class="url data">{destination.url}</p>
        <p class="figures">
          <!-- Only what is happening: no "0 uploading", no "0 failed". -->
          {count(destination.uploaded)} uploaded{#if destination.uploading > 0}{` · ${count(destination.uploading)} uploading`}{/if}{#if destination.failed > 0}
            {" · "}<span class="problem failed">{count(destination.failed)} failed</span>{/if}
          {#if destination.throttled > 0}<span title="Requests it asked Kapto to slow down for"
              >{` · ${count(destination.throttled)} throttled`}</span
            >{/if}
        </p>
        {#if tested === undefined || tested === "testing"}
          <!-- Nothing to say until it answers. -->
        {:else if "took_ms" in tested}
          <p class="ok" role="status">
            {tested.removed
              ? `Works: a probe written, read back and removed in ${tested.took_ms} ms`
              : `Writes and reads, in ${tested.took_ms} ms, but can't remove: the probe is still at ${tested.key}`}
          </p>
        {:else}
          <p class="problem" role="alert" title={tested.message}>{sentence(plain(tested.message))} <span class="remedy">{tested.remedy}</span></p>
          <p class="code">{tested.code}</p>
        {/if}
      </li>
    {/each}
  </ul>

  {#if going.length > 0}
    <h3>Uploading</h3>
    <ul class="uploads">
      {#each going as upload (uploadKey(upload))}
        <li aria-label="{upload.episode} to {upload.destination}">
          <p><span class="data">{upload.episode}</span> → {upload.destination}</p>
          <progress max={Math.max(upload.bytes_total, 1)} value={upload.bytes_sent}></progress>
          <p class="figures">{progress(upload)}</p>
        </li>
      {/each}
    </ul>
  {/if}

  {#if failed.length > 0}
    <h3>Failed</h3>
    <ul class="uploads">
      {#each failed as upload (uploadKey(upload))}
        {@const retried = retrying[upload.episode]}
        <li aria-label="{upload.episode} to {upload.destination}">
          <p><span class="data">{upload.episode}</span> → {upload.destination}</p>
          <p class="problem" title={upload.failure_message}>
            {plain(upload.failure_message ?? "Failed")}{#if upload.failed_utc_ns}<span class="when"
                >{` · ${time(upload.failed_utc_ns)}`}</span
              >{/if}
          </p>
          {#if upload.failure_code}<p class="code">{upload.failure_code}</p>{/if}
          <!-- Retried by hand, it is made due at once: its time of failure zero. -->
          {#if upload.failed_utc_ns === 0}
            <p class="remedy">Queued to be tried again.</p>
          {:else}
            <p class="remedy">It is tried again by itself later, or now:</p>
            <button type="button" disabled={retried === true} onclick={() => void retry(upload.episode)}>
              {retried === true ? "Retrying…" : "Retry now"}
            </button>
          {/if}
          {#if retried !== undefined && retried !== true}
            <p class="problem" role="alert" title={retried.message}>{sentence(plain(retried.message))} <span class="remedy">{retried.remedy}</span></p>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
{/if}

<style>
  p {
    margin: 0;
  }
  ul {
    display: grid;
    gap: var(--space-3);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .destinations li,
  .uploads li {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--line);
    border-radius: var(--radius);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h3 {
    margin: var(--space-4) 0 var(--space-2);
    font-size: var(--text-md);
  }
  .waiting {
    margin-bottom: var(--space-3);
  }
  .quiet {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .figures {
    font-variant-numeric: tabular-nums;
  }
  .failed {
    font-weight: 600;
  }
  .url,
  .code {
    color: var(--muted);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  .code {
    font-family: var(--font-data);
  }
  .data {
    overflow-wrap: anywhere;
  }
  .when,
  .remedy,
  .unknown {
    color: var(--muted);
    font-weight: normal;
  }
  .remedy {
    font-size: var(--text-sm);
  }
  .ok {
    color: var(--ok);
    font-size: var(--text-sm);
  }
  .problem {
    color: var(--warning);
  }
  .warning {
    margin-bottom: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    background: var(--warning-background);
  }
  progress {
    width: 100%;
    height: 0.5rem;
    accent-color: var(--accent);
  }
  button {
    justify-self: start;
    padding: 0.2rem 0.8rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
  }
</style>
