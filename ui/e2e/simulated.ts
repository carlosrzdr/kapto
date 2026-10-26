// How a simulation ran, answered for a demo episode: the demo isn't a
// simulation. Over the episode's span: at half real time for its first
// second, paused for the next, then reset to zero and at real time to its
// end.

import type { Page } from "@playwright/test";

/** Answers every episode's `/simulated` with such a run, over its replay's span. */
export async function answerSimulation(page: Page): Promise<void> {
  await page.route("**/api/episodes/*/simulated", async (route) => {
    const id = new URL(route.request().url()).pathname.split("/")[3]!;
    const opening = await (await page.request.get(`/api/episodes/${id}/replay/opening`)).json();
    const [start, end] = [opening.start_utc_ns as number, opening.end_utc_ns as number];
    const S = 1e9;
    const after = end - start - 2.01 * S;
    await route.fulfill({
      json: {
        computed_utc_ns: end,
        simulated: {
          start_ns: 0,
          end_ns: after,
          simulated_ns: 0.5 * S + after,
          real_ns: end - start,
          paused_ns: S,
          ratio: (0.5 * S + after) / (end - start - S),
          pauses: [{ start_utc_ns: start + S, end_utc_ns: start + 2 * S, simulated_ns: 0.5 * S }],
          resets: [{ at_utc_ns: start + 2.01 * S, from_ns: 0.5 * S, to_ns: 0 }],
          segments: [
            [
              { receive_utc_ns: start, simulated_ns: 0 },
              { receive_utc_ns: start + S, simulated_ns: 0.5 * S },
              { receive_utc_ns: start + 2 * S, simulated_ns: 0.5 * S },
            ],
            [
              { receive_utc_ns: start + 2.01 * S, simulated_ns: 0 },
              { receive_utc_ns: end, simulated_ns: after },
            ],
          ],
        },
      },
    });
  });
}
