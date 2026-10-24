// An episode's events as its details list them: each window, a stretch or a
// stream silent until it came back, one row from its start to its end.

import type { SentEvent } from "../api-types";
import { windowEnd } from "./marks";

/** One row of an episode's events. */
export interface EventRow {
  /** What happened: the event's kind, or the window's. */
  kind: string;
  /** The stream it is about, for Kapto's own events. */
  stream: string | null;
  source: string;
  text: string | null;
  fromNs: number;
  /** When the window ended; `null` for a moment. */
  untilNs: number | null;
  /** A window nothing ended before the recording did. */
  open: boolean;
  /** An end whose beginning isn't in the recording. */
  endOnly: boolean;
}

/** `events`, as recorded, made into rows in order of their start. */
export function eventRows(events: SentEvent[], endedNs: number): EventRow[] {
  const rows: EventRow[] = [];
  const open = new Map<string, EventRow>();
  for (const event of events) {
    const stream = (event.attributes as Record<string, unknown> | null)?.["stream"];
    const base = {
      kind: event.kind,
      stream: typeof stream === "string" ? stream : null,
      source: event.source,
      text: event.text ?? null,
      fromNs: event.at_utc_ns,
      untilNs: null as number | null,
      open: false,
      endOnly: false,
    };
    const window = windowEnd(event);
    if (window?.end === "begin") {
      const row = { ...base, kind: window.kind, untilNs: endedNs, open: true };
      open.set(window.key, row);
      rows.push(row);
    } else if (window?.end === "end") {
      const begun = open.get(window.key);
      if (begun) {
        begun.untilNs = Math.max(event.at_utc_ns, begun.fromNs);
        begun.open = false;
        begun.text = [begun.text, event.text].filter(Boolean).join(" · ") || null;
        open.delete(window.key);
      } else {
        rows.push({ ...base, kind: window.kind, endOnly: true });
      }
    } else if (event.phase === "stretch") {
      rows.push({ ...base, untilNs: event.until_utc_ns ?? event.at_utc_ns });
    } else {
      rows.push(base);
    }
  }
  return rows.sort((a, b) => a.fromNs - b.fromNs);
}
