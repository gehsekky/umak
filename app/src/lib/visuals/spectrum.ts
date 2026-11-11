// A spectrum analyzer: bars that jump up with the music and fall back slowly, with peak marks
// that hold a moment before they fall.

import type { VisualFrame } from "../api/generated/VisualFrame";
import type { Visualizer, VisualizerPlugin, VisualView } from "./plugin";

// The core's VISUAL_LOW_HZ, VISUAL_HIGH_HZ and VISUAL_BANDS, which a test compares.
export const LOW_HZ = 20;
export const HIGH_HZ = 20_000;
export const BANDS = 96;

/** Levels at or below this draw nothing. */
export const MIN_DB = -72;
/** Bars at least this wide, gaps included, so a narrow canvas gets fewer of them. */
const BAR_PITCH = 6;
const MIN_BARS = 16;
/** How fast bars fall, in heights per second. */
const FALL_RATE = 1.6;
const PEAK_HOLD_MS = 600;
const PEAK_FALL_RATE = 0.5;
const LABEL_HZ = [100, 1000, 10_000];
/** Only tall enough canvases show the frequency labels. */
const LABEL_MIN_HEIGHT = 120;

/** A level in dB as a height from 0 to 1. */
export function heightOf(db: number): number {
  return Math.min(1, Math.max(0, (db - MIN_DB) / -MIN_DB));
}

/** Combines the bands into `count` bars, each taking the loudest of its bands. */
export function groupBands(bands: readonly number[], count: number): number[] {
  if (count >= bands.length) return [...bands];
  const bars: number[] = [];
  for (let bar = 0; bar < count; bar++) {
    const first = Math.floor((bar * bands.length) / count);
    const end = Math.max(first + 1, Math.floor(((bar + 1) * bands.length) / count));
    let loudest = -Infinity;
    for (let band = first; band < end; band++) loudest = Math.max(loudest, bands[band]);
    bars.push(loudest);
  }
  return bars;
}

/** How many bars fit across this width. */
export function barCount(width: number): number {
  return Math.min(BANDS, Math.max(MIN_BARS, Math.floor(width / BAR_PITCH)));
}

/** Where a frequency falls across the width, from 0 to 1. */
export function positionOf(hz: number): number {
  return Math.log(hz / LOW_HZ) / Math.log(HIGH_HZ / LOW_HZ);
}

/** Heights that rise at once and fall at a steady rate, with peaks that hold, then fall. */
export class Falloff {
  heights: number[] = [];
  peaks: number[] = [];
  private peakTimes: number[] = [];
  private last: number | null = null;

  /** Moves toward `targets`, heights from 0 to 1, at time `now` in milliseconds. */
  update(targets: readonly number[], now: number): void {
    if (targets.length !== this.heights.length) {
      this.heights = targets.map(() => 0);
      this.peaks = targets.map(() => 0);
      this.peakTimes = targets.map(() => now);
    }
    const seconds = this.last === null ? 0 : Math.max(0, now - this.last) / 1000;
    this.last = now;
    targets.forEach((target, index) => {
      this.heights[index] = Math.max(target, this.heights[index] - FALL_RATE * seconds);
      if (this.heights[index] >= this.peaks[index]) {
        this.peaks[index] = this.heights[index];
        this.peakTimes[index] = now;
      } else if (now - this.peakTimes[index] > PEAK_HOLD_MS) {
        this.peaks[index] = Math.max(
          this.heights[index],
          this.peaks[index] - PEAK_FALL_RATE * seconds,
        );
      }
    });
  }
}

function spectrum(): Visualizer {
  let bands: readonly number[] = [];
  const falloff = new Falloff();

  return {
    receive(frame: VisualFrame) {
      bands = frame.bands;
    },
    draw({ context, width, height, colors, now }: VisualView) {
      context.fillStyle = colors.background;
      context.fillRect(0, 0, width, height);

      const labels = height >= LABEL_MIN_HEIGHT;
      const labelHeight = labels ? 16 : 0;
      const top = 4;
      const area = height - top - labelHeight;
      const count = barCount(width);
      const targets = bands.length
        ? groupBands(bands, count).map(heightOf)
        : new Array(count).fill(0);
      falloff.update(targets, now);

      const pitch = width / count;
      const gap = pitch > 4 ? 2 : 1;
      context.fillStyle = colors.fill;
      falloff.heights.forEach((level, index) => {
        const bar = level * area;
        if (bar > 0) context.fillRect(index * pitch, top + area - bar, pitch - gap, bar);
      });
      context.fillStyle = colors.peak;
      falloff.peaks.forEach((level, index) => {
        if (level > 0)
          context.fillRect(index * pitch, top + area - level * area - 2, pitch - gap, 2);
      });

      if (labels) {
        context.fillStyle = colors.text;
        context.font = colors.font;
        context.textBaseline = "bottom";
        context.textAlign = "center";
        for (const hz of LABEL_HZ) {
          const label = hz >= 1000 ? `${hz / 1000}k` : String(hz);
          context.fillText(label, positionOf(hz) * width, height - 2);
        }
      }
    },
  };
}

export const spectrumPlugin: VisualizerPlugin = {
  id: "spectrum",
  label: "Spectrum",
  create: spectrum,
};
