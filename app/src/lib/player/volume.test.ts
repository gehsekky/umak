import { describe, expect, it } from "vitest";
import { gainToSlider, latestOnly, sliderToGain } from "./volume";

describe("the volume curve", () => {
  it("keeps the ends and lowers the middle", () => {
    expect(sliderToGain(0)).toBe(0);
    expect(sliderToGain(1)).toBe(1);
    expect(sliderToGain(0.5)).toBeCloseTo(0.125);
  });

  it("converts back to the same position", () => {
    for (const position of [0, 0.1, 0.33, 0.5, 0.9, 1]) {
      expect(gainToSlider(sliderToGain(position))).toBeCloseTo(position);
    }
  });

  it("stays in range", () => {
    expect(sliderToGain(2)).toBe(1);
    expect(sliderToGain(-1)).toBe(0);
    expect(gainToSlider(2)).toBe(1);
  });
});

describe("latestOnly", () => {
  function controlled() {
    const sent: number[] = [];
    const finishers: (() => void)[] = [];
    const call = (value: number) => {
      sent.push(value);
      return new Promise<void>((resolve) => finishers.push(resolve));
    };
    const finish = async () => {
      finishers.shift()?.();
      await new Promise((resolve) => setTimeout(resolve));
    };
    return { sent, call, finish };
  }

  it("runs one at a time and skips to the newest value", async () => {
    const { sent, call, finish } = controlled();
    const send = latestOnly(call);
    send(1);
    send(2);
    send(3);
    send(4);
    expect(sent).toEqual([1]);
    await finish();
    expect(sent).toEqual([1, 4]);
    await finish();
    expect(sent).toEqual([1, 4]);
  });

  it("runs a later value right away once idle", async () => {
    const { sent, call, finish } = controlled();
    const send = latestOnly(call);
    send(1);
    await finish();
    send(2);
    expect(sent).toEqual([1, 2]);
  });

  it("keeps going after a failure", async () => {
    const sent: number[] = [];
    const send = latestOnly(async (value: number) => {
      sent.push(value);
      throw new Error("no");
    });
    send(1);
    send(2);
    await new Promise((resolve) => setTimeout(resolve));
    expect(sent).toEqual([1, 2]);
  });
});
