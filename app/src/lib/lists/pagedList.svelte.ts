// A list too long to hold in memory: rows load by page as they scroll into view, and
// only pages near the view are kept.

export type Key = string | number;
export type Page<T> = { rows: T[]; total: number };
export type Source<T> = (offset: number, limit: number) => Promise<Page<T>>;

const PAGE_SIZE = 200;
const BULK_SIZE = 1000;
const MAX_PAGES = 30;

export class PagedList<T> {
  total = $state(0);
  error = $state<string | null>(null);
  /** False until the current source has answered once. */
  ready = $state(false);
  /** Changes when pages should be requested again. */
  generation = $state(0);
  /** Changes when any row changes. */
  private version = $state(0);

  private source: Source<T>;
  private pages = new Map<number, T[]>();
  /** Rows from before a reload, shown until their replacements arrive. */
  private stale = new Map<number, T[]>();
  private pending = new Map<number, Promise<void>>();
  private lastRange: [number, number] = [0, 0];
  readonly key: (row: T) => Key;

  constructor(source: Source<T>, key: (row: T) => Key) {
    this.source = source;
    this.key = key;
  }

  /** The row at an index, if it has loaded. */
  row(index: number): T | undefined {
    void this.version;
    const page = Math.floor(index / PAGE_SIZE);
    const offset = index % PAGE_SIZE;
    return this.pages.get(page)?.[offset] ?? this.stale.get(page)?.[offset];
  }

  /** Requests the rows in a range of indexes, and resolves once they have loaded. */
  ensure(start: number, end: number): Promise<void> {
    this.lastRange = [start, end];
    const first = Math.floor(Math.max(start, 0) / PAGE_SIZE);
    const last = Math.floor(Math.max(end - 1, start, 0) / PAGE_SIZE);
    const loads: Promise<void>[] = [];
    for (let page = first; page <= last; page++) {
      if (this.pages.has(page)) continue;
      loads.push(this.pending.get(page) ?? this.load(page));
    }
    return Promise.all(loads).then(() => undefined);
  }

  private load(page: number): Promise<void> {
    const generation = this.generation;
    const loading = this.source(page * PAGE_SIZE, PAGE_SIZE)
      .then(({ rows, total }) => {
        if (generation !== this.generation) return;
        this.pages.set(page, rows);
        this.total = total;
        this.ready = true;
        this.error = null;
        this.pending.delete(page);
        this.trim(page);
        this.version++;
      })
      .catch((failure: { message?: string }) => {
        if (generation !== this.generation) return;
        this.pending.delete(page);
        this.error = failure.message ?? String(failure);
        this.ready = true;
      });
    this.pending.set(page, loading);
    return loading;
  }

  private trim(around: number): void {
    if (this.pages.size <= MAX_PAGES) return;
    const farthest = [...this.pages.keys()].sort(
      (a, b) => Math.abs(b - around) - Math.abs(a - around),
    );
    for (const page of farthest.slice(0, this.pages.size - MAX_PAGES)) {
      this.pages.delete(page);
    }
  }

  private restart(): void {
    this.generation++;
    this.pending.clear();
    this.pages = new Map();
  }

  /** Fetches the visible rows again, keeping the old ones on screen until they arrive. */
  reload(): Promise<void> {
    this.stale = this.pages;
    this.restart();
    return this.ensure(...this.lastRange);
  }

  /** Switches to different contents, such as a new search. */
  setSource(source: Source<T>): void {
    this.source = source;
    this.stale = new Map();
    this.ready = false;
    this.restart();
    this.version++;
  }

  /** A range of rows, loaded or not. `end` is exclusive. */
  async rowsInRange(start: number, end: number): Promise<T[]> {
    const found: T[] = [];
    for (let offset = start; offset < end; offset += BULK_SIZE) {
      const { rows } = await this.source(offset, Math.min(BULK_SIZE, end - offset));
      found.push(...rows);
    }
    return found;
  }

  /** The keys of a range of rows, loaded or not. `end` is exclusive. */
  async keysInRange(start: number, end: number): Promise<Key[]> {
    return (await this.rowsInRange(start, end)).map((row) => this.key(row));
  }
}
