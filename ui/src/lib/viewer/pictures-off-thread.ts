// Camera pictures turned into bitmaps in a worker of their own, off the
// page's main thread: with several cameras, making their bitmaps there took
// most of its time, and the robot's frames, which it also draws, waited
// behind them. The page only draws the bitmaps that come back.

/** A picture asked to be decoded, as the worker receives it. */
export interface DecodeAsk {
  id: number;
  /** The encoded picture; its buffer is handed to the worker, not copied. */
  bytes: Uint8Array<ArrayBuffer>;
  type: string;
  /** How wide to decode it, scaled down; null for its own size. */
  resizeWidth: number | null;
}

/** The worker's answer to a `DecodeAsk`. */
export type DecodeAnswer = { id: number; bitmap: ImageBitmap } | { id: number; error: string };

/** Decodes `ask` as the browser's own decoder does, wherever it runs. */
export function decode(ask: Omit<DecodeAsk, "id">): Promise<ImageBitmap> {
  const blob = new Blob([ask.bytes], { type: ask.type });
  return ask.resizeWidth === null
    ? createImageBitmap(blob)
    : createImageBitmap(blob, { resizeWidth: ask.resizeWidth, resizeQuality: "medium" });
}

/**
 * Decodes pictures in the worker `make` starts, each answer matched to its
 * question; where the browser runs no workers, as in tests, on the page's
 * own thread instead.
 */
export class OffThreadDecoder {
  private readonly worker: Worker | null;
  private next = 0;
  private readonly waiting = new Map<number, { resolve: (bitmap: ImageBitmap) => void; reject: (error: Error) => void }>();

  constructor(make: () => Worker | null = startWorker) {
    this.worker = make();
    if (this.worker) {
      this.worker.onmessage = (message: MessageEvent<DecodeAnswer>) => {
        const answer = message.data;
        const asked = this.waiting.get(answer.id);
        this.waiting.delete(answer.id);
        if ("bitmap" in answer) {
          asked?.resolve(answer.bitmap);
          // Asked for by nobody any more: let go of it.
          if (!asked) {
            answer.bitmap.close();
          }
        } else {
          asked?.reject(new Error(answer.error));
        }
      };
    }
  }

  /** `bytes` decoded as `type`, scaled down to `resizeWidth` if given; `bytes` can't be read after. */
  decode(bytes: Uint8Array<ArrayBuffer>, type: string, resizeWidth: number | null): Promise<ImageBitmap> {
    if (!this.worker) {
      return decode({ bytes, type, resizeWidth });
    }
    const id = this.next++;
    const worker = this.worker;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      const ask: DecodeAsk = { id, bytes, type, resizeWidth };
      worker.postMessage(ask, [bytes.buffer]);
    });
  }

  /** Stops the worker; pictures still being decoded are never answered. */
  stop() {
    this.worker?.terminate();
    this.waiting.clear();
  }
}

function startWorker(): Worker | null {
  return typeof Worker === "undefined" ? null : new Worker(new URL("./pictures-worker.ts", import.meta.url), { type: "module" });
}
