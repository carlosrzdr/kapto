<script lang="ts">
  import type { LiveViewCounts, StreamCounts } from "./api-types";
  import { count } from "./format";

  interface Props {
    streams: StreamCounts[];
    /** Messages received per second, by stream. */
    rates: Record<string, number>;
    /** Whether an episode is being recorded, so written counts mean something. */
    recording: boolean;
    /** Messages per second each stream is declared to send, where declared. */
    declared?: Record<string, number | null | undefined>;
    /**
     * The streams whose messages are kept for late subscribers: they may
     * send them once, so their silence isn't a problem.
     */
    kept?: Record<string, boolean>;
    /** Whether capture answered, so its streams are known. */
    known?: boolean;
  }

  let { streams, rates, recording, declared = {}, kept = {}, known = true }: Props = $props();

  /** Below this share of its declared rate, a stream is shown as a problem. */
  const SLOW = 0.9;

  /** How a stream is arriving; `null` until its rate is known. */
  function health(stream: StreamCounts): "silent" | "slow" | "ok" | "kept" | null {
    const rate = rates[stream.stream];
    if (rate === undefined) {
      return null;
    }
    if (rate === 0) {
      return kept[stream.stream] ? "kept" : "silent";
    }
    const expected = declared[stream.stream];
    return expected && rate < expected * SLOW ? "slow" : "ok";
  }

  /** What became of a camera's frames on their way to the live view, in words; only what happened. */
  function toLiveView(camera: LiveViewCounts): string {
    const parts = [`${count(camera.sent)} of ${count(camera.offered)} frames sent`];
    if (camera.superseded > 0) {
      parts.push(`${count(camera.superseded)} replaced by newer ones while the viewer was slow`);
    }
    if (camera.skipped > 0) {
      parts.push(`${count(camera.skipped)} skipped`);
    }
    if (camera.not_sent > 0) {
      parts.push(`${count(camera.not_sent)} while no viewer listened`);
    }
    return `Live view: ${parts.join(" · ")}`;
  }

  /** A stream's name in the pieces a line may break between: after each `_`. */
  const words = (name: string): string[] => name.split(/(?<=_)/);

  /** What a stream's dot says, for a screen reader and on hover. */
  function said(state: "silent" | "slow" | "ok" | "kept", expected: number | null | undefined): string {
    if (state === "silent") {
      return "nothing arriving";
    }
    if (state === "kept") {
      return "sends its messages once; every episode starts with the newest";
    }
    if (state === "slow") {
      return "slower than declared";
    }
    return expected ? "arriving at its declared rate" : "arriving";
  }
</script>

<!-- A loss count: quiet while there is none, loud as soon as there is. -->
{#snippet loss(n: number)}
  {#if n > 0}
    <td class="problem loss">{count(n)}</td>
  {:else}
    <td class="none-lost"><span aria-hidden="true">–</span><span class="visually-hidden">none</span></td>
  {/if}
{/snippet}

<table>
  <thead>
    <tr>
      <th scope="col">Stream</th>
      <th scope="col">Rate</th>
      <th scope="col">Received</th>
      <th scope="col">Written</th>
      <th scope="col">Dropped</th>
      <th scope="col">Lost</th>
    </tr>
  </thead>
  <tbody>
    {#each streams as stream (stream.stream)}
      {@const state = health(stream)}
      <tr>
        <th scope="row">
          <span class="named"
            ><span class="name"
              >{#each words(stream.stream) as word, index (index)}{#if index > 0}<wbr />{/if}{word}{/each}</span
            >{#if state}<span
                class="dot {state}"
                role="img"
                aria-label={said(state, declared[stream.stream])}
                title={said(state, declared[stream.stream])}
              ></span>{/if}</span
          >
        </th>
        <td class:problem={state === "silent" || state === "slow"} title={declared[stream.stream] ? `declared ${count(declared[stream.stream] ?? 0)}/s` : undefined}>
          {#if state === null}–{:else if state === "silent"}silent{:else if state === "kept"}kept{:else}{`${count(rates[stream.stream] ?? 0)}/s`}{/if}{state === "slow" ? ` of ${count(declared[stream.stream] ?? 0)}` : ""}
        </td>
        <td>{count(stream.received)}</td>
        <td>{recording ? count(stream.written) : "–"}</td>
        {@render loss(stream.dropped_by_us)}
        {@render loss(stream.lost_upstream)}
      </tr>
      {#if stream.live_view}
        <tr class="camera">
          <td colspan="6">{toLiveView(stream.live_view)}</td>
        </tr>
      {/if}
    {:else}
      <tr>
        <td colspan="6" class="none">
          {known ? "No streams declared." : "Not known while Kapto can't reach the capture process."}
        </td>
      </tr>
    {/each}
  </tbody>
</table>

<style>
  .none {
    color: var(--muted);
    text-align: left;
  }
  .camera td {
    padding-top: 0;
    color: var(--muted);
    font-size: var(--text-xs);
    text-align: left;
  }
  /* The camera's line belongs to the row above it. */
  tr:has(+ .camera) > * {
    border-bottom: none;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-variant-numeric: tabular-nums;
  }
  /* Narrow sides: six columns share a side panel, and counts grow long. */
  th,
  td {
    padding: 0.35rem 0.25rem;
    text-align: right;
    border-bottom: 1px solid var(--line);
  }
  th[scope="row"],
  thead th:first-child {
    text-align: left;
    font-weight: 500;
  }
  thead th {
    color: var(--muted);
    font-weight: 500;
    font-size: var(--text-xs);
  }
  .problem {
    color: var(--warning);
    font-weight: 600;
  }
  /* Loss is never silent: a count above zero in the danger colour. */
  .problem.loss {
    color: var(--danger);
  }
  .none-lost {
    color: var(--muted);
  }
  /* The dot stays beside its stream's name; a long name wraps before it. */
  .named {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    max-width: 100%;
  }
  /* It breaks between its pieces, words kept whole: as narrow as its longest
     piece, so the table fits the panel without splitting a word. */
  .name {
    min-width: 0;
    overflow-wrap: break-word;
  }
  .dot {
    flex: none;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
  }
  .dot.ok {
    background: var(--ok);
  }
  /* Neither a good state nor a problem: a hollow ring in the muted colour. */
  .dot.kept {
    border: 2px solid var(--muted);
    width: 0.35rem;
    height: 0.35rem;
  }
  /* A ring, not a filled dot: told apart by shape as well as colour. */
  .dot.slow,
  .dot.silent {
    border: 2px solid var(--warning);
    width: 0.35rem;
    height: 0.35rem;
  }
</style>
