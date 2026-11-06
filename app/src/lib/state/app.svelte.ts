// State shared across views. It reloads itself from core events, so views never poll.

import { call, onCoreEvent } from "../api/client";
import type { CoreEvent } from "../api/generated/CoreEvent";
import type { ImportPreview } from "../api/generated/ImportPreview";
import type { ImportReport } from "../api/generated/ImportReport";
import type { LibraryRoot } from "../api/generated/LibraryRoot";
import type { Playlist } from "../api/generated/Playlist";

export type View =
  | { kind: "library" }
  | { kind: "folders" }
  | { kind: "import" }
  | { kind: "playlist"; id: string };

/** A background job reading the library: a scan, or the scan inside an import. */
export type ScanState = { id: string; kind: string; done: number; total: number };

const QUEUE_OPEN_KEY = "umak.queueOpen";
/** Below this width the queue covers the main view, so it starts closed. */
const NARROW_WINDOW = "(max-width: 900px)";

function initialQueueOpen(): boolean {
  try {
    const saved = localStorage.getItem(QUEUE_OPEN_KEY);
    if (saved !== null) return saved === "true";
  } catch {
    // Storage can be unavailable; the panel just falls back to the default.
  }
  return !window.matchMedia(NARROW_WINDOW).matches;
}

class AppState {
  view = $state<View>({ kind: "library" });
  queueOpen = $state(initialQueueOpen());
  roots = $state.raw<LibraryRoot[]>([]);
  playlists = $state.raw<Playlist[]>([]);
  scan = $state<ScanState | null>(null);
  notice = $state<{ text: string; error: boolean } | null>(null);
  creatingPlaylist = $state(false);
  importPreview = $state.raw<ImportPreview | null>(null);
  importReport = $state.raw<ImportReport | null>(null);
  /** Increases when the search box should take focus. */
  searchFocusRequests = $state(0);
  /** Increase when their contents change, so list views know to reload. */
  libraryVersion = $state(0);
  playlistsVersion = $state(0);

  readonly trackCount = $derived(this.roots.reduce((sum, root) => sum + root.trackCount, 0));

  setQueueOpen(open: boolean): void {
    this.queueOpen = open;
    try {
      localStorage.setItem(QUEUE_OPEN_KEY, String(open));
    } catch {
      // Remembering this is only a convenience.
    }
  }

  notify(text: string): void {
    this.notice = { text, error: false };
  }

  fail(text: string): void {
    this.notice = { text, error: true };
  }

  playlist(id: string): Playlist | undefined {
    return this.playlists.find((playlist) => playlist.id === id);
  }

  async loadRoots(): Promise<void> {
    this.roots = (await call("library.roots.list", {})).roots;
  }

  async loadPlaylists(): Promise<void> {
    this.playlists = (await call("playlists.list", {})).playlists;
  }

  private async finishScan(id: string): Promise<void> {
    this.scan = null;
    await this.loadRoots();
    this.libraryVersion++;
    const job = (await call("jobs.list", {})).jobs.find((candidate) => candidate.id === id);
    if (job?.status === "failed") {
      this.fail(`The scan failed: ${job.error ?? "unknown error"}`);
    } else if (job?.scan) {
      const { added, updated, moved, missing, tagErrors, readErrors, offlineRoots } = job.scan;
      const parts = [
        `${added.toLocaleString()} added`,
        `${updated.toLocaleString()} updated`,
        `${moved.toLocaleString()} moved`,
        `${missing.toLocaleString()} missing`,
      ];
      const problems = tagErrors + readErrors;
      if (problems > 0) parts.push(`${problems.toLocaleString()} with problems`);
      if (offlineRoots.length > 0) parts.push(`${offlineRoots.length} folders offline`);
      const verb = job.status === "canceled" ? "Scan canceled" : "Scan finished";
      this.notify(`${verb}: ${parts.join(", ")}`);
    }
  }

  private async finishImport(id: string): Promise<void> {
    this.scan = null;
    await Promise.all([this.loadRoots(), this.loadPlaylists()]);
    this.libraryVersion++;
    this.playlistsVersion++;
    const job = (await call("jobs.list", {})).jobs.find((candidate) => candidate.id === id);
    if (job?.status === "failed") {
      this.fail(`The import failed: ${job.error ?? "unknown error"}`);
    } else if (job?.import) {
      this.importReport = job.import;
      this.notify(job.status === "canceled" ? "Import canceled" : "Import finished");
    }
  }

  private handle(event: CoreEvent): void {
    switch (event.type) {
      case "libraryRootsChanged":
        void this.loadRoots();
        break;
      case "libraryChanged":
        this.libraryVersion++;
        void this.loadRoots();
        break;
      case "playlistsChanged":
        this.playlistsVersion++;
        void this.loadPlaylists();
        break;
      case "jobProgress":
        this.scan = { id: event.id, kind: event.kind, done: event.done, total: event.total };
        break;
      case "jobFinished":
        if (event.kind === "scan") void this.finishScan(event.id);
        else void this.finishImport(event.id);
        break;
    }
  }

  /** Loads the first state and starts following core events. */
  async start(): Promise<() => void> {
    const stop = await onCoreEvent((event) => this.handle(event));
    await Promise.all([this.loadRoots(), this.loadPlaylists()]);
    // A scan may already be running, for example after the window reloads.
    const running = (await call("jobs.list", {})).jobs.find((job) => job.status === "running");
    if (running) {
      this.scan = {
        id: running.id,
        kind: running.kind,
        done: running.done,
        total: running.total ?? 0,
      };
    }
    return stop;
  }
}

export const app = new AppState();
