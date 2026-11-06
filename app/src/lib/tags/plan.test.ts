import { describe, expect, it } from "vitest";
import type { TagEdit } from "../api/generated/TagEdit";
import {
  absorbValues,
  buildChanges,
  canUndo,
  clearField,
  describeEdit,
  newSharedField,
  numberInOrder,
  ownEdits,
  resetField,
  sharedEdit,
  valueProblem,
} from "./plan";

function field(...tracks: string[][]) {
  const state = newSharedField();
  absorbValues(state, tracks);
  return state;
}

function batch(changes: Partial<TagEdit>): TagEdit {
  return {
    id: "e",
    undoes: null,
    undoneBy: null,
    status: "done",
    createdAt: 0,
    finishedAt: 0,
    unchanged: 0,
    changing: 0,
    blocked: 0,
    written: 0,
    failed: 0,
    skipped: 0,
    unsure: 0,
    ...changes,
  };
}

describe("a shared field", () => {
  it("shows a value the tracks agree on, and an empty box when they differ", () => {
    expect(field(["A"], ["A"]).values).toEqual(["A"]);
    const mixed = field(["A"], ["B"]);
    expect(mixed.mixed).toBe(true);
    expect(mixed.values).toEqual([""]);
  });

  it("treats a different order of values as different", () => {
    expect(field(["A", "B"], ["B", "A"]).mixed).toBe(true);
  });

  it("is never written unless touched", () => {
    expect(sharedEdit(field(["A"], ["A"]), true)).toBeNull();
    expect(sharedEdit(field(["A"], ["B"]), true)).toBeNull();
    // Not even while the tracks are still being read, when what it shows may not apply to all.
    expect(sharedEdit(field(["A"]), false)).toBeNull();
  });

  it("gives what was typed, trimmed, without splitting it", () => {
    const state = field(["A"], ["A"]);
    state.touched = true;
    state.values = ["  Dr. Dre/Eminem ", "", "B; C"];
    expect(sharedEdit(state, true)).toEqual(["Dr. Dre/Eminem", "B; C"]);
  });

  it("leaves a field alone when typing ends up where it started", () => {
    const state = field(["A"], ["A"]);
    state.touched = true;
    state.values = [" A "];
    expect(sharedEdit(state, true)).toBeNull();
  });

  it("removes an agreed value when its box is emptied", () => {
    const state = field(["A"], ["A"]);
    state.touched = true;
    state.values = [""];
    expect(sharedEdit(state, true)).toEqual([]);
  });

  it("does not take an empty box over different values as a removal", () => {
    const state = field(["A"], ["B"]);
    state.touched = true;
    state.values = [""];
    expect(sharedEdit(state, true)).toBeNull();
    clearField(state);
    expect(sharedEdit(state, true)).toEqual([]);
  });

  it("does not take an empty box as a removal before every track is read", () => {
    const state = field(["A"]);
    state.touched = true;
    state.values = [""];
    expect(sharedEdit(state, false)).toBeNull();
  });

  it("sends typed values even if they match the tracks read so far", () => {
    const state = field(["A"]);
    state.touched = true;
    state.values = ["A"];
    expect(sharedEdit(state, false)).toEqual(["A"]);
  });

  it("keeps what was typed as more tracks arrive", () => {
    const state = field(["A"]);
    state.touched = true;
    state.values = ["Typed"];
    absorbValues(state, [["B"]]);
    expect(state.values).toEqual(["Typed"]);
    expect(state.mixed).toBe(true);
    expect(sharedEdit(state, true)).toEqual(["Typed"]);
  });

  it("can be put back", () => {
    const state = field(["A"], ["A"]);
    clearField(state);
    resetField(state);
    expect(state.values).toEqual(["A"]);
    expect(sharedEdit(state, true)).toBeNull();
  });
});

describe("number values", () => {
  it("accepts whole numbers above 0", () => {
    expect(valueProblem("trackNumber", "7")).toBeNull();
    expect(valueProblem("trackNumber", "07")).toBeNull();
    expect(valueProblem("year", "1997")).toBeNull();
  });

  it("refuses anything else", () => {
    expect(valueProblem("trackNumber", "0")).not.toBeNull();
    expect(valueProblem("trackNumber", "3/12")).not.toBeNull();
    expect(valueProblem("discNumber", "-1")).not.toBeNull();
    expect(valueProblem("year", "2001-05-03")).not.toBeNull();
    expect(valueProblem("year", "70000")).not.toBeNull();
  });

  it("allows an empty box, which removes the value", () => {
    expect(valueProblem("year", "")).toBeNull();
  });

  it("has no opinion on text fields", () => {
    expect(valueProblem("album", "0")).toBeNull();
  });
});

describe("a track's own fields", () => {
  const original = { title: "One", trackNumber: "1" };

  it("changes only what differs", () => {
    expect(ownEdits(original, {})).toEqual([]);
    expect(ownEdits(original, { title: " One " })).toEqual([]);
    expect(ownEdits(original, { title: "Two", trackNumber: "1" })).toEqual([
      { field: "title", values: ["Two"] },
    ]);
  });

  it("removes a value when emptied", () => {
    expect(ownEdits(original, { trackNumber: "" })).toEqual([{ field: "trackNumber", values: [] }]);
  });

  it("does nothing to an empty tag left empty", () => {
    expect(ownEdits({ title: "", trackNumber: "" }, { title: "" })).toEqual([]);
  });
});

describe("the request", () => {
  const album = { field: "album" as const, values: ["X"] };
  const number = { field: "trackNumber" as const, values: ["2"] };

  it("is null when nothing would change", () => {
    expect(buildChanges(["a"], [], new Map())).toBeNull();
    expect(buildChanges(["a"], [], new Map([["a", []]]))).toBeNull();
  });

  it("groups tracks with only the shared edits", () => {
    expect(buildChanges(["a", "b"], [album], new Map())).toEqual([
      { trackIds: ["a", "b"], edits: [album] },
    ]);
  });

  it("gives a track with its own values both", () => {
    expect(buildChanges(["a", "b"], [album], new Map([["b", [number]]]))).toEqual([
      { trackIds: ["a"], edits: [album] },
      { trackIds: ["b"], edits: [album, number] },
    ]);
  });

  it("leaves out tracks with nothing to change", () => {
    expect(buildChanges(["a", "b"], [], new Map([["b", [number]]]))).toEqual([
      { trackIds: ["b"], edits: [number] },
    ]);
  });

  it("names each track once", () => {
    const changes = buildChanges(["a", "b", "c"], [album], new Map([["c", [number]]]));
    expect(changes?.flatMap((change) => change.trackIds).sort()).toEqual(["a", "b", "c"]);
  });
});

describe("numbering", () => {
  it("counts from 1", () => {
    expect(numberInOrder(3)).toEqual(["1", "2", "3"]);
  });
});

describe("describing a batch", () => {
  it("says what a preview would do", () => {
    expect(describeEdit(batch({ status: "ready", changing: 2, unchanged: 1, blocked: 3 }))).toBe(
      "2 files will change, 1 file already correct, 3 files cannot be changed",
    );
  });

  it("says what a write did, leaving out what did not happen", () => {
    expect(describeEdit(batch({ written: 1 }))).toBe("1 file written");
    expect(describeEdit(batch({ written: 2, failed: 1, skipped: 4 }))).toBe(
      "2 files written, 1 failed, 4 not reached",
    );
  });

  it("offers undo only when a file was or may have been written", () => {
    expect(canUndo(batch({ written: 1 }))).toBe(true);
    expect(canUndo(batch({ status: "interrupted", unsure: 1 }))).toBe(true);
    expect(canUndo(batch({ failed: 3 }))).toBe(false);
    expect(canUndo(batch({ status: "ready", changing: 3 }))).toBe(false);
    expect(canUndo(batch({ status: "writing", written: 3 }))).toBe(false);
  });
});
