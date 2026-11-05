// Native dialogs. These belong to the desktop shell, so a web client would replace
// this file; library actions go through the command layer instead.

import { open, save } from "@tauri-apps/plugin-dialog";

const EXPORT_FILTERS = [{ name: "Umak library export", extensions: ["json"] }];

/** Asks for a folder, returning its path or null if the user canceled. */
export async function pickFolder(title: string): Promise<string | null> {
  const choice = await open({ directory: true, multiple: false, title });
  return typeof choice === "string" ? choice : null;
}

/** Asks where to save an export, returning the path or null if the user canceled. */
export function pickExportTarget(defaultName: string): Promise<string | null> {
  return save({ title: "Export library", defaultPath: defaultName, filters: EXPORT_FILTERS });
}

/** Asks for an export to import, returning its path or null if the user canceled. */
export async function pickExportFile(): Promise<string | null> {
  const choice = await open({ title: "Import library", multiple: false, filters: EXPORT_FILTERS });
  return typeof choice === "string" ? choice : null;
}
