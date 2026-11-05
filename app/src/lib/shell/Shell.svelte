<script lang="ts">
  // The app's frame: named regions that registered views fill.
  import { onMount } from "svelte";
  import { runShortcut, shortcutName } from "../actions/registry";
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

  /** Plain keys belong to whatever is being typed in or pressed. */
  function wantsPlainKeys(target: EventTarget | null): boolean {
    return (
      target instanceof HTMLElement &&
      ["INPUT", "TEXTAREA", "SELECT", "BUTTON"].includes(target.tagName)
    );
  }

  function keydown(event: KeyboardEvent): void {
    const name = shortcutName(event);
    if (!name.includes("+") && wantsPlainKeys(event.target)) return;
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

<style>
  .shell {
    display: grid;
    grid-template-columns: var(--size-sidebar) minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr) auto auto;
    grid-template-areas:
      "sidebar main"
      "transport transport"
      "status status";
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
