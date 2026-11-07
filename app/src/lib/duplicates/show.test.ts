import { describe, expect, it } from "vitest";
import type { DuplicateTrack } from "../api/generated/DuplicateTrack";
import type { TrackRow } from "../api/generated/TrackRow";
import { copyFacts, formatSize, fullPath, searchText, tagsDiffer } from "./show";

function track(fields: Partial<TrackRow> = {}): TrackRow {
  return {
    id: "1",
    title: "Song",
    artist: "Artist",
    album: "Album",
    durationMs: 1000,
    trackNo: 1,
    discNo: 0,
    year: null,
    codec: "mp3",
    sampleRate: 44100,
    bitDepth: null,
    missing: false,
    ...fields,
  };
}

function copy(fields: Partial<TrackRow> = {}, bitrate: number | null = 320): DuplicateTrack {
  return { track: track(fields), rootId: "r", relPath: "a.mp3", size: 5 * 1024 * 1024, bitrate };
}

describe("fullPath", () => {
  it("writes the whole path the way the root is written", () => {
    expect(fullPath("M:\\Music", "a/b.mp3")).toBe("M:\\Music\\a\\b.mp3");
    expect(fullPath("M:\\", "a/b.mp3")).toBe("M:\\a\\b.mp3");
    expect(fullPath("/music", "a/b.mp3")).toBe("/music/a/b.mp3");
    expect(fullPath("/music/", "a/b.mp3")).toBe("/music/a/b.mp3");
  });

  it("falls back on the relative path while the root is unknown", () => {
    expect(fullPath(undefined, "a/b.mp3")).toBe("a/b.mp3");
  });
});

describe("copyFacts", () => {
  it("lists what there is to compare and leaves out what is unknown", () => {
    expect(copyFacts(copy({ trackNo: 3 }), "MP3", "0:01")).toBe("#3 · MP3 · 320 kbps · 0:01 · 5.0 MB");
    expect(copyFacts(copy({ trackNo: 0 }, null), "MP3", "")).toBe("MP3 · 5.0 MB");
  });

  it("gives small files in kilobytes", () => {
    expect(formatSize(1500)).toBe("2 KB");
  });
});

describe("tagsDiffer", () => {
  it("notices copies that disagree on what the heading shows", () => {
    expect(tagsDiffer([copy(), copy()])).toBe(false);
    expect(tagsDiffer([copy(), copy({ title: "Other" })])).toBe(true);
    expect(tagsDiffer([copy(), copy({ album: "Other" })])).toBe(true);
    expect(tagsDiffer([copy(), copy({ codec: "flac" })])).toBe(false);
  });
});

describe("searchText", () => {
  it("leaves out an artist that only stands for a missing tag", () => {
    expect(searchText(track())).toBe("Song Artist");
    expect(searchText(track({ artist: "Unknown Artist" }))).toBe("Song");
  });
});
