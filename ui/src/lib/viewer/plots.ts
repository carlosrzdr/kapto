// What the live plots draw from: each stream's newest samples, every value's
// minimum and maximum, in arrays of a fixed size made once. However long the
// page stays open, a stream's plots hold at most `capacity` samples: the
// oldest is written over by the newest.

import { clock, figure } from "../format";
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

/** What a chart says at the time under the pointer. */
export interface ValuesAt {
  /** The sample's time, `14:46:05.250`. */
  time: string;
  /** Each value: its label and what it was, a range while it changed. */
  values: { label: string; value: string }[];
}

/** How far apart, in pixels, a value's minimum and maximum are drawn. */
export type Spread = (low: number, high: number) => number;

/**
 * The pixels a sample's range must span for the tip to give the range
 * rather than one value: anything less is the line's own width, as a value
 * moving smoothly changes within the few milliseconds a sample covers.
 */
export const RANGE_PIXELS = 3;

/**
 * The values of the sample at `index` of a chart's `data` (uPlot's: times
 * in seconds, then each value's minima and maxima), named by `labels`.
 * Each is one value, the middle of its range, unless `spread` draws that
 * range over at least [`RANGE_PIXELS`]: a jump within the sample, given as
 * `0.12 to 0.95`.
 */
export function valuesAt(
  data: ArrayLike<number | null | undefined>[],
  index: number,
  labels: string[],
  spread: Spread = () => Number.POSITIVE_INFINITY,
): ValuesAt {
  return {
    time: clock((data[0]?.[index] ?? Number.NaN) * 1000),
    values: labels.map((label, place) => {
      const low = data[1 + place * 2]?.[index] ?? Number.NaN;
      const high = data[2 + place * 2]?.[index] ?? Number.NaN;
      const one = figure(low) === figure(high) || !(spread(low, high) >= RANGE_PIXELS);
      const value = one ? figure((low + high) / 2) : `${figure(low)} to ${figure(high)}`;
      return { label, value };
    }),
  };
}

/**
 * The stretch a plot of `widthPx` shows, `spanMs` long and ending at
 * `endMs`, in seconds as uPlot takes them. Its end is moved back to a whole
 * pixel, so that, drawn again as samples arrive, the curve moves by whole
 * pixels and keeps its shape rather than shimmering.
 */
export function shownStretch(endMs: number, spanMs: number, widthPx: number): [number, number] {
  const msPerPixel = spanMs / widthPx;
  const end = widthPx > 0 ? Math.floor(endMs / msPerPixel) * msPerPixel : endMs;
  return [(end - spanMs) / 1000, end / 1000];
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

/** What one plot panel draws of one stream: some of its values, each with its label. */
export interface PlotPart {
  stream: PlotStream;
  /** The values' positions in the stream's samples. */
  values: number[];
  /** Each value's label: the role it plays, or its name. */
  labels: string[];
}

/**
 * What a layout's plot panel draws: each of `fields` (`stream.field`,
 * `stream.field[3]` or `role:gripper.opening`, a role's field found in
 * `roles`) among the values `streams` plot, grouped by stream in the order
 * first named. A field naming nothing plotted is left out, as the panel
 * says what its layout lacks.
 */
export function resolvePlot(fields: string[], roles: Record<string, string>, streams: PlotStream[]): PlotPart[] {
  const parts: PlotPart[] = [];
  for (const named of fields) {
    const role = named.startsWith("role:") ? named.slice("role:".length) : null;
    const reference = role === null ? named : roles[role];
    const match = reference?.match(/^([^.]+)\.([^[]+)(?:\[(\d+)\])?$/);
    const stream = streams.find((each) => each.name === match?.[1]);
    const field = stream?.fields.find((each) => each.name === match?.[2]);
    if (!match || !stream || !field) {
      continue;
    }
    const picked = match[3] === undefined ? field.names.map((_, place) => place) : [Number(match[3])];
    for (const place of picked) {
      const value = field.values[place];
      const name = field.names[place];
      if (value === undefined || name === undefined) {
        continue;
      }
      let part = parts.find((each) => each.stream === stream);
      if (!part) {
        part = { stream, values: [], labels: [] };
        parts.push(part);
      }
      part.values.push(value);
      part.labels.push(role ?? name);
    }
  }
  return parts;
}

/**
 * Labels made short for a panel's key: the dot-separated segments every
 * label shares at its start and end (`joints.` and `.position` of
 * `joints.joint_1.position`) said once as the heading, each label keeping
 * what tells it apart. A single label, or labels sharing nothing, are kept.
 */
export function shortLabels(labels: string[]): { heading: string; labels: string[] } {
  const split = labels.map((label) => label.split("."));
  const shortest = Math.min(...split.map((segments) => segments.length));
  if (labels.length < 2) {
    return { heading: "", labels };
  }
  const same = (at: (segments: string[]) => string | undefined) => split.every((segments) => at(segments) === at(split[0]!));
  let start = 0;
  while (start < shortest - 1 && same((segments) => segments[start])) {
    start++;
  }
  let end = 0;
  while (end < shortest - 1 - start && same((segments) => segments[segments.length - 1 - end])) {
    end++;
  }
  const first = split[0]!;
  return {
    heading: [...first.slice(0, start), ...first.slice(first.length - end)].join(" · "),
    labels: split.map((segments) => segments.slice(start, segments.length - end).join(".")),
  };
}
