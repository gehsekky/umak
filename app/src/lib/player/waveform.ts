// Fitting a track's waveform to the width of the seek bar.

/** Columns stored per track can outnumber the pixels across, or fall short of them. */
export function resample(values: readonly number[], count: number): number[] {
  if (values.length === 0 || count <= 0) return [];
  const columns: number[] = [];
  for (let column = 0; column < count; column++) {
    const first = Math.floor((column * values.length) / count);
    const end = Math.max(first + 1, Math.floor(((column + 1) * values.length) / count));
    // Each pixel shows the loudest of the stored columns under it, so peaks never vanish.
    let loudest = 0;
    for (let index = first; index < end; index++) loudest = Math.max(loudest, values[index]);
    columns.push(loudest);
  }
  return columns;
}

/** Where playback has reached across the bar, from 0 to 1. */
export function playedShare(positionMs: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return Math.min(1, Math.max(0, positionMs / durationMs));
}
