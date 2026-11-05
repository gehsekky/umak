import { describe, expect, it } from "vitest";
import { Selection } from "./selection.svelte";

describe("Selection", () => {
  it("selects one row alone", () => {
    const selection = new Selection();
    selection.only(3, "a");
    selection.only(5, "b");
    expect([...selection.keys]).toEqual(["b"]);
    expect(selection.anchor).toBe(5);
    expect(selection.cursor).toBe(5);
  });

  it("toggles rows in and out", () => {
    const selection = new Selection();
    selection.toggle(1, "a");
    selection.toggle(2, "b");
    expect(selection.size).toBe(2);
    selection.toggle(1, "a");
    expect([...selection.keys]).toEqual(["b"]);
    expect(selection.has("a")).toBe(false);
  });

  it("computes a range from the anchor in either direction", () => {
    const selection = new Selection();
    expect(selection.rangeTo(4)).toEqual([4, 5]);
    selection.only(10, "a");
    expect(selection.rangeTo(12)).toEqual([10, 13]);
    expect(selection.rangeTo(8)).toEqual([8, 11]);
  });

  it("keeps the anchor when a range is set, so it can extend again", () => {
    const selection = new Selection();
    selection.only(10, "a");
    selection.setRange(["a", "b"], 11);
    expect(selection.anchor).toBe(10);
    expect(selection.cursor).toBe(11);
    expect(selection.size).toBe(2);
  });

  it("clears everything", () => {
    const selection = new Selection();
    selection.only(1, "a");
    selection.clear();
    expect(selection.size).toBe(0);
    expect(selection.anchor).toBeNull();
    expect(selection.cursor).toBeNull();
  });
});
