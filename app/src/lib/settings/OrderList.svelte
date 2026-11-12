<script lang="ts">
  // Chooses some of a setting's options, in order: a check box for each, and buttons to move
  // the chosen ones up and down. The chosen ones come first, in their order.
  import type { SettingOption } from "../api/generated/SettingOption";
  import { moveOption, orderedOptions } from "./show";

  type Props = {
    id: string;
    options: SettingOption[];
    value: string[];
    disabled?: boolean;
    onchange: (value: string[]) => void;
  };

  let { id, options, value, disabled = false, onchange }: Props = $props();

  const listed = $derived(orderedOptions(options, value));

  function toggle(option: string, chosen: boolean): void {
    onchange(chosen ? [...value, option] : value.filter((name) => name !== option));
  }
</script>

<ul {id} class="order">
  {#each listed as option (option.value)}
    {@const place = value.indexOf(option.value)}
    <li class:off={place < 0}>
      <label>
        <input
          type="checkbox"
          checked={place >= 0}
          {disabled}
          onchange={(event) => toggle(option.value, event.currentTarget.checked)}
        />
        {option.label}
      </label>
      {#if place >= 0}
        <button
          aria-label="Move {option.label} up"
          disabled={disabled || place === 0}
          onclick={() => onchange(moveOption(value, place, -1))}>▲</button
        >
        <button
          aria-label="Move {option.label} down"
          disabled={disabled || place === value.length - 1}
          onclick={() => onchange(moveOption(value, place, 1))}>▼</button
        >
      {/if}
    </li>
  {/each}
</ul>

<style>
  .order {
    max-width: 20em;
    margin: 0;
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

  li.off label {
    color: var(--color-text-muted);
  }

  label {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    cursor: pointer;
  }

  input {
    margin: 0;
    accent-color: var(--color-accent);
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
    border-color: var(--color-border);
  }

  button:disabled {
    opacity: 0.3;
    cursor: default;
  }

  input:focus-visible,
  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }
</style>
