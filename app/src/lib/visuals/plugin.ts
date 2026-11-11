// Visualizers are plugins: each receives the player's visual frames and draws to a canvas.
// The host owns the canvas, its size and the drawing loop, so a plugin is only the drawing.

import type { VisualFrame } from "../api/generated/VisualFrame";

/** Theme colors and fonts, read from the tokens, since a canvas cannot use CSS variables. */
export type VisualColors = {
  background: string;
  fill: string;
  peak: string;
  line: string;
  grid: string;
  text: string;
  /** A complete canvas font, such as "12px system-ui". */
  font: string;
};

/** What a plugin draws into, in CSS pixels. The context is already scaled for the screen. */
export type VisualView = {
  context: CanvasRenderingContext2D;
  width: number;
  height: number;
  colors: VisualColors;
  /** A time in milliseconds, for movement between frames. */
  now: number;
};

export interface Visualizer {
  /** A frame arrived. Frames come about 60 times a second while something plays. */
  receive(frame: VisualFrame): void;
  /** Draws the whole picture. Called once per screen refresh while shown. */
  draw(view: VisualView): void;
}

export type VisualizerPlugin = {
  id: string;
  label: string;
  /** Makes a visualizer with state of its own. Each place showing a plugin has one. */
  create(): Visualizer;
};

const plugins = new Map<string, VisualizerPlugin>();

export function registerVisualizer(plugin: VisualizerPlugin): void {
  plugins.set(plugin.id, plugin);
}

/** In the order registered. */
export function visualizerPlugins(): VisualizerPlugin[] {
  return [...plugins.values()];
}

/** The plugin with this ID, or the first registered if there is none. */
export function findVisualizer(id: string | null): VisualizerPlugin | undefined {
  return (id !== null ? plugins.get(id) : undefined) ?? plugins.values().next().value;
}

/** The plugin after this one, going round. */
export function nextVisualizer(id: string): VisualizerPlugin | undefined {
  const all = visualizerPlugins();
  const index = all.findIndex((plugin) => plugin.id === id);
  return all[(index + 1) % all.length];
}
