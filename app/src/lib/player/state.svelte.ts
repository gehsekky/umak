// What the views show about playback. It follows the player client's updates.

import type { EqPresets } from "../api/generated/EqPresets";
import type { PlayerSettings } from "../api/generated/PlayerSettings";
import type { PlayerStatus } from "../api/generated/PlayerStatus";
import { app } from "../state/app.svelte";
import { playerClient } from "./client";
import { gainToSlider, latestOnly, sliderToGain } from "./volume";

class PlayerState {
  status = $state.raw<PlayerStatus | null>(null);
  settings = $state.raw<PlayerSettings | null>(null);
  eqPresets = $state.raw<EqPresets | null>(null);
  positionMs = $state(0);
  /** Increases when the player playlist changes, so views showing it reload. */
  queueVersion = $state(0);
  /** Increases when it becomes a different list altogether. */
  queueReplacements = $state(0);

  readonly playingTrackId = $derived(
    this.status && this.status.state !== "stopped" ? (this.status.track?.id ?? null) : null,
  );

  private sendVolume = latestOnly((volume: number) =>
    playerClient.setVolume(volume).catch((failure: { message?: string }) => {
      app.fail(failure.message ?? String(failure));
    }),
  );

  /** Sets the volume without waiting. A burst of changes sends only the newest. */
  setVolumeSoon(volume: number): void {
    // Shown at once, so the next change in the burst starts from this one.
    if (this.settings) this.settings = { ...this.settings, volume, muted: false };
    this.sendVolume(volume);
  }

  /** Moves the volume by a share of the slider. */
  nudgeVolume(sliderStep: number): void {
    const settings = this.settings;
    if (!settings) return;
    const from = settings.muted ? 0 : gainToSlider(settings.volume);
    this.setVolumeSoon(sliderToGain(from + sliderStep));
  }

  /** Moves within the current track, staying inside it. */
  async skip(deltaMs: number): Promise<void> {
    const status = this.status;
    if (!status || status.state === "stopped") return;
    const end = status.durationMs ?? Number.MAX_SAFE_INTEGER;
    const ms = Math.max(0, Math.min(end, this.positionMs + deltaMs));
    this.positionMs = ms;
    await playerClient.seek(ms);
  }

  private async loadEqPresets(): Promise<void> {
    this.eqPresets = await playerClient.eqPresets();
  }

  async start(): Promise<() => void> {
    const stop = await playerClient.subscribe((update) => {
      if (update.kind === "status") {
        this.status = update.status;
        this.positionMs = update.status.positionMs;
      } else if (update.kind === "position") {
        this.positionMs = update.positionMs;
      } else if (update.kind === "settings") {
        this.settings = update.settings;
      } else if (update.kind === "eqPresets") {
        void this.loadEqPresets();
      } else if (update.kind === "queue") {
        this.queueVersion++;
        if (update.replaced) this.queueReplacements++;
      } else {
        app.fail(update.message);
      }
    });
    this.settings = await playerClient.settings();
    await this.loadEqPresets();
    this.status = await playerClient.status();
    this.positionMs = this.status.positionMs;
    return stop;
  }
}

export const player = new PlayerState();
