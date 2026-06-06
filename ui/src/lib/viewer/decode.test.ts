import { describe, expect, it } from "vitest";

import { decodeMesh } from "./mesh";
import { decodeFrame, decodeStretch } from "./stream";

/** Little-endian bytes, written field after field as the specifications list them. */
class Bytes {
  private parts: number[] = [];
  private view = new DataView(new ArrayBuffer(8));
  text(value: string) {
    for (const char of value) this.parts.push(char.charCodeAt(0));
    return this;
  }
  private put(size: number, write: (view: DataView) => void) {
    write(this.view);
    for (let index = 0; index < size; index++) this.parts.push(this.view.getUint8(index));
    return this;
  }
  u8(value: number) {
    return this.put(1, (view) => view.setUint8(0, value));
  }
  u16(value: number) {
    return this.put(2, (view) => view.setUint16(0, value, true));
  }
  u32(value: number) {
    return this.put(4, (view) => view.setUint32(0, value, true));
  }
  i64(value: bigint) {
    return this.put(8, (view) => view.setBigInt64(0, value, true));
  }
  f32(value: number) {
    return this.put(4, (view) => view.setFloat32(0, value, true));
  }
  f64(value: number) {
    return this.put(8, (view) => view.setFloat64(0, value, true));
  }
  buffer(): ArrayBuffer {
    return new Uint8Array(this.parts).buffer;
  }
}

describe("decodeMesh", () => {
  it("reads each part with only what it has", () => {
    const bytes = new Bytes().text("KMSH").u32(1).u32(2);
    // A triangle with normals and a colour of its own.
    bytes.u32(3).u32(1).u32(0b101);
    [1, 0.5, 0, 1].forEach((channel) => bytes.f32(channel));
    [0, 0, 0, 1, 0, 0, 0, 1, 0].forEach((value) => bytes.f32(value));
    [0, 0, 1, 0, 0, 1, 0, 0, 1].forEach((value) => bytes.f32(value));
    [0, 1, 2].forEach((index) => bytes.u32(index));
    // One with vertex colours only.
    bytes.u32(3).u32(1).u32(0b010);
    [0, 0, 0, 0].forEach((channel) => bytes.f32(channel));
    [0, 0, 0, 0, 0, 0, 0, 0, 0].forEach((value) => bytes.f32(value));
    [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255].forEach((channel) => bytes.u8(channel));
    [2, 1, 0].forEach((index) => bytes.u32(index));

    const [first, second] = decodeMesh(bytes.buffer());
    expect(Array.from(first!.positions)).toEqual([0, 0, 0, 1, 0, 0, 0, 1, 0]);
    expect(Array.from(first!.normals!)).toEqual([0, 0, 1, 0, 0, 1, 0, 0, 1]);
    expect(first!.colors).toBeNull();
    expect(first!.color).toEqual([1, 0.5, 0, 1]);
    expect(Array.from(first!.indices)).toEqual([0, 1, 2]);
    expect(second!.normals).toBeNull();
    expect(Array.from(second!.colors!.slice(0, 4))).toEqual([255, 0, 0, 255]);
    expect(second!.color).toBeNull();
    expect(Array.from(second!.indices)).toEqual([2, 1, 0]);
  });

  it("refuses what isn't a mesh this page reads", () => {
    expect(() => decodeMesh(new Bytes().text("KMSH").u32(2).u32(0).buffer())).toThrow(/version 2/);
    expect(() => decodeMesh(new Bytes().text("PNG!").u32(1).u32(0).buffer())).toThrow(/not a mesh/);
  });
});

describe("decodeFrame", () => {
  const ms = (value: number) => BigInt(value) * 1_000_000n;

  it("reads the poses, each sample's extremes and when it was received", () => {
    const bytes = new Bytes().text("KVFR").u16(1).u16(1).i64(ms(1_772_700_400_123)).i64(ms(1_772_700_400_128));
    bytes.u32(2).u32(1);
    [0, 0, 0, 0, 0, 0, 1, 0.1, 0.2, 0.3, 0, 0, 0.7071, 0.7071].forEach((value) => bytes.f32(value));
    bytes.u16(3).u16(0).u32(4).u32(2).i64(ms(1_772_700_400_100)).i64(ms(1_772_700_400_120));
    [-1, 1, 0.25, 0.5].forEach((value) => bytes.f64(value));

    const frame = decodeFrame(bytes.buffer());
    expect(frame.receivedMs).toBe(1_772_700_400_123);
    expect(frame.sentMs - frame.receivedMs).toBe(5);
    expect(frame.poses).toHaveLength(14);
    expect(frame.poses![7]).toBeCloseTo(0.1);
    expect(frame.samples).toEqual([
      {
        stream: 3,
        messages: 4,
        firstMs: 1_772_700_400_100,
        newestMs: 1_772_700_400_120,
        extremes: new Float64Array([-1, 1, 0.25, 0.5]),
      },
    ]);
  });

  it("has no poses when the robot didn't move, and refuses another kind of message", () => {
    const still = new Bytes().text("KVFR").u16(1).u16(0).i64(0n).i64(0n).u32(0).u32(0);
    expect(decodeFrame(still.buffer()).poses).toBeNull();
    const other = new Bytes().text("KVHI").u16(1).u16(0).i64(0n).i64(0n).u32(0).u32(0);
    expect(() => decodeFrame(other.buffer())).toThrow(/not a frame/);
  });
});

describe("decodeStretch", () => {
  const ms = (value: number) => BigInt(value) * 1_000_000n;

  it("reads what the bridge keeps, each part's pose and each stream's merged parts", () => {
    const bytes = new Bytes().text("KVHI").u16(1).u16(0).u32(7);
    bytes.i64(ms(1_000)).i64(ms(61_000)).u32(1).u32(2);
    bytes.i64(ms(20_000));
    [0, 0, 0, 0, 0, 0, 1].forEach((value) => bytes.f32(value));
    bytes.i64(ms(40_000));
    [0.5, 0, 0, 0, 0, 0, 1].forEach((value) => bytes.f32(value));
    bytes.u32(1).u16(2).u16(0).u32(1).u32(2);
    bytes.i64(ms(10_000)).i64(ms(30_000)).u32(1200).u32(0).f64(-1).f64(1);
    bytes.i64(ms(30_010)).i64(ms(50_000)).u32(1199).u32(0).f64(-0.5).f64(3);

    const stretch = decodeStretch(bytes.buffer());
    expect(stretch.id).toBe(7);
    expect(stretch.keptMs).toEqual([1_000, 61_000]);
    expect(stretch.poses.map((pose) => pose.timeMs)).toEqual([20_000, 40_000]);
    expect(stretch.poses[1]!.poses[0]).toBeCloseTo(0.5);
    expect(stretch.streams).toHaveLength(1);
    expect(stretch.streams[0]!.stream).toBe(2);
    expect(stretch.streams[0]!.parts.map((part) => [part.firstMs, part.newestMs, part.messages])).toEqual([
      [10_000, 30_000, 1200],
      [30_010, 50_000, 1199],
    ]);
    expect(Array.from(stretch.streams[0]!.parts[1]!.extremes)).toEqual([-0.5, 3]);
  });

  it("says when the bridge keeps nothing yet, and refuses a frame", () => {
    const empty = new Bytes().text("KVHI").u16(1).u16(0).u32(1).i64(0n).i64(0n).u32(0).u32(0).u32(0);
    expect(decodeStretch(empty.buffer()).keptMs).toBeNull();
    const frame = new Bytes().text("KVFR").u16(1).u16(0).i64(0n).i64(0n).u32(0).u32(0);
    expect(() => decodeStretch(frame.buffer())).toThrow(/not a stretch/);
  });
});
