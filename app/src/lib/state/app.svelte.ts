// State shared across views. It reloads itself from core events, so views never poll.

import { call, onCoreEvent } from "../api/client";
import type { CoreEvent } from "../api/generated/CoreEvent";
import type { DuplicateMatch } from "../api/generated/DuplicateMatch";
import type { ImportPreview } from "../api/generated/ImportPreview";
import type { ImportReport } from "../api/generated/ImportReport";
import type { LibraryRoot } from "../api/generated/LibraryRoot";
import type { Playlist } from "../api/generated/Playlist";
import { describeReport, needsAttention } from "../convert/options";
import { describeEdit } from "../tags/plan";
import { loadChoices, saveChoices, type VisualChoices, type VisualPlace } from "../visuals/choices";

export type View =
  | { kind: "library" }
  | { kind: "folders" }
  | { kind: "import" }
  | { kind: "settings" }
  | { kind: "playlist"; id: string }
  | { kind: "tagEditor" }
  | { kind: "tagReview"; id: string }
  | { kind: "tagEdits" }
  | { kind: "duplicates" }
  | { kind: "smartEditor"; id: string | null }
  | { kind: "conversions" };

/** A running background job and how far it has got. */
export type JobState = { id: string; kind: string; done: number; total: number };
/** A job using the library: a scan, an import or a tag edit. */
export type ScanState = JobState;

const QUEUE_OPEN_KEY = "umak.queueOpen";
const VISUALS_DOCKED_KEY = "umak.visualsDocked";
/** Below this width the queue covers the main view, so it starts closed. */
const NARROW_WINDOW = "(max-width: 1100px)";

function initialQueueOpen(): boolean {
  try {
    const saved = localStorage.getItem(QUEUE_OPEN_KEY);
    if (saved !== null) return saved === "true";
  } catch {
    // Storage can be unavailable; the panel just falls back to the default.
  }
  return !window.matchMedia(NARROW_WINDOW).matches;
}

function initialVisualsDocked(): boolean {
  try {
    return localStorage.getItem(VISUALS_DOCKED_KEY) === "true";
  } catch {
    return false;
  }
}

class AppState {
  view = $state<View>({ kind: "library" });
  queueOpen = $state(initialQueueOpen());
  /** Whether a visualizer shows docked below the main view. */
  visualsDocked = $state(initialVisualsDocked());
  /** Whether a visualizer fills the screen. */
  visualsFull = $state(false);
  visualChoices = $state<VisualChoices>(loadChoices());
  roots = $state.raw<LibraryRoot[]>([]);
  playlists = $state.raw<Playlist[]>([]);
  scan = $state<ScanState | null>(null);
  /** The conversion batch running beside library jobs. */
  conversion = $state<JobState | null>(null);
  /** The tracks the conversion dialog is open for. */
  convertTracks = $state.raw<string[] | null>(null);
  notice = $state<{ text: string; error: boolean } | null>(null);
  creatingPlaylist = $state(false);
  importPreview = $state.raw<ImportPreview | null>(null);
  importReport = $state.raw<ImportReport | null>(null);
  /** Increases when the search box should take focus. */
  searchFocusRequests = $state(0);
  /** What the library should search for when it next shows. */
  librarySearch = $state<string | null>(null);
  /** Kept here so going to edit a group's tags and back stays on the same comparison. */
  duplicatesBy = $state<DuplicateMatch>("audio");
  /** Increase when their contents change, so list views know to reload. */
  libraryVersion = $state(0);
  playlistsVersion = $state(0);
  tagEditsVersion = $state(0);
  conversionsVersion = $state(0);
  /** A previewed batch not yet written or discarded. */
  waitingEdit = $state<string | null>(null);
  /** The batch being written, to report on when its job ends. */
  writingEdit: string | null = null;
  /** Where an edit started, to return to when it is done. */
  editOrigin = $state<View>({ kind: "library" });

  /** The playlists tracks can be added to. A smart playlist's rule decides what it holds. */
  readonly manualPlaylists = $derived(this.playlists.filter((playlist) => !playlist.smart));

  readonly trackCount = $derived(this.roots.reduce((sum, root) => sum + root.trackCount, 0));

  setQueueOpen(open: boolean): void {
    this.queueOpen = open;
    try {
      localStorage.setItem(QUEUE_OPEN_KEY, String(open));
    } catch {
      // Remembering this is only a convenience.
    }
  }

  setVisualsDocked(docked: boolean): void {
    this.visualsDocked = docked;
    try {
      localStorage.setItem(VISUALS_DOCKED_KEY, String(docked));
    } catch {
      // Remembering this is only a convenience.
    }
  }

  chooseVisual(place: VisualPlace, id: string): void {
    this.visualChoices = { ...this.visualChoices, [place]: id };
    saveChoices(this.visualChoices);
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

  /** The review screen shows a batch in full; this says how it went from anywhere else. */
  private async finishTagJob(id: string): Promise<void> {
    this.scan = null;
    this.libraryVersion++;
    this.tagEditsVersion++;
    const job = (await call("jobs.list", {})).jobs.find((candidate) => candidate.id === id);
    if (job?.status === "failed") {
      this.fail(`Tag editing failed: ${job.error ?? "unknown error"}`);
    }
    const written = this.writingEdit;
    if (job?.kind === "tagWrite" && written) {
      this.writingEdit = null;
      const edit = await call("tags.edits.get", { id: written });
      const text = describeEdit(edit);
      if (edit.failed > 0) this.fail(`Tag edit finished: ${text}`);
      else this.notify(`Tag edit ${edit.status === "canceled" ? "canceled" : "finished"}: ${text}`);
    }
  }

  private async finishConversion(id: string): Promise<void> {
    this.conversion = null;
    this.conversionsVersion++;
    const batch = await call("convert.get", { jobId: id });
    if (batch.status === "failed") {
      this.fail(`The conversion failed: ${batch.error ?? "unknown error"}`);
      return;
    }
    const verb = batch.status === "canceled" ? "Conversion canceled" : "Conversion finished";
    const text = `${verb}: ${describeReport(batch.report)}`;
    if (needsAttention(batch.report)) this.fail(text);
    else this.notify(text);
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
      case "tagEditsChanged":
        this.tagEditsVersion++;
        break;
      case "jobProgress": {
        const job = { id: event.id, kind: event.kind, done: event.done, total: event.total };
        if (event.kind === "convert") this.conversion = job;
        else this.scan = job;
        break;
      }
      case "jobFinished":
        if (event.kind === "convert") void this.finishConversion(event.id);
        else if (event.kind === "scan") void this.finishScan(event.id);
        else if (event.kind === "import") void this.finishImport(event.id);
        else void this.finishTagJob(event.id);
        break;
    }
  }

  /** Loads the first state and starts following core events. */
  async start(): Promise<() => void> {
    const stop = await onCoreEvent((event) => this.handle(event));
    await Promise.all([this.loadRoots(), this.loadPlaylists()]);
    // Jobs may already be running, for example after the window reloads.
    const running = (await call("jobs.list", {})).jobs.filter((job) => job.status === "running");
    for (const job of running) {
      const state = { id: job.id, kind: job.kind, done: job.done, total: job.total ?? 0 };
      if (job.kind === "convert") this.conversion = state;
      else this.scan = state;
    }
    return stop;
  }
}

export const app = new AppState();
