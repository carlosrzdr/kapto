import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import RecordButton from "./RecordButton.svelte";

describe("RecordButton", () => {
  it("starts when idle and stops when recording", async () => {
    const onstart = vi.fn();
    const onstop = vi.fn();
    const { rerender } = render(RecordButton, { recording: false, busy: false, onstart, onstop });
    await fireEvent.click(screen.getByRole("button", { name: "Record" }));
    expect(onstart).toHaveBeenCalledOnce();

    await rerender({ recording: true, busy: false, onstart, onstop });
    await fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(onstop).toHaveBeenCalledOnce();
  });

  it("can't be pressed again while its request is on its way", () => {
    render(RecordButton, { recording: true, busy: true, onstart: vi.fn(), onstop: vi.fn() });
    const button = screen.getByRole("button", { name: "Finalising…" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });

  it("held, can't be pressed but still says what it would do, and why not", () => {
    const why = "Kapto can't reach the capture process, so nothing can be recorded";
    render(RecordButton, { recording: false, busy: false, held: why, onstart: vi.fn(), onstop: vi.fn() });
    const button = screen.getByRole("button", { name: "Record" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.title).toBe(`Record an episode (R): ${why}`);
    // Read out with it by a screen reader.
    expect(screen.getByRole("button", { description: why })).toBe(button);
    expect(button.getAttribute("aria-busy")).toBe("false");
  });

  it("waiting for its answer, says so and isn't held for another reason", () => {
    render(RecordButton, { recording: false, busy: true, held: "Pre-flight is running", onstart: vi.fn(), onstop: vi.fn() });
    const button = screen.getByRole("button", { name: "Starting…" });
    expect(button.getAttribute("aria-busy")).toBe("true");
    expect(button.title).toBe("Record an episode (R)");
    expect(button.getAttribute("aria-describedby")).toBeNull();
  });
});
