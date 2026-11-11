import { describe, expect, it } from "vitest";
import { countOf, formatDuration, formatQuality, formatSize, formatTotalTime } from "./format";

describe("formatting", () => {
  it("formats durations", () => {
    expect(formatDuration(null)).toBe("");
    expect(formatDuration(5000)).toBe("0:05");
    expect(formatDuration(65_400)).toBe("1:05");
    expect(formatDuration(3_725_000)).toBe("1:02:05");
  });

  it("formats total times", () => {
    expect(formatTotalTime(90_000)).toBe("2 min");
    expect(formatTotalTime(7_500_000)).toBe("2 hr 5 min");
  });

  it("formats file sizes", () => {
    expect(formatSize(1500)).toBe("2 KB");
    expect(formatSize(5 * 1024 * 1024)).toBe("5.0 MB");
  });

  it("counts with the right plural", () => {
    expect(countOf(1, "track")).toBe("1 track");
    expect(countOf(0, "track")).toBe("0 tracks");
    expect(countOf(21387, "track")).toBe((21387).toLocaleString() + " tracks");
  });

  it("names audio quality", () => {
    expect(formatQuality({ codec: "mp3", sampleRate: 44100, bitDepth: null })).toBe("MP3");
    expect(formatQuality({ codec: "flac", sampleRate: 96000, bitDepth: 24 })).toBe("FLAC 24/96");
    expect(formatQuality({ codec: "flac", sampleRate: 44100, bitDepth: 16 })).toBe("FLAC 16/44.1");
    expect(formatQuality({ codec: null, sampleRate: null, bitDepth: null })).toBe("");
  });
});
