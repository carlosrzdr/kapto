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

  it("held, can't be pressed but still says what it would do", () => {
    render(RecordButton, { recording: false, busy: false, held: true, onstart: vi.fn(), onstop: vi.fn() });
    expect((screen.getByRole("button", { name: "Record" }) as HTMLButtonElement).disabled).toBe(true);
  });
});
