// Which rows of a list are selected. Rows are held by key, so a selection survives a
// reload that shifts positions, and can cover rows that have not loaded.

import type { Key } from "./pagedList.svelte";

export class Selection {
  keys = $state.raw<ReadonlySet<Key>>(new Set());
  /** Where a shift-selection extends from. */
  anchor = $state<number | null>(null);
  /** The row keyboard movement starts from. */
  cursor = $state<number | null>(null);

  has(key: Key): boolean {
    return this.keys.has(key);
  }

  get size(): number {
    return this.keys.size;
  }

  clear(): void {
    this.keys = new Set();
    this.anchor = null;
    this.cursor = null;
  }

  /** Selects one row alone, as a plain click does. */
  only(index: number, key: Key): void {
    this.keys = new Set([key]);
    this.anchor = index;
    this.cursor = index;
  }

  /** Adds or removes one row, as a ctrl-click does. */
  toggle(index: number, key: Key): void {
    const next = new Set(this.keys);
    if (!next.delete(key)) next.add(key);
    this.keys = next;
    this.anchor = index;
    this.cursor = index;
  }

  /** Replaces the selection with a range, keeping the anchor, as a shift-click does. */
  setRange(keys: Iterable<Key>, cursor: number): void {
    this.keys = new Set(keys);
    this.cursor = cursor;
  }

  /** The range a shift-selection to `index` covers, with an exclusive end. */
  rangeTo(index: number): [number, number] {
    const anchor = this.anchor ?? index;
    return [Math.min(anchor, index), Math.max(anchor, index) + 1];
  }
}
