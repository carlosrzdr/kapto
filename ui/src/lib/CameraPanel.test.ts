import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import CameraPanel from "./CameraPanel.svelte";
import { LATE_MS, type Shown } from "./viewer/cameras";

const bitmap = (width: number, height: number) => ({ width, height, close: vi.fn() }) as unknown as ImageBitmap;

const camera = (changes: Partial<Shown>): Shown => ({
  stream: 1,
  name: "wrist_camera",
  bitmap: null,
  delayMs: null,
  unshown: null,
  pictures: 0,
  ...changes,
});

describe("CameraPanel", () => {
  it("draws each camera's newest picture at its size, with its name, and its delay only on hover while it is short", () => {
    render(CameraPanel, {
      cameras: [
        camera({ bitmap: bitmap(640, 480), delayMs: 23.6, pictures: 3 }),
        camera({ stream: 2, name: "head_camera", bitmap: bitmap(320, 240), delayMs: 8, pictures: 1 }),
      ],
    });
    const canvas = (name: string) =>
      screen.getByRole("img", { name: `${name}, its newest picture` }).querySelector("canvas") as HTMLCanvasElement;
    expect([canvas("wrist_camera").width, canvas("wrist_camera").height]).toEqual([640, 480]);
    expect(canvas("head_camera").width).toBe(320);
    expect(screen.queryByText(/ ms$/)).toBeNull();
    const caption = screen.getByText("wrist_camera").closest("figcaption");
    expect(caption?.title).toMatch(/^24 ms from Kapto receiving the frame/);
    expect(caption?.closest("figure")?.dataset.delayMs).toBe("24");
    expect(screen.getByText("head_camera").closest("figure")?.dataset.delayMs).toBe("8");
  });

  it("says a camera's delay in its caption, in the warning colour, once it is long", () => {
    render(CameraPanel, { cameras: [camera({ bitmap: bitmap(640, 480), delayMs: LATE_MS + 30.4, pictures: 1 })] });
    expect(screen.getByText(`${LATE_MS + 30} ms`).classList.contains("late")).toBe(true);
  });

  it("says on hover what became of a camera's frames on their way to the live view, only what happened", () => {
    const tally = { offered: 200, skipped: 0, superseded: 10, not_sent: 4, sent: 186 };
    render(CameraPanel, { cameras: [camera({ bitmap: bitmap(640, 480) })], tallies: { wrist_camera: tally } });
    expect(screen.getByText("wrist_camera").closest("figcaption")?.title).toBe(
      "Live view: 186 of 200 frames sent · 10 replaced by newer ones while the viewer was slow · 4 while no viewer listened",
    );
    // A few frames the page missed: not worth saying in the caption.
    expect(screen.queryByText(/not shown$/)).toBeNull();
  });

  it("says in the caption how many frames the live view didn't show, once they are many", () => {
    const tally = { offered: 200, skipped: 12, superseded: 19, not_sent: 0, sent: 169 };
    render(CameraPanel, { cameras: [camera({ bitmap: bitmap(640, 480) })], tallies: { wrist_camera: tally } });
    expect(screen.getByText("31 not shown").classList.contains("missed")).toBe(true);
    expect(screen.getByText("wrist_camera").closest("figcaption")?.title).toContain("12 skipped");
  });

  it("waits for a camera's first picture, and says why one can't be shown", () => {
    render(CameraPanel, {
      cameras: [
        camera({}),
        camera({ stream: 2, name: "depth", unshown: "raw pictures in `16UC1`, which the live view can't show" }),
      ],
    });
    expect(screen.getByText("Waiting for pictures…")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain("16UC1");
    expect(screen.queryAllByRole("img")).toHaveLength(0);
  });

  it("in a replay, says which frame shows and steps to the one before or after", async () => {
    const onstep = vi.fn();
    render(CameraPanel, {
      cameras: [camera({ bitmap: bitmap(640, 480), frame: { n: 11, of: 300 }, pictures: 1 })],
      onstep,
    });
    expect(screen.getByText("frame 12 of 300")).toBeTruthy();
    expect(screen.getByRole("img", { name: "wrist_camera, frame 12" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "wrist_camera: the frame after" }));
    await fireEvent.click(screen.getByRole("button", { name: "wrist_camera: the frame before" }));
    expect(onstep.mock.calls).toEqual([
      [1, 1],
      [1, -1],
    ]);
  });

  it("offers no steps in the live view", () => {
    render(CameraPanel, { cameras: [camera({ bitmap: bitmap(640, 480) })] });
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });
});

