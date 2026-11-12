import { describe, expect, it } from "vitest";
import { restorable, startingView } from "./startView";

describe("the view the app opens on", () => {
  it("is the one the setting names", () => {
    expect(startingView("library", { kind: "albums" }, [])).toEqual({ kind: "library" });
    expect(startingView("albums", null, [])).toEqual({ kind: "albums" });
    expect(startingView(undefined, null, [])).toEqual({ kind: "library" });
  });

  it("is the one left last time when asked, if it still makes sense", () => {
    expect(startingView("last", { kind: "settings" }, [])).toEqual({ kind: "settings" });
    expect(startingView("last", { kind: "playlist", id: "p" }, ["p"])).toEqual({
      kind: "playlist",
      id: "p",
    });
    expect(startingView("last", { kind: "playlist", id: "gone" }, ["p"])).toEqual({
      kind: "library",
    });
    expect(startingView("last", { kind: "album", id: "a" }, [])).toEqual({ kind: "album", id: "a" });
    expect(startingView("last", { kind: "tagEditor" }, [])).toEqual({ kind: "library" });
    expect(startingView("last", { kind: "album" }, [])).toEqual({ kind: "library" });
    expect(startingView("last", "albums", [])).toEqual({ kind: "library" });
  });

  it("keeps only views worth opening on", () => {
    expect(restorable({ kind: "albums" })).toEqual({ kind: "albums" });
    expect(restorable({ kind: "tagReview", id: "e" })).toBeNull();
  });
});
