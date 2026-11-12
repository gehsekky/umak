<script lang="ts">
  // The output device setting's control: the devices there are now, the system's default, and
  // a chosen one that is not connected, with the one playback is going through.
  import { onMount } from "svelte";
  import { runAction } from "../actions/registry";
  import { playerClient } from "../player/client";
  import { player } from "../player/state.svelte";
  import { app } from "../state/app.svelte";
  import { settings } from "./state.svelte";

  let { id }: { id: string } = $props();

  const chosen = $derived(String(settings.value("player.outputDevice") ?? ""));
  let devices = $state<string[] | null>(null);
  let looking = $state(false);
  const missing = $derived(chosen !== "" && devices !== null && !devices.includes(chosen));

  onMount(() => {
    void look();
  });

  async function look(): Promise<void> {
    looking = true;
    try {
      devices = await playerClient.outputDevices();
    } catch (failure) {
      app.fail((failure as { message?: string }).message ?? String(failure));
    } finally {
      looking = false;
    }
  }
</script>

<div class="row">
  <select
    {id}
    value={chosen}
    onchange={(event) =>
      runAction("settings.set", { key: "player.outputDevice", value: event.currentTarget.value })}
  >
    <option value="">System default</option>
    {#each devices ?? [] as device (device)}
      <option value={device}>{device}</option>
    {/each}
    {#if missing || (devices === null && chosen !== "")}
      <option value={chosen}>{chosen}{missing ? " (not connected)" : ""}</option>
    {/if}
  </select>
  <button onclick={look} disabled={looking}>Look again</button>
</div>
{#if player.status?.device}
  <p class="note" data-testid="output-device">Playing through {player.status.device}.</p>
{/if}

<style>
  .row {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    flex-wrap: wrap;
  }

  select {
    max-width: 100%;
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
  }

  button {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
    font-size: var(--font-size-small);
    cursor: pointer;
  }

  button:hover:enabled {
    background: var(--color-hover);
  }

  button:disabled {
    color: var(--color-text-muted);
    cursor: default;
  }

  select:focus-visible,
  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .note {
    margin: var(--space-sm) 0 0;
    font-size: var(--font-size-small);
    color: var(--color-text-muted);
  }
</style>
