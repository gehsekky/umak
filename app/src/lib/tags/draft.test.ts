import { describe, expect, it, vi } from "vitest";
import type { TrackTagValues } from "../api/generated/TrackTagValues";
import { TagDraft } from "./draft.svelte";

function track(id: string, values: { album?: string; title?: string; n?: string } = {}): TrackTagValues {
  return {
    trackId: id,
    relPath: `${id}.mp3`,
    error: null,
    fields: [
      { field: "album", values: values.album ? [values.album] : [] },
      { field: "title", values: values.title ? [values.title] : [] },
      { field: "trackNumber", values: values.n ? [values.n] : [] },
    ],
  };
}

function draftOver(tracks: Record<string, TrackTagValues>) {
  const reader = vi.fn(async (ids: string[]) => ids.map((id) => tracks[id]));
  return { draft: new TagDraft(reader), reader };
}

describe("TagDraft", () => {
  it("reads a selection in small pieces and finds what the tracks share", async () => {
    const ids = Array.from({ length: 60 }, (_, index) => `t${index}`);
    const { draft, reader } = draftOver(
      Object.fromEntries(ids.map((id) => [id, track(id, { album: "Same", title: id })])),
    );
    await draft.open(ids);
    expect(reader.mock.calls.length).toBeGreaterThan(1);
    expect(draft.loaded).toBe(60);
    expect(draft.allRead).toBe(true);
    expect(draft.shared.album.values).toEqual(["Same"]);
    expect(draft.tracks[59].title).toBe("t59");
    expect(draft.changes).toBeNull();
  });

  it("edits a track named twice only once", async () => {
    const { draft } = draftOver({ a: track("a") });
    await draft.open(["a", "a"]);
    expect(draft.tracks).toHaveLength(1);
  });

  it("builds a request from shared and own changes", async () => {
    const { draft } = draftOver({
      a: track("a", { album: "Old", n: "1" }),
      b: track("b", { album: "Old", n: "2" }),
    });
    await draft.open(["a", "b"]);
    draft.shared.album.touched = true;
    draft.shared.album.values = ["New"];
    draft.setOwn("b", "title", "Bee");
    expect(draft.changes).toEqual([
      { trackIds: ["a"], edits: [{ field: "album", values: ["New"] }] },
      {
        trackIds: ["b"],
        edits: [
          { field: "album", values: ["New"] },
          { field: "title", values: ["Bee"] },
        ],
      },
    ]);
  });

  it("numbers in list order, and leaves tracks already right out of the request", async () => {
    const { draft } = draftOver({
      a: track("a", { n: "1" }),
      b: track("b", { n: "9" }),
    });
    await draft.open(["a", "b"]);
    draft.numberTracks();
    expect(draft.shownValue(draft.tracks[1], "trackNumber")).toBe("2");
    expect(draft.changes).toEqual([
      { trackIds: ["b"], edits: [{ field: "trackNumber", values: ["2"] }] },
    ]);
  });

  it("holds back values the core would refuse", async () => {
    const { draft } = draftOver({ a: track("a") });
    await draft.open(["a"]);
    expect(draft.problems).toEqual([]);
    draft.shared.year.touched = true;
    draft.shared.year.values = ["soon"];
    draft.setOwn("a", "trackNumber", "x");
    expect(draft.problems).toHaveLength(2);
  });

  it("includes a file it could not read, so the preview can report it", async () => {
    const { draft } = draftOver({
      a: track("a", { album: "Same" }),
      b: { trackId: "b", relPath: "b.mp3", fields: [], error: "this file is missing" },
    });
    await draft.open(["a", "b"]);
    // Its missing values do not make the field look mixed.
    expect(draft.shared.album.mixed).toBe(false);
    draft.shared.album.touched = true;
    draft.shared.album.values = ["New"];
    expect(draft.changes?.[0].trackIds).toEqual(["a", "b"]);
  });

  it("leaves out a track gone from the library, which the core would refuse", async () => {
    const { draft } = draftOver({
      a: track("a"),
      b: { trackId: "b", relPath: null, fields: [], error: "no longer in the library" },
    });
    await draft.open(["a", "b"]);
    draft.shared.album.touched = true;
    draft.shared.album.values = ["New"];
    expect(draft.changes?.[0].trackIds).toEqual(["a"]);
  });

  it("drops a read still going when another selection opens", async () => {
    let finish: (value: TrackTagValues[]) => void = () => {};
    const slow = new Promise<TrackTagValues[]>((resolve) => (finish = resolve));
    const reader = vi.fn((ids: string[]) =>
      ids[0] === "old" ? slow : Promise.resolve([track("new", { album: "Fresh" })]),
    );
    const draft = new TagDraft(reader);
    const first = draft.open(["old"]);
    await draft.open(["new"]);
    finish([track("old", { album: "Stale" })]);
    await first;
    expect(draft.tracks.map((row) => row.id)).toEqual(["new"]);
    expect(draft.shared.album.values).toEqual(["Fresh"]);
  });

  it("reports a failed read but keeps the form", async () => {
    const draft = new TagDraft(async () => {
      throw { message: "no" };
    });
    await draft.open(["a"]);
    expect(draft.readError).toBe("no");
    expect(draft.active).toBe(true);
  });
});
