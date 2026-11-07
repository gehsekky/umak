<script lang="ts" module>
  import type { TrackSort } from "../api/generated/TrackSort";

  export type SortState = { sort: TrackSort; descending: boolean };
</script>

<script lang="ts" generics="R">
  // The track list used by every view: virtualized rows, sortable headers, and
  // mouse and keyboard selection.
  import type { Snippet } from "svelte";
  import { runAction } from "../actions/registry";
  import type { TrackRow } from "../api/generated/TrackRow";
  import { formatDuration, formatQuality } from "./format";
  import { formatPlays, ratingForDigit } from "./rating";
  import StarRating from "./StarRating.svelte";
  import { player } from "../player/state.svelte";
  import type { PagedList } from "./pagedList.svelte";
  import type { Selection } from "./selection.svelte";
  import VirtualList from "./VirtualList.svelte";

  type Props = {
    list: PagedList<R>;
    selection: Selection;
    track: (row: R) => TrackRow;
    /** The text of the first column. */
    number: (row: R, index: number) => string;
    label: string;
    sort?: SortState;
    onsort?: (sort: TrackSort) => void;
    /** A row was double-clicked, or Enter was pressed on it. */
    onactivate?: (row: R, index: number) => void;
    /** Fewer columns and no header, for a narrow panel. */
    compact?: boolean;
    /** Which row is the one playing. By default, any row of the playing track. */
    isCurrent?: (row: R, index: number) => boolean;
    /** Sees keys first, and returns whether it used the key. */
    onkeys?: (event: KeyboardEvent) => boolean;
    /** Lets rows be dragged. The selected rows were dropped at this place between rows. */
    onreorder?: (gap: number) => void;
    empty: Snippet;
  };

  let {
    list,
    selection,
    track,
    number,
    label,
    sort,
    onsort,
    onactivate,
    compact = false,
    isCurrent,
    onkeys,
    onreorder,
    empty,
  }: Props = $props();

  /** How far the pointer moves before a press becomes a drag. */
  const DRAG_THRESHOLD = 4;
  const DRAG_SCROLL_MS = 50;

  let virtual = $state<VirtualList<R>>();
  let start = $state(0);
  let end = $state(0);

  $effect(() => {
    void list.generation;
    void list.ensure(start, Math.max(end, 1));
  });

  export function scrollToTop(): void {
    virtual?.scrollToTop();
  }

  export function focus(): void {
    virtual?.focus();
  }

  export function reveal(index: number): void {
    virtual?.reveal(index);
  }

  function current(row: R, index: number): boolean {
    return isCurrent ? isCurrent(row, index) : player.playingTrackId === track(row).id;
  }

  let dropGap = $state<number | null>(null);
  /** The click that ends a drag should not also select. */
  let justDragged = false;

  /**
   * Starts watching a press on a row. The listeners are on the window because rows come
   * and go as the list scrolls, and the pointer may leave the list.
   */
  function press(event: PointerEvent, index: number): void {
    if (!onreorder || event.button !== 0 || event.shiftKey || event.ctrlKey || event.metaKey) {
      return;
    }
    const startY = event.clientY;
    let lastY = startY;
    let scrolling: ReturnType<typeof setInterval> | undefined;

    const move = (moved: PointerEvent): void => {
      lastY = moved.clientY;
      if (dropGap === null) {
        if (Math.abs(lastY - startY) < DRAG_THRESHOLD) return;
        const row = list.row(index);
        if (row && !selection.has(list.key(row))) selection.only(index, list.key(row));
        scrolling = setInterval(() => {
          virtual?.scrollNear(lastY);
          dropGap = virtual?.gapAt(lastY) ?? null;
        }, DRAG_SCROLL_MS);
      }
      dropGap = virtual?.gapAt(lastY) ?? null;
    };
    const finish = (): void => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", release);
      window.removeEventListener("keydown", cancel, true);
      clearInterval(scrolling);
    };
    const release = (): void => {
      finish();
      if (dropGap !== null) {
        justDragged = true;
        setTimeout(() => (justDragged = false));
        onreorder?.(dropGap);
        dropGap = null;
      }
    };
    const cancel = (keyed: KeyboardEvent): void => {
      if (keyed.key !== "Escape") return;
      keyed.stopPropagation();
      finish();
      if (dropGap !== null) {
        justDragged = true;
        setTimeout(() => (justDragged = false));
      }
      dropGap = null;
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", release);
    window.addEventListener("keydown", cancel, true);
  }

  async function extendTo(index: number): Promise<void> {
    const [from, to] = selection.rangeTo(index);
    selection.setRange(await list.keysInRange(from, to), index);
  }

  function click(event: MouseEvent, index: number): void {
    if (justDragged) return;
    const row = list.row(index);
    if (!row) return;
    const key = list.key(row);
    if (event.shiftKey) {
      void extendTo(index);
    } else if (event.ctrlKey || event.metaKey) {
      selection.toggle(index, key);
    } else {
      selection.only(index, key);
    }
  }

  async function moveTo(index: number, extend: boolean): Promise<void> {
    const target = Math.max(0, Math.min(list.total - 1, index));
    virtual?.reveal(target);
    if (extend) {
      await extendTo(target);
      return;
    }
    await list.ensure(target, target + 1);
    const row = list.row(target);
    if (row) selection.only(target, list.key(row));
  }

  /** Alt with a number rates the selected rows. */
  async function rateSelection(rating: number | null): Promise<void> {
    const rows = await list.rowsInRange(0, list.total);
    const trackIds = rows
      .filter((row) => selection.has(list.key(row)))
      .map((row) => track(row).id);
    if (trackIds.length > 0) await runAction("tracks.rate", { trackIds: [...new Set(trackIds)], rating });
  }

  function keydown(event: KeyboardEvent): void {
    if (onkeys?.(event)) {
      event.preventDefault();
      return;
    }
    const rating = event.altKey ? ratingForDigit(event.key) : undefined;
    if (rating !== undefined) {
      event.preventDefault();
      void rateSelection(rating);
      return;
    }
    const cursor = selection.cursor ?? -1;
    const page = virtual?.pageRows() ?? 1;
    const steps: Record<string, number> = {
      ArrowDown: cursor + 1,
      ArrowUp: Math.max(cursor - 1, 0),
      PageDown: cursor + page,
      PageUp: cursor - page,
      Home: 0,
      End: list.total - 1,
    };
    // Ctrl with an arrow is a shortcut for the player, not movement in the list.
    if (event.key in steps && list.total > 0 && !event.ctrlKey) {
      event.preventDefault();
      void moveTo(steps[event.key], event.shiftKey);
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "a") {
      event.preventDefault();
      void list.keysInRange(0, list.total).then((keys) => selection.setRange(keys, cursor));
    } else if (event.key === "Escape") {
      selection.clear();
    } else if (event.key === "Enter" && selection.cursor !== null) {
      const row = list.row(selection.cursor);
      if (row) onactivate?.(row, selection.cursor);
    }
  }

  /** The optional columns go first when the list is narrow, so titles stay readable. */
  const headers: { name: string; sort?: TrackSort; align?: "end"; optional?: boolean }[] = [
    { name: "#", align: "end" },
    { name: "Title", sort: "title" },
    { name: "Artist", sort: "artist" },
    { name: "Album", sort: "album" },
    { name: "Year", align: "end", optional: true },
    { name: "Time", align: "end" },
    { name: "Format", optional: true },
    { name: "Rating" },
    { name: "Plays", align: "end", optional: true },
  ];
</script>

<div class="table" class:compact>
  {#if !compact}<div class="header row" role="row">
    {#each headers as header}
      <div class:end={header.align === "end"} class:optional={header.optional} role="columnheader">
        {#if header.sort && onsort}
          <button class="sort" onclick={() => onsort?.(header.sort as TrackSort)}>
            {header.name}
            {#if sort?.sort === header.sort}
              <span aria-label={sort?.descending ? "descending" : "ascending"}>
                {sort?.descending ? "▼" : "▲"}
              </span>
            {/if}
          </button>
        {:else}
          {header.name}
        {/if}
      </div>
    {/each}
  </div>{/if}

  {#if list.error}
    <p class="message error">Could not load this list: {list.error}</p>
  {:else if list.ready && list.total === 0}
    <div class="message">{@render empty()}</div>
  {/if}

  <VirtualList
    bind:this={virtual}
    bind:start
    bind:end
    total={list.total}
    {label}
    {dropGap}
    onkeydown={keydown}
  >
    {#snippet row(index)}
      {@const item = list.row(index)}
      {#if item}
        {@const info = track(item)}
        {@const isPlaying = current(item, index)}
        <!-- Keyboard selection is handled by the list, which holds focus. -->
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="row body"
          class:selected={selection.has(list.key(item))}
          class:missing={info.missing}
          class:playing={isPlaying}
          role="row"
          tabindex="-1"
          aria-rowindex={index + 1}
          aria-selected={selection.has(list.key(item))}
          onclick={(event) => click(event, index)}
          onpointerdown={(event) => press(event, index)}
          ondblclick={() => onactivate?.(item, index)}
          onmousedown={(event) => {
            if (event.shiftKey) event.preventDefault();
          }}
        >
          <div class="end muted number" role="gridcell">
            {isPlaying ? "▶" : number(item, index)}
          </div>
          <div role="gridcell" title={compact ? `${info.title} - ${info.artist}` : info.title}>
            <!-- First, so a long title cannot push it out of sight. -->
            {#if info.missing}<span class="tag">Missing</span>{/if}
            {info.title}
            {#if compact}<span class="muted">{info.artist}</span>{/if}
          </div>
          {#if !compact}
            <div role="gridcell" title={info.artist}>{info.artist}</div>
            <div role="gridcell" title={info.album}>{info.album}</div>
            <div class="end muted optional" role="gridcell">{info.year ?? ""}</div>
          {/if}
          <div class="end muted time" role="gridcell">{formatDuration(info.durationMs)}</div>
          {#if !compact}
            {@const plays = formatPlays(info)}
            <div class="muted optional" role="gridcell">{formatQuality(info)}</div>
            <div role="gridcell">
              <StarRating
                rating={info.rating}
                label="Rating of {info.title}"
                onrate={(rating) => runAction("tracks.rate", { trackIds: [info.id], rating })}
              />
            </div>
            <div class="end muted plays optional" role="gridcell" title={plays.title}>
              {plays.count}
            </div>
          {/if}
        </div>
      {:else}
        <div class="row body placeholder" aria-hidden="true"></div>
      {/if}
    {/snippet}
  </VirtualList>
</div>

<style>
  .table {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    position: relative;
    container-type: inline-size;
  }

  .row {
    display: grid;
    grid-template-columns: 4em 4fr 3fr 3fr 4em 5em 7em 5.5em 3em;
    gap: var(--space-sm);
    align-items: center;
    padding: 0 var(--space-md) 0 var(--space-sm);
  }

  @container (max-width: 800px) {
    .row {
      grid-template-columns: 3em 4fr 3fr 3fr 4em 5.5em;
    }

    .optional {
      display: none;
    }
  }

  .compact .row {
    grid-template-columns: 3em minmax(0, 1fr) auto;
    padding: 0 var(--space-sm) 0 0;
  }

  .row > div {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .header {
    height: var(--size-row);
    flex: none;
    /* An invisible scrollbar, so the columns line up with the list below. */
    overflow-y: scroll;
    scrollbar-color: transparent transparent;
    border-bottom: 1px solid var(--color-border);
    color: var(--color-text-muted);
  }

  /* On the cells, not the row: the row's font size sets its column widths. */
  .header > div {
    font-size: var(--font-size-small);
  }

  .body {
    height: 100%;
    box-sizing: border-box;
    cursor: default;
    outline: none;
    user-select: none;
  }

  .body:hover {
    background: var(--color-hover);
  }

  .body.selected {
    background: var(--color-selected);
  }

  .missing > div {
    color: var(--color-text-muted);
  }

  .playing > div {
    color: var(--color-accent);
  }

  .end {
    text-align: right;
  }

  .muted {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .time,
  .plays {
    font-variant-numeric: tabular-nums;
  }

  .tag {
    margin-right: var(--space-xs);
    padding: 0 var(--space-xs);
    border: 1px solid var(--color-danger);
    border-radius: var(--radius);
    color: var(--color-danger);
    font-size: var(--font-size-small);
  }

  .sort {
    all: unset;
    cursor: pointer;
  }

  .sort:hover {
    color: var(--color-text);
  }

  .sort:focus-visible {
    outline: 1px solid var(--color-accent);
  }

  .compact .message {
    inset: var(--space-lg) var(--space-sm) auto;
  }

  .message {
    position: absolute;
    inset: calc(var(--size-row) + var(--space-lg)) 0 auto;
    text-align: center;
    color: var(--color-text-muted);
    margin: 0;
    z-index: 1;
  }

  .message.error {
    color: var(--color-danger);
  }
</style>
