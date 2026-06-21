<script lang="ts">
  import { onMount } from "svelte";

  import { ApiFailure, getEventKinds, sendEvent } from "./api";
  import type { EventSent, SendEvent, VocabularyKind } from "./api-types";
  import { sentence } from "./format";

  interface Props {
    /** The kinds that may be sent, once known; read from the API if not given. */
    kinds?: VocabularyKind[] | null;
    /** Told the kinds once they are read, for the timeline's colours. */
    onkinds?: (kinds: VocabularyKind[]) => void;
    /** Sends an event; through the API if not given. */
    send?: (event: SendEvent) => Promise<EventSent>;
  }

  let { kinds = null, onkinds = () => {}, send = sendEvent }: Props = $props();

  let known = $state<VocabularyKind[]>([]);
  let chosen = $state("");
  /** The stretches this page began and hasn't ended, by kind: their IDs. */
  let begun = $state<Record<string, string>>({});
  /** What the last send did. */
  let said = $state("");
  let failed = $state(false);
  let sending = $state(false);
  let count = 0;

  const kind = $derived(known.find((each) => each.kind === chosen));
  const action = $derived(
    kind === undefined ? "Mark" : !kind.stretch ? `Mark ${kind.name}` : begun[kind.kind] ? `End ${kind.name}` : `Begin ${kind.name}`,
  );

  /** Sends the kind `which`: a moment, or a stretch's beginning, or the end of the one this page began. */
  async function mark(which: VocabularyKind) {
    if (sending) {
      return;
    }
    sending = true;
    const open = begun[which.kind];
    const event = !which.stretch
      ? { kind: which.kind, source: "page" }
      : open
        ? { kind: which.kind, phase: "end" as const, id: open, source: "page" }
        : { kind: which.kind, phase: "begin" as const, id: `page-${Date.now().toString(36)}-${(count += 1)}`, source: "page" };
    try {
      const sent = await send(event);
      if (which.stretch) {
        const next = { ...begun };
        if (open) {
          delete next[which.kind];
        } else {
          next[which.kind] = event.id ?? "";
        }
        begun = next;
      }
      const what = !which.stretch ? which.name : open ? `end of ${which.name}` : `beginning of ${which.name}`;
      said = sent.episode ? `Marked the ${what}` : `Noted the ${what}; not recording, so not kept`;
      failed = false;
    } catch (error) {
      said = error instanceof ApiFailure ? `${sentence(error.message)} ${error.remedy}` : String(error);
      failed = true;
    } finally {
      sending = false;
    }
  }

  /** Keys 1 to 9 send the first nine kinds, unless typing somewhere. */
  function key(event: KeyboardEvent) {
    const typing = event.target instanceof Element && event.target.closest("input, select, textarea") !== null;
    if (event.altKey || event.ctrlKey || event.metaKey || typing) {
      return;
    }
    const index = Number(event.key) - 1;
    const which = Number.isInteger(index) && index >= 0 && index < 9 ? known[index] : undefined;
    if (which) {
      event.preventDefault();
      chosen = which.kind;
      void mark(which);
    }
  }

  function take(read: VocabularyKind[]) {
    known = read;
    if (!read.some((each) => each.kind === chosen)) {
      chosen = read[0]?.kind ?? "";
    }
    onkinds(read);
  }

  onMount(() => {
    if (kinds) {
      take(kinds);
    } else {
      getEventKinds()
        .then((read) => take(read.kinds))
        .catch(() => {
          said = "The kinds of events couldn't be read; reload the page to try again.";
          failed = true;
        });
    }
  });
</script>

<svelte:window onkeydown={key} />

<div class="marks" role="group" aria-label="Mark what happened">
  <select aria-label="Kind of event" bind:value={chosen} disabled={known.length === 0}>
    {#each known as each, index (each.kind)}
      <option value={each.kind}>{index < 9 ? `${index + 1} · ` : ""}{each.name}{begun[each.kind] ? " (begun)" : ""}</option>
    {/each}
  </select>
  <button type="button" disabled={kind === undefined || sending} onclick={() => kind && mark(kind)}>{action}</button>
  {#if said}
    <span class="said" class:failed role="status">{said}</span>
  {/if}
</div>

<style>
  .marks {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.4rem 0.2rem 0;
    font-size: var(--text-sm);
  }
  select {
    padding: 0.15rem 0.3rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
  }
  button {
    min-width: 6.5rem;
    padding: 0.2rem 0.7rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .said {
    color: var(--muted);
  }
  .said.failed {
    color: var(--warning);
  }
</style>
