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
