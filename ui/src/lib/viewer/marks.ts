// What happened, as the viewer bridge tells it (`docs/reference/viewer-stream.md`,
// What happened): events and each episode's start and end, made into what
// the timeline draws.

import { clock, duration } from "../format";

/** An event as capture kept it. */
export interface StreamEvent {
  kind: string;
  at_utc_ns: number;
  until_utc_ns?: number;
  phase?: "moment" | "begin" | "end" | "stretch";
  id?: string;
  source: string;
  text?: string;
}

/** One thing the bridge says happened. */
export type Mark =
  | { noted: { event: StreamEvent; episode: string | null } }
  | { episode_started: { episode: string; at_utc_ns: number } }
  | { episode_ended: { episode: string; at_utc_ns: number; finalised: boolean } };

/** Something the timeline draws: a moment, or a stretch, open until its end comes. */
export interface Item {
  /** The event's kind, or `episode` for an episode's start or end. */
  kind: string;
  startMs: number;
  /** When it ended; equal to `startMs` for a moment, `null` while a stretch is open. */
  endMs: number | null;
  /** In words, for its tooltip. */
  label: string;
}

/** What a mark's tip says: what it is, when, and how long it lasted. */
export interface MarkTip {
  what: string;
  when: string;
  /** How long a stretch lasted, or that it goes on; none for a moment. */
  lasting: string | null;
}

/** What `item`'s tip says. */
export function markTip(item: Item): MarkTip {
  const lasting = (): string | null => {
    if (item.endMs === null) {
      return "still going";
    }
    const seconds = (item.endMs - item.startMs) / 1000;
    if (seconds <= 0) {
      return null;
    }
    if (seconds < 1) {
      return `for ${Math.round(seconds * 1000)} ms`;
    }
    return seconds < 60 ? `for ${seconds.toFixed(1)} s` : `for ${duration(seconds)}`;
  };
  return { what: item.label, when: clock(item.startMs), lasting: lasting() };
}

/** The most items kept: the bridge keeps no more. */
const MAX_ITEMS = 10_000;

const ms = (ns: number) => ns / 1e6;

/** The marks heard so far, as items, oldest first. */
export class MarkLog {
  /** Changed in place: copy it to show it. */
  readonly items: Item[] = [];
  /** Stretches begun and not ended yet, by `kind/id`. */
  private open = new Map<string, Item>();

  /** Forgets everything: the bridge tells it all again when it reconnects. */
  clear(): void {
    this.items.length = 0;
    this.open.clear();
  }

  add(marks: Mark[]): void {
    for (const mark of marks) {
      if ("noted" in mark) {
        this.note(mark.noted.event);
      } else if ("episode_started" in mark) {
        const at = ms(mark.episode_started.at_utc_ns);
        this.push({ kind: "episode", startMs: at, endMs: at, label: "Episode started" });
      } else if ("episode_ended" in mark) {
        const at = ms(mark.episode_ended.at_utc_ns);
        const label = mark.episode_ended.finalised ? "Episode ended" : "Episode aborted";
        this.push({ kind: "episode", startMs: at, endMs: at, label });
      }
    }
    if (this.items.length > MAX_ITEMS) {
      this.items.splice(0, this.items.length - MAX_ITEMS);
    }
  }

  private note(event: StreamEvent): void {
    const at = ms(event.at_utc_ns);
    const said = event.text ? ` · ${event.text}` : "";
    const key = `${event.kind}/${event.id ?? ""}`;
    switch (event.phase ?? "moment") {
      case "begin": {
        const item = { kind: event.kind, startMs: at, endMs: null, label: `${event.kind} (${event.source})${said}` };
        this.open.set(key, item);
        this.push(item);
        break;
      }
      case "end": {
        const begun = this.open.get(key);
        if (begun) {
          begun.endMs = Math.max(at, begun.startMs);
          this.open.delete(key);
        } else {
          // Its beginning came before what the bridge keeps.
          this.push({ kind: event.kind, startMs: at, endMs: at, label: `end of ${event.kind}${said}` });
        }
        break;
      }
      case "stretch":
        this.push({
          kind: event.kind,
          startMs: at,
          endMs: ms(event.until_utc_ns ?? event.at_utc_ns),
          label: `${event.kind} (${event.source})${said}`,
        });
        break;
      default:
        this.push({ kind: event.kind, startMs: at, endMs: at, label: `${event.kind} (${event.source})${said}` });
    }
  }

  private push(item: Item): void {
    this.items.push(item);
  }
}
