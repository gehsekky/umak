import { describe, expect, it } from "vitest";
import { visualsWatch } from "./visuals";

function fixture() {
  const enabled: boolean[] = [];
  const listeners = new Set<(frame: number) => void>();
  const watch = visualsWatch<number>(
    async (on) => {
      enabled.push(on);
    },
    async (handler) => {
      const listener = (frame: number) => handler(frame);
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  );
  return {
    watch,
    enabled,
    emit: (frame: number) => {
      for (const listener of listeners) listener(frame);
    },
    listening: () => listeners.size,
  };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("watching visuals", () => {
  it("turns frames on for the first watcher and off after the last", async () => {
    const { watch, enabled, emit, listening } = fixture();
    const first: number[] = [];
    const second: number[] = [];
    const stopFirst = await watch.watch((frame) => first.push(frame));
    const stopSecond = await watch.watch((frame) => second.push(frame));
    expect(enabled).toEqual([true]);
    expect(listening()).toBe(1);

    emit(1);
    stopFirst();
    stopFirst();
    emit(2);
    expect(first).toEqual([1]);
    expect(second).toEqual([1, 2]);
    expect(enabled).toEqual([true]);

    stopSecond();
    await settle();
    expect(enabled).toEqual([true, false]);
    expect(listening()).toBe(0);
  });

  it("starts again after everything stopped", async () => {
    const { watch, enabled, emit } = fixture();
    (await watch.watch(() => {}))();
    const frames: number[] = [];
    await watch.watch((frame) => frames.push(frame));
    emit(3);
    expect(frames).toEqual([3]);
    expect(enabled).toEqual([true, false, true]);
  });

  it("resets frames left on only when nothing here watches", async () => {
    const { watch, enabled } = fixture();
    await watch.reset();
    expect(enabled).toEqual([false]);
    await watch.watch(() => {});
    await watch.reset();
    expect(enabled).toEqual([false, true]);
  });
});
