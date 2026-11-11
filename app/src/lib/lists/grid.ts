// Layout and keyboard movement for a grid of equal tiles, such as album covers.

export type GridLayout = {
  columns: number;
  /** The width of a tile, which is also the height of its picture. */
  tileWidth: number;
  /** From the top of one row to the top of the next. */
  rowHeight: number;
  rows: number;
};

/**
 * Fits as many tiles across as keep at least `minTile` wide, then widens them to fill the
 * row. A row is a square picture with a caption below, and rows are `gap` apart.
 */
export function gridLayout(
  width: number,
  minTile: number,
  gap: number,
  caption: number,
  total: number,
): GridLayout {
  const columns = Math.max(1, Math.floor((width + gap) / (minTile + gap)));
  const tileWidth = Math.max(1, (width - gap * (columns - 1)) / columns);
  return {
    columns,
    tileWidth,
    rowHeight: tileWidth + caption + gap,
    rows: Math.ceil(total / columns),
  };
}

/** The rows drawn for a scroll position, with an exclusive end, and some to spare. */
export function visibleRows(
  layout: GridLayout,
  scrollTop: number,
  viewHeight: number,
  overscan: number,
): [number, number] {
  const first = Math.floor(scrollTop / layout.rowHeight);
  const last = Math.ceil((scrollTop + viewHeight) / layout.rowHeight);
  return [Math.max(0, first - overscan), Math.min(layout.rows, last + overscan)];
}

/**
 * Where a key moves the cursor, or null if the key does not move it. Moving past an edge
 * stops at the first or last tile.
 */
export function gridMove(
  key: string,
  index: number,
  columns: number,
  total: number,
  pageRows: number,
): number | null {
  if (total === 0) return null;
  const steps: Record<string, number> = {
    ArrowLeft: -1,
    ArrowRight: 1,
    ArrowUp: -columns,
    ArrowDown: columns,
    PageUp: -columns * pageRows,
    PageDown: columns * pageRows,
  };
  if (key === "Home") return 0;
  if (key === "End") return total - 1;
  const step = steps[key];
  if (step === undefined) return null;
  return Math.max(0, Math.min(total - 1, index + step));
}
