<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  type AppInfo = { name: string; version: string };

  const info = invoke<AppInfo>("app_info");
</script>

<main>
  {#await info}
    <p>Loading</p>
  {:then { name, version }}
    <h1>{name}</h1>
    <p>Version {version}</p>
  {:catch error}
    <p>Could not reach the core: {error}</p>
  {/await}
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
    margin: 0;
  }
</style>
