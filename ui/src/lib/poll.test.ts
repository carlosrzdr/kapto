import { describe, expect, it } from "vitest";

import { oneAtATime } from "./poll";

describe("oneAtATime", () => {
  it("skips a run while the last one waits, and runs again once it has its answer", async () => {
    let runs = 0;
    let answer: () => void = () => {};
    const poll = oneAtATime(() => {
      runs += 1;
      return new Promise<void>((resolve) => (answer = resolve));
    });
    poll();
    poll();
    poll();
    expect(runs).toBe(1);
    answer();
    await Promise.resolve();
    await Promise.resolve();
    poll();
    expect(runs).toBe(2);
  });

  it("runs again after a run that failed", async () => {
    let runs = 0;
    const poll = oneAtATime(() => {
      runs += 1;
      return Promise.reject(new Error("no answer"));
    });
    poll();
    await Promise.resolve();
    await Promise.resolve();
    poll();
    expect(runs).toBe(2);
  });
});
