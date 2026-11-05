// Text for values shown in lists.

import type { TrackRow } from "../api/generated/TrackRow";

export function formatDuration(ms: number | null): string {
  if (ms === null) return "";
  const total = Math.round(ms / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = String(total % 60).padStart(2, "0");
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}`
    : `${minutes}:${seconds}`;
}

export function formatTotalTime(ms: number): string {
  const minutes = Math.round(ms / 60000);
  const hours = Math.floor(minutes / 60);
  return hours > 0 ? `${hours} hr ${minutes % 60} min` : `${minutes} min`;
}

export function countOf(count: number, noun: string): string {
  return `${count.toLocaleString()} ${noun}${count === 1 ? "" : "s"}`;
}

/** For example "MP3", or "FLAC 24/96" when the bit depth is known. */
export function formatQuality(
  track: Pick<TrackRow, "codec" | "sampleRate" | "bitDepth">,
): string {
  const codec = (track.codec ?? "").toUpperCase();
  if (track.bitDepth === null || track.sampleRate === null) return codec;
  const rate = track.sampleRate / 1000;
  return `${codec} ${track.bitDepth}/${Number.isInteger(rate) ? rate : rate.toFixed(1)}`;
}
