<script lang="ts">
  // A cover picture filling its box, or a placeholder when there is none or it fails to load.
  import { artUrl, type ArtSize } from "../api/client";

  type Props = {
    /** The picture's name from the core, or null for none. */
    art: string | null;
    size: ArtSize;
    alt: string;
  };

  let { art, size, alt }: Props = $props();

  /** The picture that failed to load, so a different one is tried again. */
  let broken = $state<string | null>(null);
</script>

<div class="cover">
  {#if art && art !== broken}
    <img src={artUrl(art, size)} {alt} onerror={() => (broken = art)} />
  {:else}
    <span class="placeholder" aria-hidden="true">♪</span>
  {/if}
</div>

<style>
  .cover {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    overflow: hidden;
    box-sizing: border-box;
  }

  img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }

  .placeholder {
    color: var(--color-text-muted);
    font-size: var(--font-size-title);
  }
</style>
