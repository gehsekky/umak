<script lang="ts" generics="T">
  // Renders only the rows in view, so the length of the list does not affect drawing.
  import { onMount, type Snippet } from "svelte";

  const OVERSCAN = 10;
  const FALLBACK_ROW_HEIGHT = 30;

  type Props = {
    total: number;
    row: Snippet<[number]>;
    /** The range of indexes on screen, with an exclusive end. */
    start?: number;
    end?: number;
    onkeydown?: (event: KeyboardEvent) => void;
    label: string;
    /** Where a dragged row would land, as a place between rows. */
    dropGap?: number | null;
  };

  let {
    total,
    row,
    start = $bindable(0),
    end = $bindable(0),
    onkeydown,
    label,
    dropGap = null,
  }: Props = $props();

  let container: HTMLDivElement;
  let scrollTop = $state(0);
  let viewHeight = $state(0);
  let rowHeight = $state(FALLBACK_ROW_HEIGHT);

  const firstVisible = $derived(Math.floor(scrollTop / rowHeight));
  const visibleCount = $derived(Math.ceil(viewHeight / rowHeight));
  const from = $derived(Math.max(0, firstVisible - OVERSCAN));
  const to = $derived(Math.min(total, firstVisible + visibleCount + OVERSCAN));
  const indexes = $derived(Array.from({ length: Math.max(0, to - from) }, (_, i) => from + i));

  $effect(() => {
    start = from;
    end = to;
  });

  onMount(() => {
    // The row height is a theme token, so the theme decides it.
    const token = getComputedStyle(container).getPropertyValue("--size-row");
    rowHeight = Number.parseFloat(token) || FALLBACK_ROW_HEIGHT;
    const observer = new ResizeObserver(() => {
      viewHeight = container.clientHeight;
    });
    observer.observe(container);
    return () => observer.disconnect();
  });

  export function scrollToTop(): void {
    container.scrollTop = 0;
    scrollTop = 0;
  }

  /** Scrolls just enough to bring a row fully into view. */
  export function reveal(index: number): void {
    const top = index * rowHeight;
    if (top < container.scrollTop) {
      container.scrollTop = top;
    } else if (top + rowHeight > container.scrollTop + container.clientHeight) {
      container.scrollTop = top + rowHeight - container.clientHeight;
    }
  }

  /** The place between rows nearest a point on screen. */
  export function gapAt(clientY: number): number {
    const offset = clientY - container.getBoundingClientRect().top + container.scrollTop;
    return Math.max(0, Math.min(total, Math.round(offset / rowHeight)));
  }

  /** Scrolls a little if a point is near the top or bottom edge. */
  export function scrollNear(clientY: number): void {
    const bounds = container.getBoundingClientRect();
    if (clientY < bounds.top + rowHeight) {
      container.scrollTop -= rowHeight;
    } else if (clientY > bounds.bottom - rowHeight) {
      container.scrollTop += rowHeight;
    }
  }

  /** How many rows fit on screen, for page-up and page-down. */
  export function pageRows(): number {
    return Math.max(1, visibleCount - 1);
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
  aria-rowcount={total}
  tabindex="0"
  {onkeydown}
  onscroll={() => (scrollTop = container.scrollTop)}
>
  <div class="content" style:height="{total * rowHeight}px">
    {#each indexes as index (index)}
      <div class="slot" style:transform="translateY({index * rowHeight}px)" style:height="{rowHeight}px">
        {@render row(index)}
      </div>
    {/each}
    {#if dropGap !== null}
      <div class="drop" style:transform="translateY({dropGap * rowHeight}px)"></div>
    {/if}
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

  .drop {
    position: absolute;
    left: 0;
    right: 0;
    top: -1px;
    height: 2px;
    background: var(--color-accent);
    pointer-events: none;
  }

  .slot {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
  }
</style>
