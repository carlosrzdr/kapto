import { rate } from "./format";

/** How many messages each stream had received when the status was asked for. */
export interface Reading {
  /** When it was asked for, in the page's milliseconds. */
  at: number;
  received: Map<string, number>;
}

/**
 * How many status readings each stream's rate is measured over. One reply
 * arriving late would otherwise make a healthy stream look slow.
 */
export const RATE_READINGS = 5;

/**
 * The readings to measure over once `reading` is taken: the latest few.
 * Counts start again from zero when an episode starts or stops, here or
 * elsewhere, which a count gone down shows; the readings before measure
 * nothing then. Nor does the one that shows it: asked for while the
 * episode was starting, its counts were taken some time after it was asked
 * for, how long unknown, so a rate from it reads low. Rates start again
 * from the reading after.
 */
export function withReading(readings: Reading[], reading: Reading): Reading[] {
  const last = readings.at(-1);
  const restarted =
    last !== undefined && [...reading.received].some(([stream, count]) => count < (last.received.get(stream) ?? 0));
  return restarted ? [] : [...readings, reading].slice(-RATE_READINGS);
}

/**
 * Each of `streams`' messages a second over `readings`, or `null` while
 * they can't say, when the rates measured before still stand.
 */
export function ratesOver(readings: Reading[], streams: string[]): Record<string, number> | null {
  const oldest = readings[0];
  const newest = readings.at(-1);
  if (!oldest || !newest || oldest === newest) {
    return null;
  }
  const seconds = (newest.at - oldest.at) / 1000;
  return Object.fromEntries(
    streams.map((stream) => [
      stream,
      rate(oldest.received.get(stream) ?? 0, newest.received.get(stream) ?? 0, seconds),
    ]),
  );
}
