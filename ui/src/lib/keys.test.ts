import { describe, expect, it } from "vitest";

import { shortcut } from "./keys";

const pressed = (key: string, target: Element = document.body, extra: Partial<KeyboardEventInit> = {}) => {
  const event = new KeyboardEvent("keydown", { key, ...extra });
  Object.defineProperty(event, "target", { value: target });
  return shortcut(event);
};

describe("shortcut", () => {
  it("names the key pressed on the page", () => {
    expect(pressed("R")).toBe("r");
    expect(pressed(" ")).toBe("space");
    expect(pressed("Escape")).toBe("escape");
  });

  it("is none while typing or with a modifier held, nor Space on a control it presses", () => {
    const field = document.createElement("input");
    const button = document.createElement("button");
    expect(pressed("r", field)).toBeNull();
    expect(pressed("r", document.body, { ctrlKey: true })).toBeNull();
    expect(pressed(" ", button)).toBeNull();
    // A button just clicked keeps focus: R still stops.
    expect(pressed("r", button)).toBe("r");
  });
});
