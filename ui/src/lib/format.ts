// How numbers and times are shown.

/** `75` → `1:15`, `3725` → `1:02:05`. */
export function duration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rest = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${rest}` : `${m}:${rest}`;
}

/**
 * Thousands separated by narrow spaces, so long counts stay readable; ones
 * no line breaks at, so a count is never split over two.
 */
export function count(value: number): string {
  return Math.round(value)
    .toString()
    .replace(/\B(?=(\d{3})+(?!\d))/g, " ");
}

/**
 * A message as people read it: the operating system's error numbers
 * (`(os error 111)`) say nothing to an operator, and stay in its tooltip.
 */
export const plain = (message: string): string => message.replace(/\s*\(os error \d+\)/g, "");

/**
 * A message ended as a sentence, so the remedy written after it reads as
 * the next one: Kapto's messages are phrases (`Can't read the mesh file:
 * meshes/gripper.stl`), without a full stop of their own.
 */
export const sentence = (message: string): string => (/[.!?]$/.test(message.trimEnd()) ? message : `${message.trimEnd()}.`);

/** Messages per second between two readings `seconds` apart. */
export function rate(before: number, after: number, seconds: number): number {
  return seconds > 0 && after >= before ? (after - before) / seconds : 0;
}

/** A UTC time in nanoseconds as local date and time, to the second. */
export function time(utcNs: number): string {
  const date = new Date(utcNs / 1e6);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/**
 * A time axis's labels, written as the rest of the page writes times: each
 * tick at `seconds` (since the Unix epoch, local time) to the precision its
 * spacing `step` needs (`14:46:05`, `14:46`, `14:46:05.25`). Only when the
 * ticks cross midnight is the date (`2026-10-26`) written, under the first
 * tick and under each where it changes: the day being today's goes without
 * saying, on every chart.
 */
export function axisTimes(seconds: number[], step: number): string[] {
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  // As many decimals as the step has: 0.5 s needs one, 0.25 s two.
  const decimals = [0, 1, 2].find((places) => Math.abs(step * 10 ** places - Math.round(step * 10 ** places)) < 1e-6) ?? 3;
  const dayOf = (date: Date) => `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
  const days = new Set(seconds.map((at) => dayOf(new Date(at * 1000))));
  let lastDate = days.size > 1 ? "" : ([...days][0] ?? "");
  return seconds.map((at) => {
    const date = new Date(at * 1000);
    const day = dayOf(date);
    let clock = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
    if (step < 60) {
      clock += `:${pad(date.getSeconds())}`;
    }
    if (decimals > 0) {
      clock += `.${pad(date.getMilliseconds(), 3).slice(0, decimals)}`;
    }
    if (step >= 86_400) {
      clock = day;
    }
    const label = day !== lastDate && step < 86_400 ? `${clock}\n${day}` : clock;
    lastDate = day;
    return label;
  });
}

/**
 * The steps a time axis's ticks may be apart, in seconds: the clock's own
 * divisions, so ticks fall on round times. Two seconds is among them, so a
 * few seconds of history on a narrow chart still gets more than one label.
 */
export const AXIS_TIME_STEPS = [
  0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1_800, 3_600, 7_200, 10_800,
  21_600, 43_200, 86_400, 172_800, 604_800,
];

/**
 * The step a time axis's ticks are apart over `min` to `max` (seconds since
 * the Unix epoch) across `dim` pixels, and the room each tick then needs:
 * the labels written at that step, as `measure` makes them in the axis's
 * font, and a gap. Whole seconds (`12:32:40`) need less room than
 * fractions of one (`12:32:40.25`), so ticks are only as far apart as the
 * labels actually written; the date, when the ticks cross midnight, is
 * measured too.
 */
export function axisTimesStep(
  min: number,
  max: number,
  dim: number,
  measure: (text: string) => number,
  gap = 12,
): { step: number; space: number } {
  const dayOf = (at: number) => new Date(at * 1000).toDateString();
  const crossing = dayOf(min) !== dayOf(max);
  // Midnight writes every digit as a zero, as wide as any other time in a
  // font whose digits share a width.
  const midnight = new Date(2026, 0, 1).getTime() / 1000;
  const rooms = AXIS_TIME_STEPS.map((step) => {
    const lines = axisTimes(crossing ? [midnight - step, midnight] : [midnight], step).flatMap((label) => label.split("\n"));
    return Math.ceil(Math.max(...lines.map(measure))) + gap;
  });
  const perSecond = max > min ? dim / (max - min) : 0;
  const found = AXIS_TIME_STEPS.findIndex((step, place) => step * perSecond >= rooms[place]!);
  const place = found === -1 ? AXIS_TIME_STEPS.length - 1 : found;
  // A chart takes the smallest step at least `space` apart: asking a hair
  // more than the step below is apart keeps it from taking that one, whose
  // longer labels (`12:32:40.5` rather than `12:32:41`) wouldn't fit.
  const below = place > 0 ? AXIS_TIME_STEPS[place - 1]! * perSecond * (1 + 1e-6) : 0;
  return { step: AXIS_TIME_STEPS[place]!, space: Math.max(rooms[place]!, below) };
}

/** Bytes in binary units: `87203840` → `83 MiB`, `1610612736` → `1.5 GiB`. */
export function bytes(value: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let size = Math.max(0, value);
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const digits = unit > 0 && size < 10 ? 1 : 0;
  return `${size.toFixed(digits)} ${units[unit]}`;
}

/**
 * CPU use in cores, `0.26` → `0.26 cores`: a percentage would read as a
 * share of the whole machine, when it is of one core of several.
 */
export function cores(value: number): string {
  return `${value.toFixed(2)} cores`;
}

/**
 * A measured value to three significant figures: `0.012345` → `0.0123`,
 * `1.5` → `1.5`; from 1000 as a whole count, `12345.6` → `12 346`.
 */
export function figure(value: number): string {
  if (!Number.isFinite(value)) {
    return "–";
  }
  return Math.abs(value) >= 1000 ? count(value) : String(Number(value.toPrecision(3)));
}

/** Seconds into something, to the millisecond: `2.13` → `0:02.130`. */
export function offset(seconds: number): string {
  const whole = Math.floor(Math.max(0, seconds));
  return `${duration(whole)}.${String(Math.round((Math.max(0, seconds) - whole) * 1000) % 1000).padStart(3, "0")}`;
}

/** How long something lasted: `0.45` → `450 ms`, `2.4` → `2.4 s`, `75` → `1:15`. */
export function lasting(seconds: number): string {
  if (seconds < 1) {
    return `${Math.round(seconds * 1000)} ms`;
  }
  return seconds < 60 ? `${seconds.toFixed(1)} s` : duration(seconds);
}

/** A moment, UTC milliseconds, on the local clock to the millisecond: `14:46:05.250`. */
export function clock(ms: number): string {
  if (!Number.isFinite(ms)) {
    return "–";
  }
  const date = new Date(ms);
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(date.getMilliseconds(), 3)}`;
}

/** Nanoseconds as milliseconds, or seconds from 10 s: `520_000_000` → `520 ms`. */
export function milliseconds(ns: number): string {
  const ms = ns / 1e6;
  if (ms >= 10_000) {
    return `${(ms / 1000).toFixed(0)} s`;
  }
  return ms >= 10 ? `${ms.toFixed(0)} ms` : `${ms.toFixed(1)} ms`;
}

/**
 * A name or path in pieces a line may end after: each `.`, `_` or `/`, so
 * `arm_joints.joint_positions[0]` or a file's path wraps between its parts,
 * never mid-word. Shown with a `<wbr>` after each piece.
 */
export function breakable(text: string): string[] {
  return text.split(/(?<=[._/])/);
}
