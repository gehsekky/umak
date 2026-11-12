import { describe, expect, it } from "vitest";
import { formatTrackNumber, gridColumns, shownColumns } from "./columns";

describe("track list columns", () => {
  it("come from the setting, in its order, leaving out names not known", () => {
    expect(shownColumns(["rating", "title", "genre", "title"])).toEqual(["rating", "title"]);
  });

  it("fall back to the defaults for a value without the title", () => {
    const defaults = shownColumns(undefined);
    expect(defaults).toContain("title");
    expect(shownColumns(["artist"])).toEqual(defaults);
    expect(shownColumns("title")).toEqual(defaults);
  });

  it("leave the optional ones out of a narrow grid", () => {
    const columns = shownColumns(["number", "title", "year", "time"]);
    expect(gridColumns(columns, false)).toBe("4em 4fr 4em 5em");
    expect(gridColumns(columns, true)).toBe("4em 4fr 5em");
  });

  it("number tracks with their disc when there is more than one", () => {
    expect(formatTrackNumber({ discNo: 1, trackNo: 7 })).toBe("7");
    expect(formatTrackNumber({ discNo: 2, trackNo: 5 })).toBe("2-05");
    expect(formatTrackNumber({ discNo: 0, trackNo: 0 })).toBe("");
  });
});
