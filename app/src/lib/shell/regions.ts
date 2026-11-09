// Views register into named regions of the app shell instead of being wired into its
// layout, so a later layout change does not touch the views.

import type { Component } from "svelte";

/** "overlay" holds dialogs, which show themselves over the rest when needed. */
export type Region = "sidebar" | "main" | "queue" | "transport" | "status" | "overlay";

export type RegisteredView = {
  /** In the main region, the kind of `app.view` this view shows. */
  id: string;
  component: Component;
};

const regions = new Map<Region, RegisteredView[]>();

export function registerView(region: Region, view: RegisteredView): void {
  const views = regions.get(region) ?? [];
  regions.set(region, [...views.filter((existing) => existing.id !== view.id), view]);
}

export function viewsIn(region: Region): readonly RegisteredView[] {
  return regions.get(region) ?? [];
}
