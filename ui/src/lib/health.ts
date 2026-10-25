// The page's health in one line: everything the page already receives,
// ranked, so the header can say the one thing most in need of attention.

import type { DestinationStatus, PreflightCheck, ProcessStatus, Status, Storage, StreamCounts } from "./api-types";
import { count } from "./format";

/** How much a problem needs attention: the scale every state colour follows. */
export type Level = "ok" | "warning" | "danger";

/** The side panel's tab holding a problem's details, and the element there it is about. */
export interface Target {
  tab: "live" | "system" | "uploads";
  /** The ID of the element to bring into view and focus. */
  anchor: string;
}

/** One problem, as the chip says it. */
export interface Problem {
  level: Level;
  text: string;
  target: Target;
}

/** What the chip shows: the worst problem, how many more there are, and every one. */
export interface Health {
  level: Level;
  text: string;
  /** How many problems there are besides the one said. */
  more: number;
  /** Where the one said is shown in full; `null` while all is well. */
  target: Target | null;
  /** Every problem, worst first. */
  problems: Problem[];
}

/** Everything the page already asks for that says whether anything is wrong. */
export interface HealthInput {
  status: Status | null;
  /** Why the last status request failed, if it did. */
  failure: { code: string } | null;
  /** Messages received per second, by stream, once measured. */
  rates: Record<string, number>;
  /** Messages per second each stream is declared to send, where declared. */
  declared: Record<string, number | null | undefined>;
  /** The streams whose messages are kept for late subscribers. */
  kept: Record<string, boolean>;
  storage: Storage | null;
  processes: ProcessStatus[];
  destinations: DestinationStatus[];
}

/** Below this share of its declared rate, a stream is slow. */
export const SLOW = 0.9;

/** How a stream is arriving; `null` until its rate is known. */
export type StreamState = "silent" | "slow" | "ok" | "kept";

/** How `stream` arrives at `rate`: the one rule the stream table and the chip share. */
export function streamState(
  rate: number | undefined,
  declared: number | null | undefined,
  kept: boolean | undefined,
): StreamState | null {
  if (rate === undefined) {
    return null;
  }
  if (rate === 0) {
    return kept ? "kept" : "silent";
  }
  return declared && rate < declared * SLOW ? "slow" : "ok";
}

/**
 * Every kind of problem, most in need of attention first. Losing data or
 * being unable to record is a danger; what may lead to it, a warning.
 */
export const RANK = {
  unreachable: { rank: 0, level: "danger" },
  disk_full: { rank: 1, level: "danger" },
  lost: { rank: 2, level: "danger" },
  dropped: { rank: 3, level: "danger" },
  preflight: { rank: 4, level: "warning" },
  silent: { rank: 5, level: "warning" },
  slow: { rank: 6, level: "warning" },
  disk_low: { rank: 7, level: "warning" },
  process: { rank: 8, level: "warning" },
  destination: { rank: 9, level: "warning" },
} as const satisfies Record<string, { rank: number; level: Level }>;

export type Kind = keyof typeof RANK;

/**
 * What the status request failing says, by its cause code; any other
 * means the status can't be read either.
 */
export const UNREACHABLE: Record<string, string> = {
  "capture.control_unreachable": "Capture not reachable",
  "ui.server_unreachable": "Kapto not reachable",
  "ui.server_slow": "Kapto not answering",
  "ui.unexpected_response": "Status not readable",
  "ui.page_failed": "Status not readable",
};

/**
 * What each pre-flight check says when it fails, by its cause code. Every
 * `preflight.*` code Kapto has must be here (`cargo xtask ci` checks it);
 * one the page doesn't know yet still shows, as a failed pre-flight.
 */
export const PREFLIGHT: Record<string, (check: PreflightCheck) => string> = {
  "preflight.disk_too_small": () => "Pre-flight: disk too small",
  "preflight.disk_too_slow": () => "Pre-flight: disk too slow",
  "preflight.stream_silent": (check) => `Pre-flight: ${check.stream ?? "a stream"} silent`,
  "preflight.kept_missing": (check) => `Pre-flight: ${check.stream ?? "a stream"} has nothing kept`,
  "preflight.stream_slow": (check) => `Pre-flight: ${check.stream ?? "a stream"} slow`,
};

/** The ID of a stream's row in the Live tab. */
export const streamAnchor = (stream: string): string => `stream-${stream}`;
/** The ID of a process's row in the System tab. */
export const processAnchor = (name: string): string => `process-${name}`;
/** The ID of a destination in the Uploads tab. */
export const destinationAnchor = (name: string): string => `destination-${name}`;

/** The IDs of the Live tab's sections. */
export const LIVE = { preflight: "live-preflight", streams: "live-streams", storage: "live-storage" } as const;

interface Found extends Problem {
  rank: number;
  /** Among problems of one kind, the larger first: the most messages lost. */
  weight: number;
}

function found(kind: Kind, text: string, target: Target, weight = 0): Found {
  return { ...RANK[kind], text, target, weight };
}

function streamProblems(stream: StreamCounts, input: HealthInput): Found[] {
  const target: Target = { tab: "live", anchor: streamAnchor(stream.stream) };
  const problems: Found[] = [];
  if (stream.lost_upstream > 0) {
    problems.push(found("lost", `${stream.stream} ${count(stream.lost_upstream)} lost`, target, stream.lost_upstream));
  }
  if (stream.dropped_by_us > 0) {
    problems.push(found("dropped", `${stream.stream} ${count(stream.dropped_by_us)} dropped`, target, stream.dropped_by_us));
  }
  const state = streamState(input.rates[stream.stream], input.declared[stream.stream], input.kept[stream.stream]);
  if (state === "silent") {
    problems.push(found("silent", `${stream.stream} silent`, target));
  } else if (state === "slow") {
    problems.push(found("slow", `${stream.stream} slow`, target));
  }
  return problems;
}

function diskProblem(storage: Storage | null): Found | null {
  if (storage === null || storage.free_bytes >= storage.warn_below_bytes || storage.total_bytes <= 0) {
    return null;
  }
  const share = Math.floor((storage.free_bytes / storage.total_bytes) * 100);
  const kind = storage.free_bytes < storage.stop_below_bytes ? "disk_full" : "disk_low";
  return found(kind, `Disk ${share} % free`, { tab: "live", anchor: LIVE.storage });
}

/** Every problem the page knows of, worst first. */
export function problems(input: HealthInput): Problem[] {
  const all: Found[] = [];
  if (input.failure !== null) {
    const text = UNREACHABLE[input.failure.code] ?? "Status not readable";
    all.push(found("unreachable", text, { tab: "live", anchor: LIVE.streams }));
  }
  const disk = diskProblem(input.storage);
  if (disk) {
    all.push(disk);
  }
  // While capture doesn't answer, its last streams and pre-flight aren't
  // what is wrong now: that it doesn't answer is.
  const capture = input.failure === null ? input.status?.capture : undefined;
  for (const stream of capture?.streams ?? []) {
    all.push(...streamProblems(stream, input));
  }
  // A check the operator waived to record anyway was decided on.
  for (const check of capture?.preflight?.checks ?? []) {
    if (!check.passed && !check.waived) {
      const said = PREFLIGHT[check.code]?.(check) ?? `Pre-flight: ${check.summary || check.code}`;
      all.push(found("preflight", said, { tab: "live", anchor: LIVE.preflight }));
    }
  }
  for (const process of input.processes) {
    if (process.state === "restarting" || process.state === "stopped") {
      all.push(found("process", `${process.name} ${process.state}`, { tab: "system", anchor: processAnchor(process.name) }));
    }
  }
  for (const destination of input.destinations) {
    if (destination.failed > 0) {
      const target: Target = { tab: "uploads", anchor: destinationAnchor(destination.name) };
      all.push(found("destination", `Uploads to ${destination.name} failing`, target, destination.failed));
    }
  }
  // Stable: problems of one rank and weight keep the order they are listed in.
  all.sort((a, b) => a.rank - b.rank || b.weight - a.weight);
  return all.map(({ level, text, target }) => ({ level, text, target }));
}

/** The chip's line: "All well", or the worst problem and how many more. */
export function health(input: HealthInput): Health {
  const found = problems(input);
  const worst = found[0];
  if (worst === undefined) {
    // Nothing is known to be wrong before the first answer, nor well.
    const text = input.status === null ? "Checking…" : "All well";
    return { level: "ok", text, more: 0, target: null, problems: [] };
  }
  // Every danger ranks above every warning, so the worst says the colour.
  return { level: worst.level, text: worst.text, more: found.length - 1, target: worst.target, problems: found };
}
