<script lang="ts">
  // The pictures in one file's tags, shown but not edited.
  import { untrack } from "svelte";
  import { call } from "../api/client";
  import type { TrackPictures } from "../api/generated/TrackPictures";
  import Cover from "../lists/Cover.svelte";
  import { app } from "../state/app.svelte";
  import { pictureFacts } from "./pictures";

  type Props = {
    trackId: string | null;
    /** The file's name, for the heading. */
    name: string;
  };

  let { trackId, name }: Props = $props();

  let found = $state.raw<TrackPictures | null>(null);
  let error = $state<string | null>(null);

  async function load(id: string): Promise<void> {
    try {
      const pictures = await call("tags.pictures", { trackId: id });
      if (id !== trackId) return;
      found = pictures;
      error = null;
    } catch (failure) {
      if (id !== trackId) return;
      found = null;
      error = (failure as { message?: string }).message ?? String(failure);
    }
  }

  $effect(() => {
    const id = trackId;
    // A write changes the file, though not its pictures yet.
    void app.tagEditsVersion;
    untrack(() => {
      if (found?.trackId !== id) found = null;
      error = null;
      if (id !== null) void load(id);
    });
  });
</script>

<section class="pictures" aria-label="Pictures in {name}">
  <h2>Pictures{name ? ` in ${name}` : ""}</h2>
  {#if error}
    <p class="note error">{error}</p>
  {:else if found === null}
    <p class="note">{trackId === null ? "" : "Reading..."}</p>
  {:else if found.pictures.length === 0}
    <p class="note">This file has no pictures in its tags.</p>
  {:else}
    <ul>
      {#each found.pictures as picture, index (index)}
        <li>
          <div class="picture">
            <Cover art={picture.art} size="small" alt="{picture.kind} of {name}" />
          </div>
          <div class="kind">{picture.kind}</div>
          {#if picture.description}
            <div class="note" title={picture.description}>{picture.description}</div>
          {/if}
          <div class="note facts">
            {#each pictureFacts(picture) as fact, factIndex (factIndex)}
              {#if factIndex > 0}{" · "}{/if}<span>{fact}</span>
            {/each}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  h2 {
    margin: var(--space-md) 0 var(--space-sm);
    font-size: var(--font-size-body);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  ul {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-md);
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    width: var(--size-tag-picture);
    min-width: 0;
  }

  .picture {
    width: var(--size-tag-picture);
    height: var(--size-tag-picture);
    margin-bottom: var(--space-xs);
  }

  .kind,
  .note {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .note {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .facts {
    white-space: normal;
  }

  .facts span {
    white-space: nowrap;
  }

  .error {
    color: var(--color-danger);
  }
</style>
