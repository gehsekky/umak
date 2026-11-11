import { describe, expect, it } from "vitest";
import protocol from "../../../../crates/umak-audio/src/protocol.rs?raw";
import { parseChoices } from "./choices";
import { SPAN, triggerIndex } from "./oscilloscope";
import {
  findVisualizer,
  nextVisualizer,
  registerVisualizer,
  visualizerPlugins,
  type VisualizerPlugin,
} from "./plugin";
import {
  BANDS,
  barCount,
  Falloff,
  groupBands,
  HIGH_HZ,
  heightOf,
  LOW_HZ,
  MIN_DB,
  positionOf,
} from "./spectrum";

describe("the spectrum", () => {
  it("lays out bands as the core makes them", () => {
    const constant = (name: string) => {
      const found = protocol.match(new RegExp(`pub const ${name}: \\w+ = ([\\d_.]+);`));
      expect(found, name).not.toBeNull();
      return Number(found![1].replaceAll("_", ""));
    };
    expect(constant("VISUAL_BANDS")).toBe(BANDS);
    expect(constant("VISUAL_LOW_HZ")).toBe(LOW_HZ);
    expect(constant("VISUAL_HIGH_HZ")).toBe(HIGH_HZ);
    expect(positionOf(LOW_HZ)).toBe(0);
    expect(positionOf(HIGH_HZ)).toBeCloseTo(1);
  });

  it("maps levels to heights between the floor and full scale", () => {
    expect(heightOf(0)).toBe(1);
    expect(heightOf(6)).toBe(1);
    expect(heightOf(MIN_DB)).toBe(0);
    expect(heightOf(-100)).toBe(0);
    expect(heightOf(MIN_DB / 2)).toBeCloseTo(0.5);
  });

  it("groups bands into bars by the loudest of each group", () => {
    const bands = [-10, -20, -30, -5, -50, -60];
    expect(groupBands(bands, 3)).toEqual([-10, -5, -50]);
    expect(groupBands(bands, 6)).toEqual(bands);
    expect(groupBands(bands, 10)).toEqual(bands);
    // Uneven groups still cover every band.
    expect(groupBands(bands, 4)).toEqual([-10, -20, -5, -50]);
  });

  it("fits fewer bars on a narrow canvas, within limits", () => {
    expect(barCount(10)).toBe(16);
    expect(barCount(300)).toBe(50);
    expect(barCount(4000)).toBe(96);
  });

  it("rises at once, falls steadily, and holds peaks before they fall", () => {
    const falloff = new Falloff();
    falloff.update([1, 0.5], 0);
    expect(falloff.heights).toEqual([1, 0.5]);
    falloff.update([0, 0], 250);
    expect(falloff.heights[0]).toBeCloseTo(0.6);
    expect(falloff.heights[1]).toBeCloseTo(0.1);
    expect(falloff.peaks).toEqual([1, 0.5]);
    falloff.update([0, 0], 1000);
    expect(falloff.heights).toEqual([0, 0]);
    expect(falloff.peaks[0]).toBeCloseTo(1 - 0.5 * 0.75);
    falloff.update([0.9, 0], 1100);
    expect(falloff.heights[0]).toBe(0.9);
    expect(falloff.peaks[0]).toBe(0.9);
  });

  it("starts again when the number of bars changes", () => {
    const falloff = new Falloff();
    falloff.update([1, 1], 0);
    falloff.update([0.2, 0.2, 0.2], 10);
    expect(falloff.heights).toEqual([0.2, 0.2, 0.2]);
  });
});

describe("the oscilloscope", () => {
  it("starts at the first rise through zero that leaves room", () => {
    const wave = [0.5, -0.2, -0.1, 0.0, 0.3, -0.4, 0.2, 0.1];
    expect(triggerIndex(wave, 4)).toBe(3);
    // The next rise is too late to show four samples after it.
    expect(triggerIndex(wave.slice(4), 2)).toBe(2);
  });

  it("shows the latest samples when nothing rises through zero", () => {
    const wave = new Array(SPAN + 100).fill(0.25);
    expect(triggerIndex(wave, SPAN)).toBe(100);
    expect(triggerIndex([0.1, 0.2], SPAN)).toBe(0);
  });
});

describe("visualizer plugins", () => {
  const plugin = (id: string): VisualizerPlugin => ({
    id,
    label: id,
    create: () => ({ receive() {}, draw() {} }),
  });

  it("are found by ID, falling back to the first, and cycle in order", () => {
    registerVisualizer(plugin("one"));
    registerVisualizer(plugin("two"));
    const ids = visualizerPlugins().map((each) => each.id);
    expect(ids.slice(-2)).toEqual(["one", "two"]);
    expect(findVisualizer("two")?.id).toBe("two");
    expect(findVisualizer("gone")?.id).toBe(ids[0]);
    expect(findVisualizer(null)?.id).toBe(ids[0]);
    expect(nextVisualizer("two")?.id).toBe(ids[0]);
    expect(nextVisualizer("one")?.id).toBe("two");
  });

  it("are remembered per place, forgetting damaged entries", () => {
    expect(parseChoices(null)).toEqual({ dock: null, full: null });
    expect(parseChoices('{"dock":"spectrum","full":7}')).toEqual({ dock: "spectrum", full: null });
    expect(parseChoices("not json")).toEqual({ dock: null, full: null });
    expect(parseChoices("null")).toEqual({ dock: null, full: null });
  });
});
