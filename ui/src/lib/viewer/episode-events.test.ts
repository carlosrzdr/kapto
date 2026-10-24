import { describe, expect, it } from "vitest";

import type { SentEvent } from "../api-types";
import { eventRows } from "./episode-events";

const s = 1e9;
const event = (kind: string, at: number, more: Partial<SentEvent> = {}): SentEvent => ({
  kind,
  at_utc_ns: at * s,
  phase: "moment",
  source: "kapto",
  attributes: {},
  ...more,
});
const silent = (stream: string, at: number) => event("kapto.stream_silent", at, { attributes: { stream } });
const back = (stream: string, at: number) => event("kapto.stream_back", at, { attributes: { stream } });

describe("eventRows", () => {
  it("makes a stream silent until it came back one row, each stream its own", () => {
    const rows = eventRows([silent("left", 2), silent("right", 2), back("right", 8), back("left", 8.5)], 20 * s);
    expect(rows.map((row) => [row.kind, row.stream, row.fromNs / s, (row.untilNs ?? 0) / s, row.open])).toEqual([
      ["kapto.stream_silent", "left", 2, 8.5, false],
      ["kapto.stream_silent", "right", 2, 8, false],
    ]);
  });

  it("makes a beginning and its end one row, and says which never ended", () => {
    const rows = eventRows(
      [
        event("intervention", 1, { phase: "begin", id: "a", source: "page" }),
        event("intervention", 3, { phase: "begin", id: "b", source: "page" }),
        event("intervention", 4, { phase: "end", id: "a", source: "page" }),
      ],
      10 * s,
    );
    expect(rows.map((row) => [row.fromNs / s, (row.untilNs ?? 0) / s, row.open])).toEqual([
      [1, 4, false],
      [3, 10, true],
    ]);
  });

  it("keeps moments, whole stretches and ends without their beginning as they are", () => {
    const rows = eventRows(
      [
        event("note", 5, { source: "page", text: "dropped the cup" }),
        event("grasp", 6, { phase: "stretch", until_utc_ns: 7 * s }),
        back("left", 9),
      ],
      10 * s,
    );
    expect(rows.map((row) => [row.kind, row.untilNs === null ? null : row.untilNs / s, row.endOnly, row.text])).toEqual([
      ["note", null, false, "dropped the cup"],
      ["grasp", 7, false, null],
      ["kapto.stream_silent", null, true, null],
    ]);
  });
});
