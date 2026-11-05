<script lang="ts" module>
  import type { TrackSort } from "../api/generated/TrackSort";

  export type SortState = { sort: TrackSort; descending: boolean };
</script>

<script lang="ts" generics="R">
  // The track list used by every view: virtualized rows, sortable headers, and
  // mouse and keyboard selection.
  import type { Snippet } from "svelte";
  import type { TrackRow } from "../api/generated/TrackRow";
  import { formatDuration, formatQuality } from "./format";
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
    empty: Snippet;
  };

  let { list, selection, track, number, label, sort, onsort, onactivate, empty }: Props = $props();

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

  async function extendTo(index: number): Promise<void> {
    const [from, to] = selection.rangeTo(index);
    selection.setRange(await list.keysInRange(from, to), index);
  }

  function click(event: MouseEvent, index: number): void {
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

  function keydown(event: KeyboardEvent): void {
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
    if (event.key in steps && list.total > 0) {
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

  const headers: { name: string; sort?: TrackSort; align?: "end" }[] = [
    { name: "#", align: "end" },
    { name: "Title", sort: "title" },
    { name: "Artist", sort: "artist" },
    { name: "Album", sort: "album" },
    { name: "Year", align: "end" },
    { name: "Time", align: "end" },
    { name: "Format" },
  ];
</script>

<div class="table">
  <div class="header row" role="row">
    {#each headers as header}
      <div class:end={header.align === "end"} role="columnheader">
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
  </div>

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
    onkeydown={keydown}
  >
    {#snippet row(index)}
      {@const item = list.row(index)}
      {#if item}
        {@const info = track(item)}
        <!-- Keyboard selection is handled by the list, which holds focus. -->
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="row body"
          class:selected={selection.has(list.key(item))}
          class:missing={info.missing}
          class:playing={player.playingTrackId === info.id}
          role="row"
          tabindex="-1"
          aria-rowindex={index + 1}
          aria-selected={selection.has(list.key(item))}
          onclick={(event) => click(event, index)}
          ondblclick={() => onactivate?.(item, index)}
          onmousedown={(event) => {
            if (event.shiftKey) event.preventDefault();
          }}
        >
          <div class="end muted number" role="gridcell">
            {player.playingTrackId === info.id ? "▶" : number(item, index)}
          </div>
          <div role="gridcell" title={info.title}>
            {info.title}
            {#if info.missing}<span class="tag">Missing</span>{/if}
          </div>
          <div role="gridcell" title={info.artist}>{info.artist}</div>
          <div role="gridcell" title={info.album}>{info.album}</div>
          <div class="end muted" role="gridcell">{info.year ?? ""}</div>
          <div class="end muted time" role="gridcell">{formatDuration(info.durationMs)}</div>
          <div class="muted" role="gridcell">{formatQuality(info)}</div>
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
  }

  .row {
    display: grid;
    grid-template-columns: 4em 4fr 3fr 3fr 4em 5em 7em;
    gap: var(--space-sm);
    align-items: center;
    padding: 0 var(--space-md) 0 var(--space-sm);
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

  .time {
    font-variant-numeric: tabular-nums;
  }

  .tag {
    margin-left: var(--space-xs);
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
