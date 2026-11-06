// The one interface the UI uses to reach the player. This implementation goes through
// the core, which runs the engine in-process; a remote
// player needs a new implementation of this interface and no change to any view.

import { call, onCoreEvent } from "../api/client";
import type { PlayerAction } from "../api/generated/PlayerAction";
import type { PlayerStatus } from "../api/generated/PlayerStatus";

export type PlayerUpdate =
  | { kind: "status"; status: PlayerStatus }
  | { kind: "position"; positionMs: number }
  | { kind: "error"; message: string };

export interface PlayerClient {
  /** Replaces the player playlist with these tracks and plays from one of them. */
  playTracks(trackIds: string[], startIndex: number): Promise<void>;
  /** Copies a saved playlist into the player playlist and plays from one entry. */
  playPlaylist(id: string, startIndex: number): Promise<void>;
  control(action: PlayerAction): Promise<void>;
  seek(ms: number): Promise<void>;
  status(): Promise<PlayerStatus>;
  subscribe(handler: (update: PlayerUpdate) => void): Promise<() => void>;
}

export const playerClient: PlayerClient = {
  async playTracks(trackIds, startIndex) {
    await call("player.playTracks", { trackIds, startIndex });
  },
  async playPlaylist(id, startIndex) {
    await call("player.playPlaylist", { id, startIndex });
  },
  async control(action) {
    await call("player.control", { action });
  },
  async seek(ms) {
    await call("player.seek", { ms });
  },
  status() {
    return call("player.status", {});
  },
  subscribe(handler) {
    return onCoreEvent((event) => {
      if (event.type === "playerChanged") {
        handler({ kind: "status", status: event.status });
      } else if (event.type === "playerPosition") {
        handler({ kind: "position", positionMs: event.positionMs });
      } else if (event.type === "playerError") {
        handler({ kind: "error", message: event.message });
      }
    });
  },
};
