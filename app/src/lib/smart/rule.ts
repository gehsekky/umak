// What the smart playlist editor offers for each field, and what it asks of a rule. The core
// checks a rule again before using it, and this only says why one is not ready yet.

import type { SmartCondition } from "../api/generated/SmartCondition";
import type { SmartField } from "../api/generated/SmartField";
import type { SmartOp } from "../api/generated/SmartOp";
import type { SmartRule } from "../api/generated/SmartRule";
import type { SmartSort } from "../api/generated/SmartSort";

export type FieldKind = "text" | "format" | "number" | "rating" | "date";

const DEFAULT_DAYS = 180;
const DEFAULT_RATING = 8;

export const FIELDS: { field: SmartField; label: string; kind: FieldKind; unit?: string }[] = [
  { field: "title", label: "Title", kind: "text" },
  { field: "artist", label: "Artist", kind: "text" },
  { field: "album", label: "Album", kind: "text" },
  { field: "genre", label: "Genre", kind: "text" },
  { field: "format", label: "Format", kind: "format" },
  { field: "year", label: "Year", kind: "number" },
  { field: "rating", label: "Rating", kind: "rating" },
  { field: "plays", label: "Plays", kind: "number" },
  { field: "lastPlayed", label: "Last played", kind: "date" },
  { field: "added", label: "Added", kind: "date" },
  { field: "length", label: "Length", kind: "number", unit: "minutes" },
  { field: "bitrate", label: "Bitrate", kind: "number", unit: "kbps" },
];

export const SORTS: { sort: SmartSort; label: string }[] = [
  { sort: "album", label: "Album" },
  { sort: "artist", label: "Artist" },
  { sort: "title", label: "Title" },
  { sort: "added", label: "Date added" },
  { sort: "lastPlayed", label: "Last played" },
  { sort: "rating", label: "Rating" },
  { sort: "plays", label: "Plays" },
  { sort: "year", label: "Year" },
  { sort: "length", label: "Length" },
];

export const DAY_UNITS: { label: string; days: number }[] = [
  { label: "days", days: 1 },
  { label: "weeks", days: 7 },
  { label: "months", days: 30 },
  { label: "years", days: 365 },
];

/** Half-stars, for choosing a rating: 1 is half a star and 10 is five stars. */
export const RATING_CHOICES: { value: number; label: string }[] = Array.from(
  { length: 10 },
  (_, index) => ({
    value: index + 1,
    label: `${(index + 1) / 2} ${index === 1 ? "star" : "stars"}`,
  }),
);

export function kindOf(field: SmartField): FieldKind {
  return FIELDS.find((one) => one.field === field)?.kind ?? "text";
}

export function unitOf(field: SmartField): string | undefined {
  return FIELDS.find((one) => one.field === field)?.unit;
}

const NUMBER_OPS: { op: SmartOp; label: string }[] = [
  { op: "atLeast", label: "at least" },
  { op: "atMost", label: "at most" },
  { op: "is", label: "is" },
];

export function opsFor(field: SmartField): { op: SmartOp; label: string }[] {
  switch (kindOf(field)) {
    case "text":
      return [
        { op: "contains", label: "contains" },
        { op: "notContains", label: "does not contain" },
        { op: "is", label: "is" },
        { op: "isNot", label: "is not" },
      ];
    case "format":
      return [
        { op: "is", label: "is" },
        { op: "isNot", label: "is not" },
      ];
    case "rating":
      return [...NUMBER_OPS, { op: "empty", label: "is not rated" }];
    case "date": {
      const ops: { op: SmartOp; label: string }[] = [
        { op: "withinDays", label: "in the last" },
        { op: "notWithinDays", label: "not in the last" },
      ];
      return field === "lastPlayed" ? [...ops, { op: "empty", label: "never" }] : ops;
    }
    default:
      return field === "year" ? [...NUMBER_OPS, { op: "empty", label: "is not set" }] : NUMBER_OPS;
  }
}

/** A new condition on a field. The values are ones that make sense to start from. */
export function defaultCondition(field: SmartField): SmartCondition {
  const op = opsFor(field)[0].op;
  switch (kindOf(field)) {
    case "text":
    case "format":
      return { field, op, text: "", number: null };
    case "rating":
      return { field, op, text: null, number: DEFAULT_RATING };
    case "date":
      return { field, op, text: null, number: DEFAULT_DAYS };
    default:
      return { field, op, text: null, number: null };
  }
}

export function newRule(): SmartRule {
  return {
    matchAll: true,
    conditions: [defaultCondition("rating")],
    sort: "album",
    descending: false,
    limit: null,
  };
}

/** Switching to "empty" has no value to keep, and switching away brings one back. */
export function withOp(condition: SmartCondition, op: SmartOp): SmartCondition {
  if (op === "empty") return { ...condition, op, text: null, number: null };
  if (condition.op === "empty") return { ...defaultCondition(condition.field), op };
  return { ...condition, op };
}

/** Shows a number of days in the largest unit it fits exactly, so 180 reads as 6 months. */
export function splitDays(days: number | null): { amount: number | null; unit: number } {
  if (days === null) return { amount: null, unit: 1 };
  const unit = [...DAY_UNITS].reverse().find((one) => days % one.days === 0)?.days ?? 1;
  return { amount: days / unit, unit };
}

export function conditionProblem(condition: SmartCondition): string | null {
  const label = FIELDS.find((one) => one.field === condition.field)?.label ?? "This rule";
  if (condition.op === "empty") return null;
  switch (kindOf(condition.field)) {
    case "text":
    case "format":
      return condition.text?.trim() ? null : `${label} needs something to compare with.`;
    case "date":
      return condition.number !== null && Number.isInteger(condition.number) && condition.number >= 1
        ? null
        : `${label} needs a whole number of at least 1.`;
    default:
      return condition.number !== null && Number.isInteger(condition.number) && condition.number >= 0
        ? null
        : `${label} needs a whole number.`;
  }
}

/** Why a rule cannot be used yet, if it cannot. */
export function ruleProblem(rule: SmartRule): string | null {
  for (const condition of rule.conditions) {
    const problem = conditionProblem(condition);
    if (problem) return problem;
  }
  if (rule.limit !== null && (!Number.isInteger(rule.limit) || rule.limit < 1)) {
    return "The limit needs a whole number of at least 1.";
  }
  return null;
}
