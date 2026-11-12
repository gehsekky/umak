<script lang="ts">
  // The seek bar: the track's waveform, played part in the accent color, with a range input
  // over it. The input is invisible but takes the pointer and the keyboard, so seeking works
  // as a slider does, with or without a waveform yet.
  import { onMount } from "svelte";
  import { appearance } from "../theme/appearance.svelte";
  import { playedShare, resample } from "./waveform";

  let {
    peaks,
    levels,
    positionMs,
    durationMs,
    disabled,
    title,
    oninput,
    onchange,
  }: {
    peaks: readonly number[];
    levels: readonly number[];
    positionMs: number;
    durationMs: number;
    disabled: boolean;
    title: string;
    oninput: (ms: number) => void;
    onchange: (ms: number) => void;
  } = $props();

  let host = $state<HTMLDivElement>();
  let canvas = $state<HTMLCanvasElement>();
  let size = $state({ width: 0, height: 0 });

  function token(style: CSSStyleDeclaration, name: string): string {
    return style.getPropertyValue(`--${name}`).trim();
  }

  $effect(() => {
    const context = canvas?.getContext("2d");
    const { width, height } = size;
    // The colors are theme tokens, so a new theme draws again.
    void appearance.version;
    if (!context || !canvas || width === 0) return;
    const scale = window.devicePixelRatio || 1;
    canvas.width = Math.round(width * scale);
    canvas.height = Math.round(height * scale);
    context.setTransform(scale, 0, 0, scale, 0, 0);
    context.clearRect(0, 0, width, height);

    const style = getComputedStyle(document.documentElement);
    const played = token(style, "color-wave-played");
    const unplayed = token(style, "color-wave");
    const split = Math.round(playedShare(positionMs, durationMs) * width);
    const middle = height / 2;

    if (peaks.length === 0) {
      // No waveform, or not yet: a plain track, as a slider has.
      context.fillStyle = unplayed;
      context.fillRect(0, middle - 1, width, 2);
      context.fillStyle = played;
      context.fillRect(0, middle - 1, split, 2);
      return;
    }

    const columns = Math.floor(width);
    const peakColumns = resample(peaks, columns);
    const levelColumns = resample(levels, columns);
    for (let x = 0; x < columns; x++) {
      context.fillStyle = x < split ? played : unplayed;
      // Peaks faint, loudness solid, both mirrored about the middle, never thinner than a line.
      const peak = Math.max(1, (peakColumns[x] / 255) * middle);
      const level = Math.max(1, (levelColumns[x] / 255) * middle);
      context.globalAlpha = 0.45;
      context.fillRect(x, middle - peak, 1, peak * 2);
      context.globalAlpha = 1;
      context.fillRect(x, middle - level, 1, level * 2);
    }
  });

  onMount(() => {
    const observer = new ResizeObserver(() => {
      if (host) size = { width: host.clientWidth, height: host.clientHeight };
    });
    if (host) observer.observe(host);
    return () => observer.disconnect();
  });
</script>

<div class="seek" class:disabled bind:this={host}>
  <canvas bind:this={canvas}></canvas>
  <input
    type="range"
    min="0"
    max={durationMs}
    step="100"
    value={positionMs}
    {disabled}
    {title}
    aria-label="Position"
    oninput={(event) => oninput(event.currentTarget.valueAsNumber)}
    onchange={(event) => onchange(event.currentTarget.valueAsNumber)}
  />
</div>

<style>
  .seek {
    position: relative;
    flex: 1;
    min-width: 0;
    height: var(--size-waveform);
    border-radius: var(--radius);
  }

  .seek:focus-within {
    outline: 1px solid var(--color-accent);
  }

  .disabled {
    opacity: 0.5;
  }

  canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }

  input {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    opacity: 0;
    cursor: pointer;
  }

  input:disabled {
    cursor: default;
  }
</style>
