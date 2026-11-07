<script lang="ts">
  // Five stars, set in halves. Pressing the rating a track already has clears it.
  import { ratingAt, starsLabel, stepRating } from "./rating";

  type Props = {
    rating: number | null;
    onrate: (rating: number | null) => void;
    /** What is being rated, for screen readers. */
    label: string;
    /** Whether the keyboard can reach it. A list row leaves that to the list's own keys. */
    focusable?: boolean;
  };

  let { rating, onrate, label, focusable = false }: Props = $props();

  let hovered = $state<number | null>(null);
  const shown = $derived(hovered ?? rating ?? 0);

  function at(event: MouseEvent): number {
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
    return ratingAt(event.clientX - box.left, box.width);
  }

  function press(event: MouseEvent): void {
    // The row underneath would otherwise select itself, or start playing on a double-click.
    event.stopPropagation();
    const chosen = at(event);
    onrate(chosen === rating ? null : chosen);
  }

  function keydown(event: KeyboardEvent): void {
    const steps: Record<string, number> = { ArrowRight: 1, ArrowUp: 1, ArrowLeft: -1, ArrowDown: -1 };
    if (!(event.key in steps)) return;
    event.preventDefault();
    event.stopPropagation();
    onrate(stepRating(rating, steps[event.key]));
  }
</script>

<!-- The list's own keys rate selected rows, and the player bar's stars take the arrow keys. -->
<div
  class="stars"
  role="slider"
  tabindex={focusable ? 0 : -1}
  aria-label={label}
  aria-valuemin={0}
  aria-valuemax={5}
  aria-valuenow={(rating ?? 0) / 2}
  aria-valuetext={starsLabel(rating)}
  title={starsLabel(rating)}
  onclick={press}
  ondblclick={(event) => event.stopPropagation()}
  onpointerdown={(event) => event.stopPropagation()}
  onmousemove={(event) => (hovered = at(event))}
  onmouseleave={() => (hovered = null)}
  onkeydown={keydown}
>
  <span class="empty" aria-hidden="true">★★★★★</span>
  <span class="full" aria-hidden="true" style:width="{shown * 10}%">★★★★★</span>
</div>

<style>
  .stars {
    position: relative;
    display: inline-block;
    white-space: nowrap;
    line-height: 1;
    cursor: pointer;
    outline: none;
    user-select: none;
  }

  .stars:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .empty {
    color: var(--color-border);
  }

  .full {
    position: absolute;
    inset: 0 auto 0 0;
    overflow: hidden;
    color: var(--color-accent);
  }
</style>
