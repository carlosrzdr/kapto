import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import Timeline from "./Timeline.svelte";

function props(overrides: Record<string, unknown> = {}) {
  return {
    paused: false,
    keptMs: [1_000_000, 1_600_000] as [number, number],
    cursorMs: null,
    spanMs: 30_000,
    onpause: vi.fn(),
    onlive: vi.fn(),
    onseek: vi.fn(),
    onspan: vi.fn(),
    ...overrides,
  };
}

describe("Timeline", () => {
  it("follows now until paused", async () => {
    const shown = props();
    render(Timeline, shown);
    expect(screen.getByText("now")).toBeTruthy();
    const slider = screen.getByRole("slider", { name: "Moment shown" }) as HTMLInputElement;
    expect(Number(slider.value)).toBe(1_600_000);
    await fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(shown.onpause).toHaveBeenCalledOnce();
  });

  it("paused, says how far back it shows and goes back to now", async () => {
    const shown = props({ paused: true, cursorMs: 1_580_000 });
    render(Timeline, shown);
    expect(screen.getByText("−0:20")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Back to now" }));
    expect(shown.onlive).toHaveBeenCalledOnce();
  });

  it("seeks where the slider is moved, and shows the span chosen", async () => {
    const shown = props();
    render(Timeline, shown);
    await fireEvent.input(screen.getByRole("slider", { name: "Moment shown" }), { target: { value: "1300000" } });
    expect(shown.onseek).toHaveBeenCalledWith(1_300_000);
    await fireEvent.change(screen.getByRole("combobox", { name: "Span" }), { target: { value: "120000" } });
    expect(shown.onspan).toHaveBeenCalledWith(120_000);
  });

  it("replaying, says where in the episode it is, of how long, and goes back to live", async () => {
    const shown = props({ paused: true, cursorMs: 1_180_000, replaying: true });
    render(Timeline, shown);
    expect(screen.getByText("3:00")).toBeTruthy();
    expect(screen.getByText("of 10:00")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Back to live" }));
    expect(shown.onlive).toHaveBeenCalledOnce();
  });

  it("can't be paused before anything is kept", () => {
    render(Timeline, props({ keptMs: null }));
    expect((screen.getByRole("button", { name: "Pause" }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("Timeline's reach", () => {
  it("says how far back can be looked, and what bounds it", () => {
    const { unmount } = render(Timeline, props({ historySeconds: 600 }));
    const kept = screen.getByText("10:00 kept");
    expect(kept.getAttribute("title")).toContain("up to the last 10:00 (viewer.history_age)");
    expect(kept.getAttribute("title")).toContain("viewer.history (MiB)");
    unmount();
    render(Timeline, props({ paused: true, cursorMs: 1_580_000 }));
    expect(screen.getByText("of 10:00")).toBeTruthy();
  });
});

describe("Timeline's marks", () => {
  it("draws what happened along the slider and shows one when clicked", async () => {
    const shown = props({
      items: [
        { kind: "episode", startMs: 1_100_000, endMs: 1_100_000, label: "Episode started" },
        { kind: "grasp", startMs: 1_200_000, endMs: null, label: "grasp (page)" },
        { kind: "note", startMs: 900_000, endMs: 900_000, label: "before what is kept" },
      ],
      colors: { grasp: "#123456" },
    });
    render(Timeline, shown);
    const marks = screen.getByRole("group", { name: "What happened" });
    expect(marks.getAttribute("data-marks")).toBe("2");
    const grasp = screen.getByRole("button", { name: "grasp (page)" }).querySelector("rect.shown")!;
    expect(grasp.getAttribute("fill")).toBe("#123456");
    // Open until it ends: drawn to now.
    expect(Number(grasp.getAttribute("x")) + Number(grasp.getAttribute("width"))).toBeCloseTo(1000);
    await fireEvent.click(screen.getByRole("button", { name: "Episode started" }));
    expect(shown.onseek).toHaveBeenCalledWith(1_100_000);
  });

  it("says what a mark is, when and for how long, while it is pointed at or focused", async () => {
    const items = [{ kind: "grasp", startMs: 1_200_000, endMs: 1_202_400, label: "grasp (page)" }];
    render(Timeline, props({ items }));
    const mark = screen.getByRole("button", { name: "grasp (page)" });
    const tip = screen.getByRole("tooltip", { hidden: true });
    expect(tip.hidden).toBe(true);
    await fireEvent.pointerEnter(mark);
    expect(tip.hidden).toBe(false);
    expect(tip.textContent).toContain("grasp (page)");
    expect(tip.textContent).toMatch(/\d{2}:\d{2}:\d{2}\.\d{3} · for 2\.4 s/);
    expect(mark.getAttribute("aria-describedby")).toBe(tip.id);
    await fireEvent.pointerLeave(mark);
    expect(tip.hidden).toBe(true);
    await fireEvent.focus(mark);
    expect(tip.hidden).toBe(false);
    await fireEvent.blur(mark);
    expect(tip.hidden).toBe(true);
  });
});

describe("Timeline's playback", () => {
  it("paused, plays on, holds again, and changes speed", async () => {
    const onplay = vi.fn();
    const onspeed = vi.fn();
    const { unmount } = render(Timeline, props({ paused: true, cursorMs: 1_580_000, onplay, onspeed }));
    await fireEvent.click(screen.getByRole("button", { name: "Play" }));
    expect(onplay).toHaveBeenCalledOnce();
    await fireEvent.change(screen.getByRole("combobox", { name: "Speed" }), { target: { value: "2" } });
    expect(onspeed).toHaveBeenCalledWith(2);
    unmount();
    const onhold = vi.fn();
    render(Timeline, props({ paused: true, playing: true, cursorMs: 1_580_000, onhold }));
    await fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(onhold).toHaveBeenCalledOnce();
  });

  it("live, offers no play or speed: only pausing", () => {
    render(Timeline, props());
    expect(screen.queryByRole("button", { name: "Play" })).toBeNull();
    expect(screen.queryByRole("combobox", { name: "Speed" })).toBeNull();
  });

  it("opens an event to watch it, playing on", async () => {
    const onwatch = vi.fn();
    const onseek = vi.fn();
    const items = [{ kind: "note", label: "note (api)", startMs: 1_300_000, endMs: 1_300_000 }];
    render(Timeline, props({ items, onwatch, onseek }));
    await fireEvent.click(screen.getByRole("button", { name: "note (api)" }));
    expect(onwatch).toHaveBeenCalledWith(1_300_000);
    expect(onseek).not.toHaveBeenCalled();
  });
});
