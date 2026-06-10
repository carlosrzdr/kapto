// The viewer bridge's stream, as `docs/reference/viewer-stream.md` gives
// it: an opening message naming the links and values, then binary frames
// with the robot's poses and each stream's extremes as they arrive,
// cameras' pictures, and what happened as text.

import type { Mark } from "./marks";

/** What the stream says first. */
export interface Opening {
  version: number;
  /** Each link's name, in the order its pose comes in a frame. */
  links: string[];
  streams: { name: string; values: { name: string; plotted: boolean }[] }[];
  /** How far back, in seconds, the bridge keeps what it streamed. */
  history_s: number;
  /** The streams that are cameras, by their position in `streams`; absent from an older bridge. */
  cameras?: number[];
}

/** A camera's picture. */
export interface Picture {
  /** The camera's position among the opening's streams. */
  stream: number;
  /** Its media type, which the browser decodes it as. */
  type: "image/jpeg" | "image/png";
  /** Its size in pixels, or null if not known without decoding it. */
  size: [number, number] | null;
  /** When Kapto received the frame, UTC milliseconds. */
  receivedMs: number;
  /** When the bridge sent it, the same way. */
  sentMs: number;
  /** The encoded picture, in the message's own buffer. */
  bytes: Uint8Array<ArrayBuffer>;
}

/** What the bridge says of a camera: why its pictures can't be shown, or null once they can again. */
export interface CameraSaid {
  stream: number;
  unshown: string | null;
}

/** One stream's sample: the extremes of the messages it summarises. */
export interface Sample {
  stream: number;
  messages: number;
  /** When the oldest and newest of them were received, UTC milliseconds. */
  firstMs: number;
  newestMs: number;
  /** Each value's minimum then maximum. */
  extremes: Float64Array;
}

/** One frame. */
export interface Frame {
  /** When Kapto received the newest message it shows, UTC milliseconds. */
  receivedMs: number;
  /** When the bridge sent it, the same way. */
  sentMs: number;
  /** Seven numbers per link (x, y, z, then a quaternion x, y, z, w), or none if the robot didn't move. */
  poses: Float32Array | null;
  samples: Sample[];
}

/** The version of the stream this page reads. */
export const STREAM_VERSION = 1;

const MAGIC = "KVFR";
const PICTURE_MAGIC = "KVPC";
/** The bytes of a picture message before the picture. */
const PICTURE_HEADER = 40;

/** UTC nanoseconds, as Kapto sends them, in milliseconds. */
const milliseconds = (view: DataView, at: number) => Number(view.getBigInt64(at, true) / 1000n) / 1000;

/** The frame in `buffer`; throws if it isn't one this page reads. */
export function decodeFrame(buffer: ArrayBuffer): Frame {
  const view = new DataView(buffer);
  const magic = String.fromCharCode(...new Uint8Array(buffer, 0, 4));
  if (magic !== MAGIC || view.getUint16(4, true) !== STREAM_VERSION) {
    throw new Error(`not a frame this page reads: ${magic}`);
  }
  const links = view.getUint32(24, true);
  const count = view.getUint32(28, true);
  let at = 32;
  let poses: Float32Array | null = null;
  if (links > 0) {
    // Not aligned in the frame: copied out.
    poses = new Float32Array(links * 7);
    for (let index = 0; index < poses.length; index++, at += 4) {
      poses[index] = view.getFloat32(at, true);
    }
  }
  const samples: Sample[] = [];
  for (let sample = 0; sample < count; sample++) {
    const stream = view.getUint16(at, true);
    const messages = view.getUint32(at + 4, true);
    const values = view.getUint32(at + 8, true);
    const firstMs = milliseconds(view, at + 12);
    const newestMs = milliseconds(view, at + 20);
    at += 28;
    const extremes = new Float64Array(values * 2);
    for (let index = 0; index < extremes.length; index++, at += 8) {
      extremes[index] = view.getFloat64(at, true);
    }
    samples.push({ stream, messages, firstMs, newestMs, extremes });
  }
  return { receivedMs: milliseconds(view, 8), sentMs: milliseconds(view, 16), poses, samples };
}

/** The picture in `buffer`; throws if it isn't one this page reads. */
export function decodePicture(buffer: ArrayBuffer): Picture {
  const view = new DataView(buffer);
  const magic = String.fromCharCode(...new Uint8Array(buffer, 0, 4));
  if (magic !== PICTURE_MAGIC || view.getUint16(4, true) !== STREAM_VERSION) {
    throw new Error(`not a picture this page reads: ${magic}`);
  }
  const format = view.getUint8(8);
  if (format > 1) {
    throw new Error(`a picture in a format this page doesn't know: ${format}`);
  }
  const width = view.getUint32(12, true);
  const height = view.getUint32(16, true);
  const length = view.getUint32(36, true);
  return {
    stream: view.getUint16(6, true),
    type: format === 0 ? "image/jpeg" : "image/png",
    size: width > 0 && height > 0 ? [width, height] : null,
    receivedMs: milliseconds(view, 20),
    sentMs: milliseconds(view, 28),
    bytes: new Uint8Array(buffer, PICTURE_HEADER, length),
  };
}

/** One part of a stretch of a stream: its samples merged. */
export interface Part {
  firstMs: number;
  newestMs: number;
  messages: number;
  /** Each value's minimum then maximum over the part. */
  extremes: Float64Array;
}

/** A stretch of the bridge's last minutes, as it answers a question. */
export interface Stretch {
  id: number;
  /** The oldest and newest times the bridge keeps, UTC milliseconds; null if it keeps nothing. */
  keptMs: [number, number] | null;
  /** The robot's newest pose in each part where it moved, with its time. */
  poses: { timeMs: number; poses: Float32Array }[];
  /** Each stream with samples in the stretch, by its position, with its parts in time order. */
  streams: { stream: number; parts: Part[] }[];
}

const STRETCH_MAGIC = "KVHI";

/** The answer in `buffer`; throws if it isn't one this page reads. */
export function decodeStretch(buffer: ArrayBuffer): Stretch {
  const view = new DataView(buffer);
  const magic = String.fromCharCode(...new Uint8Array(buffer, 0, 4));
  if (magic !== STRETCH_MAGIC || view.getUint16(4, true) !== STREAM_VERSION) {
    throw new Error(`not a stretch this page reads: ${magic}`);
  }
  const id = view.getUint32(8, true);
  const oldest = milliseconds(view, 12);
  const newest = milliseconds(view, 20);
  const links = view.getUint32(28, true);
  const count = view.getUint32(32, true);
  let at = 36;
  const poses: Stretch["poses"] = [];
  for (let pose = 0; pose < count; pose++) {
    const timeMs = milliseconds(view, at);
    at += 8;
    const values = new Float32Array(links * 7);
    for (let index = 0; index < values.length; index++, at += 4) {
      values[index] = view.getFloat32(at, true);
    }
    poses.push({ timeMs, poses: values });
  }
  const streams: Stretch["streams"] = [];
  const streamCount = view.getUint32(at, true);
  at += 4;
  for (let index = 0; index < streamCount; index++) {
    const stream = view.getUint16(at, true);
    const values = view.getUint32(at + 4, true);
    const partCount = view.getUint32(at + 8, true);
    at += 12;
    const parts: Part[] = [];
    for (let part = 0; part < partCount; part++) {
      const firstMs = milliseconds(view, at);
      const newestMs = milliseconds(view, at + 8);
      const messages = view.getUint32(at + 16, true);
      at += 24;
      const extremes = new Float64Array(values * 2);
      for (let value = 0; value < extremes.length; value++, at += 8) {
        extremes[value] = view.getFloat64(at, true);
      }
      parts.push({ firstMs, newestMs, messages, extremes });
    }
    streams.push({ stream, parts });
  }
  return { id, keptMs: oldest === 0 && newest === 0 ? null : [oldest, newest], poses, streams };
}

/** How long to wait before connecting again after the stream closed. */
const RECONNECT_MS = 2000;
/** How long a question may go unanswered. */
const ANSWER_MS = 10_000;

/** A watched stream: stop watching it, or ask it for a stretch of its last minutes. */
export interface Watched {
  stop: () => void;
  /** The stretch from `fromMs` to `toMs`, merged to `points` parts. */
  ask: (fromMs: number, toMs: number, points: number) => Promise<Stretch>;
}

/**
 * Watches the stream on `port` of the page's host: `onopening` with each
 * connection's opening message, `onframe` with each frame, `onclosed` when
 * it closes (it is opened again by itself).
 */
export function watchStream(
  port: number,
  handlers: {
    onopening: (opening: Opening) => void;
    onframe: (frame: Frame) => void;
    onclosed: () => void;
    /** What happened: all the bridge keeps right after the opening, then each new one. */
    onmarks?: (marks: Mark[]) => void;
    /** Each camera's picture as it comes: each one's newest right after the opening, then each new one. */
    onpicture?: (picture: Picture) => void;
    /** What the bridge says of a camera whose pictures can't be shown, or can again. */
    oncamera?: (said: CameraSaid) => void;
  },
): Watched {
  let socket: WebSocket | null = null;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;
  let asked = 0;
  const waiting = new Map<number, { resolve: (stretch: Stretch) => void; reject: (error: Error) => void }>();
  const refuseAll = (why: string) => {
    waiting.forEach(({ reject }) => reject(new Error(why)));
    waiting.clear();
  };
  const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.hostname}:${port}/`;
  const open = () => {
    socket = new WebSocket(url);
    socket.binaryType = "arraybuffer";
    socket.onmessage = (message) => {
      try {
        if (typeof message.data === "string") {
          const said = JSON.parse(message.data) as Partial<Opening> & {
            error?: { id: number | null; message: string };
            marks?: Mark[];
            camera?: CameraSaid;
          };
          if (said.links !== undefined) {
            handlers.onopening(said as Opening);
          } else if (said.marks) {
            handlers.onmarks?.(said.marks);
          } else if (said.camera) {
            handlers.oncamera?.(said.camera);
          } else if (said.error) {
            const id = said.error.id;
            if (id !== null && waiting.has(id)) {
              waiting.get(id)?.reject(new Error(said.error.message));
              waiting.delete(id);
            }
          }
        } else if (message.data instanceof ArrayBuffer) {
          const kind = String.fromCharCode(...new Uint8Array(message.data, 0, 4));
          if (kind === STRETCH_MAGIC) {
            const stretch = decodeStretch(message.data);
            waiting.get(stretch.id)?.resolve(stretch);
            waiting.delete(stretch.id);
          } else if (kind === PICTURE_MAGIC) {
            handlers.onpicture?.(decodePicture(message.data));
          } else {
            handlers.onframe(decodeFrame(message.data));
          }
        }
      } catch {
        // Not something this page reads; the next may be.
      }
    };
    socket.onclose = () => {
      refuseAll("the live stream closed");
      handlers.onclosed();
      if (!stopped) {
        retry = setTimeout(open, RECONNECT_MS);
      }
    };
  };
  open();
  return {
    stop: () => {
      stopped = true;
      clearTimeout(retry);
      refuseAll("stopped watching");
      socket?.close();
    },
    ask: (fromMs, toMs, points) =>
      new Promise((resolve, reject) => {
        if (socket?.readyState !== WebSocket.OPEN) {
          reject(new Error("the live stream isn't open"));
          return;
        }
        asked = (asked + 1) % 0xffff_ffff;
        const id = asked;
        const nanos = (ms: number) => (BigInt(Math.round(ms)) * 1_000_000n).toString();
        waiting.set(id, { resolve, reject });
        setTimeout(() => {
          if (waiting.delete(id)) {
            reject(new Error("the bridge didn't answer"));
          }
        }, ANSWER_MS);
        // Nanoseconds don't fit a JavaScript number exactly, so the
        // question is written by hand.
        socket.send(
          `{"history":{"id":${id},"from":${nanos(fromMs)},"to":${nanos(toMs)},"points":${Math.max(1, Math.round(points))}}}`,
        );
      }),
  };
}
