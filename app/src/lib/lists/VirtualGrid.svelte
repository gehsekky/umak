<script lang="ts">
  // A grid of equal tiles that draws only the rows in view, so the number of tiles does not
  // affect drawing. Tiles are a square picture with a caption; the theme sets their sizes.
  import { onMount, tick, type Snippet } from "svelte";
  import { gridLayout, visibleRows } from "./grid";

  const OVERSCAN_ROWS = 2;
  const FALLBACK_TILE = 160;
  const FALLBACK_CAPTION = 44;
  const FALLBACK_GAP = 16;

  type Props = {
    total: number;
    tile: Snippet<[number]>;
    /** The range of indexes drawn, with an exclusive end. */
    start?: number;
    end?: number;
    onkeydown?: (event: KeyboardEvent) => void;
    label: string;
  };

  let { total, tile, start = $bindable(0), end = $bindable(0), onkeydown, label }: Props =
    $props();

  let container: HTMLDivElement;
  let scrollTop = $state(0);
  let viewWidth = $state(0);
  let viewHeight = $state(0);
  let minTile = $state(FALLBACK_TILE);
  let caption = $state(FALLBACK_CAPTION);
  let gap = $state(FALLBACK_GAP);
  /** A tile to reveal once the grid knows its width, which decides where the tile is. */
  let revealing: number | null = null;

  const layout = $derived(gridLayout(viewWidth - 2 * gap, minTile, gap, caption, total));
  const rows = $derived(visibleRows(layout, scrollTop, viewHeight, OVERSCAN_ROWS));
  const indexes = $derived.by(() => {
    const first = rows[0] * layout.columns;
    const last = Math.min(total, rows[1] * layout.columns);
    return Array.from({ length: Math.max(0, last - first) }, (_, i) => first + i);
  });

  $effect(() => {
    start = rows[0] * layout.columns;
    end = Math.min(total, rows[1] * layout.columns);
  });

  function token(name: string, fallback: number): number {
    return Number.parseFloat(getComputedStyle(container).getPropertyValue(name)) || fallback;
  }

  onMount(() => {
    minTile = token("--size-album-tile", FALLBACK_TILE);
    caption = token("--size-album-caption", FALLBACK_CAPTION);
    gap = token("--space-md", FALLBACK_GAP);
    const observer = new ResizeObserver(() => {
      viewWidth = container.clientWidth;
      viewHeight = container.clientHeight;
      const index = revealing;
      revealing = null;
      // After the content takes its height, or the scroll position would be cut short.
      if (index !== null) void tick().then(() => reveal(index));
    });
    observer.observe(container);
    return () => observer.disconnect();
  });

  export function scrollToTop(): void {
    container.scrollTop = 0;
    scrollTop = 0;
  }

  /** Scrolls just enough to bring a tile fully into view. */
  export function reveal(index: number): void {
    if (viewWidth === 0) {
      revealing = index;
      return;
    }
    const top = gap + Math.floor(index / layout.columns) * layout.rowHeight;
    const bottom = top + layout.rowHeight - gap;
    if (top - gap < container.scrollTop) {
      container.scrollTop = top - gap;
    } else if (bottom + gap > container.scrollTop + container.clientHeight) {
      container.scrollTop = bottom + gap - container.clientHeight;
    }
  }

  /** How many tiles across, for moving up and down. */
  export function columns(): number {
    return layout.columns;
  }

  /** How many rows fit on screen, for page-up and page-down. */
  export function pageRows(): number {
    return Math.max(1, Math.floor(viewHeight / layout.rowHeight));
  }

  export function focus(): void {
    container.focus();
  }
</script>

<div
  class="scroller"
  bind:this={container}
  role="grid"
  aria-label={label}
  aria-rowcount={layout.rows}
  aria-colcount={layout.columns}
  tabindex="0"
  {onkeydown}
  onscroll={() => (scrollTop = container.scrollTop)}
>
  <div class="content" style:height="{layout.rows * layout.rowHeight + gap}px">
    {#each indexes as index (index)}
      <div
        class="slot"
        style:width="{layout.tileWidth}px"
        style:height="{layout.rowHeight - gap}px"
        style:transform="translate({gap + (index % layout.columns) * (layout.tileWidth + gap)}px,
        {gap + Math.floor(index / layout.columns) * layout.rowHeight}px)"
      >
        {@render tile(index)}
      </div>
    {/each}
  </div>
</div>

<style>
  .scroller {
    flex: 1;
    min-height: 0;
    overflow-y: scroll;
    scrollbar-color: var(--color-border) transparent;
    position: relative;
    outline: none;
  }

  .content {
    position: relative;
    contain: strict;
    width: 100%;
  }

  .slot {
    position: absolute;
    left: 0;
    top: 0;
  }
</style>
