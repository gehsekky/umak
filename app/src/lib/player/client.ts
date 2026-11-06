// The one interface the UI uses to reach the player. This implementation goes through
// the core, which runs the engine in-process; a remote
// player needs a new implementation of this interface and no change to any view.

import { call, onCoreEvent } from "../api/client";
import type { PlayerAction } from "../api/generated/PlayerAction";
import type { PlayerStatus } from "../api/generated/PlayerStatus";
import type { Playlist } from "../api/generated/Playlist";
import type { PlaylistPage } from "../api/generated/PlaylistPage";

export type PlayerUpdate =
  | { kind: "status"; status: PlayerStatus }
  | { kind: "position"; positionMs: number }
  | { kind: "queue"; replaced: boolean }
  | { kind: "error"; message: string };

export interface PlayerClient {
  /** Replaces the player playlist with these tracks and plays from one of them. */
  playTracks(trackIds: string[], startIndex: number): Promise<void>;
  /** Copies a saved playlist into the player playlist and plays from one entry. */
  playPlaylist(id: string, startIndex: number): Promise<void>;
  /** A page of the player playlist. */
  queueItems(offset: number, limit: number): Promise<PlaylistPage>;
  /** Plays one entry of the player playlist, leaving the list as it is. */
  playQueueEntry(index: number): Promise<void>;
  /** Adds tracks to the end of the player playlist. */
  addToQueue(trackIds: string[]): Promise<void>;
  /** Inserts tracks right after the current one. */
  playNext(trackIds: string[]): Promise<void>;
  removeFromQueue(positions: number[]): Promise<void>;
  moveInQueue(positions: number[], before: number | null): Promise<void>;
  clearQueue(): Promise<void>;
  saveQueue(name: string): Promise<Playlist>;
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
  queueItems(offset, limit) {
    return call("player.queue.items", { offset, limit });
  },
  async playQueueEntry(index) {
    await call("player.queue.play", { index });
  },
  async addToQueue(trackIds) {
    await call("player.queue.add", { trackIds });
  },
  async playNext(trackIds) {
    await call("player.queue.playNext", { trackIds });
  },
  async removeFromQueue(positions) {
    await call("player.queue.remove", { positions });
  },
  async moveInQueue(positions, before) {
    await call("player.queue.move", { positions, before });
  },
  async clearQueue() {
    await call("player.queue.clear", {});
  },
  saveQueue(name) {
    return call("player.queue.save", { name });
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
      } else if (event.type === "queueChanged") {
        handler({ kind: "queue", replaced: event.replaced });
      } else if (event.type === "playerError") {
        handler({ kind: "error", message: event.message });
      }
    });
  },
};
