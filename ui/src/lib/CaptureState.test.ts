import { render, screen } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";

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
    rerender({ status: null, unreachable: true });
    expect(screen.getByText("Capture not reachable")).toBeTruthy();
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
