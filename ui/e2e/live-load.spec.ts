import { readdirSync, readFileSync, writeFileSync } from "node:fs";

import { expect, test } from "@playwright/test";

// The live view watched for a long time at full load, for
// `cargo xtask bench browser`, which starts Kapto, says how long to watch in
// KAPTO_WATCH_SECONDS and reads what this writes into KAPTO_RESULTS. It
// measures, not checks: the benchmark compares the numbers with its targets.

const watchSeconds = Number(process.env.KAPTO_WATCH_SECONDS ?? "60");
const results = process.env.KAPTO_RESULTS ?? "/tmp";
const SAMPLE_EVERY_MS = 10_000;

// The browser draws with the machine's graphics card, given to the
// container, rather than on the CPU.
test.use({ launchOptions: { args: ["--use-angle=gl-egl", "--ignore-gpu-blocklist"] } });

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

test("the live view at full load, watched for a long time", async ({ page }) => {
  test.setTimeout((watchSeconds + 120) * 1000);
  // Before the page's own scripts: each frame's arrival, from the time in it
  // of the newest message it shows, and then the next picture the browser
  // draws, which is when the page draws what arrived.
  await page.addInitScript(() => {
    const arrived: number[] = [];
    const delays: number[] = [];
    Object.assign(window, { kaptoDrawDelays: delays });
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
  const delays: number[] = [];
  const samples: { at_s: number; js_heap_mib: number; browser_mib: number; frames: number }[] = [];
  const started = Date.now();
  while (Date.now() - started < watchSeconds * 1000) {
    await page.waitForTimeout(SAMPLE_EVERY_MS);
    // Taken out each time, so they don't grow the page's own memory.
    delays.push(
      ...(await page.evaluate(() => (window as unknown as { kaptoDrawDelays: number[] }).kaptoDrawDelays.splice(0))),
    );
    const { metrics } = await cdp.send("Performance.getMetrics");
    const heap = metrics.find((metric) => metric.name === "JSHeapUsedSize")?.value ?? 0;
    samples.push({
      at_s: (Date.now() - started) / 1000,
      js_heap_mib: heap / 1024 / 1024,
      browser_mib: browserMemoryMib(),
      frames: Number(await live.getAttribute("data-frames")),
    });
  }
  writeFileSync(`${results}/live-load.json`, JSON.stringify({ renderer, delays_ms: delays, samples }));
  // Live all along: frames still arriving at the end.
  const last = samples.at(-1)?.frames ?? 0;
  await expect.poll(async () => Number(await live.getAttribute("data-frames"))).toBeGreaterThan(last);
});
