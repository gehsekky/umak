// An oscilloscope: the latest samples as a line. Each picture starts where the wave rises
// through zero, so a steady tone stands still instead of sliding.

import type { VisualFrame } from "../api/generated/VisualFrame";
import type { Visualizer, VisualizerPlugin, VisualView } from "./plugin";

/** The samples shown across the width, about 11 ms at 48 kHz. */
export const SPAN = 512;
/** Of the full height, so full-scale peaks do not touch the edges. */
const SCALE = 0.45;

/** Where to start showing `span` samples: the first rise through zero that leaves room. */
export function triggerIndex(wave: readonly number[], span: number): number {
  const last = wave.length - span;
  if (last <= 0) return 0;
  for (let index = 1; index <= last; index++) {
    if (wave[index - 1] < 0 && wave[index] >= 0) return index;
  }
  return last;
}

function oscilloscope(): Visualizer {
  let wave: readonly number[] = [];

  return {
    receive(frame: VisualFrame) {
      wave = frame.wave;
    },
    draw({ context, width, height, colors }: VisualView) {
      context.fillStyle = colors.background;
      context.fillRect(0, 0, width, height);
      const middle = height / 2;
      context.fillStyle = colors.grid;
      context.fillRect(0, Math.round(middle), width, 1);
      if (wave.length === 0) return;

      const span = Math.min(SPAN, wave.length);
      const start = triggerIndex(wave, span);
      context.strokeStyle = colors.line;
      context.lineWidth = 1.5;
      context.lineJoin = "round";
      context.beginPath();
      for (let index = 0; index < span; index++) {
        const x = (index / (span - 1)) * width;
        const y = middle - wave[start + index] * height * SCALE;
        if (index === 0) context.moveTo(x, y);
        else context.lineTo(x, y);
      }
      context.stroke();
    },
  };
}

export const oscilloscopePlugin: VisualizerPlugin = {
  id: "oscilloscope",
  label: "Oscilloscope",
  create: oscilloscope,
};
