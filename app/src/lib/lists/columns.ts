// The columns a track list can show. The setting holds which, in order; the core declares the
// same names as its options.

import type { TrackSort } from "../api/generated/TrackSort";

export type ColumnId =
  | "number"
  | "title"
  | "artist"
  | "album"
  | "track"
  | "year"
  | "time"
  | "format"
  | "rating"
  | "plays"
  | "lastPlayed";

export type Column = {
  header: string;
  /** Its track in the grid, such as "4em" or "3fr". */
  width: string;
  align?: "end";
  sort?: TrackSort;
  /** Left out first when the list is narrow, so titles stay readable. */
  optional?: boolean;
};

export const COLUMNS: Record<ColumnId, Column> = {
  number: { header: "#", width: "4em", align: "end" },
  title: { header: "Title", width: "4fr", sort: "title" },
  artist: { header: "Artist", width: "3fr", sort: "artist" },
  album: { header: "Album", width: "3fr", sort: "album" },
  track: { header: "Track", width: "4em", align: "end", optional: true },
  year: { header: "Year", width: "4em", align: "end", optional: true },
  time: { header: "Time", width: "5em", align: "end" },
  format: { header: "Format", width: "7em", optional: true },
  rating: { header: "Rating", width: "5.5em" },
  plays: { header: "Plays", width: "3em", align: "end", optional: true },
  lastPlayed: { header: "Last played", width: "7em", align: "end", optional: true },
};

const DEFAULT_COLUMNS: ColumnId[] = [
  "number",
  "title",
  "artist",
  "album",
  "year",
  "time",
  "format",
  "rating",
  "plays",
];

function isColumn(name: unknown): name is ColumnId {
  return typeof name === "string" && name in COLUMNS;
}

/** The columns a setting's value shows, leaving out names this version does not know. */
export function shownColumns(value: unknown): ColumnId[] {
  if (!Array.isArray(value)) return DEFAULT_COLUMNS;
  const columns = [...new Set(value.filter(isColumn))];
  return columns.includes("title") ? columns : DEFAULT_COLUMNS;
}

/** The grid's column widths, without the optional columns when the list is narrow. */
export function gridColumns(columns: ColumnId[], narrow: boolean): string {
  return columns
    .filter((column) => !(narrow && COLUMNS[column].optional))
    .map((column) => COLUMNS[column].width)
    .join(" ");
}

/** A disc and track number, such as "2-05", or the track alone on a single disc. */
export function formatTrackNumber(track: { discNo: number; trackNo: number }): string {
  if (track.trackNo <= 0) return "";
  return track.discNo > 1
    ? `${track.discNo}-${String(track.trackNo).padStart(2, "0")}`
    : String(track.trackNo);
}

/** When a track was last played, as a short date. */
export function formatLastPlayed(lastPlayedAt: number | null): string {
  return lastPlayedAt === null ? "" : new Date(lastPlayedAt).toLocaleDateString();
}
