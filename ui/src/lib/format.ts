// How numbers and times are shown.

/** `75` → `1:15`, `3725` → `1:02:05`. */
export function duration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rest = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${rest}` : `${m}:${rest}`;
}

/** Thousands separated by thin spaces, so long counts stay readable. */
export function count(value: number): string {
  return Math.round(value)
    .toString()
    .replace(/\B(?=(\d{3})+(?!\d))/g, " ");
}

/**
 * A message as people read it: the operating system's error numbers
 * (`(os error 111)`) say nothing to an operator, and stay in its tooltip.
 */
export const plain = (message: string): string => message.replace(/\s*\(os error \d+\)/g, "");

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

/** CPU use in cores as a percentage of one core: `0.26` → `26 %`. */
export function cores(value: number): string {
  return `${Math.round(value * 100)} %`;
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
