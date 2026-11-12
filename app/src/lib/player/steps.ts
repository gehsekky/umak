// How far the skip and volume actions move, from the settings.

import { settings } from "../settings/state.svelte";

function number(key: string, fallback: number): number {
  const value = settings.value(key);
  return typeof value === "number" ? value : fallback;
}

export function skipMs(): number {
  return number("player.skipSeconds", 5) * 1000;
}

/** A share of the volume slider. */
export function volumeStep(): number {
  return number("player.volumeStep", 5) / 100;
}
