<script lang="ts">
  import { onMount } from "svelte";
  import { call, onCoreEvent } from "./lib/api/client";
  import type { ApiError } from "./lib/api/generated/ApiError";
  import type { LibraryRoot } from "./lib/api/generated/LibraryRoot";

  const info = call("app.info", {});

  let roots = $state<LibraryRoot[]>([]);
  let error = $state<string | null>(null);

  async function loadRoots() {
    try {
      roots = (await call("library.roots.list", {})).roots;
      error = null;
    } catch (failure) {
      error = (failure as ApiError).message;
    }
  }

  onMount(() => {
    loadRoots();
    const unlisten = onCoreEvent((event) => {
      if (event.type === "libraryRootsChanged") {
        loadRoots();
      }
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  });
</script>

<main>
  {#await info}
    <p>Loading</p>
  {:then { name, version }}
    <h1>{name}</h1>
    <p>Version {version}</p>
  {:catch failure}
    <p>Could not reach the core: {failure.message}</p>
  {/await}

  {#if error}
    <p>Could not load library folders: {error}</p>
  {:else}
    <p>Library folders: {roots.length}</p>
  {/if}
</main>

<style>
  main {
    padding: var(--space-lg);
  }

  h1 {
    color: var(--color-accent);
    margin: 0 0 var(--space-sm);
  }

  p {
    color: var(--color-text-muted);
    margin: 0 0 var(--space-sm);
  }
</style>
