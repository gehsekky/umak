<script lang="ts">
  // The shortcuts setting's control: every action that can take a key, under headings, with
  // its key. Changing one records the next key pressed and shows any action that has it, which
  // saving takes it from.
  import { tick } from "svelte";
  import { changedShortcuts } from "../actions/keys";
  import { runAction, shortcutActions, shortcutName, type ActionId } from "../actions/registry";
  import {
    actionGroup,
    assignShortcut,
    currentShortcuts,
    keyLabel,
    SHORTCUTS_KEY,
    unusableKey,
  } from "../actions/shortcuts";

  let { id, query }: { id: string; query: string } = $props();

  const actions = shortcutActions();
  const changed = $derived(changedShortcuts());
  const current = $derived(currentShortcuts(actions, changed));

  /** The rows matching every word searched for, or all of them if none do. */
  const shown = $derived.by(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    const matching = actions.filter((action) => {
      const key = current.get(action.id);
      const text = [actionGroup(action.id), action.label, key ? keyLabel(key) : ""]
        .join(" ")
        .toLowerCase();
      return words.every((word) => text.includes(word));
    });
    return matching.length > 0 ? matching : actions;
  });

  const groups = $derived.by(() => {
    const found: { name: string; actions: typeof actions }[] = [];
    for (const action of shown) {
      const name = actionGroup(action.id);
      let group = found.find((candidate) => candidate.name === name);
      if (!group) {
        group = { name, actions: [] };
        found.push(group);
      }
      group.actions.push(action);
    }
    return found;
  });

  /** The action being given a key, and the key pressed once there is one. */
  let recording = $state<{ id: ActionId; key: string | null; problem: string | null } | null>(
    null,
  );
  let recorder = $state<HTMLButtonElement | null>(null);
  let saver = $state<HTMLButtonElement | null>(null);

  const holder = $derived.by(() => {
    if (!recording?.key) return null;
    for (const [other, key] of current) {
      if (other !== recording.id && key === recording.key) return other;
    }
    return null;
  });

  function labelOf(action: ActionId): string {
    return actions.find((candidate) => candidate.id === action)?.label ?? action;
  }

  async function record(action: ActionId): Promise<void> {
    recording = { id: action, key: null, problem: null };
    await tick();
    recorder?.focus();
  }

  function keydown(event: KeyboardEvent): void {
    if (!recording) return;
    // Tab still moves between controls.
    if (event.key === "Tab" && !event.ctrlKey && !event.altKey && !event.metaKey) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Escape") {
      recording = null;
      return;
    }
    const name = shortcutName(event);
    const problem = unusableKey(name);
    if (problem === "") return;
    if (problem) {
      recording = { id: recording.id, key: null, problem };
      return;
    }
    recording = { id: recording.id, key: name, problem: null };
    // So Enter saves it. Choosing the key again records another.
    void tick().then(() => saver?.focus());
  }

  function save(action: ActionId, key: string | null): void {
    recording = null;
    const next = assignShortcut(actions, changed, action, key);
    void runAction("settings.set", { key: SHORTCUTS_KEY, value: next.changed });
  }

  /** Its default key may have been given to another action since, and is taken back. */
  function reset(action: ActionId): void {
    save(action, actions.find((candidate) => candidate.id === action)?.shortcut ?? null);
  }
</script>

<div {id} class="shortcuts">
  {#each groups as group (group.name)}
    <h3>{group.name}</h3>
    <ul>
      {#each group.actions as action (action.id)}
        {@const key = current.get(action.id) ?? null}
        {@const isChanged = action.id in changed}
        <li data-testid="shortcut-{action.id}">
          <span class="name">{action.label}</span>
          {#if recording?.id === action.id}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <span
              class="recording"
              onkeydown={(event) => {
                if (event.key === "Escape") {
                  event.stopPropagation();
                  recording = null;
                }
              }}
            >
              <button
                class="key waiting"
                bind:this={recorder}
                onkeydown={keydown}
                onblur={() => {
                  if (recording && !recording.key) recording = null;
                }}
              >
                {recording.key ? keyLabel(recording.key) : "Press a key"}
              </button>
              {#if recording.key}
                <button bind:this={saver} onclick={() => save(action.id, recording?.key ?? null)}>
                  Save
                </button>
              {/if}
              <button onclick={() => (recording = null)}>Cancel</button>
            </span>
          {:else}
            <span class="key" class:none={!key}>{key ? keyLabel(key) : "None"}</span>
            <span class="buttons">
              <button aria-label="Change the shortcut of {action.label}" onclick={() => record(action.id)}>
                Change
              </button>
              {#if key}
                <button aria-label="Clear the shortcut of {action.label}" onclick={() => save(action.id, null)}>
                  Clear
                </button>
              {/if}
              {#if isChanged}
                <button
                  class="link"
                  aria-label="Reset the shortcut of {action.label}"
                  title={action.shortcut ? `Back to ${keyLabel(action.shortcut)}` : "Back to none"}
                  onclick={() => reset(action.id)}
                >
                  Reset
                </button>
              {/if}
            </span>
          {/if}
          {#if recording?.id === action.id && (recording.problem || holder)}
            <p class="note" class:problem={recording.problem}>
              {#if recording.problem}
                {recording.problem}
              {:else if holder}
                {keyLabel(recording.key ?? "")} is the shortcut of {labelOf(holder)}. Saving takes
                it from there.
              {/if}
            </p>
          {/if}
        </li>
      {/each}
    </ul>
  {/each}
</div>

<style>
  .shortcuts {
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    padding: 0 var(--space-sm) var(--space-sm);
  }

  h3 {
    margin: var(--space-md) 0 var(--space-xs);
    font-size: var(--font-size-small);
    color: var(--color-text-muted);
    font-weight: 600;
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-xs) var(--space-sm);
    padding: var(--space-xs) 0;
  }

  li + li {
    border-top: 1px solid var(--color-border);
  }

  .name {
    flex: 1;
    min-width: 10em;
  }

  .key {
    min-width: 8em;
    padding: 0 var(--space-xs);
    font-size: var(--font-size-small);
    font-weight: 600;
  }

  .key.none {
    color: var(--color-text-muted);
  }

  .recording,
  .buttons {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
  }

  .buttons {
    min-width: 12em;
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

  button:hover {
    background: var(--color-hover);
  }

  button.waiting {
    border-color: var(--color-accent);
    text-align: left;
  }

  button.link {
    padding: 0;
    background: none;
    border: none;
    color: var(--color-text-muted);
    text-decoration: underline;
  }

  button.link:hover {
    color: var(--color-text);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .note {
    flex-basis: 100%;
    margin: 0;
    font-size: var(--font-size-small);
    color: var(--color-text-muted);
  }

  .note.problem {
    color: var(--color-danger);
  }
</style>
