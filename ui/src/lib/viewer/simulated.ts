// A replay played in a simulation's time: how far into it a moment is, as
// the simulation counts it, and back. The server gives each segment between
// resets as a few points of receive time and simulated time, simulated time
// moving straight from one to the next; where no clock reading covers the
// episode (before its first, after its last, between a reset's two), time
// passes as received, as the server's quality checks count it.

import type { SimulatedRun } from "../api-types";

/** One point of a segment, in milliseconds: receive time (UTC) and simulated time. */
interface Point {
  receiveMs: number;
  simulatedMs: number;
}

/** A stretch of the episode, and how far into the played time it starts and ends. */
interface Piece {
  fromMs: number;
  toMs: number;
  playedFromMs: number;
  playedToMs: number;
  /** The segment's points and its number, or none where time passes as received. */
  segment: { points: Point[]; number: number } | null;
}

/** Where a moment is in the simulation: which segment, counting from 0, and its simulated time. */
export interface SimulatedMoment {
  segment: number;
  seconds: number;
}

/** An episode's span, `[fromMs, toMs]` in UTC milliseconds, in its simulation's time. */
export class SimulatedTime {
  private readonly pieces: Piece[] = [];
  /** How long it plays for in the simulation's time, in milliseconds. */
  readonly lengthMs: number;

  constructor(run: SimulatedRun, [fromMs, toMs]: [number, number]) {
    let at = fromMs;
    let played = 0;
    const real = (until: number) => {
      if (until > at) {
        this.pieces.push({ fromMs: at, toMs: until, playedFromMs: played, playedToMs: played + until - at, segment: null });
        played += until - at;
        at = until;
      }
    };
    run.segments.forEach((knots, number) => {
      const points = knots.map((knot) => ({ receiveMs: knot.receive_utc_ns / 1e6, simulatedMs: knot.simulated_ns / 1e6 }));
      const first = points[0];
      const last = points.at(-1);
      if (!first || !last || last.receiveMs < at) {
        return;
      }
      real(first.receiveMs);
      const length = last.simulatedMs - first.simulatedMs;
      this.pieces.push({
        fromMs: first.receiveMs,
        toMs: last.receiveMs,
        playedFromMs: played,
        playedToMs: played + length,
        segment: { points, number },
      });
      played += length;
      at = last.receiveMs;
    });
    real(toMs);
    this.lengthMs = played;
  }

  /** Whether the simulation's clock covers any of the episode. */
  get covered(): boolean {
    return this.pieces.some((piece) => piece.segment !== null);
  }

  /** How far into the played time `receiveMs` is, in milliseconds. */
  played(receiveMs: number): number {
    const piece = lastWhere(this.pieces, (each) => each.fromMs <= receiveMs) ?? this.pieces[0];
    if (!piece) {
      return 0;
    }
    if (receiveMs >= piece.toMs) {
      return piece.playedToMs;
    }
    if (!piece.segment) {
      return piece.playedFromMs + Math.max(0, receiveMs - piece.fromMs);
    }
    const points = piece.segment.points;
    return piece.playedFromMs + simulatedOn(points, receiveMs) - (points[0]?.simulatedMs ?? 0);
  }

  /**
   * The moment `playedMs` into the played time, in UTC milliseconds: a
   * pause, which takes none of it, is passed at once, to where the
   * simulation played again.
   */
  receiveAt(playedMs: number): number {
    const piece = lastWhere(this.pieces, (each) => each.playedFromMs <= playedMs) ?? this.pieces[0];
    if (!piece) {
      return 0;
    }
    if (playedMs >= piece.playedToMs) {
      return piece.toMs;
    }
    if (!piece.segment) {
      return piece.fromMs + (playedMs - piece.playedFromMs);
    }
    const points = piece.segment.points;
    const target = (points[0]?.simulatedMs ?? 0) + (playedMs - piece.playedFromMs);
    // The last point at or before it: the end of a pause held at it.
    let before = 0;
    while (before + 1 < points.length && points[before + 1]!.simulatedMs <= target) {
      before += 1;
    }
    const from = points[before]!;
    const to = points[before + 1];
    if (!to || to.simulatedMs === from.simulatedMs) {
      return from.receiveMs;
    }
    return from.receiveMs + ((target - from.simulatedMs) * (to.receiveMs - from.receiveMs)) / (to.simulatedMs - from.simulatedMs);
  }

  /** The simulation's time at `receiveMs`, or `null` where no clock reading covers it. */
  at(receiveMs: number): SimulatedMoment | null {
    const piece = this.pieces.find((each) => each.segment && each.fromMs <= receiveMs && receiveMs <= each.toMs);
    if (!piece?.segment) {
      return null;
    }
    return { segment: piece.segment.number, seconds: simulatedOn(piece.segment.points, receiveMs) / 1000 };
  }
}

/** The last of `pieces` that `holds`, in order. */
function lastWhere(pieces: Piece[], holds: (piece: Piece) => boolean): Piece | undefined {
  for (let index = pieces.length - 1; index >= 0; index -= 1) {
    if (holds(pieces[index]!)) {
      return pieces[index];
    }
  }
  return undefined;
}

/** Simulated time at `receiveMs`, moving straight between `points`. */
function simulatedOn(points: Point[], receiveMs: number): number {
  const after = points.findIndex((point) => point.receiveMs > receiveMs);
  if (after === -1) {
    return points.at(-1)?.simulatedMs ?? 0;
  }
  if (after === 0) {
    return points[0]!.simulatedMs;
  }
  const from = points[after - 1]!;
  const to = points[after]!;
  const span = to.receiveMs - from.receiveMs;
  return span === 0 ? to.simulatedMs : from.simulatedMs + ((receiveMs - from.receiveMs) * (to.simulatedMs - from.simulatedMs)) / span;
}

/** The simulation's time in words, `12.5 s`, with its segment after a reset: `3.0 s after reset 1`. */
export function simulatedLabel(moment: SimulatedMoment | null, fractionDigits = 1): string {
  if (moment === null) {
    return "—";
  }
  const seconds = `${moment.seconds.toFixed(fractionDigits)} s`;
  return moment.segment === 0 ? seconds : `${seconds} after reset ${moment.segment}`;
}

/**
 * A time axis's label in the simulation's time, at ticks `step` seconds of
 * receive time apart: `12.5 s`, its segment under it after a reset. The
 * ticks fall on whole receive seconds, not simulated ones, so a tenth is
 * kept unless they are far apart.
 */
export function axisSimulated(moment: SimulatedMoment | null, step: number): string {
  if (moment === null) {
    return "";
  }
  const seconds = `${moment.seconds.toFixed(step < 10 ? 1 : 0)} s`;
  return moment.segment === 0 ? seconds : `${seconds}\nafter reset ${moment.segment}`;
}

/** How many times its usual wait a clock holds still before it reads as paused, as `qa.toml`'s default. */
export const PAUSE_SILENCE = 10;

/** However fast the clock usually moves, it holds still this long before it reads as paused, in milliseconds. */
const PAUSED_AFTER_MS = 250;

/** The waits between the clock's moves it remembers, newest last. */
const WAITS_KEPT = 32;

/** The simulation's time now, and whether it is paused. */
export interface SimulatedNow {
  seconds: number;
  paused: boolean;
}

/**
 * A simulation's clock as the live view follows it, from its newest values:
 * paused once it has held still, or fallen silent, for `PAUSE_SILENCE` times
 * its usual wait between moves, as the worker judges a recorded one.
 */
export class LiveClock {
  private seconds: number | null = null;
  private movedMs = 0;
  private readonly waits: number[] = [];

  /** Takes the clock's value, `seconds`, received at `receiveMs`. */
  observe(receiveMs: number, seconds: number) {
    if (!Number.isFinite(seconds) || seconds === this.seconds) {
      return;
    }
    // Back is a reset: its wait says nothing of the clock's pace.
    if (this.seconds !== null && seconds > this.seconds) {
      this.waits.push(receiveMs - this.movedMs);
      if (this.waits.length > WAITS_KEPT) {
        this.waits.shift();
      }
    }
    this.seconds = seconds;
    this.movedMs = receiveMs;
  }

  /** The simulation's time as of `receiveMs`, or `null` before its clock is read. */
  now(receiveMs: number): SimulatedNow | null {
    if (this.seconds === null) {
      return null;
    }
    const sorted = [...this.waits].sort((a, b) => a - b);
    const usual = sorted[Math.floor(sorted.length / 2)];
    const held = receiveMs - this.movedMs;
    const paused = usual !== undefined && held > Math.max(PAUSE_SILENCE * usual, PAUSED_AFTER_MS);
    return { seconds: this.seconds, paused };
  }
}

/** The simulation's time now in words: `simulated 12.3 s`, or `simulation paused at 12.3 s`. */
export function simulatedNowLabel(now: SimulatedNow): string {
  const seconds = `${now.seconds.toFixed(1)} s`;
  return now.paused ? `simulation paused at ${seconds}` : `simulated ${seconds}`;
}

/**
 * Where the live stream carries the value `field` names (`clock.time`, or
 * `clock.times[1]`), plotted or not: its stream's position among the
 * opening's, and the place of its maximum among the stream's samples, each
 * value's minimum then maximum.
 */
export function clockIn(
  streams: { name: string; values: { name: string }[] }[],
  field: string | undefined,
): { stream: number; at: number } | null {
  const match = field?.match(/^([^.]+)\.(.+)$/);
  if (!match) {
    return null;
  }
  const stream = streams.findIndex((each) => each.name === match[1]);
  const value = streams[stream]?.values.findIndex((each) => each.name === match[2] || each.name === `${match[2]}[0]`);
  return value === undefined || value < 0 ? null : { stream, at: value * 2 + 1 };
}
