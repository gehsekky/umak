import { describe, expect, it } from "vitest";
import { gridLayout, gridMove, visibleRows } from "./grid";

describe("gridLayout", () => {
  it("fits as many tiles as stay at least the minimum width and widens them to fill", () => {
    // Four 160 tiles and three 16 gaps need 688.
    const layout = gridLayout(700, 160, 16, 40, 10);
    expect(layout.columns).toBe(4);
    expect(layout.tileWidth).toBe((700 - 48) / 4);
    expect(layout.rowHeight).toBe(layout.tileWidth + 40 + 16);
    expect(layout.rows).toBe(3);
    expect(gridLayout(687, 160, 16, 40, 10).columns).toBe(3);
  });

  it("keeps one column however narrow", () => {
    const layout = gridLayout(50, 160, 16, 40, 3);
    expect(layout.columns).toBe(1);
    expect(layout.tileWidth).toBe(50);
    expect(layout.rows).toBe(3);
    expect(gridLayout(0, 160, 16, 40, 3).tileWidth).toBe(1);
  });

  it("has no rows when empty", () => {
    expect(gridLayout(700, 160, 16, 40, 0).rows).toBe(0);
  });
});

describe("visibleRows", () => {
  const layout = gridLayout(700, 160, 16, 40, 100); // 4 across, 25 rows of 219

  it("covers the rows in view and some to spare, within the grid", () => {
    expect(visibleRows(layout, 0, 500, 2)).toEqual([0, 5]);
    expect(visibleRows(layout, 219 * 10 + 1, 500, 2)).toEqual([8, 15]);
    expect(visibleRows(layout, 219 * 24, 500, 2)).toEqual([22, 25]);
  });
});

describe("gridMove", () => {
  it("moves by tiles, rows and pages and stops at the ends", () => {
    expect(gridMove("ArrowRight", 0, 4, 10, 2)).toBe(1);
    expect(gridMove("ArrowLeft", 0, 4, 10, 2)).toBe(0);
    expect(gridMove("ArrowDown", 1, 4, 10, 2)).toBe(5);
    expect(gridMove("ArrowDown", 7, 4, 10, 2)).toBe(9);
    expect(gridMove("ArrowUp", 5, 4, 10, 2)).toBe(1);
    expect(gridMove("ArrowUp", 2, 4, 10, 2)).toBe(0);
    expect(gridMove("PageDown", 1, 4, 10, 2)).toBe(9);
    expect(gridMove("PageUp", 9, 4, 10, 1)).toBe(5);
    expect(gridMove("Home", 6, 4, 10, 2)).toBe(0);
    expect(gridMove("End", 0, 4, 10, 2)).toBe(9);
  });

  it("ignores other keys and empty grids", () => {
    expect(gridMove("Enter", 3, 4, 10, 2)).toBeNull();
    expect(gridMove("ArrowRight", 0, 4, 0, 2)).toBeNull();
  });
});
