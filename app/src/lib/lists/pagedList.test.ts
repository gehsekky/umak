import { describe, expect, it, vi } from "vitest";
import { PagedList, type Source } from "./pagedList.svelte";

/** A source over `total` numbered rows, where each row reads `${tag}${index}`. */
function numbered(total: number, tag = "") {
  return vi.fn<Source<string>>(async (offset, limit) => ({
    rows: Array.from({ length: Math.max(0, Math.min(limit, total - offset)) }, (_, index) =>
      `${tag}${offset + index}`,
    ),
    total,
  }));
}

const keyOf = (row: string) => row;

describe("PagedList", () => {
  it("loads the pages covering a range and reports the total", async () => {
    const source = numbered(1000);
    const list = new PagedList(source, keyOf);
    expect(list.ready).toBe(false);
    await list.ensure(150, 250);
    expect(list.ready).toBe(true);
    expect(list.total).toBe(1000);
    expect(list.row(150)).toBe("150");
    expect(list.row(399)).toBe("399");
    expect(list.row(400)).toBeUndefined();
    expect(source).toHaveBeenCalledTimes(2);
  });

  it("does not fetch a page it has or is already fetching", async () => {
    const source = numbered(1000);
    const list = new PagedList(source, keyOf);
    await Promise.all([list.ensure(0, 10), list.ensure(5, 15)]);
    await list.ensure(0, 10);
    expect(source).toHaveBeenCalledTimes(1);
  });

  it("drops pages far from the view", async () => {
    const list = new PagedList(numbered(100_000), keyOf);
    await list.ensure(0, 10);
    for (let page = 1; page <= 40; page++) {
      await list.ensure(page * 200, page * 200 + 10);
    }
    expect(list.row(0)).toBeUndefined();
    expect(list.row(40 * 200)).toBe(String(40 * 200));
  });

  it("ignores an answer from a source that has been replaced", async () => {
    let answer: (page: { rows: string[]; total: number }) => void = () => {};
    const slow: Source<string> = () => new Promise((resolve) => (answer = resolve));
    const list = new PagedList(slow, keyOf);
    const first = list.ensure(0, 10);

    const replacement = numbered(5, "new");
    list.setSource(replacement);
    await list.ensure(0, 10);
    answer({ rows: ["old"], total: 999 });
    await first;

    expect(list.total).toBe(5);
    expect(list.row(0)).toBe("new0");
  });

  it("keeps old rows on screen during a reload, then replaces them", async () => {
    let contents = "old";
    const list = new PagedList<string>(
      async (offset) => ({ rows: [`${contents}${offset}`], total: 1 }),
      keyOf,
    );
    await list.ensure(0, 1);
    contents = "new";
    const reloading = list.reload();
    expect(list.row(0)).toBe("old0");
    await reloading;
    expect(list.row(0)).toBe("new0");
  });

  it("shows nothing from the old contents after switching source", async () => {
    const list = new PagedList(numbered(10, "a"), keyOf);
    await list.ensure(0, 5);
    list.setSource(numbered(10, "b"));
    expect(list.row(0)).toBeUndefined();
    expect(list.ready).toBe(false);
  });

  it("records a failure and can recover on reload", async () => {
    let fail = true;
    const source: Source<string> = async () => {
      if (fail) throw { code: "internal", message: "database locked" };
      return { rows: ["row"], total: 1 };
    };
    const list = new PagedList(source, keyOf);
    await list.ensure(0, 1);
    expect(list.error).toBe("database locked");
    expect(list.ready).toBe(true);

    fail = false;
    await list.reload();
    expect(list.error).toBeNull();
    expect(list.row(0)).toBe("row");
  });

  it("lists keys across pages without caching them", async () => {
    const source = numbered(2500);
    const list = new PagedList(source, keyOf);
    const keys = await list.keysInRange(100, 2300);
    expect(keys).toHaveLength(2200);
    expect(keys[0]).toBe("100");
    expect(keys.at(-1)).toBe("2299");
    expect(list.row(100)).toBeUndefined();
  });
});
