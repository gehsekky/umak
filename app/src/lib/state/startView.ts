// The view the app opens on: one the settings name, or the one it was left on.

import type { View } from "./app.svelte";

export const LAST_VIEW_KEY = "umak.lastView";

/** Views that make sense to open on. Editors and reviews belong to work in progress. */
const RESTORABLE = new Set<View["kind"]>([
  "library",
  "albums",
  "album",
  "folders",
  "settings",
  "playlist",
  "tagEdits",
  "duplicates",
  "conversions",
]);

/** The view to keep for next time, or null for one not to open on. */
export function restorable(view: View): View | null {
  return RESTORABLE.has(view.kind) ? view : null;
}

/**
 * The view to open on, given the setting and the view kept last time. A kept playlist that no
 * longer exists opens the library instead.
 */
export function startingView(choice: unknown, kept: unknown, playlistIds: string[]): View {
  if (choice === "albums") return { kind: "albums" };
  if (choice !== "last" || typeof kept !== "object" || kept === null) return { kind: "library" };
  const view = kept as { kind?: unknown; id?: unknown };
  if (typeof view.kind !== "string" || !RESTORABLE.has(view.kind as View["kind"])) {
    return { kind: "library" };
  }
  if (view.kind === "playlist" || view.kind === "album") {
    if (typeof view.id !== "string") return { kind: "library" };
    if (view.kind === "playlist" && !playlistIds.includes(view.id)) return { kind: "library" };
    return { kind: view.kind, id: view.id };
  }
  return { kind: view.kind } as View;
}
