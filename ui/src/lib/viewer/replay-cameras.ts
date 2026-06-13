// A replayed episode's cameras: each one's frame times, and which frame
// shows at a moment, as `docs/reference/replay-view.md` gives them.

import type { CameraFrames } from "../api-types";

/** One camera of a replay. */
export interface ReplayCamera {
  name: string;
  /** When each frame was received, UTC milliseconds, in order. */
  timesMs: number[];
}

/** A camera's frame times as the API gives them, in milliseconds. */
export function replayCamera(frames: CameraFrames): ReplayCamera {
  return { name: frames.camera, timesMs: frames.times_utc_ns.map((ns) => ns / 1e6) };
}

/** The frame shown at `ms`: the newest received at or before it, or -1 if none was yet. */
export function frameAt(camera: ReplayCamera, ms: number): number {
  let low = 0;
  let high = camera.timesMs.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    if ((camera.timesMs[middle] ?? Infinity) <= ms) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  return low - 1;
}

/**
 * The moment of the frame one step from the frame shown at `ms`, `step`
 * frames on (negative: back); null if there is none that way.
 */
export function steppedTo(camera: ReplayCamera, ms: number, step: number): number | null {
  const shown = frameAt(camera, ms);
  const next = shown < 0 && step > 0 ? step - 1 : shown + step;
  return camera.timesMs[next] ?? null;
}
