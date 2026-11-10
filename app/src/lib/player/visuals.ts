// Visual frames cost work in the core, so they flow only while something is watching them.
// Several visualizers may watch at once; frames start with the first and stop with the last.

export type Unwatch = () => void;

export interface VisualsWatch<Frame> {
  watch(handler: (frame: Frame) => void): Promise<Unwatch>;
  /** Stops frames left on by a page that has gone, unless something here watches them. */
  reset(): Promise<void>;
}

export function visualsWatch<Frame>(
  setEnabled: (enabled: boolean) => Promise<void>,
  listen: (handler: (frame: Frame) => void) => Promise<Unwatch>,
): VisualsWatch<Frame> {
  const handlers = new Set<(frame: Frame) => void>();
  let stopListening: Promise<Unwatch> | null = null;

  return {
    async watch(handler) {
      // A wrapper, so the same handler can watch twice and stop each separately.
      const entry = (frame: Frame) => handler(frame);
      handlers.add(entry);
      if (handlers.size === 1) {
        stopListening = listen((frame) => {
          for (const each of handlers) each(frame);
        });
        await stopListening;
        await setEnabled(true);
      }
      let stopped = false;
      return () => {
        if (stopped) return;
        stopped = true;
        handlers.delete(entry);
        if (handlers.size === 0 && stopListening) {
          const stop = stopListening;
          stopListening = null;
          void setEnabled(false);
          void stop.then((unlisten) => unlisten());
        }
      };
    },
    async reset() {
      if (handlers.size === 0) await setEnabled(false);
    },
  };
}
