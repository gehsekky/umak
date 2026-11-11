import { describe, expect, it } from "vitest";
import { playedShare, resample } from "./waveform";

describe("fitting a waveform to the seek bar", () => {
  it("keeps the loudest stored column under each pixel", () => {
    expect(resample([1, 9, 2, 3, 8, 4], 3)).toEqual([9, 3, 8]);
    expect(resample([1, 9, 2, 3, 8, 4], 2)).toEqual([9, 8]);
  });

  it("repeats columns when the bar is wider than the waveform", () => {
    expect(resample([5, 7], 4)).toEqual([5, 5, 7, 7]);
  });

  it("gives nothing for nothing", () => {
    expect(resample([], 10)).toEqual([]);
    expect(resample([1, 2], 0)).toEqual([]);
  });

  it("measures how much has played, within the track", () => {
    expect(playedShare(30_000, 120_000)).toBe(0.25);
    expect(playedShare(500_000, 120_000)).toBe(1);
    expect(playedShare(-5, 120_000)).toBe(0);
    expect(playedShare(1000, 0)).toBe(0);
  });
});
