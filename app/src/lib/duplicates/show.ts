// What the duplicates page shows about a copy.

import type { DuplicateTrack } from "../api/generated/DuplicateTrack";
import type { TrackRow } from "../api/generated/TrackRow";

/** Shown for a track with no artist tag, so it is not something a search can find. */
const UNKNOWN_ARTIST = "Unknown Artist";

/** The whole path, written the way this root's path is, so it can be pasted into a file manager. */
export function fullPath(root: string | undefined, relPath: string): string {
  if (root === undefined) return relPath;
  const separator = root.includes("\\") ? "\\" : "/";
  const base = root.endsWith(separator) ? root : root + separator;
  return base + relPath.replaceAll("/", separator);
}

export function formatSize(bytes: number): string {
  const megabytes = bytes / (1024 * 1024);
  return megabytes >= 1 ? `${megabytes.toFixed(1)} MB` : `${Math.ceil(bytes / 1024)} KB`;
}

export function tagsOf(track: TrackRow): string {
  return [track.title, track.artist, track.album].join("  |  ");
}

/** Whether a group's copies disagree on the tags its heading shows. */
export function tagsDiffer(tracks: DuplicateTrack[]): boolean {
  return tracks.some((copy) => tagsOf(copy.track) !== tagsOf(tracks[0].track));
}

/** What to search the library for to find a track. */
export function searchText(track: TrackRow): string {
  return track.artist === UNKNOWN_ARTIST ? track.title : `${track.title} ${track.artist}`;
}

/** The facts to compare copies by, such as "#3 · FLAC 16/44.1 · 900 kbps · 3:15 · 22.1 MB". */
export function copyFacts(copy: DuplicateTrack, quality: string, duration: string): string {
  return [
    copy.track.trackNo > 0 ? `#${copy.track.trackNo}` : "",
    quality,
    copy.bitrate === null ? "" : `${copy.bitrate} kbps`,
    duration,
    formatSize(copy.size),
  ]
    .filter((fact) => fact !== "")
    .join(" · ");
}
