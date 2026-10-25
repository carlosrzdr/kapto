<script lang="ts">
  import type { EpisodeUploads, UploadDestination } from "./api-types";
  import { breakable, bytes, plain, time } from "./format";
  import Icon from "./Icon.svelte";

  interface Props {
    uploads: EpisodeUploads;
    /** The destinations configured, for where each is. */
    destinations: UploadDestination[];
  }

  let { uploads, destinations }: Props = $props();

  /** Why it was deleted here, in words. */
  const reasons: Record<string, string> = {
    after_upload: "once uploaded",
    disk_low: "the disk ran low",
  };

  /**
   * Where an object is, to fetch it with the store's own tools: its key is
   * from the bucket's root, prefix and all. Without its destination's URL,
   * e.g. one taken out of the configuration since, only its key is known.
   */
  function uri(destination: string, key: string): string {
    const url = destinations.find((each) => each.name === destination)?.url;
    return url === undefined ? key : `${url.replace(/\/+$/, "")}/${key}`;
  }

  /** Copying needs a secure page (localhost or HTTPS); elsewhere the URI is selected by hand. */
  const canCopy = typeof navigator !== "undefined" && navigator.clipboard !== undefined;
  /** The URI just copied, to say so beside it. */
  let copied = $state<string | null>(null);

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = text;
    } catch {
      // Refused by the browser: the URI is still there to select.
    }
  }
</script>

<h3>Uploads</h3>
{#if uploads.deletion}
  <p class="remote">
    Stored only remotely: deleted here {time(uploads.deletion.deleted_utc_ns)}, {reasons[uploads.deletion.reason] ??
      uploads.deletion.reason}, with verified copies at {uploads.deletion.destinations.join(", ")}.
  </p>
{/if}
{#if uploads.uploads.length === 0}
  <p class="pending">Not uploaded anywhere.</p>
{/if}
{#each uploads.uploads as upload (upload.destination)}
  {@const folder = uri(upload.destination, upload.key)}
  <section aria-label="Upload to {upload.destination}">
    <p>
      <strong>{upload.destination}</strong>
      {#if upload.state === "uploaded"}
        · uploaded{#if upload.uploaded_utc_ns}{` ${time(upload.uploaded_utc_ns)}`}{/if}
      {:else if upload.state === "uploading"}
        · uploading, {bytes(upload.bytes_sent)} of {bytes(upload.bytes_total)}
      {:else}
        · <span class="danger" title={upload.failure_message}
          ><Icon name="attention" />{`failed: ${plain(upload.failure_message ?? "")}`}</span
        >{#if upload.failure_code}<span class="code">{` ${upload.failure_code}`}</span>{/if}
      {/if}
    </p>
    <div class="folder">
      <p class="key data">
        {#each breakable(folder) as piece, index (index)}{piece}<wbr />{/each}
      </p>
      {#if canCopy}
        <button type="button" title="Copy {folder}" onclick={() => void copy(folder)}>
          {copied === folder ? "Copied" : "Copy folder URI"}
        </button>
      {/if}
    </div>
    {#if upload.objects.length > 0}
      <!-- Each object and its URI on demand: the folder's is the one usually wanted. -->
      <details class="objects">
        <summary>
          <Icon name="chevron" />
          {upload.objects.length}
          {upload.objects.length === 1 ? "file" : "files"} · {bytes(
            upload.objects.reduce((sum, object) => sum + object.bytes, 0),
          )}
        </summary>
        <table aria-label="Objects at {upload.destination}">
          <tbody>
            {#each upload.objects as object (object.path)}
              {@const where = uri(upload.destination, object.key)}
              <tr>
                <th scope="row" title="SHA-256 {object.sha256}">{object.path}</th>
                <td>{bytes(object.bytes)}</td>
                <td class="copy">
                  {#if canCopy}
                    <button type="button" title="Copy {where}" onclick={() => void copy(where)}>
                      {copied === where ? "Copied" : "Copy URI"}
                    </button>
                  {:else}
                    <span class="data uri">{where}</span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    {/if}
  </section>
{/each}

<style>
  h3 {
    margin: var(--space-4) 0 var(--space-2);
    font-size: var(--text-md);
  }
  p {
    margin: 0;
  }
  section {
    display: grid;
    gap: var(--space-1);
    margin-bottom: var(--space-3);
  }
  .remote {
    margin-bottom: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    background: var(--warning-background);
  }
  .folder {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-2);
  }
  .folder button {
    flex: none;
  }
  .key,
  .code,
  .pending {
    color: var(--muted);
  }
  .key,
  .uri {
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
    user-select: all;
  }
  .code {
    font-family: var(--font-data);
    font-size: var(--text-xs);
  }
  /* A failed upload leaves the episode short of where it should be: danger, with its word. */
  .danger {
    color: var(--danger);
  }
  .danger :global(.icon) {
    margin-right: 0.25em;
    vertical-align: -0.125em;
  }
  /* Folded the System tab's way: a chevron, then how much it holds. */
  .objects summary {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    list-style: none;
    cursor: pointer;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .objects summary::-webkit-details-marker {
    display: none;
  }
  .objects summary :global(.icon) {
    transition: transform 150ms ease-out;
  }
  .objects[open] > summary :global(.icon) {
    transform: rotate(90deg);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  th {
    font-weight: normal;
    text-align: left;
    overflow-wrap: anywhere;
  }
  td {
    padding: 0.1rem 0 0.1rem var(--space-3);
    white-space: nowrap;
    text-align: right;
  }
  td.copy {
    white-space: normal;
  }
  button {
    padding: 0 var(--space-2);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-xs);
  }
</style>
