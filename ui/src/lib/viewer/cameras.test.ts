import { describe, expect, it, vi } from "vitest";

import { camerasOf, Decoder, withPicture, withSaid } from "./cameras";
import type { Opening, Picture } from "./stream";

const opening: Opening = {
  version: 1,
  links: [],
  streams: [
    { name: "arm_joints", values: [] },
    { name: "wrist_camera", values: [] },
  ],
  history_s: 600,
  cameras: [1],
};

const picture = (stream: number, receivedMs: number): Picture => ({
  stream,
  type: "image/jpeg",
  size: null,
  receivedMs,
  sentMs: receivedMs + 3,
  bytes: new Uint8Array([0xff, 0xd8]),
});

/** A decoded picture, as far as these tests need one. */
const bitmap = (name: string) => ({ name, width: 640, height: 480, close: vi.fn() }) as unknown as ImageBitmap;

describe("cameras", () => {
  it("are the streams the opening names cameras, by name, none shown yet", () => {
    expect(camerasOf(opening)).toEqual([
      { stream: 1, name: "wrist_camera", bitmap: null, delayMs: null, unshown: null, pictures: 0 },
    ]);
    // An older bridge names none.
    expect(camerasOf({ ...opening, cameras: undefined })).toEqual([]);
  });

  it("show each camera's newest picture with its delay, handing back the one it replaces", () => {
    const first = bitmap("first");
    let shown = withPicture(camerasOf(opening), picture(1, 1000), first, 1040);
    expect(shown.replaced).toBeNull();
    expect(shown.cameras[0]).toMatchObject({ bitmap: first, delayMs: 40, pictures: 1 });

    const second = bitmap("second");
    shown = withPicture(shown.cameras, picture(1, 1033), second, 1050);
    expect(shown.replaced).toBe(first);
    expect(shown.cameras[0]).toMatchObject({ bitmap: second, delayMs: 17, pictures: 2 });

    // A picture of a stream that isn't a camera is handed back unused.
    const stray = bitmap("stray");
    expect(withPicture(shown.cameras, picture(0, 1060), stray, 1070)).toEqual({ cameras: shown.cameras, replaced: stray });
  });

  it("say why a camera isn't shown, until it is again", () => {
    let cameras = withSaid(camerasOf(opening), { stream: 1, unshown: "raw pictures in `yuv422`" });
    expect(cameras[0]?.unshown).toBe("raw pictures in `yuv422`");
    cameras = withSaid(cameras, { stream: 1, unshown: null });
    expect(cameras[0]?.unshown).toBeNull();
  });
});

describe("Decoder", () => {
  it("decodes one picture per camera at a time, then only the newest that came meanwhile", async () => {
    const finish: ((bitmap: ImageBitmap) => void)[] = [];
    const decoded: number[] = [];
    const decoder = new Decoder(
      () => new Promise((resolve) => finish.push(resolve)),
      (shown) => decoded.push(shown.receivedMs),
    );
    decoder.add(picture(1, 1), 1);
    decoder.add(picture(1, 2), 2);
    decoder.add(picture(1, 3), 3);
    // Another camera isn't held up.
    decoder.add(picture(2, 10), 10);
    expect(finish).toHaveLength(2);
    finish[0]?.(bitmap("1"));
    await vi.waitFor(() => expect(finish).toHaveLength(3));
    finish[1]?.(bitmap("10"));
    finish[2]?.(bitmap("3"));
    await vi.waitFor(() => expect(decoded).toEqual([1, 10, 3]));
  });

  it("carries on past a picture the browser can't decode", async () => {
    const decoded: number[] = [];
    let calls = 0;
    const decoder = new Decoder(
      () => (calls++ === 0 ? Promise.reject(new Error("not a JPEG")) : Promise.resolve(bitmap("ok"))),
      (shown) => decoded.push(shown.receivedMs),
    );
    decoder.add(picture(1, 1), 1);
    decoder.add(picture(1, 2), 2);
    await vi.waitFor(() => expect(decoded).toEqual([2]));
  });
});
