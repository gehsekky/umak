<script lang="ts">
  // Hosts one visualizer plugin: sizes the canvas to the screen, watches the player's frames
  // while the window is visible, and draws once per screen refresh while frames arrive.
  import { onMount } from "svelte";
  import type { VisualFrame } from "../api/generated/VisualFrame";
  import { playerClient } from "../player/client";
  import type { Unwatch } from "../player/visuals";
  import type { VisualColors, Visualizer, VisualizerPlugin } from "./plugin";

  let { plugin, ondblclick }: { plugin: VisualizerPlugin; ondblclick?: () => void } = $props();

  /** With no frames for this long, the picture has settled, so drawing stops until one comes. */
  const IDLE_MS = 2000;

  let host = $state<HTMLDivElement>();
  let canvas = $state<HTMLCanvasElement>();
  let visualizer: Visualizer | null = null;
  let size = { width: 0, height: 0 };
  let colors: VisualColors | null = null;
  let lastFrame = 0;
  let request: number | null = null;

  $effect(() => {
    // A new plugin starts with no state, and draws at once so the old picture goes.
    visualizer = plugin.create();
    wake();
  });

  function readColors(): VisualColors {
    const style = getComputedStyle(document.documentElement);
    const token = (name: string) => style.getPropertyValue(`--${name}`).trim();
    return {
      background: token("color-visual-bg"),
      fill: token("color-visual-fill"),
      peak: token("color-visual-peak"),
      line: token("color-visual-line"),
      grid: token("color-border"),
      text: token("color-text-muted"),
      font: `${token("font-size-small")} ${token("font-body")}`,
    };
  }

  function draw(now: number): void {
    request = null;
    const context = canvas?.getContext("2d");
    if (!context || !visualizer || size.width === 0) return;
    colors ??= readColors();
    const scale = window.devicePixelRatio || 1;
    context.setTransform(scale, 0, 0, scale, 0, 0);
    visualizer.draw({ context, width: size.width, height: size.height, colors, now });
    if (now - lastFrame < IDLE_MS) request = requestAnimationFrame(draw);
  }

  function wake(): void {
    request ??= requestAnimationFrame(draw);
  }

  function receive(frame: VisualFrame): void {
    visualizer?.receive(frame);
    lastFrame = performance.now();
    wake();
  }

  function resize(): void {
    if (!host || !canvas) return;
    const scale = window.devicePixelRatio || 1;
    size = { width: host.clientWidth, height: host.clientHeight };
    canvas.width = Math.round(size.width * scale);
    canvas.height = Math.round(size.height * scale);
    wake();
  }

  onMount(() => {
    let unwatch: Promise<Unwatch> | null = null;
    const follow = () => {
      // Frames cost work in the core, so a hidden window does not take them.
      if (document.visibilityState === "visible") {
        unwatch ??= playerClient.watchVisuals(receive);
      } else if (unwatch) {
        void unwatch.then((stop) => stop());
        unwatch = null;
      }
    };
    follow();
    document.addEventListener("visibilitychange", follow);
    const observer = new ResizeObserver(resize);
    if (host) observer.observe(host);
    resize();
    return () => {
      document.removeEventListener("visibilitychange", follow);
      observer.disconnect();
      if (request !== null) cancelAnimationFrame(request);
      void unwatch?.then((stop) => stop());
    };
  });
</script>

<div class="host" bind:this={host}>
  <canvas bind:this={canvas} {ondblclick}></canvas>
</div>

<style>
  .host {
    position: relative;
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: var(--color-visual-bg);
  }

  canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
</style>
