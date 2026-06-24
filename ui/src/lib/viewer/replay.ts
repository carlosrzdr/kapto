// An episode replayed in the live view: what its stretches hold, and what
// happened in it, drawn along the timeline.

import type { EpisodeDetail, EpisodeEvents, EpisodeQuality, ReplayOpening } from "../api-types";
import { type Item, MarkLog } from "./marks";
import { type Opening, STREAM_VERSION } from "./stream";

/** The episode the live view replays, and where to open it. */
export interface Replaying {
  id: string;
  /** What its stretches hold. */
  opening: ReplayOpening;
  /** What happened in it, and what the checks found. */
  items: Item[];
  /** Opened a little before this moment, UTC milliseconds; at its start if `null`. */
  atMs: number | null;
  /** Changed to open it there again. */
  asked: number;
}

/** Where a replay is playing, for the bar above it. */
export interface Played {
  playing: boolean;
  /** The moment shown, UTC milliseconds. */
  atMs: number;
  /** Stopped on its last moment. */
  over: boolean;
}

/**
 * Where a replay is playing, as the bar above it says it: times in seconds
 * from its first message.
 */
export interface Playback {
  playing: boolean;
  at: number;
  length: number;
  /** Stopped on its last moment. */
  over: boolean;
}

/** A replay's opening as the live stream's, so the live view's plots read it. */
export function openingOf(replay: ReplayOpening): Opening {
  return { version: STREAM_VERSION, links: replay.links, streams: replay.streams, history_s: 0 };
}

/**
 * What the timeline draws for a replayed episode: when Start and Stop were
 * asked, as the live view marks an episode's start and end (an episode
 * with a pre-roll or post-roll holds data on either side), its events, as
 * the live view draws them, and each moment a check found, as a `check`
 * stretch. The commands are drawn within `span`, the replay's from its
 * first message to its last, UTC nanoseconds: Stop is asked a moment after
 * the last message of an episode without a post-roll, and is drawn at its
 * end rather than left off.
 */
export function replayItems(
  events: EpisodeEvents | null,
  quality: EpisodeQuality | null,
  episode: EpisodeDetail | null = null,
  span: [number, number] | null = null,
): Item[] {
  const log = new MarkLog();
  log.add(
    (events?.events ?? []).map((event) => ({
      noted: {
        event: {
          kind: event.kind,
          at_utc_ns: event.at_utc_ns,
          until_utc_ns: event.until_utc_ns ?? undefined,
          phase: event.phase,
          id: event.id ?? undefined,
          source: event.source,
          text: event.text ?? undefined,
        },
        episode: null,
      },
    })),
  );
  const found = (quality?.checks ?? []).flatMap((check) =>
    check.ranges.map((range) => ({
      kind: "check",
      startMs: range.start_utc_ns / 1e6,
      endMs: range.end_utc_ns / 1e6,
      label: `${check.check}: ${check.message}`,
    })),
  );
  const commands: Item[] = [
    [episode?.commanded_start_utc_ns, "Start asked"] as const,
    [episode?.commanded_stop_utc_ns, "Stop asked"] as const,
  ].flatMap(([asked, label]) => {
    if (typeof asked !== "number") {
      return [];
    }
    const at = span ? Math.min(Math.max(asked, span[0]), span[1]) / 1e6 : asked / 1e6;
    return [{ kind: "episode", startMs: at, endMs: at, label }];
  });
  return [...commands, ...log.items, ...found].sort((a, b) => a.startMs - b.startMs);
}
