<script lang="ts">
  // A list setting's control: short pieces of text in order, each removable and movable, and a
  // box to add another. The core checks what is added.
  import { moveOption } from "./show";

  type Props = {
    id: string;
    label: string;
    value: string[];
    disabled?: boolean;
    onchange: (value: string[]) => void;
  };

  let { id, label, value, disabled = false, onchange }: Props = $props();

  let draft = $state("");

  function add(event: SubmitEvent): void {
    event.preventDefault();
    const text = draft.trim();
    if (!text) return;
    onchange([...value, text]);
    draft = "";
  }
</script>

<div class="list">
  {#if value.length > 0}
    <ul aria-label={label}>
      {#each value as item, place (item)}
        <li>
          <span class="item">{item}</span>
          <button
            aria-label="Move {item} earlier"
            disabled={disabled || place === 0}
            onclick={() => onchange(moveOption(value, place, -1))}>▲</button
          >
          <button
            aria-label="Move {item} later"
            disabled={disabled || place === value.length - 1}
            onclick={() => onchange(moveOption(value, place, 1))}>▼</button
          >
          <button
            aria-label="Remove {item}"
            {disabled}
            onclick={() => onchange(value.filter((_, index) => index !== place))}>✕</button
          >
        </li>
      {/each}
    </ul>
  {:else}
    <p class="empty">None.</p>
  {/if}
  <form onsubmit={add}>
    <input {id} bind:value={draft} {disabled} aria-label="Add to {label}" placeholder="Add" />
    <button type="submit" disabled={disabled || !draft.trim()}>Add</button>
  </form>
</div>

<style>
  .list {
    max-width: 24em;
  }

  ul {
    margin: 0 0 var(--space-xs);
    padding: 0;
    list-style: none;
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }

  li {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    padding: var(--space-xs) var(--space-sm);
  }

  li + li {
    border-top: 1px solid var(--color-border);
  }

  .item {
    flex: 1;
    overflow-wrap: anywhere;
  }

  .empty {
    margin: 0 0 var(--space-xs);
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  form {
    display: flex;
    gap: var(--space-xs);
  }

  input {
    flex: 1;
    min-width: 0;
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
  }

  button {
    padding: 0 var(--space-xs);
    background: none;
    color: var(--color-text-muted);
    border: 1px solid transparent;
    border-radius: var(--radius);
    font: inherit;
    font-size: var(--font-size-small);
    cursor: pointer;
  }

  button:hover:enabled {
    color: var(--color-text);
    background: var(--color-hover);
  }

  button:disabled {
    opacity: 0.4;
    cursor: default;
  }

  form button {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border-color: var(--color-border);
  }

  input:focus-visible,
  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }
</style>
