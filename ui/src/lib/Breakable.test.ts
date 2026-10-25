import { render } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";

import Breakable from "./Breakable.svelte";

describe("Breakable", () => {
  it("offers a line break only after a name's dots and underscores, its text unchanged", () => {
    const { container } = render(Breakable, { text: "capture.control_max_connections" });
    expect(container.textContent).toBe("capture.control_max_connections");
    const before = [...container.querySelectorAll("wbr")].map((wbr) => wbr.previousSibling?.textContent);
    expect(before).toEqual(["capture.", "control_", "max_", "connections"]);
  });
});
