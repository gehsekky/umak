// The only module in the UI that touches the transport. Everything else calls the
// core through these functions, so another transport changes only this file.

import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CommandMap } from "./generated/commands";
import type { CoreEvent } from "./generated/CoreEvent";

const EVENT_CHANNEL = "umak:event";

export function call<K extends keyof CommandMap>(
  name: K,
  args: CommandMap[K]["args"],
): Promise<CommandMap[K]["output"]> {
  return invoke("call", { name, args });
}

/** The two sizes cover art is kept at: for grids and lists, and for the player. */
export type ArtSize = "small" | "large";

/** Where a picture named by `tracks.art` can be loaded, as an ordinary image source. */
export function artUrl(art: string, size: ArtSize): string {
  return convertFileSrc(`${art}-${size}`, "umak-art");
}

export function onCoreEvent(handler: (event: CoreEvent) => void): Promise<UnlistenFn> {
  return listen<CoreEvent>(EVENT_CHANNEL, (message) => handler(message.payload));
}
