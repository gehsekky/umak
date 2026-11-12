// What double-clicking a track, or pressing Enter on it, does, as the settings say.

import { runAction } from "../actions/registry";
import { settings } from "../settings/state.svelte";

export type TrackActivation = "here" | "album" | "next" | "queue";

export function trackActivation(): TrackActivation {
  const chosen = settings.value("behavior.trackActivate");
  return chosen === "album" || chosen === "next" || chosen === "queue" ? chosen : "here";
}

/**
 * Acts on an activated track. `here` plays from it in the list it is in, which each list does
 * its own way. Queueing takes the tracks selected with it, which `selected` gives only when
 * the track is one of them.
 */
export async function activateTrack(
  trackId: string,
  selected: () => string[] | Promise<string[]>,
  here: () => unknown,
): Promise<void> {
  const how = trackActivation();
  if (how === "here") {
    await here();
    return;
  }
  const chosen = await selected();
  const trackIds = chosen.includes(trackId) ? chosen : [trackId];
  switch (how) {
    case "album":
      void runAction("player.playAlbum", { trackId });
      return;
    case "next":
      void runAction("queue.playNext", { trackIds });
      return;
    case "queue":
      void runAction("queue.add", { trackIds });
      return;
  }
}
