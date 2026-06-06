// Playing what the bridge kept: the moment shown moves forward at a chosen
// speed from where it was started, on the page's clock, until it reaches
// the newest moment kept and the view is live again.

/** How long before an event it is opened, so what led to it is seen. */
export const LEAD_MS = 2_000;

/** The speeds offered. */
export const SPEEDS = [0.5, 1, 2, 4] as const;

/** Where a play started: the moment shown, and the page's clock then, in milliseconds. */
export interface Started {
  fromMs: number;
  wallMs: number;
}

/** The moment shown at `wallMs` on the page's clock, playing at `speed` since `started`. */
export function playedTo(started: Started, wallMs: number, speed: number): number {
  return started.fromMs + Math.max(0, wallMs - started.wallMs) * speed;
}

/** Where an event at `atMs` is opened: a little before it, but not before the oldest moment kept. */
export function openedAt(atMs: number, oldestMs: number): number {
  return Math.max(atMs - LEAD_MS, oldestMs);
}
