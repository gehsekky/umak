<script lang="ts">
  // The app's frame: named regions that registered views fill.
  import { onMount } from "svelte";
  import { keyBelongsToElement, runShortcut, shortcutName } from "../actions/registry";
  import { player } from "../player/state.svelte";
  import { app } from "../state/app.svelte";
  import { viewsIn } from "./regions";

  const MainView = $derived(viewsIn("main").find((view) => view.id === app.view.kind)?.component);

  let startError = $state<string | null>(null);

  onMount(() => {
    const stops: (() => void)[] = [];
    Promise.all([app.start(), player.start()])
      .then((unlisten) => stops.push(...unlisten))
      .catch((failure: { message?: string }) => {
        startError = failure.message ?? String(failure);
      });
    return () => stops.forEach((stop) => stop());
  });

  function keydown(event: KeyboardEvent): void {
    const name = shortcutName(event);
    // Keys in an open dialog belong to it.
    if (event.target instanceof Element && event.target.closest("dialog")) return;
    const tag = event.target instanceof HTMLElement ? event.target.tagName : null;
    if (keyBelongsToElement(name, tag)) return;
    if (runShortcut(name)) event.preventDefault();
  }
</script>

<svelte:window onkeydown={keydown} />

<div class="shell">
  <nav class="sidebar" aria-label="Library">
    {#each viewsIn("sidebar") as view (view.id)}
      <view.component />
    {/each}
  </nav>
  <main class="main">
    {#if startError}
      <p class="error">Could not reach the core: {startError}</p>
    {:else if MainView}
      <MainView />
    {/if}
  </main>
  {#if app.visualsDocked && !app.visualsFull}
    <section class="dock" aria-label="Visualizer">
      {#each viewsIn("dock") as view (view.id)}
        <view.component />
      {/each}
    </section>
  {/if}
  {#if app.queueOpen}
    <aside class="queue" aria-label="Queue">
      {#each viewsIn("queue") as view (view.id)}
        <view.component />
      {/each}
    </aside>
  {/if}
  <section class="transport" aria-label="Player">
    {#each viewsIn("transport") as view (view.id)}
      <view.component />
    {/each}
  </section>
  <footer class="status">
    {#each viewsIn("status") as view (view.id)}
      <view.component />
    {/each}
  </footer>
</div>

{#each viewsIn("overlay") as view (view.id)}
  <view.component />
{/each}

<style>
  .shell {
    display: grid;
    grid-template-columns: var(--size-sidebar) minmax(0, 1fr) auto;
    grid-template-rows: minmax(0, 1fr) auto auto auto;
    grid-template-areas:
      "sidebar main queue"
      "sidebar dock queue"
      "transport transport transport"
      "status status status";
    height: 100%;
  }

  .sidebar {
    grid-area: sidebar;
    background: var(--color-surface);
    border-right: 1px solid var(--color-border);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  .main {
    grid-area: main;
    display: flex;
    flex-direction: column;
    min-height: 0;
    /* The page itself never scrolls sideways. */
    overflow: hidden;
  }

  .dock {
    grid-area: dock;
    height: var(--size-visual-dock);
    border-top: 1px solid var(--color-border);
    display: flex;
    min-width: 0;
  }

  .queue {
    grid-area: queue;
    width: var(--size-queue);
    background: var(--color-surface);
    border-left: 1px solid var(--color-border);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  /* No room beside the main view, so it goes over it. */
  @media (max-width: 1100px) {
    .queue {
      grid-area: main;
      justify-self: end;
      max-width: 100%;
      z-index: 2;
    }
  }

  .transport {
    grid-area: transport;
    background: var(--color-surface-raised);
    border-top: 1px solid var(--color-border);
  }

  .status {
    grid-area: status;
    background: var(--color-surface);
    border-top: 1px solid var(--color-border);
  }

  .error {
    color: var(--color-danger);
    padding: var(--space-lg);
  }
</style>
