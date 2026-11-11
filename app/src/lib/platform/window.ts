// The app window. This belongs to the desktop shell, so a web client would replace this file.

import { getCurrentWindow } from "@tauri-apps/api/window";

/** Fills the screen with the window, or puts it back. A failure leaves the window as it is. */
export async function setFullScreen(on: boolean): Promise<void> {
  try {
    await getCurrentWindow().setFullscreen(on);
  } catch {
    // Without the desktop shell the view still fills the window.
  }
}
