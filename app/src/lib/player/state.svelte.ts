// What the views show about playback. It follows the player client's updates.

import type { PlayerStatus } from "../api/generated/PlayerStatus";
import { app } from "../state/app.svelte";
import { playerClient } from "./client";

class PlayerState {
  status = $state.raw<PlayerStatus | null>(null);
  positionMs = $state(0);
  /** Increases when the player playlist changes, so views showing it reload. */
  queueVersion = $state(0);
  /** Increases when it becomes a different list altogether. */
  queueReplacements = $state(0);

  readonly playingTrackId = $derived(
    this.status && this.status.state !== "stopped" ? (this.status.track?.id ?? null) : null,
  );

  async start(): Promise<() => void> {
    const stop = await playerClient.subscribe((update) => {
      if (update.kind === "status") {
        this.status = update.status;
        this.positionMs = update.status.positionMs;
      } else if (update.kind === "position") {
        this.positionMs = update.positionMs;
      } else if (update.kind === "queue") {
        this.queueVersion++;
        if (update.replaced) this.queueReplacements++;
      } else {
        app.fail(update.message);
      }
    });
    this.status = await playerClient.status();
    this.positionMs = this.status.positionMs;
    return stop;
  }
}

export const player = new PlayerState();
