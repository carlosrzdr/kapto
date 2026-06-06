// What the live plots draw from: each stream's newest samples, every value's
// minimum and maximum, in arrays of a fixed size made once. However long the
// page stays open, a stream's plots hold at most `capacity` samples: the
// oldest is written over by the newest.

import type { Opening } from "./stream";

/** One plotted field of a stream: its name and which of the stream's values it holds. */
export interface PlotField {
  name: string;
  /** The values' positions in the stream's samples. */
  values: number[];
  /** Each value's name, e.g. `joint_positions[2]`. */
  names: string[];
}

/** A stream's plots: its fields, in decoding order, and its samples. */
export interface PlotStream {
  name: string;
  /** Its position in the opening message, which frames name it by. */
  index: number;
  fields: PlotField[];
  samples: SampleBuffer;
}

/** The samples a stream keeps: about 68 s at the tap's 60 a second. */
export const SAMPLES_KEPT = 4096;

/** A stream's newest samples, in arrays of a fixed size. */
export class SampleBuffer {
  readonly capacity: number;
  readonly values: number;
  private readonly times: Float64Array;
  private readonly extremes: Float64Array;
  private next = 0;
  private count = 0;
  /** What `stretch` hands out, made once and reused. */
  private readonly shownTimes: Float64Array;
  private readonly shown: Float64Array[];

  constructor(values: number, capacity = SAMPLES_KEPT) {
    this.capacity = capacity;
    this.values = values;
    this.times = new Float64Array(capacity);
    this.extremes = new Float64Array(capacity * values * 2);
    this.shownTimes = new Float64Array(capacity);
    this.shown = Array.from({ length: values * 2 }, () => new Float64Array(capacity));
  }

  /** How many samples it holds. */
  get size(): number {
    return this.count;
  }

  /** When the newest sample was received, or `null` if there is none. */
  get newestMs(): number | null {
    return this.count === 0 ? null : (this.times[(this.next - 1 + this.capacity) % this.capacity] ?? null);
  }

  /** Adds a sample received at `timeMs`, each value's minimum then maximum; the oldest goes if it is full. */
  push(timeMs: number, extremes: Float64Array) {
    this.times[this.next] = timeMs;
    this.extremes.set(extremes.subarray(0, this.values * 2), this.next * this.values * 2);
    this.next = (this.next + 1) % this.capacity;
    this.count = Math.min(this.count + 1, this.capacity);
  }

  /** Forgets every sample, e.g. when the stream starts again. */
  clear() {
    this.next = 0;
    this.count = 0;
  }

  /**
   * The samples from `fromMs` on, oldest first, as uPlot takes them: times
   * in seconds, then, for each of `values`, its minima and its maxima. The
   * arrays are views of the buffer's own, valid until the next call.
   */
  stretch(fromMs: number, values: number[]): Float64Array[] {
    let shown = 0;
    const oldest = (this.next - this.count + this.capacity) % this.capacity;
    for (let index = 0; index < this.count; index++) {
      const at = (oldest + index) % this.capacity;
      const time = this.times[at] ?? 0;
      if (time < fromMs) {
        continue;
      }
      this.shownTimes[shown] = time / 1000;
      values.forEach((value, place) => {
        const base = (at * this.values + value) * 2;
        this.shown[place * 2]![shown] = this.extremes[base] ?? NaN;
        this.shown[place * 2 + 1]![shown] = this.extremes[base + 1] ?? NaN;
      });
      shown++;
    }
    return [
      this.shownTimes.subarray(0, shown),
      ...values.flatMap((_, place) => [
        this.shown[place * 2]!.subarray(0, shown),
        this.shown[place * 2 + 1]!.subarray(0, shown),
      ]),
    ];
  }
}

/**
 * Each stream's plots from the stream's opening message: a field holding
 * one value is one plot, an array's values share one; values that aren't
 * plotted (counters, clocks) are left out, and a stream with none has no
 * plots.
 */
export function plotStreams(opening: Opening): PlotStream[] {
  return opening.streams
    .map((stream, index) => {
      const fields: PlotField[] = [];
      stream.values.forEach((value, position) => {
        if (!value.plotted) {
          return;
        }
        const name = value.name.replace(/\[\d+\]$/, "");
        const field = fields.at(-1);
        if (field && field.name === name) {
          field.values.push(position);
          field.names.push(value.name);
        } else {
          fields.push({ name, values: [position], names: [value.name] });
        }
      });
      return { name: stream.name, index, fields, samples: new SampleBuffer(stream.values.length) };
    })
    .filter((stream) => stream.fields.length > 0);
}
