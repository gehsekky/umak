// The logic of the tag editor, kept free of the screen. An untouched field is never written:
// a value shown here may differ from the file by the time of the preview, and only what the
// person changed should go into a batch.

import type { FieldEdit } from "../api/generated/FieldEdit";
import type { TagEdit } from "../api/generated/TagEdit";
import type { TagField } from "../api/generated/TagField";
import type { TrackEdits } from "../api/generated/TrackEdits";
import { countOf } from "../lists/format";

export type FieldInfo = {
  field: TagField;
  label: string;
  /** Whether the field can hold several values. */
  list: boolean;
};

/** The fields edited for the whole selection at once. */
export const SHARED_FIELDS: readonly FieldInfo[] = [
  { field: "album", label: "Album", list: false },
  { field: "artists", label: "Artist", list: true },
  { field: "albumArtists", label: "Album artist", list: true },
  { field: "composers", label: "Composer", list: true },
  { field: "genres", label: "Genre", list: true },
  { field: "year", label: "Year", list: false },
  { field: "discNumber", label: "Disc", list: false },
];

const FIELD_LABELS: Record<TagField, string> = {
  title: "Title",
  album: "Album",
  artists: "Artist",
  albumArtists: "Album artist",
  composers: "Composer",
  genres: "Genre",
  year: "Year",
  trackNumber: "Track",
  discNumber: "Disc",
};

export function fieldLabel(field: TagField): string {
  return FIELD_LABELS[field];
}

/** A field shared by the selection, as the form holds it. */
export type SharedField = {
  /** What the tracks hold, once all have been read and they agree. */
  original: string[];
  /** The tracks read so far hold different values. */
  mixed: boolean;
  /** How many tracks have been read. */
  seen: number;
  /** What the form shows. Each entry is one whole value; nothing here splits text. */
  values: string[];
  /** The person changed this field. */
  touched: boolean;
  /** The person asked to remove the field, which an empty box alone cannot say when mixed. */
  cleared: boolean;
};

export function newSharedField(): SharedField {
  return { original: [], mixed: false, seen: 0, values: [""], touched: false, cleared: false };
}

function sameValues(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((value, index) => value === b[index]);
}

function formValues(values: readonly string[]): string[] {
  return values.length > 0 ? [...values] : [""];
}

/** Takes in what some more tracks hold. What the person typed stays. */
export function absorbValues(state: SharedField, tracks: readonly (readonly string[])[]): void {
  for (const values of tracks) {
    if (state.seen === 0) state.original = [...values];
    else if (!sameValues(state.original, values)) state.mixed = true;
    state.seen++;
  }
  if (!state.touched) state.values = state.mixed ? [""] : formValues(state.original);
}

/** Puts a field back to what the tracks hold. */
export function resetField(state: SharedField): void {
  state.touched = false;
  state.cleared = false;
  state.values = state.mixed ? [""] : formValues(state.original);
}

export function clearField(state: SharedField): void {
  state.touched = true;
  state.cleared = true;
  state.values = [""];
}

function cleaned(values: readonly string[]): string[] {
  return values.map((value) => value.trim()).filter((value) => value !== "");
}

/**
 * The values a field should be given, or null to leave it alone. Emptying a box removes a value
 * every track agreed on, but an empty box over different values, or over tracks not read yet,
 * only means nothing was typed.
 */
export function sharedEdit(state: SharedField, allRead: boolean): string[] | null {
  if (!state.touched) return null;
  if (state.cleared) return [];
  const values = cleaned(state.values);
  const agreed = allRead && !state.mixed;
  if (values.length === 0) return agreed && state.original.length > 0 ? [] : null;
  return agreed && sameValues(values, state.original) ? null : values;
}

const MAX_YEAR = 65535;
const MAX_NUMBER = 4294967295;

/** What is wrong with a value, in the same terms the core would refuse it. */
export function valueProblem(field: TagField, value: string): string | null {
  const text = value.trim();
  if (text === "") return null;
  if (field !== "year" && field !== "trackNumber" && field !== "discNumber") return null;
  const number = /^\d+$/.test(text) ? Number(text) : NaN;
  if (field === "year") {
    return number > 0 && number <= MAX_YEAR ? null : "A year is a whole number, such as 1997";
  }
  return number > 0 && number <= MAX_NUMBER ? null : "A whole number above 0";
}

/** What one track's own fields would change, given what its file holds. */
export function ownEdits(
  original: { title: string; trackNumber: string },
  typed: { title?: string; trackNumber?: string },
): FieldEdit[] {
  const edits: FieldEdit[] = [];
  const add = (field: TagField, was: string, now: string | undefined): void => {
    if (now === undefined) return;
    const text = now.trim();
    if (text !== was.trim()) edits.push({ field, values: text === "" ? [] : [text] });
  };
  add("title", original.title, typed.title);
  add("trackNumber", original.trackNumber, typed.trackNumber);
  return edits;
}

/**
 * The request for a batch. Tracks with only the shared edits go together; a track with values
 * of its own gets its own entry. Null when nothing would change.
 */
export function buildChanges(
  trackIds: readonly string[],
  shared: readonly FieldEdit[],
  own: ReadonlyMap<string, FieldEdit[]>,
): TrackEdits[] | null {
  const changes: TrackEdits[] = [];
  const plain: string[] = [];
  for (const id of trackIds) {
    const edits = own.get(id);
    if (edits && edits.length > 0) changes.push({ trackIds: [id], edits: [...shared, ...edits] });
    else plain.push(id);
  }
  if (shared.length > 0 && plain.length > 0) changes.unshift({ trackIds: plain, edits: [...shared] });
  return changes.length > 0 ? changes : null;
}

/** Track numbers for rows in list order, from 1. */
export function numberInOrder(count: number): string[] {
  return Array.from({ length: count }, (_, index) => String(index + 1));
}

function isWaiting(edit: TagEdit): boolean {
  return edit.status === "ready" || edit.status === "previewing";
}

/** How a batch stands, in a few words. */
export function describeEdit(edit: TagEdit): string {
  const parts: string[] = [];
  if (isWaiting(edit)) {
    parts.push(`${countOf(edit.changing, "file")} will change`);
    if (edit.unchanged > 0) parts.push(`${countOf(edit.unchanged, "file")} already correct`);
    if (edit.blocked > 0) parts.push(`${countOf(edit.blocked, "file")} cannot be changed`);
    return parts.join(", ");
  }
  parts.push(`${countOf(edit.written, "file")} written`);
  if (edit.failed > 0) parts.push(`${edit.failed.toLocaleString()} failed`);
  if (edit.blocked > 0) parts.push(`${edit.blocked.toLocaleString()} could not be changed`);
  if (edit.skipped > 0) parts.push(`${edit.skipped.toLocaleString()} not reached`);
  if (edit.unsure > 0) parts.push(`${edit.unsure.toLocaleString()} unsure`);
  if (edit.unchanged > 0) parts.push(`${edit.unchanged.toLocaleString()} already correct`);
  return parts.join(", ");
}

/** Whether undoing a batch has anything to do. */
export function canUndo(edit: TagEdit): boolean {
  return !isWaiting(edit) && edit.status !== "writing" && edit.written + edit.unsure > 0;
}

const STATUS_LABELS: Record<string, string> = {
  pending: "Will change",
  blocked: "Cannot change",
  writing: "Writing",
  written: "Written",
  failed: "Failed",
  skipped: "Not reached",
  unsure: "Unsure",
};

export function itemStatusLabel(status: string): string {
  return STATUS_LABELS[status] ?? status;
}

/** Whether an item's status means it did not, or may not, get written. */
export function isProblem(status: string): boolean {
  return ["blocked", "failed", "skipped", "unsure"].includes(status);
}
