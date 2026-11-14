import { describe, expect, it } from "vitest";
import { browseOf } from "./browse";

describe("browseOf", () => {
  it("places each library view, and an album by where it was opened from", () => {
    expect(browseOf({ kind: "library" })).toBe("library");
    expect(browseOf({ kind: "artists" })).toBe("artists");
    expect(browseOf({ kind: "artist", id: "a" })).toBe("artists");
    expect(browseOf({ kind: "albums" })).toBe("albums");
    expect(browseOf({ kind: "album", id: "b" })).toBe("albums");
    expect(browseOf({ kind: "album", id: "b", from: { id: "a", name: "A" } })).toBe("artists");
  });

  it("leaves views outside the library out", () => {
    expect(browseOf({ kind: "settings" })).toBeNull();
    expect(browseOf({ kind: "playlist", id: "p" })).toBeNull();
  });
});
