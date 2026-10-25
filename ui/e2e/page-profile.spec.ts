import { readdirSync, readFileSync, writeFileSync } from "node:fs";

import { expect, test } from "@playwright/test";

// The live view watched under a scenario's load, for
// `cargo xtask profile page`, which starts Kapto, says how long to watch in
// KAPTO_WATCH_SECONDS and how long to profile the page's main thread in
// KAPTO_PROFILE_SECONDS, and reads what this writes into KAPTO_RESULTS:
// pictures drawn a second, each frame's delay, memory over time, the main
// thread's busy time, and its CPU profile. It measures, not checks: the
// command compares the numbers with perf-budget.toml.

const watchSeconds = Number(process.env.KAPTO_WATCH_SECONDS ?? "60");
const profileSeconds = Number(process.env.KAPTO_PROFILE_SECONDS ?? "20");
const results = process.env.KAPTO_RESULTS ?? "/tmp";
const SAMPLE_EVERY_MS = 10_000;

// The browser draws with the machine's graphics card, given to the
// container, rather than on the CPU.
test.use({
  launchOptions: { args: ["--use-angle=gl-egl", "--ignore-gpu-blocklist"] },
  viewport: { width: 1600, height: 1000 },
});

/** The resident memory of every browser process in this container, in MiB. */
const browserMemoryMib = () => {
  let kib = 0;
  for (const pid of readdirSync("/proc").filter((name) => /^\d+$/.test(name))) {
    try {
      const status = readFileSync(`/proc/${pid}/status`, "utf8");
      if (/^Name:\s+(chrome|chromium|headless_shell)/m.test(status)) {
        kib += Number(/^VmRSS:\s+(\d+)/m.exec(status)?.[1] ?? 0);
      }
    } catch {
      // A process that ended meanwhile.
    }
  }
  return kib / 1024;
};

type Window = { kaptoDrawDelays: number[]; kaptoPictures: () => number };

test("the live view under a scenario's load, profiled", async ({ page }) => {
  test.setTimeout((watchSeconds + 120) * 1000);
  // Before the page's own scripts: each frame's arrival, from the time in it
  // of the newest message it shows; the next picture drawn is when the page
  // shows it. Every picture drawn is counted.
  await page.addInitScript(() => {
    const arrived: number[] = [];
    const delays: number[] = [];
    let pictures = 0;
    Object.assign(window, { kaptoDrawDelays: delays, kaptoPictures: () => pictures });
    const Native = window.WebSocket;
    window.WebSocket = class extends Native {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        this.addEventListener("message", (message: MessageEvent) => {
          const data: unknown = message.data;
          if (data instanceof ArrayBuffer && data.byteLength >= 16) {
            if (String.fromCharCode(...new Uint8Array(data, 0, 4)) === "KVFR") {
              arrived.push(Number(new DataView(data).getBigInt64(8, true)) / 1e6);
            }
          }
        });
      }
    };
    const drawn = () => {
      pictures += 1;
      const now = Date.now();
      for (const received of arrived) {
        delays.push(now - received);
      }
      arrived.length = 0;
      requestAnimationFrame(drawn);
    };
    requestAnimationFrame(drawn);
  });
  await page.goto("/");
  const live = page.getByTitle("Live view of the robot");
  await expect(live.getByText(/^Live( · \d+ ms)?$/)).toBeVisible({ timeout: 30_000 });

  const renderer = await page.evaluate(() => {
    const gl = document.createElement("canvas").getContext("webgl");
    const info = gl?.getExtension("WEBGL_debug_renderer_info");
    return gl && info ? String(gl.getParameter(info.UNMASKED_RENDERER_WEBGL)) : "none";
  });
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");
  const metric = async (name: string) =>
    (await cdp.send("Performance.getMetrics")).metrics.find((one) => one.name === name)?.value ?? 0;
  const pictures = () => page.evaluate(() => (window as unknown as Window).kaptoPictures());

  const delays: number[] = [];
  // Whether the profiler ran in it, which slows the page down.
  const samples: {
    at_s: number;
    js_heap_mib: number;
    browser_mib: number;
    fps: number;
    busy: number;
    profiled: boolean;
  }[] = [];
  const started = Date.now();
  let last = { at: started, pictures: await pictures(), task: await metric("TaskDuration") };
  let profile: unknown = null;
  // The main thread profiled over the middle of the watch, once the page has
  // settled and before it ends.
  const profileFrom = Math.max(0, (watchSeconds - profileSeconds) / 2) * 1000;
  while (Date.now() - started < watchSeconds * 1000) {
    const profiling = profile === null && Date.now() - started >= profileFrom;
    if (profiling) {
      await cdp.send("Profiler.enable");
      await cdp.send("Profiler.setSamplingInterval", { interval: 1000 });
      await cdp.send("Profiler.start");
      await page.waitForTimeout(profileSeconds * 1000);
      profile = (await cdp.send("Profiler.stop")).profile;
    } else {
      await page.waitForTimeout(SAMPLE_EVERY_MS);
    }
    // Taken out each time, so they don't grow the page's own memory.
    delays.push(...(await page.evaluate(() => (window as unknown as Window).kaptoDrawDelays.splice(0))));
    const now = { at: Date.now(), pictures: await pictures(), task: await metric("TaskDuration") };
    const seconds = (now.at - last.at) / 1000;
    samples.push({
      at_s: (now.at - started) / 1000,
      js_heap_mib: (await metric("JSHeapUsedSize")) / 1024 / 1024,
      browser_mib: browserMemoryMib(),
      fps: (now.pictures - last.pictures) / seconds,
      busy: (now.task - last.task) / seconds,
      profiled: profiling,
    });
    last = now;
  }
  writeFileSync(`${results}/page.cpuprofile`, JSON.stringify(profile));
  writeFileSync(`${results}/page.json`, JSON.stringify({ renderer, delays_ms: delays, samples }));
  // Live all along: frames still arriving at the end.
  const frames = Number(await live.getAttribute("data-frames"));
  await expect.poll(async () => Number(await live.getAttribute("data-frames"))).toBeGreaterThan(frames);
});
