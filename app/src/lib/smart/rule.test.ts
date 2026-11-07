import { describe, expect, it } from "vitest";
import type { SmartCondition } from "../api/generated/SmartCondition";
import {
  conditionProblem,
  defaultCondition,
  FIELDS,
  newRule,
  opsFor,
  RATING_CHOICES,
  ruleProblem,
  splitDays,
  withOp,
} from "./rule";

describe("opsFor", () => {
  it("offers only what the core accepts for each field", () => {
    expect(opsFor("title").map((one) => one.op)).toEqual(["contains", "notContains", "is", "isNot"]);
    expect(opsFor("format").map((one) => one.op)).toEqual(["is", "isNot"]);
    expect(opsFor("rating").map((one) => one.op)).toContain("empty");
    expect(opsFor("year").map((one) => one.op)).toContain("empty");
    expect(opsFor("plays").map((one) => one.op)).not.toContain("empty");
    expect(opsFor("lastPlayed").map((one) => one.op)).toContain("empty");
    expect(opsFor("added").map((one) => one.op)).not.toContain("empty");
  });

  it("starts every field on an operation it has", () => {
    for (const { field } of FIELDS) {
      expect(opsFor(field).map((one) => one.op)).toContain(defaultCondition(field).op);
    }
  });
});

describe("withOp", () => {
  const rated: SmartCondition = { field: "rating", op: "atLeast", text: null, number: 6 };

  it("keeps the value when moving between comparisons", () => {
    expect(withOp(rated, "atMost").number).toBe(6);
  });

  it("drops the value for empty, and brings back a starting one afterwards", () => {
    const empty = withOp(rated, "empty");
    expect(empty.number).toBeNull();
    expect(conditionProblem(empty)).toBeNull();
    const again = withOp(empty, "atMost");
    expect(again.op).toBe("atMost");
    expect(again.number).toBe(8);
  });
});

describe("splitDays", () => {
  it("uses the largest unit that fits exactly", () => {
    expect(splitDays(180)).toEqual({ amount: 6, unit: 30 });
    expect(splitDays(730)).toEqual({ amount: 2, unit: 365 });
    expect(splitDays(14)).toEqual({ amount: 2, unit: 7 });
    expect(splitDays(5)).toEqual({ amount: 5, unit: 1 });
    expect(splitDays(null)).toEqual({ amount: null, unit: 1 });
  });
});

describe("problems", () => {
  it("asks for what a condition is missing", () => {
    expect(conditionProblem(defaultCondition("title"))).toMatch(/Title needs/);
    expect(conditionProblem({ ...defaultCondition("title"), text: "  " })).toMatch(/Title needs/);
    expect(conditionProblem({ ...defaultCondition("title"), text: "x" })).toBeNull();
    expect(conditionProblem(defaultCondition("year"))).toMatch(/Year needs/);
    expect(conditionProblem({ ...defaultCondition("year"), number: 1.5 })).toMatch(/Year needs/);
    expect(conditionProblem({ ...defaultCondition("plays"), number: 0 })).toBeNull();
    expect(conditionProblem({ ...defaultCondition("added"), number: 0 })).toMatch(/at least 1/);
  });

  it("is happy with a new rule, and with no conditions at all", () => {
    expect(ruleProblem(newRule())).toBeNull();
    expect(ruleProblem({ ...newRule(), conditions: [] })).toBeNull();
  });

  it("checks the limit", () => {
    expect(ruleProblem({ ...newRule(), limit: 0 })).toMatch(/limit/);
    expect(ruleProblem({ ...newRule(), limit: 25 })).toBeNull();
  });
});

describe("rating choices", () => {
  it("run in half-stars and read naturally", () => {
    expect(RATING_CHOICES[0]).toEqual({ value: 1, label: "0.5 stars" });
    expect(RATING_CHOICES[1]).toEqual({ value: 2, label: "1 star" });
    expect(RATING_CHOICES[9]).toEqual({ value: 10, label: "5 stars" });
  });
});
