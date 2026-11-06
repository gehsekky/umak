// The edit being prepared. It lives outside the screen, so that going back from a preview, or
// looking at another view, does not lose what was typed.

import type { FieldEdit } from "../api/generated/FieldEdit";
import type { TagField } from "../api/generated/TagField";
import type { TrackEdits } from "../api/generated/TrackEdits";
import type { TrackTagValues } from "../api/generated/TrackTagValues";
import {
  absorbValues,
  buildChanges,
  newSharedField,
  numberInOrder,
  ownEdits,
  SHARED_FIELDS,
  sharedEdit,
  valueProblem,
  type SharedField,
} from "./plan";

/** Small, so a selection on a slow disk keeps showing progress and can stop soon. */
const READ_CHUNK = 25;

export type DraftTrack = {
  id: string;
  /** Null until read, and for a track no longer in the library. */
  relPath: string | null;
  title: string;
  trackNumber: string;
  /** Read, whether that worked or not. */
  read: boolean;
  error: string | null;
};

type Typed = { title?: string; trackNumber?: string };
type Reader = (trackIds: string[]) => Promise<TrackTagValues[]>;

function valuesOf(track: TrackTagValues, field: TagField): string[] {
  return track.fields.find((found) => found.field === field)?.values ?? [];
}

export class TagDraft {
  active = $state(false);
  tracks = $state.raw<DraftTrack[]>([]);
  shared = $state<Record<string, SharedField>>({});
  typed = $state.raw<ReadonlyMap<string, Typed>>(new Map());
  loaded = $state(0);
  readError = $state<string | null>(null);
  /** The batch this draft has been previewed as. */
  editId = $state<string | null>(null);

  /** Replaced to stop a read still going for an earlier selection. */
  private reading = 0;
  private readonly reader: Reader;

  constructor(reader: Reader) {
    this.reader = reader;
  }

  readonly allRead = $derived(this.loaded >= this.tracks.length);

  /** Starts on a selection. A track picked twice, as a playlist can hold, is edited once. */
  open(trackIds: readonly string[]): Promise<void> {
    const ids = [...new Set(trackIds)];
    this.active = true;
    this.editId = null;
    this.readError = null;
    this.loaded = 0;
    this.typed = new Map();
    this.tracks = ids.map((id) => ({
      id,
      relPath: null,
      title: "",
      trackNumber: "",
      read: false,
      error: null,
    }));
    this.shared = Object.fromEntries(
      SHARED_FIELDS.map(({ field }) => [field, newSharedField()]),
    );
    return this.readAll(ids, ++this.reading);
  }

  close(): void {
    this.reading++;
    this.active = false;
    this.editId = null;
    this.tracks = [];
    this.shared = {};
    this.typed = new Map();
    this.loaded = 0;
  }

  private async readAll(ids: string[], reading: number): Promise<void> {
    for (let start = 0; start < ids.length; start += READ_CHUNK) {
      let found: TrackTagValues[];
      try {
        found = await this.reader(ids.slice(start, start + READ_CHUNK));
      } catch (failure) {
        if (reading === this.reading) {
          this.readError = (failure as { message?: string }).message ?? String(failure);
        }
        return;
      }
      if (reading !== this.reading) return;
      this.absorb(start, found);
    }
  }

  private absorb(start: number, found: TrackTagValues[]): void {
    const tracks = [...this.tracks];
    found.forEach((track, offset) => {
      tracks[start + offset] = {
        id: track.trackId,
        relPath: track.relPath,
        title: valuesOf(track, "title")[0] ?? "",
        trackNumber: valuesOf(track, "trackNumber")[0] ?? "",
        read: true,
        error: track.error,
      };
    });
    this.tracks = tracks;
    // A file that cannot be read has no values to compare; the preview reports it.
    const readable = found.filter((track) => track.error === null);
    for (const { field } of SHARED_FIELDS) {
      absorbValues(
        this.shared[field],
        readable.map((track) => valuesOf(track, field)),
      );
    }
    this.loaded += found.length;
  }

  /** What the title or track number box of a track shows. */
  shownValue(track: DraftTrack, field: "title" | "trackNumber"): string {
    return this.typed.get(track.id)?.[field] ?? track[field];
  }

  setOwn(id: string, field: "title" | "trackNumber", value: string): void {
    const typed = new Map(this.typed);
    typed.set(id, { ...typed.get(id), [field]: value });
    this.typed = typed;
  }

  /** Numbers the tracks from 1, in the order they are listed. */
  numberTracks(): void {
    const numbers = numberInOrder(this.tracks.length);
    const typed = new Map(this.typed);
    this.tracks.forEach((track, index) => {
      typed.set(track.id, { ...typed.get(track.id), trackNumber: numbers[index] });
    });
    this.typed = typed;
  }

  private sharedEdits(): FieldEdit[] {
    const edits: FieldEdit[] = [];
    for (const { field } of SHARED_FIELDS) {
      const values = sharedEdit(this.shared[field], this.allRead);
      if (values) edits.push({ field, values });
    }
    return edits;
  }

  private ownEditsByTrack(): Map<string, FieldEdit[]> {
    const own = new Map<string, FieldEdit[]>();
    for (const track of this.tracks) {
      const typed = this.typed.get(track.id);
      if (!typed) continue;
      const edits = ownEdits(track, typed);
      if (edits.length > 0) own.set(track.id, edits);
    }
    return own;
  }

  /** The request to preview, or null when nothing would change. */
  readonly changes: TrackEdits[] | null = $derived.by(() => {
    if (!this.active) return null;
    // A track that has left the library has nothing to edit, and the core refuses its ID.
    const ids = this.tracks
      .filter((track) => !track.read || track.relPath !== null)
      .map((track) => track.id);
    return buildChanges(ids, this.sharedEdits(), this.ownEditsByTrack());
  });

  /** Values the core would refuse, so that the preview can wait until they are fixed. */
  readonly problems: string[] = $derived.by(() => {
    const found: string[] = [];
    for (const { field, label } of SHARED_FIELDS) {
      const state = this.shared[field];
      const problem = state?.values.map((value) => valueProblem(field, value)).find(Boolean);
      if (state?.touched && problem) found.push(`${label}: ${problem}`);
    }
    for (const [id, typed] of this.typed) {
      const problem = typed.trackNumber && valueProblem("trackNumber", typed.trackNumber);
      if (!problem) continue;
      const track = this.tracks.find((candidate) => candidate.id === id);
      found.push(`Track number of ${track?.relPath ?? "a track"}: ${problem}`);
      break;
    }
    return found;
  });
}
