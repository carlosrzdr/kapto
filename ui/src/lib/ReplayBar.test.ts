import { fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ReplayStatus } from "./api-types";
import ReplayBar from "./ReplayBar.svelte";

const episode = "20260310T120000.000Z-00000001";
const label = "2026-03-10 14:00:00";

describe("ReplayBar", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("says the replay is being prepared until it is made", () => {
    const replay: ReplayStatus = { episode, state: "queued", lacks: [] };
    render(ReplayBar, { replay, label });
    expect(screen.getByRole("status", { name: "Replay" }).textContent).toContain(
      `Preparing the replay of the episode recorded ${label}…`,
    );
  });

  it("while the replay is prepared, shows a bar that can't say how far along it is, and the time taken so far", async () => {
    vi.useFakeTimers();
    const replay: ReplayStatus = { episode, state: "queued", lacks: [] };
    const { rerender } = render(ReplayBar, { replay, label });
    const progress = screen.getByRole("progressbar", { name: "Preparing the replay" }) as HTMLProgressElement;
    // No value: indeterminate.
    expect(progress.hasAttribute("value")).toBe(false);
    expect(progress.getAttribute("aria-valuetext")).toBe("0:00 so far");
    await vi.advanceTimersByTimeAsync(5_000);
    expect(screen.getByRole("status", { name: "Replay" }).textContent).toContain("0:05");
    // Still the same replay, now being made: the time goes on from when it was asked for.
    await rerender({ replay: { ...replay, state: "making" }, label });
    await vi.advanceTimersByTimeAsync(70_000);
    expect(progress.getAttribute("aria-valuetext")).toBe("1:15 so far");
    // Made: the bar and the time go.
    await rerender({ replay: { ...replay, state: "ready" }, label });
    expect(screen.queryByRole("progressbar")).toBeNull();
    // Another episode's replay starts its own time.
    await rerender({ replay: { episode: "20260310T130000.000Z-00000002", state: "queued", lacks: [] }, label });
    expect(screen.getByRole("progressbar").getAttribute("aria-valuetext")).toBe("0:00 so far");
  });

  it("names the episode it shows and what the replay lacks", () => {
    const replay: ReplayStatus = {
      episode,
      state: "ready",
      bytes: 1000,
      lacks: [
        {
          code: "robot.mesh_unreadable",
          message: "meshes/base.stl isn't in the recording",
          remedy: "Record with capture.attach_meshes.",
        },
      ],
    };
    render(ReplayBar, { replay, label });
    const bar = screen.getByRole("status", { name: "Replay" });
    expect(bar.textContent).toContain(`Replay of the episode recorded ${label}`);
    // One line, the file and what to do folded under it.
    const lacks = bar.querySelector("details")!;
    expect(lacks.open).toBe(false);
    expect(lacks.querySelector("summary")?.textContent).toBe("The robot is drawn without one of its meshes, which couldn't be read.");
    expect(lacks.querySelector(".remedy")?.textContent).toBe("Record with capture.attach_meshes.");
    // The message ended as a sentence.
    expect(lacks.querySelector("li")?.textContent).toContain("meshes/base.stl isn't in the recording.");
  });

  it("says in one line what else it lacks, each with its own remedy folded under it", () => {
    const replay: ReplayStatus = {
      episode,
      state: "ready",
      lacks: [
        { code: "robot.mesh_unreadable", message: "meshes/base.stl isn't in the recording", remedy: "Record with capture.attach_meshes." },
        { code: "viewer.replay_layout", message: "The layout couldn't be read", remedy: "Check the layout." },
      ],
    };
    render(ReplayBar, { replay, label });
    const lacks = screen.getByRole("status", { name: "Replay" }).querySelector("details")!;
    expect(lacks.querySelector("summary")?.textContent).toBe("The replay lacks 2 things it was recorded with.");
    expect(Array.from(lacks.querySelectorAll("li"), (item) => item.textContent?.replace(/\s+/g, " ").trim())).toEqual([
      "meshes/base.stl isn't in the recording. Record with capture.attach_meshes. robot.mesh_unreadable",
      "The layout couldn't be read. Check the layout. viewer.replay_layout",
    ]);
  });

  it("says each mesh it lacks once, however many it lacks and however often each is named", () => {
    const missing = (file: string) => ({
      code: "robot.mesh_unreadable",
      message: `Can't read the mesh file: meshes/${file} isn't in the recording`,
      remedy: "Record with capture.attach_meshes.",
    });
    const replay: ReplayStatus = {
      episode,
      state: "ready",
      bytes: 1000,
      // A link draws and collides with the same file: it is missing twice.
      lacks: [missing("pelvis.STL"), missing("pelvis.STL"), missing("knee.STL"), missing("hip.STL")],
    };
    render(ReplayBar, { replay, label });
    const bar = screen.getByRole("status", { name: "Replay" });
    expect(bar.querySelector("summary")?.textContent).toBe("The robot is drawn without 3 of its meshes, which couldn't be read.");
    const lines = Array.from(bar.querySelectorAll(".lacks li"), (line) => line.textContent ?? "");
    expect(lines).toHaveLength(3);
    expect(lines.filter((line) => line.includes("pelvis.STL"))).toHaveLength(1);
  });

  it("says why a replay couldn't be made, with its cause code", () => {
    const replay: ReplayStatus = {
      episode,
      state: "failed",
      lacks: [],
      failure: { code: "viewer.replay_disk_low", message: "No room to make the replay", remedy: "Free space." },
    };
    render(ReplayBar, { replay, label });
    const bar = screen.getByRole("status", { name: "Replay" });
    expect(bar.textContent).toContain("couldn't be made");
    expect(bar.textContent).toContain("No room to make the replay. Free space.");
    expect(bar.textContent).toContain("viewer.replay_disk_low");
  });

  it("says where the replay is playing, and when it is over offers to play it again", async () => {
    const replay: ReplayStatus = { episode, state: "ready", lacks: [] };
    const onplayagain = vi.fn();
    const playing = { playing: true, at: 4.6, length: 9.2, over: false };
    const { rerender } = render(ReplayBar, { replay, label, playback: playing, onplayagain });
    const bar = screen.getByRole("status", { name: "Replay" });
    expect(bar.textContent).toContain("Playing 0:04 of 0:09");
    expect(screen.queryByRole("button", { name: "Play again" })).toBeNull();
    await rerender({ replay, label, playback: { ...playing, playing: false }, onplayagain });
    expect(bar.textContent).toContain("Paused at 0:04 of 0:09");
    await rerender({ replay, label, playback: { playing: false, at: 9.2, length: 9.2, over: true }, onplayagain });
    expect(bar.textContent).toContain("Episode over · 0:09");
    await fireEvent.click(screen.getByRole("button", { name: "Play again" }));
    expect(onplayagain).toHaveBeenCalledOnce();
    // Playing again, the note goes.
    await rerender({ replay, label, playback: { ...playing, at: 0.1 }, onplayagain });
    expect(bar.textContent).not.toContain("Episode over");
  });
});
