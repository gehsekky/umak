// Native dialogs. These belong to the desktop shell, so a web client would replace
// this file; library actions go through the command layer instead.

import { open } from "@tauri-apps/plugin-dialog";

/** Asks for a folder, returning its path or null if the user canceled. */
export async function pickFolder(title: string): Promise<string | null> {
  const choice = await open({ directory: true, multiple: false, title });
  return typeof choice === "string" ? choice : null;
}
