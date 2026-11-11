<script lang="ts">
  // What can be done with the selected tracks of a list: play, queue, edit, convert, or add
  // them to a playlist.
  import { actionLabel, runAction } from "../actions/registry";
  import { app } from "../state/app.svelte";
  import { countOf } from "./format";

  type Props = {
    /** The selected tracks, in selection order. */
    trackIds: string[];
    /** Shown when nothing is selected. */
    hint: string;
    /** Whether to offer playing the album of a single selected track. */
    playAlbum?: boolean;
  };

  let { trackIds, hint, playAlbum = true }: Props = $props();

  let addTarget = $state("");

  async function addToPlaylist(): Promise<void> {
    if (!addTarget) return;
    await runAction("playlists.addTracks", { id: addTarget, trackIds });
    addTarget = "";
  }
</script>

<!-- Always here, so selecting a row cannot shift the list between the two clicks of a
     double-click, whatever the window's width. -->
<div class="actions">
  {#if trackIds.length > 0}
    <span class="count">{countOf(trackIds.length, "track")} selected</span>
    <button class="primary" onclick={() => runAction("player.playTracks", { trackIds, startIndex: 0 })}>
      {actionLabel("player.playTracks")}
    </button>
    {#if playAlbum && trackIds.length === 1}
      <button onclick={() => runAction("player.playAlbum", { trackId: trackIds[0] })}>
        {actionLabel("player.playAlbum")}
      </button>
    {/if}
    <button onclick={() => runAction("queue.playNext", { trackIds })}>
      {actionLabel("queue.playNext")}
    </button>
    <button onclick={() => runAction("queue.add", { trackIds })}>
      {actionLabel("queue.add")}
    </button>
    <button onclick={() => runAction("tags.edit", { trackIds })}>
      {actionLabel("tags.edit")}
    </button>
    <button onclick={() => runAction("convert.open", { trackIds })}>
      {actionLabel("convert.open")}
    </button>
    <select
      bind:value={addTarget}
      onchange={addToPlaylist}
      aria-label={actionLabel("playlists.addTracks")}
      disabled={app.manualPlaylists.length === 0}
    >
      <option value="">
        {app.manualPlaylists.length === 0 ? "Create a playlist first" : actionLabel("playlists.addTracks")}
      </option>
      {#each app.manualPlaylists as playlist (playlist.id)}
        <option value={playlist.id}>{playlist.name}</option>
      {/each}
    </select>
  {:else}
    <span class="count">{hint}</span>
  {/if}
</div>

<style>
  .actions {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-xs) var(--space-md);
    border-bottom: 1px solid var(--color-border);
    /* The height with its buttons, so the list stays put with or without them. */
    min-height: calc(var(--size-row) + var(--space-sm));
    box-sizing: border-box;
    /* Never wraps, which would move the list. Off-screen buttons scroll into view. */
    overflow-x: auto;
    scrollbar-width: none;
  }

  .actions > button {
    flex: none;
  }

  .count {
    margin-right: auto;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    white-space: nowrap;
  }

  select {
    min-width: 0;
    flex: 0 1 auto;
  }

  button,
  select {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
    font-size: var(--font-size-small);
    white-space: nowrap;
    cursor: pointer;
  }

  button:hover {
    background: var(--color-hover);
  }

  button.primary {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
  }

  button:focus-visible,
  select:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }
</style>
