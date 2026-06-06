import { describe, expect, it } from "vitest";

import { Robots } from "./robots";

/** Robots named after what was loaded, counting each load. */
function counted(fail: Set<string> = new Set()) {
  const loads: (string | null)[] = [];
  const robots = new Robots<string>((id) => {
    loads.push(id);
    const name = id ?? "live";
    return fail.has(name) ? Promise.reject(new Error(`${name} failed`)) : Promise.resolve(name);
  });
  return { robots, loads };
}

describe("the robots the view draws", () => {
  it("loads the live robot once, however often it comes back to it", async () => {
    const { robots, loads } = counted();
    expect(await robots.of(null)).toBe("live");
    expect(await robots.of("e1")).toBe("e1");
    expect(await robots.of(null)).toBe("live");
    expect(loads).toEqual([null, "e1"]);
  });

  it("keeps the replayed episode's robot while it is replayed, and only the last", async () => {
    const { robots, loads } = counted();
    await robots.of("e1");
    await robots.of("e1");
    await robots.of("e2");
    await robots.of("e1");
    expect(loads).toEqual(["e1", "e2", "e1"]);
  });

  it("loads a robot that failed again the next time", async () => {
    const fail = new Set(["e1", "live"]);
    const { robots, loads } = counted(fail);
    await expect(robots.of("e1")).rejects.toThrow("e1 failed");
    await expect(robots.of(null)).rejects.toThrow("live failed");
    fail.clear();
    expect(await robots.of("e1")).toBe("e1");
    expect(await robots.of(null)).toBe("live");
    expect(loads).toEqual(["e1", null, "e1", null]);
  });
});
