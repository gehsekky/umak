// The one interface the UI uses to reach the player. This implementation goes through
// the core, which runs the engine in-process; a remote
// player needs a new implementation of this interface and no change to any view.

import { call, onCoreEvent } from "../api/client";
import type { EqPresets } from "../api/generated/EqPresets";
import type { GainCoverage } from "../api/generated/GainCoverage";
import type { PlayerAction } from "../api/generated/PlayerAction";
import type { PlayerSettings } from "../api/generated/PlayerSettings";
import type { ReplayGainMode } from "../api/generated/ReplayGainMode";
import type { PlayerStatus } from "../api/generated/PlayerStatus";
import type { Playlist } from "../api/generated/Playlist";
import type { TrackArt } from "../api/generated/TrackArt";
import type { PlaylistPage } from "../api/generated/PlaylistPage";
import type { TrackWaveform } from "../api/generated/TrackWaveform";
import type { VisualFrame } from "../api/generated/VisualFrame";
import { type Unwatch, visualsWatch } from "./visuals";

export type PlayerUpdate =
  | { kind: "status"; status: PlayerStatus }
  | { kind: "position"; positionMs: number }
  | { kind: "queue"; replaced: boolean }
  | { kind: "settings"; settings: PlayerSettings }
  | { kind: "eqPresets" }
  | { kind: "waveform"; trackId: string }
  | { kind: "art"; trackIds: string[] }
  | { kind: "error"; message: string };

export interface PlayerClient {
  /** Replaces the player playlist with these tracks and plays from one of them. */
  playTracks(trackIds: string[], startIndex: number): Promise<void>;
  /** Copies a saved playlist into the player playlist and plays from one entry. */
  playPlaylist(id: string, startIndex: number): Promise<void>;
  /** A page of the player playlist. */
  queueItems(offset: number, limit: number): Promise<PlaylistPage>;
  /** Replaces the player playlist with the album a track is on, and plays it. */
  playAlbum(trackId: string): Promise<void>;
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
  settings(): Promise<PlayerSettings>;
  /** Sets the output gain, from 0 to 1. */
  setVolume(volume: number): Promise<void>;
  setMuted(muted: boolean): Promise<void>;
  setGain(replayGain: ReplayGainMode, preampDb: number): Promise<void>;
  setEq(enabled: boolean, bands: number[]): Promise<void>;
  eqPresets(): Promise<EqPresets>;
  saveEqPreset(name: string, bands: number[]): Promise<void>;
  deleteEqPreset(name: string): Promise<void>;
  /** How much of the library has ReplayGain tags. */
  gainCoverage(): Promise<GainCoverage>;
  /** A track's waveform. If it is pending, a "waveform" update follows when it is not. */
  waveform(trackId: string): Promise<TrackWaveform>;
  /** A track's cover art. If it is pending, an "art" update naming the track follows. */
  art(trackId: string): Promise<TrackArt>;
  /** Follows the player. Also stops visual frames a page that has gone left on. */
  subscribe(handler: (update: PlayerUpdate) => void): Promise<() => void>;
  /** Receives visual frames, at a steady rate while something plays, until stopped. */
  watchVisuals(handler: (frame: VisualFrame) => void): Promise<Unwatch>;
}

const visuals = visualsWatch<VisualFrame>(
  async (enabled) => {
    await call("player.setVisuals", { enabled });
  },
  (handler) =>
    onCoreEvent((event) => {
      if (event.type === "visualFrame") handler(event.frame);
    }),
);

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
  async playAlbum(trackId) {
    await call("player.playAlbum", { trackId });
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
  settings() {
    return call("player.settings", {});
  },
  async setVolume(volume) {
    await call("player.setVolume", { volume });
  },
  async setMuted(muted) {
    await call("player.setMuted", { muted });
  },
  async setGain(replayGain, preampDb) {
    await call("player.setGain", { replayGain, preampDb });
  },
  async setEq(enabled, bands) {
    await call("player.setEq", { enabled, bands });
  },
  eqPresets() {
    return call("player.eqPresets", {});
  },
  async saveEqPreset(name, bands) {
    await call("player.eqPresets.save", { name, bands });
  },
  async deleteEqPreset(name) {
    await call("player.eqPresets.delete", { name });
  },
  gainCoverage() {
    return call("player.gainCoverage", {});
  },
  waveform(trackId) {
    return call("tracks.waveform", { trackId });
  },
  art(trackId) {
    return call("tracks.art", { trackId });
  },
  async subscribe(handler) {
    const stop = await onCoreEvent((event) => {
      if (event.type === "playerChanged") {
        handler({ kind: "status", status: event.status });
      } else if (event.type === "playerPosition") {
        handler({ kind: "position", positionMs: event.positionMs });
      } else if (event.type === "playerSettingsChanged") {
        handler({ kind: "settings", settings: event.settings });
      } else if (event.type === "eqPresetsChanged") {
        handler({ kind: "eqPresets" });
      } else if (event.type === "waveformReady") {
        handler({ kind: "waveform", trackId: event.trackId });
      } else if (event.type === "artReady") {
        handler({ kind: "art", trackIds: event.trackIds });
      } else if (event.type === "queueChanged") {
        handler({ kind: "queue", replaced: event.replaced });
      } else if (event.type === "playerError") {
        handler({ kind: "error", message: event.message });
      }
    });
    await visuals.reset();
    return stop;
  },
  watchVisuals(handler) {
    return visuals.watch(handler);
  },
};
