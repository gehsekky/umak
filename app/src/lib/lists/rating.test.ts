import { describe, expect, it } from "vitest";
import { formatPlays, ratingAt, ratingForDigit, starsLabel, stepRating } from "./rating";

describe("ratingAt", () => {
  it("gives half a star on the left of a star and a whole one on the right", () => {
    expect(ratingAt(1, 100)).toBe(1);
    expect(ratingAt(15, 100)).toBe(2);
    expect(ratingAt(55, 100)).toBe(6);
    expect(ratingAt(100, 100)).toBe(10);
  });

  it("stays in range at the edges", () => {
    expect(ratingAt(0, 100)).toBe(1);
    expect(ratingAt(120, 100)).toBe(10);
    expect(ratingAt(5, 0)).toBe(1);
  });
});

describe("stepRating", () => {
  it("moves by a half-star, stops at the top and clears at the bottom", () => {
    expect(stepRating(null, 1)).toBe(1);
    expect(stepRating(4, 1)).toBe(5);
    expect(stepRating(10, 1)).toBe(10);
    expect(stepRating(1, -1)).toBeNull();
    expect(stepRating(null, -1)).toBeNull();
  });
});

describe("starsLabel", () => {
  it("reads out the rating", () => {
    expect(starsLabel(null)).toBe("Not rated");
    expect(starsLabel(2)).toBe("1 star");
    expect(starsLabel(7)).toBe("3.5 stars");
  });
});

describe("ratingForDigit", () => {
  it("gives whole stars, a clear for zero, and nothing for other keys", () => {
    expect(ratingForDigit("1")).toBe(2);
    expect(ratingForDigit("5")).toBe(10);
    expect(ratingForDigit("0")).toBeNull();
    expect(ratingForDigit("6")).toBeUndefined();
    expect(ratingForDigit("a")).toBeUndefined();
  });
});

describe("formatPlays", () => {
  it("shows nothing for an unplayed track", () => {
    expect(formatPlays({ playCount: 0, lastPlayedAt: null })).toEqual({
      count: "",
      title: "Not played yet",
    });
  });

  it("counts plays and says when the last one was", () => {
    const once = formatPlays({ playCount: 1, lastPlayedAt: null });
    expect(once).toEqual({ count: "1", title: "Played 1 time" });
    const many = formatPlays({ playCount: 3, lastPlayedAt: 1000 });
    expect(many.count).toBe("3");
    expect(many.title).toMatch(/^Played 3 times, last /);
  });
});
