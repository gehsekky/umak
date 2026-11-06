// Turning the volume down by ear follows a curve, not a straight line: at half the
// slider, a straight line would still sound nearly full. The core takes output gain.

const CURVE = 3;

/** The output gain for a slider position, both from 0 to 1. */
export function sliderToGain(position: number): number {
  return Math.max(0, Math.min(1, position)) ** CURVE;
}

/** The slider position for an output gain, both from 0 to 1. */
export function gainToSlider(gain: number): number {
  return Math.max(0, Math.min(1, gain)) ** (1 / CURVE);
}

/**
 * Wraps a call so at most one runs at a time, and only the newest waiting value runs
 * next. A dragged slider then keeps up however slow the call is, without a backlog.
 */
export function latestOnly<T>(call: (value: T) => Promise<unknown>): (value: T) => void {
  let running = false;
  let waiting: { value: T } | null = null;

  async function run(value: T): Promise<void> {
    running = true;
    try {
      await call(value);
    } catch {
      // The caller reports failures itself; a later value should still go through.
    }
    running = false;
    if (waiting) {
      const next = waiting.value;
      waiting = null;
      void run(next);
    }
  }

  return (value) => {
    if (running) {
      waiting = { value };
    } else {
      void run(value);
    }
  };
}
