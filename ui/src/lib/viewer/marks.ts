// What happened, as the viewer bridge tells it (`docs/reference/viewer-stream.md`,
// What happened): events and each episode's start and end, made into what
// the timeline draws.

import { clock, lasting as lasted } from "../format";
import { SERIES } from "../theme";

/** An event as capture kept it. */
export interface StreamEvent {
  kind: string;
  at_utc_ns: number;
  until_utc_ns?: number;
  phase?: "moment" | "begin" | "end" | "stretch";
  id?: string;
  source: string;
  text?: string;
  attributes?: Record<string, string>;
}

/**
 * Which end of a window `event` is, if it is one: the beginning or end of
 * a stretch, by kind and ID, or a stream of Kapto's falling silent and
 * coming back, by stream. Its `key` is the same at both ends; `kind` is
 * what the whole window is called.
 */
export function windowEnd(event: {
  kind: string;
  phase?: string;
  id?: string | null;
  attributes?: unknown;
}): { end: "begin" | "end"; key: string; kind: string } | null {
  const stream = (event.attributes as Record<string, unknown> | undefined)?.["stream"];
  if (typeof stream === "string") {
    if (event.kind === "kapto.stream_silent") {
      return { end: "begin", key: `kapto.stream_silent/${stream}`, kind: event.kind };
    }
    if (event.kind === "kapto.stream_back") {
      return { end: "end", key: `kapto.stream_silent/${stream}`, kind: "kapto.stream_silent" };
    }
  }
  if (event.phase === "begin" || event.phase === "end") {
    return { end: event.phase, key: `${event.kind}/${event.id ?? ""}`, kind: event.kind };
  }
  return null;
}

/** The stream an event of Kapto's is about, ` · stream`, or nothing. */
function about(event: StreamEvent): string {
  const stream = event.attributes?.["stream"];
  return stream ? ` · ${stream}` : "";
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
    return seconds <= 0 ? null : `for ${lasted(seconds)}`;
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
  /** When the episode being recorded started, if one is. */
  recordingSinceMs: number | null = null;

  /** Forgets everything: the bridge tells it all again when it reconnects. */
  clear(): void {
    this.items.length = 0;
    this.open.clear();
    this.recordingSinceMs = null;
  }

  add(marks: Mark[]): void {
    for (const mark of marks) {
      if ("noted" in mark) {
        this.note(mark.noted.event);
      } else if ("episode_started" in mark) {
        const at = ms(mark.episode_started.at_utc_ns);
        this.push({ kind: "episode", startMs: at, endMs: at, label: "Episode started" });
        this.recordingSinceMs = at;
      } else if ("episode_ended" in mark) {
        const at = ms(mark.episode_ended.at_utc_ns);
        const label = mark.episode_ended.finalised ? "Episode ended" : "Episode aborted";
        this.push({ kind: "episode", startMs: at, endMs: at, label });
        this.recordingSinceMs = null;
      }
    }
    if (this.items.length > MAX_ITEMS) {
      this.items.splice(0, this.items.length - MAX_ITEMS);
    }
  }

  private note(event: StreamEvent): void {
    const at = ms(event.at_utc_ns);
    const said = `${about(event)}${event.text ? ` · ${event.text}` : ""}`;
    const window = windowEnd(event);
    if (window?.end === "begin") {
      const item = { kind: window.kind, startMs: at, endMs: null, label: `${window.kind} (${event.source})${said}` };
      this.open.set(window.key, item);
      this.push(item);
      return;
    }
    if (window?.end === "end") {
      const begun = this.open.get(window.key);
      if (begun) {
        begun.endMs = Math.max(at, begun.startMs);
        this.open.delete(window.key);
      } else {
        // Its beginning came before what the bridge keeps.
        this.push({ kind: event.kind, startMs: at, endMs: at, label: `end of ${window.kind}${said}` });
      }
      return;
    }
    switch (event.phase ?? "moment") {
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

/** The lane's width in its own units, drawn stretched along the timeline's track. */
export const LANE_WIDTH = 1000;
/** The narrowest a mark is drawn, so a moment can be seen. */
const NARROWEST = 4;

/**
 * `kind`'s colour on the lane: one of the series colours, never a state's,
 * as a mark says what happened rather than whether it is good or bad.
 * Kinds of the page's and Kapto's own (episodes, checks, `kapto.*`) take
 * one by their name; others the colour configured for them, if any.
 */
export function markColor(kind: string, colors: Record<string, string>): string {
  const own = kind === "episode" || kind === "check" || kind.startsWith("kapto.");
  const given = own ? undefined : colors[kind];
  if (given) {
    return given;
  }
  // By the kind's name, so each keeps its own from one page to the next.
  let hash = 0;
  for (const character of kind) {
    hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
  }
  return SERIES[hash % SERIES.length]!;
}

/** A mark placed on the lane, in its units. */
export interface LaneMark {
  item: Item;
  left: number;
  width: number;
  /** Where the part that takes the pointer starts, and how wide it is: never narrower than `hitWidth`. */
  hitLeft: number;
  hit: number;
  color: string;
}

/**
 * The marks of `items` within `fromMs`..`toMs`, placed on a lane
 * [`LANE_WIDTH`] wide: where the timeline's thumb stands at a moment, its
 * mark starts. A stretch still open is drawn up to `toMs`; each is at
 * least as wide as can be seen, and takes the pointer at least `hitWidth`
 * wide, centred on it, both kept on the lane.
 */
export function laneMarks(
  items: Item[],
  fromMs: number,
  toMs: number,
  colors: Record<string, string>,
  hitWidth: number,
): LaneMark[] {
  const x = (ms: number) => ((ms - fromMs) / Math.max(toMs - fromMs, 1)) * LANE_WIDTH;
  return items
    .filter((item) => (item.endMs ?? toMs) >= fromMs && item.startMs <= toMs)
    .map((item) => {
      const left = Math.max(0, x(item.startMs));
      const right = Math.min(LANE_WIDTH, x(item.endMs ?? toMs));
      const width = Math.max(right - left, NARROWEST);
      const shown = Math.min(left, LANE_WIDTH - width);
      const hit = Math.min(Math.max(width, hitWidth), LANE_WIDTH);
      const hitLeft = Math.min(Math.max(shown + width / 2 - hit / 2, 0), LANE_WIDTH - hit);
      return { item, left: shown, width, hitLeft, hit, color: markColor(item.kind, colors) };
    });
}
