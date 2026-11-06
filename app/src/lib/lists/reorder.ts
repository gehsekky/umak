// Works out how to move selected rows of a list. Rows are identified by position, and
// `gap` is a place between rows: 0 is before the first row, and the row count is after
// the last.

export type MovePlan = {
  positions: number[];
  /** The entry to move before, or null for the end. */
  before: number | null;
  /** The index of the first moved row afterward. */
  newStart: number;
};

function chosenIndexes(all: readonly number[], selected: (position: number) => boolean): number[] {
  return all.flatMap((position, index) => (selected(position) ? [index] : []));
}

/** The move that puts the selected rows together at a gap, or null if nothing would change. */
export function planDrop(
  all: readonly number[],
  selected: (position: number) => boolean,
  gap: number,
): MovePlan | null {
  const chosen = chosenIndexes(all, selected);
  if (chosen.length === 0) return null;
  const first = chosen[0];
  const last = chosen[chosen.length - 1];
  const together = last - first + 1 === chosen.length;
  if (together && gap >= first && gap <= last + 1) return null;
  return {
    positions: chosen.map((index) => all[index]),
    before: gap < all.length ? all[gap] : null,
    newStart: gap - chosen.filter((index) => index < gap).length,
  };
}

/** The move that shifts the selected rows up or down by one, or null at the edge. */
export function planStep(
  all: readonly number[],
  selected: (position: number) => boolean,
  direction: -1 | 1,
): MovePlan | null {
  const chosen = chosenIndexes(all, selected);
  if (chosen.length === 0) return null;
  const first = chosen[0];
  const last = chosen[chosen.length - 1];
  if (direction < 0 && first === 0) return null;
  if (direction > 0 && last === all.length - 1) return null;
  return planDrop(all, selected, direction < 0 ? first - 1 : last + 2);
}
