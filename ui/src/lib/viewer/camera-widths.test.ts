import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { type CameraWidth, CameraWidths, SETTLED_MS } from "./camera-widths";

describe("the widths the page draws its cameras at", () => {
  let said: CameraWidth[][];
  let widths: CameraWidths;
  beforeEach(() => {
    vi.useFakeTimers();
    said = [];
    widths = new CameraWidths((cameras) => said.push(cameras));
  });
  afterEach(() => {
    widths.stop();
    vi.useRealTimers();
  });

  it("are said for every camera once the panels settle, rounded up, 0 for one not drawn", () => {
    widths.connected([1, 2]);
    widths.drawn({}, 1, 601);
    vi.advanceTimersByTime(SETTLED_MS - 1);
    expect(said).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(said).toEqual([
      [
        { stream: 1, width: 640 },
        { stream: 2, width: 0 },
      ],
    ]);
  });

  it("are said once a drag ends, not at each movement, and not again unchanged", () => {
    widths.connected([1]);
    const panel = {};
    for (let width = 300; width <= 700; width += 10) {
      widths.drawn(panel, 1, width);
      vi.advanceTimersByTime(16);
    }
    vi.advanceTimersByTime(SETTLED_MS);
    expect(said).toEqual([[{ stream: 1, width: 768 }]]);
    widths.drawn(panel, 1, 700);
    vi.advanceTimersByTime(SETTLED_MS);
    expect(said).toHaveLength(1);
  });

  it("take a camera's widest panel, and its next once that one goes", () => {
    widths.connected([1]);
    const [wide, narrow] = [{}, {}];
    widths.drawn(wide, 1, 1000);
    widths.drawn(narrow, 1, 300);
    vi.advanceTimersByTime(SETTLED_MS);
    widths.drawn(wide, 1, 0);
    vi.advanceTimersByTime(SETTLED_MS);
    expect(said).toEqual([[{ stream: 1, width: 1024 }], [{ stream: 1, width: 384 }]]);
  });

  it("are said again to a new connection", () => {
    widths.connected([1]);
    widths.drawn({}, 1, 500);
    vi.advanceTimersByTime(SETTLED_MS);
    widths.connected([1]);
    vi.advanceTimersByTime(SETTLED_MS);
    expect(said).toEqual([[{ stream: 1, width: 512 }], [{ stream: 1, width: 512 }]]);
  });

  it("say nothing to a stream without cameras", () => {
    widths.connected([]);
    vi.advanceTimersByTime(SETTLED_MS);
    expect(said).toEqual([]);
  });
});
