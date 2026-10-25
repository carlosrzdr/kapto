/// <reference types="node" />
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { DASHES, SERIES, seriesStyle } from "./theme";

// Read from the file (the tests run in `ui/`), as the test runner hands stylesheets to tests empty.
const css = readFileSync("src/app.css", "utf8");

/** The value of `--name` in `app.css`. */
const tokenOf = (name: string) => css.match(new RegExp(`--${name}:\\s*(#[0-9a-f]{6})`))?.[1];

describe("SERIES", () => {
  it("is the page's series tokens, in order", () => {
    expect(SERIES.map((_, place) => tokenOf(`series-${place}`))).toEqual(SERIES);
    expect(tokenOf(`series-${SERIES.length}`)).toBeUndefined();
  });

  it("uses none of the state colours, which keep their meaning", () => {
    for (const state of ["accent", "ok", "info", "warning", "danger"]) {
      expect(SERIES).not.toContain(tokenOf(state));
    }
  });
});

describe("seriesStyle", () => {
  it("gives each colour solid first, then each again only with another pattern", () => {
    const styles = Array.from({ length: SERIES.length * DASHES.length }, (_, place) => seriesStyle(place));
    const seen = new Set(styles.map((style) => `${style.color}/${style.dash.join(",")}`));
    expect(seen.size).toBe(styles.length);
    expect(styles.slice(0, SERIES.length).every((style) => style.dash.length === 0)).toBe(true);
    for (const [place, style] of styles.entries()) {
      const before = styles.slice(0, place).filter((earlier) => earlier.color === style.color);
      expect(before.every((earlier) => earlier.pattern !== style.pattern)).toBe(true);
    }
  });

  it("names the classes the key draws its swatch with", () => {
    expect(seriesStyle(0)).toEqual({ color: SERIES[0], hue: 0, dash: [], pattern: 0 });
    expect(seriesStyle(SERIES.length + 1)).toEqual({ color: SERIES[1], hue: 1, dash: DASHES[1], pattern: 1 });
  });
});
