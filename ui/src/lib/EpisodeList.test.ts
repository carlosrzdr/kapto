import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import type {
  EpisodeDetail,
  EpisodeEvents,
  EpisodeQuality,
  EpisodeStatistics,
  EpisodeSummary,
  ReplayState,
  ReplayStatus,
} from "./api-types";
import { ApiFailure } from "./api";
import EpisodeList from "./EpisodeList.svelte";
import { offset } from "./format";

const summary: EpisodeSummary = {
  id: "20260310T120000.000Z-00000001",
  started_utc_ns: 1_772_700_400_000_000_000,
  ended_utc_ns: 1_772_700_475_000_000_000,
  kapto_version: "0.1.0",
  package: { id: "arm7", version: "1.0.0", hash: "9f2c1a2b3c4d5e6f" },
  streams: [{ stream: "arm_joints", received: 150_000, written: 150_000, dropped_by_us: 0, lost_upstream: 0 }],
  stored_here: true,
};

const detail: EpisodeDetail = {
  id: summary.id,
  folder: "/data/20260310T120000.000Z-00000001",
  stored_here: true,
  started_utc_ns: summary.started_utc_ns,
  ended_utc_ns: summary.ended_utc_ns,
  kapto_version: "0.1.0",
  package: summary.package,
  streams: [
    {
      stream: "arm_joints",
      loss_detection: "sequence",
      received: 150_000,
      decoded: 150_000,
      written: 150_000,
      dropped_by_us: 0,
      lost_upstream: 0,
      rejected: 4,
    },
    {
      stream: "base",
      loss_detection: "rate_gap",
      received: 7_000,
      decoded: 7_000,
      written: 7_000,
      dropped_by_us: 0,
      lost_upstream: 12,
      disconnects: 2,
    },
    {
      stream: "tf_static",
      loss_detection: "undetectable",
      received: 0,
      decoded: 0,
      written: 0,
      dropped_by_us: 0,
      lost_upstream: 0,
      carried: 2,
    },
  ],
  files: [
    { path: "recording.mcap", bytes: 9 * 1024 * 1024 },
    { path: "episode.json", bytes: 1024 * 1024 },
  ],
  buffers: { batches: 1022, peak_batches: 55, memory_bytes: 64 * 1024 * 1024 },
  durability: { sync_interval_ns: 500_000_000, syncs: 150, p99_unsynced_ns: 520_000_000, max_unsynced_ns: 519_000_000 },
  clock: { samples: 75, max_uncertainty_ns: 40_000, steps: 0 },
};

describe("EpisodeList", () => {
  it("asks to show an episode in full when it is clicked, and to close it again", async () => {
    const onselect = vi.fn();
    const { rerender } = render(EpisodeList, { episodes: [summary], open: null, onselect });
    const button = screen.getByTitle("Show this episode in full");
    expect(button.getAttribute("aria-expanded")).toBe("false");
    await fireEvent.click(button);
    expect(onselect).toHaveBeenCalledWith(summary.id);

    await rerender({ episodes: [summary], open: detail, onselect });
    expect(button.getAttribute("aria-expanded")).toBe("true");
    await fireEvent.click(button);
    expect(onselect).toHaveBeenLastCalledWith(null);
  });

  it("shows the open episode's size, durability, buffers and every stream's counts", () => {
    render(EpisodeList, { episodes: [summary], open: detail, onselect: vi.fn() });
    const details = screen.getByRole("region", { name: `Episode ${summary.id}` });
    expect(details.textContent).toContain("10 MiB in 2 files");
    expect(details.textContent).toContain("99 % within 520 ms");
    expect(details.textContent).toContain("peak 55 of 1022 batches");
    expect(details.textContent).toContain("steady");
    // Datagrams from elsewhere, said beside the loss: they aren't the robot's.
    const arm = screen.getByRole("row", { name: /^arm_joints/ });
    expect(arm.textContent).toContain("0 · 4 from sources not allowed");
    const base = screen.getByRole("row", { name: /^base/ });
    expect(base.textContent).toContain("12 · 2 disconnects");
    expect(base.textContent).toContain("silences (estimate)");
    expect(base.querySelectorAll("td")[3]?.classList.contains("warning")).toBe(true);
    const kept = screen.getByRole("row", { name: /^tf_static/ });
    expect(kept.querySelectorAll("td")[0]?.textContent?.trim()).toBe("0 + 2 kept");
  });

  it("says so when there are no episodes", () => {
    render(EpisodeList, { episodes: [], open: null, onselect: vi.fn() });
    expect(screen.getByText("No episodes recorded yet.")).toBeTruthy();
  });

  it("names the robot package an episode was recorded with, and marks one recorded without", async () => {
    const unchecked = { ...summary, id: "20260310T130000.000Z-00000002", package: null };
    render(EpisodeList, { episodes: [summary, unchecked], open: detail, onselect: () => {} });
    expect(screen.getByRole("region", { name: /^Episode / }).textContent).toContain("arm7 1.0.0 · 9f2c1a2b3c4d");
    expect(screen.getAllByText("unchecked")).toHaveLength(1);
  });

  it("shows an unchecked episode's details as such", () => {
    render(EpisodeList, { episodes: [summary], open: { ...detail, package: null }, onselect: () => {} });
    expect(screen.getByText("unchecked: recorded without a robot package")).toBeTruthy();
  });

  it("marks a recovered episode with its loss unknown, and shows how it was recovered in full", () => {
    const recovered = { at_utc_ns: 1_772_701_000_000_000_000, bytes_cut: 4096, last_message_utc_ns: 1_772_700_462_000_000_000, reason: "recording stopped" };
    const salvaged: EpisodeSummary = { ...summary, id: "20260310T130000.000Z-00000002", recovered };
    render(EpisodeList, { episodes: [summary, salvaged], open: { ...detail, id: salvaged.id, recovered }, onselect: vi.fn() });
    const [finished, cut] = screen.getAllByTitle("Show this episode in full");
    // A complete one says nothing of its loss; the recovered one does, in danger's colour.
    expect(finished?.textContent).not.toMatch(/recovered|loss unknown|missing|complete/);
    expect(cut?.textContent).toContain("recovered");
    expect(cut?.querySelector(".recovered")).toBeTruthy();
    expect(cut?.textContent).toContain("loss unknown");
    const details = screen.getByRole("region", { name: `Episode ${salvaged.id}` });
    expect(details.textContent).toContain("4.0 KiB cut off its end · last message at 1:02 · what it lost isn't known");
    expect(details.textContent).toContain("not known for a recovered episode, nor its buffers and clock");
    expect(details.textContent).not.toContain("steady");
  });

  it("names what an episode was recorded for and with, and shows both in full", () => {
    const recorded: EpisodeSummary = { ...summary, id: "20260310T120500.000Z-00000002", profile: "lab_ana", mode: "teleoperated", task: "pick_and_place", task_name: "Pick and place" };
    const full: EpisodeDetail = {
      ...detail,
      id: recorded.id,
      profile: { id: "lab_ana", hash: "ab12", operator: "Ana", mode: "teleoperated", tags: ["shift-a"] },
      task: { id: "pick_and_place", hash: "cd34", name: "Pick and place", prompt: "Pick up the red cube", objects: [], subtasks: ["reach", "grasp"] },
    };
    const { rerender } = render(EpisodeList, { episodes: [summary, recorded], open: null, onselect: vi.fn() });
    const [without, withSession] = screen.getAllByTitle("Show this episode in full");
    expect(without?.textContent).not.toContain("·");
    expect(withSession?.textContent).toContain("Pick and place · lab_ana");

    void rerender({ episodes: [recorded], open: full, onselect: vi.fn() });
    return Promise.resolve().then(() => {
      const details = screen.getByRole("region", { name: `Episode ${recorded.id}` });
      expect(details.textContent).toContain("Pick and place · “Pick up the red cube” · reach → grasp");
      expect(details.textContent).toContain("lab_ana · Ana · teleoperated · shift-a");
    });
  });

  it("says whether the quality checks accepted each episode, and lists only those asked for", async () => {
    const rejected = { ...summary, id: "20260310T120500.000Z-00000002", accepted: false };
    const accepted = { ...summary, id: "20260310T121000.000Z-00000003", accepted: true };
    const pending = { ...summary, id: "20260310T121500.000Z-00000004", accepted: null };
    const onfilter = vi.fn();
    render(EpisodeList, { episodes: [rejected, accepted, pending], open: null, onselect: vi.fn(), onfilter });
    const [first, second, third] = screen.getAllByTitle("Show this episode in full");
    expect(first?.textContent).toContain("rejected");
    expect(first?.querySelector(".rejected")).toBeTruthy();
    expect(second?.textContent).toContain("accepted");
    expect(third?.textContent).toContain("not checked yet");

    await fireEvent.change(screen.getByRole("combobox", { name: "Quality" }), { target: { value: "rejected" } });
    expect(onfilter).toHaveBeenCalledWith("rejected");
  });

  it("says when no episode is of the kind asked for", () => {
    render(EpisodeList, { episodes: [], open: null, filter: "rejected", onselect: vi.fn() });
    expect(screen.getByText("No rejected episodes.")).toBeTruthy();
  });

  it("shows what each quality check found on the open episode, and a failed gate as a problem", () => {
    const quality: EpisodeQuality = {
      accepted: false,
      params_version: "7",
      computed_utc_ns: 1_772_700_500_000_000_000,
      checks: [
        {
          check: "streams.complete",
          version: 1,
          kind: "gate",
          verdict: "failed",
          message: "Kapto dropped 12 messages: arm_joints (12 of 2000 received)",
          values: { dropped_by_us: 12 },
          ranges: [],
        },
      ],
    };
    render(EpisodeList, { episodes: [summary], open: detail, quality, onselect: vi.fn() });
    const details = screen.getByRole("region", { name: `Episode ${summary.id}` });
    expect(details.textContent).toContain("rejected: a gate failed · settings 7");
    const check = screen.getByRole("row", { name: /^streams\.complete/ });
    expect(check.textContent).toContain("gate");
    expect(check.textContent).toContain("Kapto dropped 12 messages");
    // A failed gate rejects the episode: danger, the same as Delete's.
    expect(check.querySelector("td")?.classList.contains("danger")).toBe(true);
    expect(within(details).getByText(/rejected: a gate failed/).classList.contains("danger")).toBe(true);
  });

  it("lists the moments a check found, each opening its replay there, and counts those past the first five", async () => {
    const at = (second: number) => ({
      start_utc_ns: summary.started_utc_ns + second * 1e9,
      end_utc_ns: summary.started_utc_ns + (second + 1) * 1e9,
    });
    const quality: EpisodeQuality = {
      accepted: true,
      params_version: "1",
      computed_utc_ns: 1_772_700_500_000_000_000,
      checks: [
        {
          check: "motion.jump",
          version: 1,
          kind: "advisory",
          verdict: "failed",
          message: "joint_3 jumped 0.4 rad between two messages",
          values: {},
          ranges: [12, 41, 50, 55, 60, 62, 70].map(at),
        },
      ],
    };
    const onreplay = vi.fn();
    render(EpisodeList, { episodes: [summary], open: detail, quality, onselect: vi.fn(), onreplay });
    const moments = screen.getByRole("list", { name: "Moments motion.jump found" });
    expect(moments.textContent).toContain("at 0:12–0:13");
    expect(moments.textContent).toContain("and 2 more");
    const watch = screen.getAllByRole("button", { name: "Watch" });
    expect(watch).toHaveLength(5);
    await fireEvent.click(watch[1] as HTMLElement);
    expect(onreplay).toHaveBeenCalledWith(summary.id, at(41));
    await fireEvent.click(screen.getByRole("button", { name: "Replay" }));
    expect(onreplay).toHaveBeenLastCalledWith(summary.id, null);
  });

  it("shows the open episode as not checked yet until the worker has checked it", () => {
    const unchecked: EpisodeQuality = { accepted: null, params_version: null, computed_utc_ns: null, checks: [] };
    render(EpisodeList, { episodes: [summary], open: detail, quality: unchecked, onselect: vi.fn() });
    expect(screen.getByRole("region", { name: `Episode ${summary.id}` }).textContent).toContain("not checked yet");
    expect(screen.queryByRole("table", { name: "Quality checks" })).toBeNull();
  });

  it("shows the open episode's statistics: single values whole, distributions by percentile", () => {
    const statistics: EpisodeStatistics = {
      computed_utc_ns: 1_772_700_500_000_000_000,
      statistics: [
        { name: "episode.duration_s", value: 75 },
        {
          name: "joints.joint_1.speed",
          count: 14_999,
          min: 0,
          max: 1.234567,
          mean: 0.4,
          p50: 0.351,
          p90: 0.9012,
          p99: 1.2,
        },
      ],
    };
    render(EpisodeList, { episodes: [summary], open: detail, statistics, onselect: vi.fn() });
    const table = screen.getByRole("table", { name: "Statistics" });
    const duration = screen.getByRole("row", { name: /^episode\.duration_s/ });
    expect(duration.textContent).toContain("75");
    const speed = screen.getByRole("row", { name: /^joints\.joint_1\.speed/ });
    expect([...speed.querySelectorAll("td")].map((cell) => cell.textContent)).toEqual(["0.351", "0.901", "1.2", "1.23"]);
    expect(table).toBeTruthy();
  });

  it("marks an episode deleted here as stored only remotely, and can't replay it", () => {
    render(EpisodeList, {
      episodes: [{ ...summary, stored_here: false }],
      open: { ...detail, stored_here: false },
      onselect: () => {},
    });
    expect(screen.getByText("remote only")).toBeTruthy();
    expect(screen.getByText("deleted here, stored only remotely")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Replay" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("asks to replay the open episode", async () => {
    const onreplay = vi.fn();
    render(EpisodeList, { episodes: [summary], open: detail, onselect: vi.fn(), onreplay });
    await fireEvent.click(screen.getByRole("button", { name: "Replay" }));
    expect(onreplay).toHaveBeenCalledWith(summary.id, null);
  });

  it("can't replay again the episode being prepared or shown, and can once its replay failed", async () => {
    const onreplay = vi.fn();
    const props = { episodes: [summary], open: detail, onselect: vi.fn(), onreplay };
    const replay = (state: ReplayState): ReplayStatus => ({ episode: summary.id, state, lacks: [] });
    const { rerender } = render(EpisodeList, { ...props, replay: replay("queued") });
    expect((screen.getByRole("button", { name: "Preparing…" }) as HTMLButtonElement).disabled).toBe(true);
    await rerender({ ...props, replay: replay("ready") });
    expect((screen.getByRole("button", { name: "Replaying" }) as HTMLButtonElement).disabled).toBe(true);
    // Another episode's replay leaves this one's button as it is.
    await rerender({ ...props, replay: { ...replay("ready"), episode: "20260310T130000.000Z-00000002" } });
    expect((screen.getByRole("button", { name: "Replay" }) as HTMLButtonElement).disabled).toBe(false);
    await rerender({ ...props, replay: replay("failed") });
    await fireEvent.click(screen.getByRole("button", { name: "Replay" }));
    expect(onreplay).toHaveBeenCalledOnce();
  });

  it("shows no statistics until the worker has worked them out", () => {
    const none: EpisodeStatistics = { computed_utc_ns: null, statistics: [] };
    render(EpisodeList, { episodes: [summary], open: detail, statistics: none, onselect: vi.fn() });
    expect(screen.queryByRole("table", { name: "Statistics" })).toBeNull();
  });
});

describe("EpisodeList's open episode", () => {
  const clean: EpisodeDetail = {
    ...detail,
    streams: [{ ...detail.streams[0]!, rejected: null }],
  };
  const statistics: EpisodeStatistics = { computed_utc_ns: 1, statistics: [{ name: "episode.duration_s", value: 75 }] };
  const check = {
    check: "streams.complete",
    version: 1,
    kind: "gate",
    verdict: "passed",
    message: "every message kept",
    values: {},
    ranges: [],
  };
  const accepted: EpisodeQuality = { accepted: true, params_version: "1", computed_utc_ns: 1, checks: [check] };
  const failed: EpisodeQuality = { ...accepted, accepted: false, checks: [{ ...check, verdict: "failed" }] };
  const upload = {
    episode: summary.id,
    destination: "nas",
    state: "failed" as const,
    key: "ep1",
    bytes_sent: 0,
    bytes_total: 1024,
    throttled: 0,
    started_utc_ns: 1,
    failure_code: "upload.denied",
    failure_message: "the store refused Kapto's credentials",
    objects: [],
  };
  it("shows its summary, then events, quality, uploads, streams, statistics and how it was recorded", () => {
    const events: EpisodeEvents = {
      read: true,
      events: [{ kind: "note", phase: "moment", at_utc_ns: summary.started_utc_ns, until_utc_ns: null, id: null, source: "page", text: null, attributes: {} }],
      unended: [],
    };
    render(EpisodeList, {
      props: {
        episodes: [summary],
        open: clean,
        quality: accepted,
        statistics,
        events,
        uploads: { stored_here: true, uploads: [] },
        onselect: vi.fn(),
      },
    });
    const details = screen.getByRole("region", { name: `Episode ${summary.id}` });
    expect([...details.querySelectorAll("h3")].map((heading) => heading.textContent)).toEqual([
      "Events",
      "Quality checks",
      "Uploads",
      "Streams",
      "Statistics",
      "Recording details",
    ]);
    expect(details.querySelector("dl")?.textContent).toContain("1:15");
    expect(details.textContent).toContain("99 % within 520 ms");
    // Nothing wrong, so nothing said to be.
    expect(details.querySelector(".danger, .warning")).toBeNull();
  });

  it("says every problem in its summary, first", () => {
    render(EpisodeList, {
      episodes: [summary],
      open: { ...detail, clock: { ...detail.clock, steps: 3 } },
      quality: failed,
      statistics,
      uploads: { stored_here: true, uploads: [upload] },
      onselect: vi.fn(),
    });
    const first = screen.getByRole("region", { name: `Episode ${summary.id}` }).querySelector("dl") as HTMLElement;
    for (const problem of [
      "12 lost before Kapto",
      "4 from sources not allowed",
      "stepped 3 times",
      "rejected: a gate failed · settings 1",
    ]) {
      expect(within(first).getByText(problem, { exact: false, selector: "dd" })).toBeTruthy();
    }
    const nas = within(first).getByText("failed", { selector: "dd span" });
    expect(nas.classList.contains("danger")).toBe(true);
  });

  it("tries its failed uploads again from its summary, and says why when refused", async () => {
    const onretry = vi.fn(() => Promise.resolve());
    const props = { episodes: [summary], open: clean, uploads: { stored_here: true, uploads: [upload] }, onselect: vi.fn() };
    const { unmount } = render(EpisodeList, { ...props, onretry });
    await fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onretry).toHaveBeenCalledWith(summary.id);
    unmount();

    const refused = vi.fn(() => Promise.reject(new ApiFailure({ code: "upload.unknown", message: "no such upload", remedy: "" })));
    render(EpisodeList, { ...props, onretry: refused });
    await fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("no such upload")).toBeTruthy();
  });

  it("offers no retry while nothing has failed", () => {
    render(EpisodeList, {
      episodes: [summary],
      open: clean,
      uploads: { stored_here: true, uploads: [{ ...upload, state: "uploaded" as const, failure_code: null, failure_message: null }] },
      onselect: vi.fn(),
    });
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });
});

describe("EpisodeList's events", () => {
  const intervention = { kind: "intervention", name: "Intervention", stretch: true, from: "default" };
  const at = (seconds: number) => summary.started_utc_ns + seconds * 1e9;
  const event = (kind: string, phase: "moment" | "begin" | "end", seconds: number, id: string | null = null) => ({
    kind,
    phase,
    at_utc_ns: at(seconds),
    until_utc_ns: null,
    id,
    source: "page",
    text: null,
    attributes: {},
  });

  it("counts each episode's events by kind and finds the episodes with one of a kind", async () => {
    const onevent = vi.fn();
    const marked = { ...summary, events: { intervention: 2, "kapto.stream_lost": 1 } };
    render(EpisodeList, { episodes: [marked], open: null, kinds: [intervention], onselect: vi.fn(), onevent });
    expect(screen.getByText(/2 Intervention · 1 kapto.stream_lost/)).toBeTruthy();
    // The vocabulary's kinds, then the others listed episodes hold.
    const select = screen.getByRole("combobox", { name: "With an event of kind" }) as HTMLSelectElement;
    expect([...select.options].map((option) => option.textContent)).toEqual([
      "Any events",
      "Intervention",
      "kapto.stream_lost",
    ]);
    await fireEvent.change(select, { target: { value: "intervention" } });
    expect(onevent).toHaveBeenCalledWith("intervention");
    await fireEvent.change(select, { target: { value: "" } });
    expect(onevent).toHaveBeenLastCalledWith(null);
  });

  it("says when no episode holds an event of the kind asked for", () => {
    render(EpisodeList, { episodes: [], open: null, kinds: [intervention], eventFilter: "intervention", onselect: vi.fn() });
    expect(screen.getByText("No episodes with Intervention.")).toBeTruthy();
  });

  it("lists the open episode's events in order, a stretch as one row to the millisecond, marks one never ended, and opens its replay at one", async () => {
    const onreplay = vi.fn();
    const events: EpisodeEvents = {
      read: true,
      events: [
        event("intervention", "begin", 5, "i1"),
        { ...event("note", "moment", 8), source: "api", text: "slipped" },
        event("intervention", "end", 12, "i1"),
        event("intervention", "begin", 40, "i2"),
      ],
      unended: [event("intervention", "begin", 40, "i2")],
    };
    render(EpisodeList, {
      props: { episodes: [summary], open: detail, events, kinds: [intervention], onselect: vi.fn(), onreplay },
    });
    const table = screen.getByRole("table", { name: "Events" });
    const rows = [...table.querySelectorAll("tbody tr")].map((row) =>
      [...row.children].map((cell) => cell.textContent?.replace(/\s+/g, " ").trim()).join(" | "),
    );
    expect(rows).toEqual([
      "0:05.000–0:12.000 | Intervention · 7.0 s | page Watch",
      "0:08.000 | note · slipped | api Watch",
      `0:40.000–${offset((summary.ended_utc_ns - summary.started_utc_ns) / 1e9)} | Intervention · never ended | page Watch`,
    ]);
    await fireEvent.click(screen.getAllByRole("button", { name: "Watch" })[1]!);
    expect(onreplay).toHaveBeenCalledWith(summary.id, { start_utc_ns: at(8), end_utc_ns: at(8) });
    await fireEvent.click(screen.getAllByRole("button", { name: "Watch" })[0]!);
    expect(onreplay).toHaveBeenCalledWith(summary.id, { start_utc_ns: at(5), end_utc_ns: at(12) });
  });

  it("says the open episode's events aren't read yet until the worker has read them", () => {
    const unread: EpisodeEvents = { read: false, events: [], unended: [] };
    render(EpisodeList, { props: { episodes: [summary], open: detail, events: unread, onselect: vi.fn() } });
    expect(screen.getByText(/Events: not read yet/)).toBeTruthy();
    expect(screen.queryByRole("table", { name: "Events" })).toBeNull();
  });
});

describe("EpisodeList's bin", () => {
  it("deletes an episode only once confirmed, and lists the bin to restore or empty it", async () => {
    const ondelete = vi.fn();
    const onbin = vi.fn();
    render(EpisodeList, { episodes: [summary], open: detail, onselect: vi.fn(), ondelete, onbin, keepDays: 7 });
    await fireEvent.click(screen.getByRole("button", { name: "Delete…" }));
    const confirm = screen.getByRole("group", { name: "Delete this episode" });
    expect(confirm.textContent).toContain("restored for 7 days");
    expect(ondelete).not.toHaveBeenCalled();
    await fireEvent.click(within(confirm).getByRole("button", { name: "Delete" }));
    expect(ondelete).toHaveBeenCalledWith(summary.id);
    await fireEvent.click(screen.getByRole("button", { name: "In the bin" }));
    expect(onbin).toHaveBeenCalledWith(true);
  });

  it("in the bin, says when each goes for good, restores one and empties it once confirmed", async () => {
    const binnedAt = 1_772_700_400_000_000_000;
    const onrestore = vi.fn();
    const onempty = vi.fn();
    render(EpisodeList, {
      episodes: [{ ...summary, binned_utc_ns: binnedAt }],
      open: { ...detail, binned_utc_ns: binnedAt },
      onselect: vi.fn(),
      inBin: true,
      onrestore,
      onempty,
      keepDays: 7,
    });
    expect(screen.getByRole("button", { name: "In the bin" }).getAttribute("aria-pressed")).toBe("true");
    const kept = screen.getByRole("group", { name: "In the bin" });
    expect(kept.textContent).toContain("deleted for good about");
    await fireEvent.click(within(kept).getByRole("button", { name: "Restore" }));
    expect(onrestore).toHaveBeenCalledWith(summary.id);
    await fireEvent.click(screen.getByRole("button", { name: "Empty the bin…" }));
    expect(onempty).not.toHaveBeenCalled();
    await fireEvent.click(within(screen.getByRole("group", { name: "Empty the bin" })).getByRole("button", { name: "Empty the bin" }));
    expect(onempty).toHaveBeenCalledOnce();
  });
});
