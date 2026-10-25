import { expect, test, type Route } from "@playwright/test";

import { open, shot, shotWhole, tab, viewer, VIEWPORT } from "./screens";

// A route that fetches the real answer before changing it reads that answer
// after the request is made: the page's polls keep coming until the test
// ends, and an answer read once the page has closed is gone. Each test's
// routes go, and those still running finish, before the page closes.
test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "wait" });
});

// Each tab and state with everything running, for a pull request's
// description: `cargo xtask ui screenshots` saves them in target/screenshots.
test("a screenshot of each tab and state while Kapto runs", async ({ page }) => {
  // About half a minute with the viewer hidden between its screenshots,
  // and the replay's, which waits for the worker.
  test.setTimeout(360_000);
  const nothingLeft = await open(page);
  const aside = page.locator("aside");

  // As the page opens: on the episodes, the header saying all is well.
  const chip = page.getByRole("banner").getByRole("button", { name: /^Health: / });
  await expect(chip).toHaveAccessibleName("Health: All well", { timeout: 60_000 });
  await expect(page.getByTitle("Live view of the robot").getByText(/^Live( · \d+ ms)?$/)).toBeVisible({ timeout: 60_000 });
  await page.waitForTimeout(5_000);
  await shot(page, "opened");
  // Live, opened from the header, once the stream's rate is known and the
  // viewer has drawn the arm. The browser draws the viewer in software
  // here, which slows the page's first readings.
  await chip.click();
  const stream = page.locator("aside").getByRole("row", { name: /arm_joints/ });
  await expect(stream.getByRole("img", { name: "arriving at its declared rate" })).toBeVisible({ timeout: 60_000 });
  await page.waitForTimeout(5_000);
  await shot(page, "live");
  // The plot of the joints, whose key takes more than its two lines here:
  // its values picked from the menu in its corner, open.
  const jointPlot = page.getByRole("region", { name: /^Plot of role:joints\.joint_1\.position/ });
  await jointPlot.hover();
  await jointPlot.getByRole("button", { name: "Values shown" }).click();
  await jointPlot.getByRole("menuitemcheckbox", { name: "joint_2" }).click();
  await page.waitForTimeout(500);
  await shot(page, "live-plot-menu");
  await jointPlot.getByRole("menuitem", { name: "Show all" }).click();
  await page.keyboard.press("Escape");
  // On a wider screen its key fits: one value picked in it shows alone, with its band.
  await page.setViewportSize({ width: 2200, height: VIEWPORT.height });
  await jointPlot.getByRole("button", { name: "joint_1", exact: true }).click();
  await page.mouse.move(0, 0);
  await page.waitForTimeout(1_000);
  await shot(jointPlot, "live-plot-one-value");
  await jointPlot.getByRole("button", { name: "joint_1", exact: true }).click();
  await page.setViewportSize(VIEWPORT);
  // The keys for what operators do most.
  await page.keyboard.press("?");
  await expect(page.getByRole("region", { name: "Keyboard shortcuts" })).toBeVisible();
  await shot(page, "keys");
  await page.keyboard.press("Escape");
  // Paused and looking back ten seconds.
  const liveView = page.getByTitle("Live view of the robot");
  await page.getByRole("button", { name: "Pause" }).click();
  await expect(liveView).toHaveAttribute("data-looked-back", "yes");
  const held = Number(await liveView.getAttribute("data-showing"));
  await page.getByRole("slider", { name: "Moment shown" }).fill(String(held - 10_000));
  await expect(liveView).toHaveAttribute("data-showing", String(held - 10_000));
  await page.waitForTimeout(1_000);
  await shot(page, "live-paused");
  // Playing back from there.
  await page.getByRole("button", { name: "Play" }).click();
  await expect(liveView).toHaveAttribute("data-playing", "yes");
  await page.waitForTimeout(1_500);
  await shot(page, "live-playing");
  await page.getByRole("button", { name: "Back to now" }).click();
  // The arm in a cell: a table, a cube to pick, a bin.
  await page.getByRole("button", { name: "Environment" }).click();
  const environment = page.getByRole("region", { name: "Environment" });
  const place = async (values: [string, string][]) => {
    for (const [field, value] of values) {
      await environment.getByRole("spinbutton", { name: field }).fill(value);
      await environment.getByRole("spinbutton", { name: field }).press("Enter");
    }
  };
  // The sample cell, then its cube chosen to show how a thing is placed.
  await environment.getByRole("button", { name: "Add a sample cell" }).click();
  await environment.getByRole("button", { name: "Red cube" }).click();
  await place([["position y", "0.25"]]);
  await page.waitForTimeout(1_000);
  await shot(page, "live-environment-panel");
  await page.getByRole("button", { name: /^Environment/ }).click();
  await page.waitForTimeout(1_000);
  await shot(page, "live-environment");
  // Arranging the view: the editor open beside it.
  await page.getByRole("button", { name: "Arrange" }).click();
  await page.waitForTimeout(500);
  await shot(page, "live-arrange");
  await page.getByRole("button", { name: "Arrange" }).click();
  await viewer(page, false);

  await tab(page, /^Robot/);
  await shot(aside, "robot");

  await tab(page, /^System/);
  await shot(aside, "system");
  const settings = page.locator("details").filter({ hasText: /settings, / });
  await settings.locator("summary").click();
  await shotWhole(page, settings, "settings");
  const search = settings.getByRole("searchbox", { name: "Search settings" });
  await search.fill("worker");
  await expect(settings.getByText(/^\d+ of \d+ settings$/)).toBeVisible();
  await shotWhole(page, settings, "settings-search");
  await search.fill("");
  await settings.getByRole("checkbox", { name: "Changed only" }).check();
  await shotWhole(page, settings, "settings-changed");
  await settings.getByRole("checkbox", { name: "Changed only" }).uncheck();

  // Pre-flight with two streams silent, and the start it refuses. The
  // demo's arm always sends, so those answers are given here.
  await tab(page, /^Live/);
  const silent = (stream: string) => ({
    code: "preflight.stream_silent",
    summary: "A declared stream sent nothing",
    remedy:
      "Check that the robot is on and sending to the address in `streams.toml`, and that no firewall is in the way, then run pre-flight again. To record without it, waive this check when starting the episode.",
    stream,
    passed: false,
    measured: "0 messages in 5.0 s, 0/s",
    needed: "messages arriving",
    waived: false,
  });
  await page.route("**/api/status", async (route) => {
    const response = await route.fetch();
    const status = (await response.json()) as { capture: { preflight: { checks: object[] } } };
    const { preflight } = status.capture;
    preflight.checks = [...preflight.checks.slice(0, 2), silent("arm_joints"), silent("gripper")];
    await route.fulfill({ response, json: { ...status, capture: { ...status.capture, preflight: { ...preflight, passed: false } } } });
  });
  await page.route("**/api/episodes/start", (route) =>
    route.fulfill({
      status: 409,
      json: {
        code: "capture.preflight_failed",
        message:
          "Pre-flight failed: this machine may not record what the robot sends: the run at 2026-03-16T07:00:00.000Z failed preflight.stream_silent (arm_joints), preflight.stream_silent (gripper); not waived",
        remedy:
          "Fix what each failed check says and run pre-flight again (`kapto episode preflight`), or record anyway by waiving each failed check (`kapto episode start --waive <code>`); the episode then says which were waived.",
      },
    }),
  );
  await expect(page.getByText(/2 of 4 checks failed/)).toBeVisible();
  await shot(aside, "preflight-failed");
  // The header names the worst, and how many more.
  await expect(chip).toHaveAccessibleName("Health: Pre-flight: arm_joints silent +1");
  await shot(page.getByRole("banner"), "health-warning");
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await expect(page.getByRole("alert")).toBeVisible();
  await shot(page.getByRole("alert"), "preflight-refused");
  await page.getByRole("button", { name: "Dismiss" }).click();
  await page.unroute("**/api/status");
  await page.unroute("**/api/episodes/start");
  await expect(page.getByText(/^Passed at/)).toBeVisible();

  await page.getByRole("combobox", { name: "Profile" }).selectOption("lab_ana");
  await page.getByRole("combobox", { name: "Task" }).selectOption("pick_and_place");
  await page.getByRole("button", { name: "Record" }).click();
  await expect(page.getByText(/^Recording ·/)).toBeVisible();
  await expect(page.getByText(/ left at this rate$/)).toBeVisible();
  // Shown again for the one other screenshot of the whole page.
  await viewer(page, true);
  await page.waitForTimeout(5_000);
  await shot(page, "recording");
  // What happened on the timeline: a stretch begun from the page, a note
  // sent through the API.
  const marks = page.getByRole("group", { name: "Mark what happened" });
  await marks.getByRole("combobox", { name: "Kind of event" }).selectOption("intervention");
  await marks.getByRole("button", { name: "Begin Intervention" }).click();
  await page.waitForTimeout(2_000);
  await page.request.post("/api/episodes/events", {
    headers: { "Content-Type": "application/json" },
    data: JSON.stringify({ kind: "note", text: "gripper slipped" }),
  });
  await page.waitForTimeout(2_000);
  await shot(page, "recording-marks");
  await marks.getByRole("button", { name: "End Intervention" }).click();
  await viewer(page, false);
  // The list as it is before the worker has checked the new episode, which
  // it does within a second or two: held there for the screenshot.
  const listing = /\/api\/episodes\?/;
  await page.route(listing, async (route) => {
    const response = await route.fetch();
    const episodes = (await response.json()) as object[];
    await route.fulfill({ response, json: episodes.map((episode) => ({ ...episode, accepted: null })) });
  });
  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByText("Live, not recording")).toBeVisible();

  await tab(page, /^Episodes/);
  const episode = page.getByRole("listitem").filter({ has: page.getByTitle("Show this episode in full") }).first();
  await expect(episode).toContainText("not checked yet");
  await shot(aside, "episodes-not-checked");
  // Let go once a request held now has been answered: one answered after
  // fails, and the list stops asking.
  await page.unrouteAll({ behavior: "wait" });
  await expect(episode).toContainText("accepted", { timeout: 30_000 });
  await shot(aside, "episodes");
  await episode.getByTitle("Show this episode in full").click();
  const details = page.getByRole("region", { name: /^Episode / });
  await expect(details).toContainText("accepted · settings 1");
  // What happened in it, read by the worker: closed and opened again once read.
  await expect(episode).toContainText("Intervention", { timeout: 30_000 });
  await episode.getByTitle("Show this episode in full").click();
  await episode.getByTitle("Show this episode in full").click();
  await expect(details.getByRole("table", { name: "Events" })).toBeVisible();
  await shot(aside, "episode");
  await shot(details.getByRole("table", { name: "Events" }), "episode-events");
  // Only the episodes with an intervention.
  await page.getByRole("combobox", { name: "With an event of kind" }).selectOption("intervention");
  await expect(episode).toContainText("Intervention");
  await shot(aside, "episodes-by-event");
  await page.getByRole("combobox", { name: "With an event of kind" }).selectOption("");

  // One a gate rejected, listed among the rejected ones. The demo's
  // recordings are complete, so that answer is given here.
  await page.route(listing, async (route) => {
    // Every episode, whatever was asked for, each said to be rejected.
    const response = await route.fetch({ url: route.request().url().replace(/&accepted=\w+/, "") });
    const episodes = (await response.json()) as object[];
    await route.fulfill({ response, json: episodes.map((listed) => ({ ...listed, accepted: false })) });
  });
  await page.route("**/api/episodes/*/qa", (route) =>
    route.fulfill({
      json: {
        accepted: false,
        params_version: "1",
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
      },
    }),
  );
  await page.getByRole("combobox", { name: "Quality" }).selectOption("rejected");
  await expect(episode).toContainText("rejected");
  await episode.getByTitle("Show this episode in full").click();
  await episode.getByTitle("Show this episode in full").click();
  await expect(details).toContainText("rejected: a gate failed");
  await shot(aside, "episode-rejected");
  await page.unrouteAll({ behavior: "wait" });
  await page.getByRole("combobox", { name: "Quality" }).selectOption("all");
  await episode.getByTitle("Show this episode in full").click();
  await episode.getByTitle("Show this episode in full").click();
  await expect(details).toContainText("accepted · settings 1");

  // One recovered after recording stopped without finishing it. The demo's
  // recordings always finish, so that answer is given here.
  const recovered = async (route: Route) => {
    const response = await route.fetch();
    const body = (await response.json()) as { started_utc_ns: number } | { started_utc_ns: number }[];
    const mark = (listed: { started_utc_ns: number }) => ({
      ...listed,
      recovered: {
        at_utc_ns: listed.started_utc_ns + 3_600e9,
        bytes_cut: 3 * 1024,
        last_message_utc_ns: listed.started_utc_ns + 4e9,
        reason: "recording stopped without finishing it: a crash, a kill or a power cut",
      },
    });
    await route.fulfill({ response, json: Array.isArray(body) ? body.map(mark) : mark(body) });
  };
  const oneEpisode = /\/api\/episodes\/[^/?]+$/;
  await page.route(listing, recovered);
  await page.route(oneEpisode, recovered);
  await page.getByRole("combobox", { name: "Quality" }).selectOption("accepted");
  await page.getByRole("combobox", { name: "Quality" }).selectOption("all");
  await expect(episode).toContainText("loss unknown");
  await episode.getByTitle("Show this episode in full").click();
  await episode.getByTitle("Show this episode in full").click();
  await expect(details).toContainText("not known for a recovered episode");
  await shot(aside, "episode-recovered");
  await page.unroute(listing);
  await page.unroute(oneEpisode);
  await page.getByRole("combobox", { name: "Quality" }).selectOption("accepted");
  await page.getByRole("combobox", { name: "Quality" }).selectOption("all");
  await episode.getByTitle("Show this episode in full").click();
  await episode.getByTitle("Show this episode in full").click();
  await expect(details).toContainText("99 % within");

  // A replay, held as being prepared for its screenshot, then as made.
  const replayStatus = "**/api/episodes/*/replay";
  await page.route(replayStatus, async (route) => {
    const response = await route.fetch();
    const status = (await response.json()) as { episode: string };
    await route.fulfill({ response, json: { episode: status.episode, state: "queued", lacks: [] } });
  });
  await page.getByRole("button", { name: "Replay", exact: true }).click();
  const viewerSection = page.locator("section.viewer");
  const bar = page.getByRole("status", { name: "Replay" });
  await expect(bar).toContainText("Preparing the replay");
  await shot(bar, "replay-preparing");
  await page.unroute(replayStatus);
  // What a replay of a robot whose meshes weren't recorded says. The demo's
  // arm has none, so that answer is given here.
  await page.route(replayStatus, async (route) => {
    const response = await route.fetch();
    const status = (await response.json()) as { state: string; lacks: object[] };
    const lacks = [
      {
        code: "robot.mesh_unreadable",
        message:
          "Can't read the mesh file: meshes/gripper.stl isn't in the recording or in a package with the same ID and content, so it isn't shown",
        remedy:
          "Check that the file (and, for glTF, its .bin buffer) exists next to where the robot description says, and that Kapto may read it.",
      },
    ];
    await route.fulfill({ response, json: status.state === "ready" ? { ...status, lacks } : status });
  });
  await expect(bar).toContainText("Replay of the episode recorded", { timeout: 90_000 });
  await viewer(page, true);
  await page.waitForTimeout(4_000);
  await shot(page, "replay");
  await shot(viewerSection, "replay-viewer");
  await expect(bar).toContainText("Episode over", { timeout: 90_000 });
  await shot(bar, "replay-over");
  await viewer(page, false);
  await shot(aside, "episode-replaying");
  await page.getByRole("banner").getByRole("button", { name: "Back to live" }).click();
  // Hidden still, as it was for the screenshot before.
  await expect(page.getByTitle("Live view of the robot")).toBeAttached();
  await page.unroute(replayStatus);

  // Moments a check found, each opening the replay there. The demo's arm
  // moves smoothly, so what the motion checks say here is given.
  const { started_utc_ns: start } = (await (
    await page.request.get(`/api/episodes/${(await details.getAttribute("aria-label"))?.replace("Episode ", "")}`)
  ).json()) as { started_utc_ns: number };
  const moment = (second: number) => ({ start_utc_ns: start + second * 1e9, end_utc_ns: start + (second + 1) * 1e9 });
  await page.route("**/api/episodes/*/qa", async (route) => {
    const response = await route.fetch();
    const quality = (await response.json()) as { checks: { check: string }[] };
    const jumps = {
      check: "motion.jump",
      version: 1,
      kind: "advisory",
      verdict: "failed",
      message: "joint_3 jumped 0.4 rad between two messages",
      values: {},
      ranges: [moment(4), moment(7)],
    };
    await route.fulfill({
      response,
      json: { ...quality, checks: quality.checks.map((check) => (check.check === "motion.jump" ? jumps : check)) },
    });
  });
  await episode.getByTitle("Show this episode in full").click();
  await episode.getByTitle("Show this episode in full").click();
  const moments = page.getByRole("list", { name: "Moments motion.jump found" });
  await expect(moments).toContainText("at 0:04–0:05");
  await shot(details.getByRole("table", { name: "Quality checks" }), "episode-moments");
  await moments.getByRole("button", { name: "Watch" }).first().click();
  // Drawn, so it plays and says where it is.
  await viewer(page, true);
  await expect(bar).toContainText(/Playing 0:0[1-3] of/, { timeout: 90_000 });
  await shot(viewerSection, "replay-moment");
  await viewer(page, false);
  await page.getByRole("banner").getByRole("button", { name: "Back to live" }).click();

  // A replay of a recording made without a robot package: its values alone.
  // The page keeps the robot of the replay it showed last, so it is opened
  // afresh, and the demo's episodes all have a robot, so that answer is
  // given here.
  const replayScene = "**/api/episodes/*/replay/scene";
  await page.route(replayScene, (route) =>
    route.fulfill({
      status: 404,
      json: {
        code: "viewer.replay_without_robot",
        message: "The replay shows no robot",
        remedy: "Record with a robot package for the robot to be shown; the plots are shown either way.",
      },
    }),
  );
  await page.reload();
  await tab(page, /^Episodes/);
  await episode.getByTitle("Show this episode in full").click();
  await page.getByRole("button", { name: "Replay", exact: true }).click();
  await viewer(page, true);
  await expect(viewerSection.getByText("Replay · recorded without a robot: its values only")).toBeVisible({
    timeout: 30_000,
  });
  await page.waitForTimeout(2_000);
  await shot(viewerSection, "replay-without-robot");
  await viewer(page, false);
  await page.getByRole("banner").getByRole("button", { name: "Back to live" }).click();
  await page.unroute(replayScene);

  // Uploads: the episodes recorded above in the folder xtask configures,
  // and failed at the one it can't make.
  await tab(page, /^Uploads/);
  const archive = page.getByRole("listitem", { name: "archive", exact: true });
  const nas = page.getByRole("listitem", { name: "nas", exact: true });
  await expect(archive).toContainText(/[1-9]\d* uploaded/, { timeout: 60_000 });
  await expect(nas.getByText(/^[1-9]\d* failed$/)).toBeVisible({ timeout: 60_000 });
  await shotWhole(page, aside, "uploads");
  await archive.getByRole("button", { name: "Test" }).click();
  await expect(archive.getByRole("status")).toBeVisible();
  await nas.getByRole("button", { name: "Test" }).click();
  await expect(nas.getByRole("alert")).toBeVisible();
  await shotWhole(page, aside, "uploads-tested");
  // An upload going: a folder takes the demo's episodes faster than a
  // screenshot, so one is added to what the API answers, going at 3 MiB/s.
  const since = Date.now();
  await page.route("**/api/uploads", async (route) => {
    const answer = await (await route.fetch()).json();
    const total = 900 * 1024 * 1024;
    answer.uploads.unshift({
      episode: "20260325T150212.448Z-3fa91c07",
      destination: "archive",
      state: "uploading",
      key: "arm7/2026-03-25/20260325T150212.448Z-3fa91c07",
      bytes_sent: Math.min(total, 310 * 1024 * 1024 + ((Date.now() - since) / 1000) * 3 * 1024 * 1024),
      bytes_total: total,
      throttled: 0,
      started_utc_ns: since * 1e6,
      reported_utc_ns: Date.now() * 1e6,
    });
    answer.destinations[0].uploading += 1;
    await route.fulfill({ json: answer });
  });
  await expect(page.getByText(/ MiB\/s · \d+:\d\d left$/)).toBeVisible({ timeout: 10_000 });
  await shotWhole(page, aside, "uploads-going");
  await page.unroute("**/api/uploads");
  // With no completeness marker written, which the page warns of.
  await page.route("**/api/settings", async (route) => {
    const answer = await (await route.fetch()).json();
    for (const setting of answer.settings) {
      if (setting.key === "upload.marker") {
        Object.assign(setting, { value: "false", default: false });
      }
    }
    await route.fulfill({ json: answer });
  });
  await page.reload();
  await viewer(page, false);
  await tab(page, /^Uploads/);
  await expect(page.getByRole("note")).toBeVisible();
  await shot(page.getByRole("note"), "uploads-no-marker");
  await page.unroute("**/api/settings");
  // An episode's details: where each of its objects went.
  await tab(page, /^Episodes/);
  const listed = page.getByRole("listitem").filter({ has: page.getByTitle("Show this episode in full") }).first();
  await listed.getByTitle("Show this episode in full").click();
  const uploadsShown = page.getByRole("region", { name: "Upload to archive" });
  await expect(uploadsShown).toContainText(/uploaded \d{4}-/, { timeout: 30_000 });
  await uploadsShown.scrollIntoViewIfNeeded();
  await shot(page.locator("aside"), "episode-uploads");
  // And one deleted here once verified elsewhere: what the API answers
  // for it, since the demo keeps every episode.
  const id = (await page.locator("section[aria-label^='Episode ']").getAttribute("aria-label"))?.slice(8) ?? "";
  const remote = async (route: Route) => {
    const answer = await (await route.fetch()).json();
    const deleted = (episode: { id?: string; stored_here: boolean; folder?: string }) => {
      if (episode.id === undefined || episode.id === id) {
        episode.stored_here = false;
      }
    };
    if (Array.isArray(answer)) {
      answer.forEach(deleted);
    } else {
      deleted(answer);
      if ("uploads" in answer) {
        answer.deletion = {
          deleted_utc_ns: Date.now() * 1e6,
          reason: "after_upload",
          deleted_by: "worker",
          destinations: ["archive"],
        };
      }
    }
    await route.fulfill({ json: answer });
  };
  await page.route("**/api/episodes?*", remote);
  await page.route(`**/api/episodes/${id}`, remote);
  await page.route(`**/api/episodes/${id}/uploads`, remote);
  await page.reload();
  await viewer(page, false);
  await tab(page, /^Episodes/);
  await page.getByRole("listitem").filter({ hasText: "remote only" }).getByTitle("Show this episode in full").click();
  await expect(page.getByText(/^Stored only remotely/)).toBeVisible();
  await shot(page.locator("aside"), "episode-remote");
  await page.unrouteAll({ behavior: "ignoreErrors" });

  // And with no destination at all.
  await page.route("**/api/uploads", (route) =>
    route.fulfill({ json: { waiting: 0, destinations: [], uploads: [] } }),
  );
  await page.reload();
  await viewer(page, false);
  await tab(page, /^Uploads/);
  await expect(page.getByText(/^No destinations/)).toBeVisible();
  await shot(aside, "uploads-none");
  await page.unroute("**/api/uploads");

  nothingLeft();
});

test("a screenshot of a camera waiting for its pictures and of one that can't be shown", async ({ page }) => {
  // The bridge as it is before the camera's first picture, then when the
  // camera sends what can't be shown: its pictures held back here.
  const toPage: { say?: (text: string) => void } = {};
  await page.routeWebSocket(/:\d+\/$/, (route) => {
    const bridge = route.connectToServer();
    toPage.say = (text) => route.send(text);
    bridge.onMessage((message) => {
      if (typeof message !== "string" && message.subarray(0, 4).toString() === "KVPC") {
        return;
      }
      route.send(message);
    });
  });
  const nothingLeft = await open(page);
  const cameras = page.getByRole("region", { name: "Cameras" });
  await expect(cameras.getByText("Waiting for pictures…")).toBeVisible({ timeout: 30_000 });
  await shot(cameras, "live-camera-waiting");
  toPage.say?.(
    JSON.stringify({
      camera: {
        stream: 1,
        unshown: "raw pictures in `yuv422`, which the live view can't show: only rgb8, bgr8, rgba8, bgra8 and mono8",
      },
    }),
  );
  await expect(cameras.getByRole("status")).toBeVisible();
  await shot(cameras, "live-camera-unshown");
  nothingLeft();
});

// The console on a tablet beside the robot, held sideways: each tab whole.
test("a screenshot of the page Kapto can't show, built for another version of its API", async ({ page }) => {
  await page.route("**/api/version", (route) => route.fulfill({ json: { kapto: "9.9.9", api: 999 } }));
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Kapto can't show this page" })).toBeVisible();
  await shot(page, "wrong-api-version");
});

test("a screenshot of each tab at a tablet's width", async ({ page }) => {
  test.setTimeout(180_000);
  const nothingLeft = await open(page);
  await page.setViewportSize({ width: 1024, height: 768 });
  await tab(page, /^Live/);
  const stream = page.locator("aside").getByRole("row", { name: /arm_joints/ });
  await expect(stream.getByRole("img", { name: "arriving at its declared rate" })).toBeVisible({ timeout: 60_000 });
  await page.waitForTimeout(3_000);
  await shot(page, "tablet-live");
  await viewer(page, false);
  await tab(page, /^Episodes/);
  const episode = page.getByRole("listitem").filter({ has: page.getByTitle("Show this episode in full") }).first();
  await episode.getByTitle("Show this episode in full").click();
  await expect(page.getByRole("region", { name: /^Episode / })).toBeVisible();
  await shot(page, "tablet-episode");
  await tab(page, /^Uploads/);
  await shot(page, "tablet-uploads");
  await tab(page, /^Robot/);
  await shot(page, "tablet-robot");
  await tab(page, /^System/);
  await page.locator("details").filter({ hasText: /settings, / }).locator("summary").click();
  await shot(page, "tablet-system");
  nothingLeft();
});
