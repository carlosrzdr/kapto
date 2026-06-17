import { expect, test, type Page, type Route } from "@playwright/test";

import { fitsPanel } from "./fits";
import { requestsLeavingTheMachine } from "./local-only";

const open = (page: Page, tab: RegExp) => page.getByRole("tab", { name: tab }).click();

test("the page shows the robot's stream, records an episode and lists it", async ({ page }) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");

  await expect(page.getByText("Live, not recording")).toBeVisible();
  // The robot, live in the page: its frames keep coming, and it says how
  // far behind Kapto it is.
  const live = page.getByTitle("Live view of the robot");
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  const seen = Number(await live.getAttribute("data-frames"));
  await expect.poll(async () => Number(await live.getAttribute("data-frames"))).toBeGreaterThan(seen + 10);
  // Beside it, arranged by the layout made from the package: the camera,
  // a plot of the joints' positions and one of the gripper, by their
  // roles; counters and clocks aren't plotted.
  const panels = page.getByRole("group", { name: "The view's panels" });
  const jointPlot = panels.getByRole("region", { name: /^Plot of role:joints\.joint_1\.position/ });
  await expect(jointPlot.getByText("joints · position", { exact: true })).toBeVisible();
  await expect(jointPlot.getByText("joint_1", { exact: true })).toBeVisible();
  await expect(panels.getByRole("region", { name: "Plot of role:gripper.opening" })).toBeVisible();
  await expect(panels.getByRole("region", { name: /sequence/ })).toHaveCount(0);
  // The streams' counts are beside the view, in the console, not in it.
  await expect(panels.getByRole("table")).toHaveCount(0);
  // The live tab: streams and storage.
  const stream = page.locator("aside").getByRole("row", { name: /arm_joints/ });
  const received = stream.getByRole("cell").nth(1);
  await expect(received).not.toHaveText("0");
  // Arriving at the rate the demo declares.
  const rate = stream.getByRole("cell").first();
  // The first rate takes two readings, while the page is busy starting the viewer.
  await expect(stream.getByRole("img", { name: "arriving at its declared rate" })).toBeVisible({ timeout: 20_000 });
  await expect(rate).toHaveText(/\/s$/);
  await expect(rate).not.toHaveClass(/problem/);
  await expect(page.getByText(/ free of /)).toBeVisible();

  await open(page, /^Episodes/);
  await expect(page.getByText("No episodes recorded yet.")).toBeVisible();

  await open(page, /^System/);
  for (const process of ["serve", "capture", "viz-bridge"]) {
    await expect(page.getByRole("row", { name: new RegExp(`^${process} up `) })).toBeVisible();
  }
  // xtask starts the demo with its ports as flags.
  await page.getByText(/ changed from the default$/).click();
  await expect(page.getByRole("row", { name: /^server\.port / })).toContainText("flag --port");

  await open(page, /^Robot/);
  const joints = page.getByRole("table", { name: "Joints" });
  const first = joints.getByRole("row", { name: /^joint_1 / });
  await expect(first).toContainText("arm_joints.joint_positions[0]");
  // Where the synthetic arm's first joint is now.
  await expect(first.getByRole("cell").first()).toHaveText(/^-?\d+\.\d°/);

  await open(page, /^Live/);
  // Recorded with a profile and a task; a profile for another robot can't be chosen.
  const profile = page.getByRole("combobox", { name: "Profile" });
  await expect(profile.getByRole("option", { name: "cobot_day · Ben (needs cobot)" })).toHaveAttribute("disabled");
  await profile.selectOption("lab_ana");
  await page.getByRole("combobox", { name: "Task" }).selectOption("pick_and_place");
  await page.getByRole("button", { name: "Record" }).click();
  await expect(page.getByText(/^Recording ·/)).toBeVisible();
  await expect(page.getByText(/ left at this rate$/)).toBeVisible();
  const written = stream.getByRole("cell").nth(2);
  await expect(written).not.toHaveText(/^(0|–)$/);

  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByText("Live, not recording")).toBeVisible();
  await expect(page.getByRole("tab", { name: "Episodes (1)" })).toBeVisible();
  await open(page, /^Episodes/);
  const episodes = page.getByRole("listitem").filter({ hasText: "messages" });
  await expect(episodes).toHaveCount(1);
  await expect(episodes).toContainText("complete");
  await expect(episodes).toContainText("Pick and place · lab_ana");
  // The worker checks it a moment later, and the list follows.
  await expect(episodes).toContainText("accepted", { timeout: 30_000 });

  // The episode in full.
  await episodes.getByTitle("Show this episode in full").click();
  const details = page.getByRole("region", { name: /^Episode / });
  await expect(details).toContainText("99 % within");
  await expect(details.getByRole("row", { name: /^arm_joints/ })).toContainText("sequence numbers");
  await expect(details).toContainText("lab_ana · Ana · teleoperated · shift-a");
  await expect(details).toContainText("reach → grasp → lift → place");
  // Checked with the quality settings the sample arm's recording carries.
  await expect(details).toContainText("accepted · settings 1");
  await expect(details.getByRole("row", { name: /^streams\.complete/ })).toContainText("passed");
  // With its statistics: the joints the sample arm moves have speeds.
  const statistics = details.getByRole("table", { name: "Statistics" });
  await expect(statistics.getByRole("row", { name: /^episode\.duration_s/ })).toBeVisible();
  await expect(statistics.getByRole("row", { name: /^joints\.joint_1\.speed/ })).toBeVisible();

  // An episode recorded by another program shows up without a reload,
  // however short: capture announces it.
  const json = { headers: { "Content-Type": "application/json" }, data: "{}" };
  expect((await page.request.post("/api/episodes/start", json)).status()).toBe(201);
  expect((await page.request.post("/api/episodes/stop", json)).status()).toBe(200);
  await expect(page.getByRole("tab", { name: "Episodes (2)" })).toBeVisible();

  // The live view's stream and the robot's meshes included.
  await page.waitForLoadState("networkidle");
  expect(elsewhere).toEqual([]);
});

test("an episode is replayed in the live view, saying why one can't be, and the live robot comes back", async ({
  page,
}) => {
  // Making the replay waits for the worker's other jobs.
  test.setTimeout(120_000);
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible();
  const view = page.getByTitle("Live view of the robot");
  await expect(view).toHaveAttribute("data-robot", "live");
  const framings = await view.getAttribute("data-framings");
  // Recorded by a program, three seconds long.
  const json = { headers: { "Content-Type": "application/json" }, data: "{}" };
  expect((await page.request.post("/api/episodes/start", json)).status()).toBe(201);
  await page.waitForTimeout(3_000);
  expect((await page.request.post("/api/episodes/stop", json)).status()).toBe(200);

  await open(page, /^Episodes/);
  const episode = page.getByRole("listitem").filter({ hasText: "messages" }).first();
  await episode.getByTitle("Show this episode in full").click();
  const opened = page.waitForResponse((response) => response.url().includes("/replay/opening"), { timeout: 90_000 });
  const windowed = page.waitForResponse((response) => response.url().includes("/replay/window?"), { timeout: 90_000 });
  const robot = page.waitForResponse((response) => response.url().includes("/replay/scene"), { timeout: 90_000 });
  await page.getByRole("button", { name: "Replay", exact: true }).click();
  const bar = page.getByRole("status", { name: "Replay" });
  await expect(bar).toContainText(/Preparing the replay of the episode recorded|Replay of the episode recorded/);
  await expect(bar).toContainText("Replay of the episode recorded", { timeout: 90_000 });
  // The live view replays it, from the episode's own stretches.
  expect((await opened).status()).toBe(200);
  expect((await windowed).status()).toBe(200);
  await expect(view).toHaveAttribute("data-replay", /^\d{8}T\d{6}\.\d{3}Z-[0-9a-f]{8}$/);
  // Drawn with the robot it was recorded with, from the replay; the same
  // arm reaches as far, so the camera stays where it was.
  expect((await robot).status()).toBe(200);
  const id = await view.getAttribute("data-replay");
  await expect(view).toHaveAttribute("data-robot", id ?? "");
  await expect(view).toHaveAttribute("data-framings", framings ?? "");
  await expect(view).toHaveAttribute("data-looked-back", "yes");
  await expect(page.getByRole("button", { name: "Back to live" })).toHaveCount(2);

  // It plays from the start and stops on its last moment, which the bar
  // says, offering to play it again.
  await expect(bar).toContainText(/Playing \d+:\d\d of 0:0[234]|Episode over/, { timeout: 30_000 });
  await expect(bar).toContainText(/Episode over · 0:0[234]/, { timeout: 30_000 });
  await bar.getByRole("button", { name: "Play again" }).click();
  await expect(bar).toContainText(/Playing 0:0[0-2] of/);
  await expect(bar).not.toContainText("Episode over");
  await expect(bar).toContainText("Episode over", { timeout: 30_000 });

  // What is shown can't be asked for again; the header goes back to live.
  await expect(episode.getByRole("button", { name: "Replaying" })).toBeDisabled();
  const back = page.getByRole("banner").getByRole("button", { name: "Back to live" });
  await back.click();
  await expect(view).toHaveAttribute("data-replay", "");
  await expect(view).toHaveAttribute("data-showing", "live");
  await expect(view).toHaveAttribute("data-robot", "live");
  await expect(bar).toHaveCount(0);
  await expect(back).toHaveCount(0);
  await expect(episode.getByRole("button", { name: "Replay", exact: true })).toBeEnabled();

  // A replay that can't be made says why, with its cause code. The demo's
  // replays are always made, so that answer is given here.
  await page.route("**/api/episodes/*/replay", async (route) => {
    const response = await route.fetch();
    const { episode: id } = (await response.json()) as { episode: string };
    await route.fulfill({
      response,
      status: 202,
      json: {
        episode: id,
        state: "failed",
        lacks: [],
        failure: {
          code: "viewer.replay_disk_low",
          message: "No room to make the replay: 9 MiB free",
          remedy: "Free space on the disk holding the data directory.",
        },
      },
    });
  });
  await page.getByRole("button", { name: "Replay", exact: true }).click();
  await expect(bar).toContainText("couldn't be made");
  await expect(bar).toContainText("viewer.replay_disk_low");
  await expect(page.getByTitle("Live view of the robot")).toBeVisible();
  // Failed, it can be asked for again.
  await expect(episode.getByRole("button", { name: "Replay", exact: true })).toBeEnabled();
  await back.click();
  await expect(bar).toHaveCount(0);

  expect(elsewhere).toEqual([]);
});

test("a moment a check found opens the episode's replay just before it, and another moves it there, with or without its robot", async ({
  page,
}) => {
  test.setTimeout(150_000);
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible();
  const json = { headers: { "Content-Type": "application/json" }, data: "{}" };
  const { episode: id } = (await (await page.request.post("/api/episodes/start", json)).json()) as { episode: string };
  await page.waitForTimeout(10_000);
  expect((await page.request.post("/api/episodes/stop", json)).status()).toBe(200);
  const { started_utc_ns: start } = (await (await page.request.get(`/api/episodes/${id}`)).json()) as {
    started_utc_ns: number;
  };
  // What the motion checks would say of jumps seven and two seconds in, and
  // of one after the episode ended. The demo's arm moves smoothly, so that
  // answer is given here.
  const moment = (second: number) => ({ start_utc_ns: start + second * 1e9, end_utc_ns: start + (second + 1) * 1e9 });
  await page.route(`**/api/episodes/${id}/qa`, (route) =>
    route.fulfill({
      json: {
        accepted: true,
        params_version: "1",
        computed_utc_ns: start + 60e9,
        checks: [
          {
            check: "motion.jump",
            version: 1,
            kind: "advisory",
            verdict: "failed",
            message: "joint_3 jumped 0.4 rad between two messages",
            values: {},
            ranges: [moment(7), moment(2), moment(600)],
          },
        ],
      },
    }),
  );

  // And as a recording made without a robot package would answer: no robot.
  await page.route(`**/api/episodes/${id}/replay/scene`, (route) =>
    route.fulfill({
      status: 404,
      json: {
        code: "viewer.replay_without_robot",
        message: "The replay shows no robot",
        remedy: "Record with a robot package for the robot to be shown; the plots are shown either way.",
      },
    }),
  );

  await open(page, /^Episodes/);
  const episode = page.getByRole("listitem").filter({ hasText: "messages" }).first();
  await episode.getByTitle("Show this episode in full").click();
  const moments = page.getByRole("list", { name: "Moments motion.jump found" });
  await expect(moments).toContainText("at 0:07–0:08");
  const watch = moments.getByRole("button", { name: "Watch" });
  await watch.nth(0).click();

  // The episode's replay, a little before the jump, playing on.
  const bar = page.getByRole("status", { name: "Replay" });
  await expect(bar).toContainText(/Playing 0:0[4-6] of 0:(09|1\d)/, { timeout: 90_000 });
  const view = page.getByTitle("Live view of the robot");
  await expect(view).toHaveAttribute("data-replay", id);
  // Without its robot, its values alone, and it says so.
  await expect(view).toHaveAttribute("data-robot", id);
  await expect(view.getByText("Replay · recorded without a robot: its values only")).toBeVisible();
  await expect(view).toHaveAttribute("data-looked-back", "yes");
  // The jump's stretch is drawn on the timeline.
  await expect(page.getByRole("button", { name: /^motion\.jump: joint_3 jumped/ }).first()).toBeVisible();

  // Another moment of the same episode moves the same replay there.
  await watch.nth(1).click();
  await expect(bar).toContainText(/Playing 0:0[0-2] of/);
  await expect(view).toHaveAttribute("data-replay", id);
  await expect(bar).toContainText("Episode over", { timeout: 30_000 });

  // Opened at a moment after the episode ended, the replay plays from its start.
  await page.getByRole("banner").getByRole("button", { name: "Back to live" }).click();
  await watch.nth(2).click();
  await expect(bar).toContainText(/Playing 0:0[0-2] of/, { timeout: 30_000 });

  await page.getByRole("banner").getByRole("button", { name: "Back to live" }).click();
  await expect(view).toHaveAttribute("data-replay", "");
  // The robot Kapto runs, drawn again.
  await expect(view).toHaveAttribute("data-robot", "live");
  await expect(view.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  expect(elsewhere).toEqual([]);
});

test("an episode recovered after recording stopped without finishing is marked, its loss unknown", async ({ page }) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible();
  const json = { headers: { "Content-Type": "application/json" }, data: "{}" };
  const { episode: id } = (await (await page.request.post("/api/episodes/start", json)).json()) as { episode: string };
  await page.waitForTimeout(2_000);
  expect((await page.request.post("/api/episodes/stop", json)).status()).toBe(200);
  const { started_utc_ns: start } = (await (await page.request.get(`/api/episodes/${id}`)).json()) as {
    started_utc_ns: number;
  };
  // What capture says of an episode it recovered after a crash. The demo's
  // recordings always finish, so that answer is given here.
  const recovered = {
    at_utc_ns: start + 3_600e9,
    bytes_cut: 3 * 1024,
    last_message_utc_ns: start + 1.5e9,
    reason: "recording stopped without finishing it: a crash, a kill or a power cut",
  };
  const said = async (route: Route) => {
    const response = await route.fetch();
    const body = (await response.json()) as { id: string } | { id: string }[];
    const mark = (episode: { id: string }) => (episode.id === id ? { ...episode, recovered } : episode);
    await route.fulfill({ response, json: Array.isArray(body) ? body.map(mark) : mark(body) });
  };
  await page.route(/\/api\/episodes\?/, said);
  await page.route(`**/api/episodes/${id}`, said);

  await open(page, /^Episodes/);
  const listed = page.getByRole("listitem").filter({ hasText: "messages" });
  const episode = listed.filter({ hasText: "recovered" });
  await expect(episode).toHaveCount(1);
  await expect(episode).toContainText("loss unknown");
  // Every other episode finished, and says whether it lost anything.
  await expect(listed.filter({ hasText: "loss unknown" })).toHaveCount(1);
  await episode.getByTitle("Show this episode in full").click();
  const details = page.getByRole("region", { name: `Episode ${id}` });
  await expect(details).toContainText("3.0 KiB cut off its end · last message at 0:01 · what it lost isn't known");
  await expect(details).toContainText("not known for a recovered episode");
  await expect(details).not.toContainText("99 % within");
  expect(elsewhere).toEqual([]);
});

test("pre-flight runs again from the page, and an episode it refuses is recorded anyway, waiving what failed", async ({
  page,
}) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible();
  // The demo's robot sends before capture starts: its first run passes.
  await expect(page.getByText(/All 4 checks passed/)).toBeVisible();
  const ran = page.waitForResponse((response) => response.url().endsWith("/api/preflight"));
  await page.getByRole("button", { name: "Run again" }).click();
  await expect(page.getByRole("button", { name: "Running…" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Record", exact: true })).toBeDisabled();
  expect((await ran).status()).toBe(200);
  await expect(page.getByRole("button", { name: "Run again" })).toBeEnabled();
  await expect(page.getByText(/All 4 checks passed/)).toBeVisible();

  // What capture says while the arm sends nothing, and refuses unless it is
  // waived. The demo's arm always sends, so those answers are given here.
  const silent = {
    code: "preflight.stream_silent",
    summary: "A declared stream sent nothing",
    remedy: "Check that the robot is on and sending to the address in `streams.toml`.",
    stream: "arm_joints",
    passed: false,
    measured: "0 messages in 5.0 s, 0/s",
    needed: "messages arriving",
    waived: false,
  };
  await page.route("**/api/status", async (route) => {
    const response = await route.fetch();
    const status = (await response.json()) as { capture: { episode: string | null; preflight: object } };
    if (status.capture.episode === null) {
      status.capture.preflight = { ...status.capture.preflight, passed: false, checks: [silent] };
    }
    await route.fulfill({ response, json: status });
  });
  const starts: unknown[] = [];
  await page.route("**/api/episodes/start", async (route) => {
    const body = route.request().postDataJSON() as { waive?: string[] };
    starts.push(body);
    if (body.waive === undefined) {
      await route.fulfill({
        status: 409,
        json: {
          code: "capture.preflight_failed",
          message: "Pre-flight found this machine may not record what the robot sends: preflight.stream_silent (arm_joints); not waived",
          remedy: "Fix what failed and run pre-flight again, or start with each failed check waived.",
        },
      });
    } else {
      await route.continue();
    }
  });

  await expect(page.getByText("1 of 1 checks failed")).toBeVisible();
  await expect(page.getByRole("tab", { name: "Live" })).toHaveClass(/alert/);
  await open(page, /^System/);
  await page.getByRole("button", { name: "Record", exact: true }).click();
  // Refused: the live tab shows what failed and what to do.
  await expect(page.getByRole("alert")).toContainText("preflight.stream_silent (arm_joints)");
  await expect(page.getByText("A declared stream sent nothing")).toBeVisible();
  await expect(page.getByText(/Check that the robot is on/)).toBeVisible();
  await page.getByRole("button", { name: "Record anyway" }).click();
  await expect(page.getByText(/^Recording ·/)).toBeVisible();
  expect(starts.at(-1)).toEqual({ profile: null, task: null, waive: ["preflight.stream_silent"] });
  await expect(page.getByRole("button", { name: "Record anyway" })).toHaveCount(0);
  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByText("Live, not recording")).toBeVisible();
  // A status asked for once a second may still be on its way: left to end
  // with this test, not to fail the next one.
  await page.unrouteAll({ behavior: "ignoreErrors" });
  expect(elsewhere).toEqual([]);
});

test("the live view pauses, looks back over the last minutes, and returns to now", async ({ page }) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  const live = page.getByTitle("Live view of the robot");
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  // A few seconds kept to look back over.
  await page.waitForTimeout(5_000);

  // How far back it can look, asked from the bridge.
  await expect(page.getByText(/^\d+:\d\d kept$/)).toBeVisible();
  await page.getByRole("button", { name: "Pause" }).click();
  await expect(live).not.toHaveAttribute("data-showing", "live");
  await expect(live).toHaveAttribute("data-looked-back", "yes");
  await expect(page.getByText(/^of \d+:\d\d$/)).toBeVisible();
  await expect(live.getByText("Paused · looking back")).toBeVisible();
  const held = Number(await live.getAttribute("data-showing"));
  // The stream keeps arriving while the view holds still.
  const frames = Number(await live.getAttribute("data-frames"));
  await expect.poll(async () => Number(await live.getAttribute("data-frames"))).toBeGreaterThan(frames + 10);
  await expect(live).toHaveAttribute("data-showing", String(held));

  // Three seconds further back.
  await page.getByRole("slider", { name: "Moment shown" }).fill(String(held - 3_000));
  await expect(live).toHaveAttribute("data-showing", String(held - 3_000));
  await expect(page.getByText(/^−0:0\d$/)).toBeVisible();

  // Played on from there: the moment shown moves forward, then holds again.
  await page.getByRole("button", { name: "Play" }).click();
  await expect(live).toHaveAttribute("data-playing", "yes");
  await expect.poll(async () => Number(await live.getAttribute("data-showing"))).toBeGreaterThan(held - 2_500);
  await page.getByRole("button", { name: "Pause" }).click();
  await expect(live).toHaveAttribute("data-playing", "no");
  const stopped = await live.getAttribute("data-showing");
  await page.waitForTimeout(500);
  await expect(live).toHaveAttribute("data-showing", stopped ?? "");
  // Faster, it catches up with now and is live again.
  await page.getByRole("combobox", { name: "Speed" }).selectOption("4");
  await page.getByRole("button", { name: "Play" }).click();
  await expect(live).toHaveAttribute("data-showing", "live", { timeout: 15_000 });
  await expect(live).toHaveAttribute("data-playing", "no");

  await page.getByRole("button", { name: "Pause" }).click();
  await page.getByRole("button", { name: "Back to now" }).click();
  await expect(live).toHaveAttribute("data-showing", "live");
  await expect(live).toHaveAttribute("data-looked-back", "no");
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible();

  // Two minutes is more than the page keeps of its own: drawn live from
  // the bridge's last minutes.
  await page.getByRole("combobox", { name: "Span" }).selectOption("120000");
  await expect(live).toHaveAttribute("data-looked-back", "yes");
  await expect(live).toHaveAttribute("data-showing", "live");
  expect(elsewhere).toEqual([]);
});

test("events marked from the page and sent through the API show on the timeline, with the episode's start and end", async ({
  page,
}) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  const live = page.getByTitle("Live view of the robot");
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  const marks = page.getByRole("group", { name: "Mark what happened" });
  const timeline = page.getByRole("group", { name: "What happened" });

  await page.getByRole("button", { name: "Record", exact: true }).click();
  await expect(page.getByText(/^Recording ·/)).toBeVisible();
  // A stretch begun and ended from the page, pressing twice.
  await marks.getByRole("combobox", { name: "Kind of event" }).selectOption("intervention");
  await marks.getByRole("button", { name: "Begin Intervention" }).click();
  await expect(marks.getByText("Marked the beginning of Intervention")).toBeVisible();
  // A moment from another program, through the API.
  const sent = await page.request.post("/api/episodes/events", {
    headers: { "Content-Type": "application/json" },
    data: JSON.stringify({ kind: "note", text: "from the api" }),
  });
  expect(sent.status()).toBe(201);
  await marks.getByRole("button", { name: "End Intervention" }).click();
  await expect(marks.getByText("Marked the end of Intervention")).toBeVisible();
  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByText("Live, not recording")).toBeVisible();

  // Earlier episodes may be on the timeline too: these are the newest.
  for (const label of ["Episode started", "intervention (page)", "note (api) · from the api", "Episode ended"]) {
    await expect(timeline.getByRole("button", { name: label }).last()).toBeVisible();
  }
  // Opening one shows that moment. Live, the marks move with every frame,
  // so it is opened from the keyboard rather than waiting for it to hold
  // still.
  const note = timeline.getByRole("button", { name: "note (api) · from the api" }).last();
  // A moment is still a target a pointer can hit: 24px each way at least.
  const target = await note.boundingBox();
  expect(target?.width ?? 0).toBeGreaterThanOrEqual(23.5);
  expect(target?.height ?? 0).toBeGreaterThanOrEqual(23.5);
  await note.focus();
  await note.press("Enter");
  await expect(live).not.toHaveAttribute("data-showing", "live");
  // Opened a little before it, playing on.
  await expect(live).toHaveAttribute("data-playing", "yes");
  await page.getByRole("button", { name: "Back to now" }).click();

  // An event the API refuses says why.
  await page.route("**/api/episodes/events", (route) =>
    route.fulfill({
      status: 409,
      json: { code: "events.unknown_kind", message: "The kind of event isn't one that may be sent.", remedy: "Add it to events.toml." },
    }),
  );
  // Where the key goes, should it be taken by something else than the
  // marks: once in a while it sent nothing here.
  const focused = await page.evaluate(() => document.activeElement?.outerHTML.slice(0, 200) ?? "none");
  test.info().annotations.push({ type: "focused before pressing 1", description: focused });
  const asked = page.waitForRequest((request) => request.url().endsWith("/api/episodes/events"), { timeout: 5_000 });
  await page.keyboard.press("1");
  await asked.catch(() => {
    throw new Error(`pressing 1 sent no event; the focus was on ${focused}`);
  });
  await expect(marks.getByText("The kind of event isn't one that may be sent. Add it to events.toml.")).toBeVisible();
  await page.unroute("**/api/episodes/events");

  // Found again among the episodes by what happened in it, once the worker
  // has read its events: the newest with an intervention.
  await open(page, /^Episodes/);
  const episodes = page.getByRole("tabpanel");
  await episodes.getByRole("combobox", { name: "With an event of kind" }).selectOption("intervention");
  const newest = episodes.getByRole("button", { name: /Intervention/ }).first();
  await expect(newest).toBeVisible({ timeout: 30_000 });
  await expect(newest).toContainText(/2 Intervention/);
  await expect(newest).toContainText(/1 Note/);
  await newest.click();
  const happened = episodes.getByRole("table", { name: "Events" });
  await expect(happened.getByRole("rowheader", { name: "Intervention began" })).toBeVisible({ timeout: 30_000 });
  await expect(happened.getByRole("rowheader", { name: "Note · from the api" })).toBeVisible();
  await expect(happened.getByRole("rowheader", { name: "Intervention ended" })).toBeVisible();
  expect(elsewhere).toEqual([]);
});

/** A binary STL of one triangle a metre wide, as a file to load. */
function triangleStl(): Buffer {
  const bytes = Buffer.alloc(84 + 50);
  bytes.writeUInt32LE(1, 80);
  [0, 0, 0, 1, 0, 0, 0, 1, 0].forEach((value, index) => bytes.writeFloatLE(value, 84 + 12 + index * 4));
  return bytes;
}

test("an environment is placed around the robot, kept in the browser after a reload, and a file it can't load is refused", async ({
  page,
}) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  const live = page.getByTitle("Live view of the robot");
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Environment" }).click();
  const environment = page.getByRole("region", { name: "Environment" });

  // A table under the arm.
  await environment.getByRole("button", { name: "Add box" }).click();
  const placing = environment.getByRole("group", { name: "Placing Box" });
  await placing.getByRole("textbox", { name: "Name" }).fill("Table");
  await placing.getByRole("textbox", { name: "Name" }).press("Enter");
  for (const [field, value] of [
    ["position x", "0"],
    ["position z", "-0.02"],
    ["scale x", "1.2"],
    ["scale y", "0.8"],
    ["scale z", "0.04"],
  ] as const) {
    await environment.getByRole("spinbutton", { name: field }).fill(value);
    await environment.getByRole("spinbutton", { name: field }).press("Enter");
  }
  // A part from a CAD file.
  await environment.getByLabel("Load a mesh").setInputFiles({ name: "part.stl", mimeType: "model/stl", buffer: triangleStl() });
  await expect(environment.getByRole("button", { name: "part.stl" })).toBeVisible();
  await expect(live).toHaveAttribute("data-environment", "2");

  // Kept in this browser.
  await page.reload();
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  await expect(live).toHaveAttribute("data-environment", "2");
  await page.getByRole("button", { name: "Environment (2)" }).click();
  await environment.getByRole("button", { name: "Table" }).click();
  await expect(environment.getByRole("spinbutton", { name: "scale x" })).toHaveValue("1.2");
  await expect(environment.getByRole("spinbutton", { name: "position z" })).toHaveValue("-0.02");

  // Removed, and a file of another kind refused.
  await environment.getByRole("button", { name: "Remove" }).click();
  await expect(live).toHaveAttribute("data-environment", "1");
  await environment.getByLabel("Load a mesh").setInputFiles({ name: "cell.dae", mimeType: "model/vnd.collada+xml", buffer: Buffer.from("<COLLADA/>") });
  await expect(environment.getByRole("alert")).toContainText("cell.dae isn't a mesh this view loads");
  expect(elsewhere).toEqual([]);
});

test("the robot is framed by its whole reach, and framed again as it stands from the button or F", async ({ page }) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  const live = page.getByTitle("Live view of the robot");
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  // Framed once, from the reach the scene gives, before any pose.
  await expect(live).toHaveAttribute("data-framings", "1");
  const scene = await (await page.request.get("/api/robot/scene")).json();
  expect(scene.reach.radius).toBeGreaterThan(0);
  // Moving doesn't frame it again: the camera stays where it was put.
  await page.waitForTimeout(2_000);
  await expect(live).toHaveAttribute("data-framings", "1");

  // Zoomed in, then framed again from the button, then from the keyboard.
  await live.locator("canvas").hover();
  await page.mouse.wheel(0, -600);
  await live.getByRole("button", { name: "Frame robot" }).click();
  await expect(live).toHaveAttribute("data-framings", "2");
  await page.keyboard.press("f");
  await expect(live).toHaveAttribute("data-framings", "3");
  // Not while typing.
  await page.getByRole("button", { name: "Environment" }).click();
  await page.getByRole("button", { name: "Add box" }).click();
  await page.getByRole("textbox", { name: "Name" }).press("f");
  await expect(live).toHaveAttribute("data-framings", "3");
  expect(elsewhere).toEqual([]);
});

test("an operator records, looks back and comes back to now from the keyboard, and finds the keys with ?", async ({
  page,
}) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  const live = page.getByTitle("Live view of the robot");
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  await page.waitForTimeout(3_000);

  // The keys, listed on ? and hidden on Esc.
  await page.keyboard.press("?");
  const keys = page.getByRole("region", { name: "Keyboard shortcuts" });
  await expect(keys).toContainText("Record an episode, or stop recording it");
  await page.keyboard.press("Escape");
  await expect(keys).toBeHidden();

  // R records, and R again stops.
  await page.keyboard.press("r");
  await expect(page.getByText(/^Recording ·/)).toBeVisible();
  await page.waitForTimeout(1_500);
  await page.keyboard.press("r");
  await expect(page.getByText("Live, not recording")).toBeVisible();

  // Space pauses; a few seconds back, Space plays back; Esc comes back to now.
  await page.keyboard.press(" ");
  await expect(live).not.toHaveAttribute("data-showing", "live");
  const held = Number(await live.getAttribute("data-showing"));
  await page.getByRole("slider", { name: "Moment shown" }).fill(String(held - 3_000));
  await page.locator("body").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press(" ");
  await expect(live).toHaveAttribute("data-playing", "yes");
  await page.keyboard.press("Escape");
  await expect(live).toHaveAttribute("data-showing", "live");

  // Not while typing: R in a field types an r, and records nothing.
  await page.getByRole("button", { name: "Environment" }).click();
  await page.getByRole("button", { name: "Add box" }).click();
  const name = page.getByRole("textbox", { name: "Name" });
  await name.press("End");
  await name.press("r");
  await expect(name).toHaveValue("Boxr");
  await page.waitForTimeout(1_000);
  await expect(page.getByText("Live, not recording")).toBeVisible();
  expect(elsewhere).toEqual([]);
});

test("episodes go up to each destination, one that can't take them says why and is retried, and each is tested", async ({
  page,
}) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible();
  // An episode of its own, uploaded once it is finished and checked.
  await page.getByRole("button", { name: "Record" }).click();
  await expect(page.getByText(/^Recording ·/)).toBeVisible();
  await page.waitForTimeout(2_000);
  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByText("Live, not recording")).toBeVisible();

  await open(page, /^Uploads/);
  // xtask configures a folder, and one under a file, which can't be made.
  const archive = page.getByRole("listitem", { name: "archive", exact: true });
  const nas = page.getByRole("listitem", { name: "nas", exact: true });
  await expect(archive).toContainText(/[1-9]\d* uploaded/, { timeout: 60_000 });
  await expect(nas.getByText(/^[1-9]\d* failed$/)).toHaveClass(/problem/, { timeout: 60_000 });
  await expect(page.getByRole("tab", { name: "Uploads" })).toHaveClass(/alert/);
  const failed = page.getByRole("listitem", { name: / to nas$/ }).first();
  await expect(failed).toContainText("upload.destination_unusable");

  // Retried now rather than in an hour: queued again for the upload lane.
  const retried = page.waitForResponse((response) => response.url().endsWith("/uploads/retry"));
  await failed.getByRole("button", { name: "Retry now" }).click();
  expect((await retried).status()).toBe(202);

  await archive.getByRole("button", { name: "Test" }).click();
  await expect(archive.getByRole("status")).toContainText(/Works: a probe written, read back and removed in \d+ ms/);
  await nas.getByRole("button", { name: "Test" }).click();
  await expect(nas.getByRole("alert")).toBeVisible();
  await expect(nas).toContainText("upload.destination_unusable");

  // The episode's own details say where each of its objects went.
  await open(page, /^Episodes/);
  await page.getByRole("listitem").filter({ hasText: "messages" }).first().getByTitle("Show this episode in full").click();
  const there = page.getByRole("region", { name: "Upload to archive" });
  await expect(there).toContainText(/uploaded \d{4}-/);
  await expect(there).toContainText(/file:\/\/\/.*\/archive\//);
  await expect(there.getByRole("button", { name: "Copy folder URI" })).toBeVisible();
  const objects = there.getByRole("table", { name: "Objects at archive" });
  await expect(objects.getByRole("row", { name: /recording\.mcap/ })).toBeVisible();
  await expect(objects.getByRole("row", { name: /episode\.json/ })).toBeVisible();
  await expect(page.getByRole("region", { name: "Upload to nas" })).toContainText("upload.destination_unusable");
  expect(elsewhere).toEqual([]);
});

test("a package's own layout arranges the view, a panel naming what the package lacks saying so", async ({ page }) => {
  // The demo's package as if its layout.toml put the plots first and named
  // a camera it doesn't have.
  await page.route("**/api/robot", async (route) => {
    const response = await route.fetch();
    const robot = (await response.json()) as { layout: object };
    const panel = (kind: string, more: object = {}) => ({ kind, height: 1, fields: [], missing: [], ...more });
    robot.layout = {
      source: "package",
      columns: [
        { width: 2, panels: [panel("plot", { fields: ["role:gripper.opening"] }), panel("camera", { stream: "head_camera", missing: ["no stream `head_camera`"] })] },
        { width: 3, panels: [panel("robot", { height: 2 }), panel("events")] },
      ],
    };
    await route.fulfill({ response, json: robot });
  });
  await page.goto("/");
  const panels = page.getByRole("group", { name: "The view's panels" });
  await expect(panels.getByRole("note")).toHaveText(
    "The camera head_camera can't be shown: the robot package has no stream `head_camera`. Change the package's layout.toml, or what it declares.",
  );
  await expect(panels.getByRole("region", { name: "Plot of role:gripper.opening" })).toBeVisible();
  // The robot, moved to the second column, still moves.
  await expect(panels.getByTitle("Live view of the robot").getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 20_000 });
  await expect(panels.getByRole("region", { name: "What happened" })).toBeVisible();
  // Nothing the layout leaves out is shown: not the demo's wrist camera.
  await expect(panels.getByRole("region", { name: "Cameras" })).toHaveCount(0);
});

test("a replay is arranged by the layout its recording carries", async ({ page }) => {
  // Making the replay waits for the worker's other jobs.
  test.setTimeout(120_000);
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible();
  const json = { headers: { "Content-Type": "application/json" }, data: "{}" };
  expect((await page.request.post("/api/episodes/start", json)).status()).toBe(201);
  await page.waitForTimeout(2_000);
  expect((await page.request.post("/api/episodes/stop", json)).status()).toBe(200);
  // As if the episode had been recorded with a layout naming a camera its
  // package didn't have.
  await page.route("**/replay/opening", async (route) => {
    const response = await route.fetch();
    const opening = (await response.json()) as { layout: object };
    const panel = (kind: string, more: object = {}) => ({ kind, height: 1, fields: [], missing: [], ...more });
    opening.layout = {
      source: "package",
      columns: [{ width: 1, panels: [panel("robot"), panel("camera", { stream: "head_camera", missing: ["no stream `head_camera`"] })] }],
    };
    await route.fulfill({ response, json: opening });
  });
  await open(page, /^Episodes/);
  const episode = page.getByRole("listitem").filter({ hasText: "messages" }).first();
  await episode.getByTitle("Show this episode in full").click();
  await page.getByRole("button", { name: "Replay", exact: true }).click();
  await expect(page.getByRole("status", { name: "Replay" })).toContainText(/Episode over/, { timeout: 90_000 });
  const panels = page.getByRole("group", { name: "The view's panels" });
  await expect(panels.getByRole("note")).toContainText("The camera head_camera can't be shown");
  // Arranged as recorded, not as the live view is: no plots, no wrist camera.
  await expect(panels.getByRole("region", { name: /^Plot of / })).toHaveCount(0);
  await expect(panels.getByRole("region", { name: "Cameras" })).toHaveCount(0);
  await page.getByRole("banner").getByRole("button", { name: "Back to live" }).click();
  await expect(panels.getByRole("region", { name: "Plot of role:gripper.opening" })).toBeVisible();
});

test("the demo's wrist camera is shown live beside the arm", async ({ page }) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  const cameras = page.getByRole("region", { name: "Cameras" });
  const picture = cameras.getByRole("img", { name: "wrist_camera, its newest picture" });
  await expect(picture).toBeVisible({ timeout: 20_000 });
  // Its pictures keep coming, decoded by the browser at the camera's size.
  const figure = cameras.locator("figure").first();
  const seen = Number(await figure.getAttribute("data-pictures"));
  await expect.poll(async () => Number(await figure.getAttribute("data-pictures"))).toBeGreaterThan(seen + 10);
  await expect.poll(() => picture.locator("canvas").evaluate((canvas: HTMLCanvasElement) => canvas.width)).toBe(640);
  await expect(figure.getByText(/^\d+ ms$/)).toBeVisible();
  // Beside the robot, which still moves.
  await expect(page.getByTitle("Live view of the robot").getByText(/^Live · \d+ ms$/)).toBeVisible();
  // Its stream says what became of its frames on their way to the page.
  await expect(page.locator("aside").getByText(/^Live view: .+ of .+ frames sent/)).toBeVisible();
  expect(elsewhere).toEqual([]);
});

test("a camera whose pictures can't be shown says why instead", async ({ page }) => {
  const why = "raw pictures in `yuv422`, which the live view can't show: only rgb8, bgr8, rgba8, bgra8 and mono8";
  // The bridge as it is when a camera sends an encoding it can't show: its
  // pictures stop, and the page is told why.
  await page.routeWebSocket(/:\d+\/$/, (page) => {
    const bridge = page.connectToServer();
    bridge.onMessage((message) => {
      if (typeof message !== "string") {
        if (message.subarray(0, 4).toString() === "KVPC") {
          return;
        }
        page.send(message);
        return;
      }
      page.send(message);
      if (message.includes('"links"')) {
        page.send(JSON.stringify({ camera: { stream: 1, unshown: why } }));
      }
    });
  });
  await page.goto("/");
  const cameras = page.getByRole("region", { name: "Cameras" });
  await expect(cameras.getByRole("status")).toHaveText(why, { timeout: 20_000 });
  await expect(cameras.getByRole("img")).toHaveCount(0);
  // The robot is shown all the same.
  await expect(page.getByTitle("Live view of the robot").getByText(/^Live · \d+ ms$/)).toBeVisible();
});

test("a replayed camera steps through every frame recorded, and says why one can't be shown", async ({ page }) => {
  // Making the replay waits for the worker's other jobs.
  test.setTimeout(120_000);
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible();
  const json = { headers: { "Content-Type": "application/json" }, data: "{}" };
  expect((await page.request.post("/api/episodes/start", json)).status()).toBe(201);
  await page.waitForTimeout(3_000);
  expect((await page.request.post("/api/episodes/stop", json)).status()).toBe(200);

  await open(page, /^Episodes/);
  const episode = page.getByRole("listitem").filter({ hasText: "messages" }).first();
  await episode.getByTitle("Show this episode in full").click();
  await page.getByRole("button", { name: "Replay", exact: true }).click();
  const bar = page.getByRole("status", { name: "Replay" });
  await expect(bar).toContainText(/Episode over/, { timeout: 90_000 });

  // Held on its last moment, the camera shows its frame then: its last,
  // or the one before if the camera sent last, after the arm's last message.
  const cameras = page.getByRole("region", { name: "Cameras" });
  const caption = cameras.getByText(/^frame \d+ of \d+$/);
  await expect(caption).toBeVisible();
  await page.waitForTimeout(500);
  const [last, of] = ((await caption.textContent()) ?? "").match(/\d+/g)?.map(Number) ?? [];
  expect(of! - last!).toBeLessThanOrEqual(1);
  // About thirty a second for three seconds: every frame recorded.
  expect(of).toBeGreaterThan(60);
  await cameras.getByRole("button", { name: "wrist_camera: the frame before" }).click();
  await expect(caption).toHaveText(`frame ${(last ?? 0) - 1} of ${of}`);
  await expect(cameras.getByRole("img", { name: `wrist_camera, frame ${(last ?? 0) - 1}` })).toBeVisible();
  await cameras.getByRole("button", { name: "wrist_camera: the frame before" }).click();
  await expect(caption).toHaveText(`frame ${(last ?? 0) - 2} of ${of}`);
  await cameras.getByRole("button", { name: "wrist_camera: the frame after" }).click();
  await expect(caption).toHaveText(`frame ${(last ?? 0) - 1} of ${of}`);
  // Stepping holds the replay where the frame is.
  await expect(bar).not.toContainText("Playing");

  // A frame that can't be shown says why. The demo's always can, so that
  // answer is given here.
  await page.route("**/cameras/*/frames/*", (route) =>
    route.fulfill({
      status: 409,
      json: {
        code: "viewer.picture_unshown",
        message: "raw pictures in `yuv422`, which the live view can't show: only rgb8, bgr8, rgba8, bgra8 and mono8",
        remedy: "The message says why.",
      },
    }),
  );
  await cameras.getByRole("button", { name: "wrist_camera: the frame before" }).click();
  await expect(cameras.getByRole("status")).toContainText("yuv422");
  expect(elsewhere).toEqual([]);
});

test("nothing in the side panel is cut off, at a laptop's width or a tablet's", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible();
  // An episode to open, with its details and uploads.
  const json = { headers: { "Content-Type": "application/json" }, data: "{}" };
  expect((await page.request.post("/api/episodes/start", json)).status()).toBe(201);
  await page.waitForTimeout(1_500);
  expect((await page.request.post("/api/episodes/stop", json)).status()).toBe(200);
  for (const size of [
    { width: 1400, height: 900 },
    { width: 1024, height: 768 },
  ]) {
    await page.setViewportSize(size);
    const at = `at ${size.width}×${size.height}`;
    await open(page, /^Live/);
    await expect(page.locator("aside").getByRole("row", { name: /wrist_camera/ })).toBeVisible();
    await fitsPanel(page, `in the Live tab ${at}`);
    await open(page, /^Episodes/);
    const episode = page.getByRole("listitem").filter({ hasText: "messages" }).first();
    if ((await episode.getByRole("button", { expanded: true }).count()) === 0) {
      await episode.getByTitle("Show this episode in full").click();
    }
    await expect(page.getByRole("region", { name: /^Episode / })).toBeVisible();
    await fitsPanel(page, `in the Episodes tab, an episode open, ${at}`);
    await open(page, /^Uploads/);
    await fitsPanel(page, `in the Uploads tab ${at}`);
    await open(page, /^Robot/);
    await fitsPanel(page, `in the Robot tab ${at}`);
    await open(page, /^System/);
    const settings = page.locator("summary").filter({ hasText: /settings, \d+ changed/ });
    if ((await page.getByRole("searchbox").count()) === 0) {
      await settings.click();
    }
    await expect(page.getByRole("searchbox")).toBeVisible();
    await fitsPanel(page, `in the System tab, its settings open, ${at}`);
  }
});

