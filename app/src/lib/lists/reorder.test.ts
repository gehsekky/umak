import { describe, expect, it } from "vitest";
import { planDrop, planStep } from "./reorder";

const ALL = [10, 20, 30, 40, 50];
const among =
  (...positions: number[]) =>
  (position: number) =>
    positions.includes(position);

describe("planDrop", () => {
  it("moves a row before the entry at the gap", () => {
    expect(planDrop(ALL, among(40), 1)).toEqual({ positions: [40], before: 20, newStart: 1 });
  });

  it("moves down, counting the rows that leave from above the gap", () => {
    expect(planDrop(ALL, among(10, 20), 4)).toEqual({
      positions: [10, 20],
      before: 50,
      newStart: 2,
    });
  });

  it("moves to the end", () => {
    expect(planDrop(ALL, among(20), 5)).toEqual({ positions: [20], before: null, newStart: 4 });
  });

  it("does nothing when rows that are together drop on themselves", () => {
    for (const gap of [1, 2, 3]) expect(planDrop(ALL, among(20, 30), gap)).toBeNull();
  });

  it("gathers scattered rows even when dropped between them", () => {
    expect(planDrop(ALL, among(10, 50), 2)).toEqual({
      positions: [10, 50],
      before: 30,
      newStart: 1,
    });
  });

  it("does nothing with no selection", () => {
    expect(planDrop(ALL, among(), 2)).toBeNull();
  });
});

describe("planStep", () => {
  it("moves up by one", () => {
    expect(planStep(ALL, among(30, 40), -1)).toEqual({
      positions: [30, 40],
      before: 20,
      newStart: 1,
    });
  });

  it("moves down by one, and to the end from the second to last place", () => {
    expect(planStep(ALL, among(20), 1)).toEqual({ positions: [20], before: 40, newStart: 2 });
    expect(planStep(ALL, among(40), 1)).toEqual({ positions: [40], before: null, newStart: 4 });
  });

  it("stops at the edges", () => {
    expect(planStep(ALL, among(10), -1)).toBeNull();
    expect(planStep(ALL, among(50), 1)).toBeNull();
    expect(planStep(ALL, among(), 1)).toBeNull();
  });
});
