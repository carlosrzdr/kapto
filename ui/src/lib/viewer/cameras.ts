// The live view's cameras: each camera named by the stream's opening, with
// its newest picture, decoded, and what the bridge says of it.

import type { LiveViewCounts } from "../api-types";
import { count } from "../format";
import type { CameraSaid, Opening, Picture } from "./stream";

/**
 * Above this delay, in milliseconds, from Kapto receiving a message to the
 * page receiving it, the live view says how far behind it is; below it,
 * only on hover.
 */
export const LATE_MS = 150;

/**
 * Above this share of a camera's frames offered to the live view, those
 * skipped or replaced on the way (a slow page or link, never a loss to the
 * recording) are said in its caption; below it, only on hover.
 */
export const NOT_SHOWN_HINT = 0.1;

/** What became of a camera's frames on their way to the live view, in words; only what happened. */
export function tallied(camera: LiveViewCounts): string {
  const parts = [`${count(camera.sent)} of ${count(camera.offered)} frames sent`];
  if (camera.superseded > 0) {
    parts.push(`${count(camera.superseded)} replaced by newer ones while the viewer was slow`);
  }
  if (camera.skipped > 0) {
    parts.push(`${count(camera.skipped)} skipped`);
  }
  if (camera.not_sent > 0) {
    parts.push(`${count(camera.not_sent)} while no viewer listened`);
  }
  return `Live view: ${parts.join(" · ")}`;
}

/** How many of a camera's frames the live view didn't show, once that is worth saying; `null` until then. */
export function notShown(camera: LiveViewCounts): number | null {
  const missed = camera.superseded + camera.skipped;
  return camera.offered > 0 && missed / camera.offered > NOT_SHOWN_HINT ? missed : null;
}

/** One camera, as the camera panel shows it. */
export interface Shown {
  /** Its position among the opening's streams. */
  stream: number;
  name: string;
  /** Its newest picture, decoded, if one came. */
  bitmap: ImageBitmap | null;
  /** From Kapto receiving the frame to this page receiving its picture, in milliseconds. */
  delayMs: number | null;
  /** Why its pictures can't be shown, if they can't. */
  unshown: string | null;
  /** Pictures shown, for the page's tests. */
  pictures: number;
  /** In a replay, which frame shows, numbered from 0, of how many. */
  frame?: { n: number; of: number } | null;
  /** Looking back at the live view: when Kapto received the frame of the picture kept that shows, UTC milliseconds. */
  keptAtMs?: number | null;
}

/** The cameras the opening names, none shown yet. */
export function camerasOf(opening: Opening): Shown[] {
  return (opening.cameras ?? []).map((stream) => ({
    stream,
    name: opening.streams[stream]?.name ?? `stream ${stream}`,
    bitmap: null,
    delayMs: null,
    unshown: null,
    pictures: 0,
  }));
}

/**
 * `cameras` with `picture`, decoded as `bitmap`, shown; the page received
 * it at `arrivedMs`. Also the bitmap it replaces, for the caller to close; a
 * picture of a camera the opening didn't name changes nothing, and its own
 * bitmap is handed back.
 */
export function withPicture(
  cameras: Shown[],
  picture: Picture,
  bitmap: ImageBitmap,
  arrivedMs: number,
): { cameras: Shown[]; replaced: ImageBitmap | null } {
  const camera = cameras.find((each) => each.stream === picture.stream);
  if (!camera) {
    return { cameras, replaced: bitmap };
  }
  const shown: Shown = {
    ...camera,
    bitmap,
    delayMs: arrivedMs - picture.receivedMs,
    pictures: camera.pictures + 1,
  };
  return { cameras: cameras.map((each) => (each === camera ? shown : each)), replaced: camera.bitmap };
}

/** `cameras` with what the bridge `said` of one of them. */
export function withSaid(cameras: Shown[], said: CameraSaid): Shown[] {
  return cameras.map((each) => (each.stream === said.stream ? { ...each, unshown: said.unshown } : each));
}

/**
 * Decodes pictures one at a time per camera, off the page's main work:
 * while one is decoded, only each camera's newest waits, so a slow
 * browser shows fewer pictures, never older ones.
 */
export class Decoder {
  private waiting = new Map<number, { picture: Picture; arrivedMs: number } | null>();

  constructor(
    private readonly decode: (picture: Picture) => Promise<ImageBitmap>,
    private readonly decoded: (picture: Picture, bitmap: ImageBitmap, arrivedMs: number) => void,
  ) {}

  /** Decodes `picture`, which arrived at `arrivedMs`, once the camera's picture before it is decoded. */
  add(picture: Picture, arrivedMs: number) {
    if (this.waiting.has(picture.stream)) {
      this.waiting.set(picture.stream, { picture, arrivedMs });
      return;
    }
    this.waiting.set(picture.stream, null);
    this.decode(picture)
      .then((bitmap) => this.decoded(picture, bitmap, arrivedMs))
      // A picture the browser can't decode isn't shown; the next may be.
      .catch(() => {})
      .finally(() => {
        const next = this.waiting.get(picture.stream);
        this.waiting.delete(picture.stream);
        if (next) {
          this.add(next.picture, next.arrivedMs);
        }
      });
  }
}
