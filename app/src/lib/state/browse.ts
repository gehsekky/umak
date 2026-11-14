// Which way of showing the library a view belongs to.

import type { View } from "./app.svelte";

/** The ways the library can be shown, each a view of its own. */
export type LibraryBrowse = "library" | "artists" | "albums";

/** The library view a view belongs to, or null for one outside the library. */
export function browseOf(view: View): LibraryBrowse | null {
  switch (view.kind) {
    case "library":
    case "artists":
    case "albums":
      return view.kind;
    case "artist":
      return "artists";
    case "album":
      return view.from ? "artists" : "albums";
    default:
      return null;
  }
}
