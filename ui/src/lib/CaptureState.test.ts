import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import type { Status } from "./api-types";
import CaptureState from "./CaptureState.svelte";

function status(episode: string | null, closing: string | null = null): Status {
  return {
    capture: { episode, seconds: 75, streams: [], values: [], preflight: null, closing },
    viewer: { stream_port: 9877 },
  };
}

describe("CaptureState", () => {
  it("says whether capture records, and how long for", () => {
    const { rerender } = render(CaptureState, { status: status(null), unreachable: false });
    expect(screen.getByText("Live, not recording")).toBeTruthy();
    rerender({ status: status("e1"), unreachable: false });
    expect(screen.getByText(/^Recording · /)).toBeTruthy();
    // Unreachable, the health chip says so, not this too.
    rerender({ status: null, unreachable: true });
    expect(screen.queryByText(/reachable|Connecting/)).toBeNull();
  });

  it("says a replay is shown, and when it was recorded, rather than live", () => {
    render(CaptureState, { status: status(null), unreachable: false, replay: { label: "15:02:12", shown: true } });
    const said = screen.getByText("Replaying · 15:02:12");
    expect(said.classList.contains("replaying")).toBe(true);
    expect(screen.queryByText(/Live, not recording/)).toBeNull();
  });

  it("says recording first while a replay is shown", () => {
    render(CaptureState, { status: status("e1"), unreachable: false, replay: { label: "15:02:12", shown: true } });
    expect(screen.getByText(/^Recording · /)).toBeTruthy();
    expect(screen.getByText("· replaying 15:02:12")).toBeTruthy();
  });

  it("stays live while a replay is prepared, offering the one way back", async () => {
    const onlive = vi.fn();
    const { rerender } = render(CaptureState, {
      status: status(null),
      unreachable: false,
      replay: { label: "15:02:12", shown: false },
      onlive,
    });
    expect(screen.getByText(/Live, not recording/)).toBeTruthy();
    expect(screen.queryByText(/Replaying/)).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Back to live" }));
    expect(onlive).toHaveBeenCalledOnce();
    // Back to live, it goes.
    rerender({ status: status(null), unreachable: false, replay: null, onlive });
    expect(screen.queryByRole("button", { name: "Back to live" })).toBeNull();
  });

  it("says an episode is being finalised while capture goes on, and which", () => {
    const { rerender } = render(CaptureState, { status: status(null, "e1"), unreachable: false });
    const note = screen.getByText("· finalising the last episode");
    expect(note.title).toContain("Episode e1 is being finalised");
    expect(screen.getByText(/Live, not recording/)).toBeTruthy();
    // Recording the next meanwhile: both said.
    rerender({ status: status("e2", "e1"), unreachable: false });
    expect(screen.getByText(/^Recording · /)).toBeTruthy();
    expect(screen.getByText("· finalising the last episode")).toBeTruthy();
    rerender({ status: status("e2"), unreachable: false });
    expect(screen.queryByText("· finalising the last episode")).toBeNull();
  });
});
