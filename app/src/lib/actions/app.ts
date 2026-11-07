// The app's actions. Buttons and shortcuts run these by ID, through the registry.

import { call } from "../api/client";
import { countOf } from "../lists/format";
import { pickExportFile, pickExportTarget, pickFolder } from "../platform/dialog";
import { playerClient } from "../player/client";
import { player } from "../player/state.svelte";
import { app } from "../state/app.svelte";
import { tagDraft } from "../tags/current.svelte";
import { onActionError, registerAction } from "./registry";

const SKIP_MS = 5000;
/** A share of the volume slider. */
const VOLUME_STEP = 0.05;

export function registerAppActions(): void {
  onActionError((message) => app.fail(message));

  registerAction({
    id: "nav.library",
    label: "Library",
    run: () => {
      app.view = { kind: "library" };
    },
  });
  registerAction({
    id: "nav.folders",
    label: "Library folders",
    run: () => {
      app.view = { kind: "folders" };
    },
  });
  registerAction({
    id: "nav.playlist",
    label: "Open playlist",
    run: ({ id }) => {
      app.view = { kind: "playlist", id };
    },
  });

  registerAction({
    id: "nav.settings",
    label: "Audio",
    run: () => {
      app.view = { kind: "settings" };
    },
  });

  registerAction({
    id: "nav.tagEdits",
    label: "Tag edits",
    run: () => {
      app.view = { kind: "tagEdits" };
    },
  });
  registerAction({
    id: "tracks.rate",
    label: "Rate",
    run: async ({ trackIds, rating }) => {
      await call("tracks.setRating", { trackIds, rating });
    },
  });
  registerAction({
    id: "nav.duplicates",
    label: "Duplicates",
    run: () => {
      app.view = { kind: "duplicates" };
    },
  });
  registerAction({
    id: "library.showMatches",
    label: "Show in library",
    run: ({ query }) => {
      app.librarySearch = query;
      app.view = { kind: "library" };
    },
  });
  registerAction({
    id: "nav.tagEditor",
    label: "Edit tags",
    run: () => {
      app.view = { kind: "tagEditor" };
    },
  });
  registerAction({
    id: "nav.tagReview",
    label: "Tag edit",
    run: ({ id }) => {
      app.editOrigin = { kind: "tagEdits" };
      app.view = { kind: "tagReview", id };
    },
  });

  registerAction({
    id: "tags.edit",
    label: "Edit tags",
    run: ({ trackIds }) => {
      app.editOrigin = app.view;
      app.view = { kind: "tagEditor" };
      // Failures show in the form, which works without the values.
      void tagDraft.open(trackIds);
    },
  });
  registerAction({
    id: "tags.preview",
    label: "Preview changes",
    run: async () => {
      if (!tagDraft.changes) return;
      const started = await call("tags.preview.start", { changes: tagDraft.changes });
      tagDraft.editId = started.editId;
      app.waitingEdit = started.editId;
      app.view = { kind: "tagReview", id: started.editId };
    },
  });
  registerAction({
    id: "tags.write",
    label: "Write",
    run: async ({ id }) => {
      app.writingEdit = id;
      await call("tags.edits.write", { id });
      app.waitingEdit = null;
      if (tagDraft.editId === id) tagDraft.close();
    },
  });
  registerAction({
    id: "tags.discard",
    label: "Discard",
    run: async ({ id }) => {
      await call("tags.edits.discard", { id });
      app.waitingEdit = null;
      if (tagDraft.editId === id) tagDraft.close();
      app.view = app.editOrigin;
    },
  });
  registerAction({
    id: "tags.backToDraft",
    label: "Back to editing",
    run: async ({ id }) => {
      await call("tags.edits.discard", { id });
      app.waitingEdit = null;
      tagDraft.editId = null;
      app.view = { kind: "tagEditor" };
    },
  });
  registerAction({
    id: "tags.closeDraft",
    label: "Cancel",
    run: () => {
      tagDraft.close();
      app.view = app.editOrigin;
    },
  });
  registerAction({
    id: "tags.leaveReview",
    label: "Done",
    run: () => {
      // A preview canceled from its own screen goes back to what was being edited.
      if (app.view.kind === "tagReview" && tagDraft.editId === app.view.id) {
        tagDraft.editId = null;
        app.waitingEdit = null;
        app.view = { kind: "tagEditor" };
        return;
      }
      if (app.view.kind === "tagReview" && app.waitingEdit === app.view.id) app.waitingEdit = null;
      app.view = app.editOrigin;
    },
  });
  registerAction({
    id: "tags.undo",
    label: "Undo",
    run: async ({ id }) => {
      const started = await call("tags.edits.undo", { id });
      app.waitingEdit = started.editId;
      app.editOrigin = { kind: "tagEdits" };
      app.view = { kind: "tagReview", id: started.editId };
    },
  });

  registerAction({
    id: "library.addFolder",
    label: "Add folder",
    run: async () => {
      const path = await pickFolder("Add a music folder");
      if (!path) return;
      const root = await call("library.roots.add", { path, name: null });
      await call("library.scan.start", { roots: [root.id] });
    },
  });
  registerAction({
    id: "library.rescan",
    label: "Scan for changes",
    run: () => call("library.scan.start", { roots: null }),
  });
  registerAction({
    id: "library.cancelScan",
    label: "Cancel",
    run: async () => {
      if (app.scan) await call("jobs.cancel", { id: app.scan.id });
    },
  });
  registerAction({
    id: "library.export",
    label: "Export library",
    run: async () => {
      const path = await pickExportTarget(`umak-library-${new Date().toISOString().slice(0, 10)}.json`);
      if (!path) return;
      const summary = await call("library.export", { path });
      app.notify(
        `Exported ${countOf(summary.tracks, "track")} and ${countOf(summary.playlists, "playlist")} to ${summary.path}`,
      );
    },
  });
  registerAction({
    id: "library.chooseImport",
    label: "Import library",
    run: async () => {
      const path = await pickExportFile();
      if (!path) return;
      app.importPreview = await call("library.import.preview", { path });
      app.importReport = null;
      app.view = { kind: "import" };
    },
  });
  registerAction({
    id: "library.startImport",
    label: "Start import",
    run: async ({ path, roots }) => {
      app.importReport = null;
      await call("library.import.start", { path, roots });
    },
  });
  registerAction({
    id: "library.focusSearch",
    label: "Search",
    shortcut: "ctrl+f",
    run: () => {
      app.view = { kind: "library" };
      app.searchFocusRequests++;
    },
  });

  registerAction({
    id: "playlists.startCreate",
    label: "New playlist",
    shortcut: "ctrl+n",
    run: () => {
      app.creatingPlaylist = true;
    },
  });
  registerAction({
    id: "playlists.create",
    label: "Create playlist",
    run: async ({ name }) => {
      const playlist = await call("playlists.create", { name });
      await app.loadPlaylists();
      app.view = { kind: "playlist", id: playlist.id };
    },
  });
  registerAction({
    id: "playlists.rename",
    label: "Rename playlist",
    run: ({ id, name }) => call("playlists.rename", { id, name }),
  });
  registerAction({
    id: "playlists.delete",
    label: "Delete playlist",
    run: async ({ id }) => {
      await call("playlists.delete", { id });
      if (app.view.kind === "playlist" && app.view.id === id) {
        app.view = { kind: "library" };
      }
    },
  });
  registerAction({
    id: "playlists.addTracks",
    label: "Add to playlist",
    run: async ({ id, trackIds }) => {
      const playlist = await call("playlists.addTracks", { id, trackIds });
      app.notify(`Added ${countOf(trackIds.length, "track")} to ${playlist.name}`);
    },
  });
  registerAction({
    id: "playlists.removeItems",
    label: "Remove from playlist",
    run: ({ id, positions }) => call("playlists.removeItems", { id, positions }),
  });
  registerAction({
    id: "playlists.moveItems",
    label: "Move in playlist",
    run: ({ id, positions, before }) => call("playlists.moveItems", { id, positions, before }),
  });

  registerAction({
    id: "player.playTracks",
    label: "Play",
    run: ({ trackIds, startIndex }) => playerClient.playTracks(trackIds, startIndex),
  });
  registerAction({
    id: "player.playPlaylist",
    label: "Play playlist",
    run: ({ id, startIndex }) => playerClient.playPlaylist(id, startIndex),
  });
  registerAction({
    id: "player.toggle",
    label: "Play or pause",
    shortcut: " ",
    run: () => playerClient.control(player.status?.state === "playing" ? "pause" : "play"),
  });
  registerAction({ id: "player.stop", label: "Stop", run: () => playerClient.control("stop") });
  registerAction({
    id: "player.next",
    label: "Next",
    shortcut: "ctrl+arrowright",
    run: () => playerClient.control("next"),
  });
  registerAction({
    id: "player.previous",
    label: "Previous",
    shortcut: "ctrl+arrowleft",
    run: () => playerClient.control("previous"),
  });
  registerAction({
    id: "player.skipForward",
    label: "Skip forward",
    shortcut: "shift+arrowright",
    run: () => player.skip(SKIP_MS),
  });
  registerAction({
    id: "player.skipBack",
    label: "Skip back",
    shortcut: "shift+arrowleft",
    run: () => player.skip(-SKIP_MS),
  });
  registerAction({
    id: "player.playAlbum",
    label: "Play album",
    run: ({ trackId }) => playerClient.playAlbum(trackId),
  });
  registerAction({ id: "player.seek", label: "Seek", run: ({ ms }) => playerClient.seek(ms) });

  registerAction({
    id: "player.setVolume",
    label: "Volume",
    run: ({ volume }) => playerClient.setVolume(volume),
  });
  registerAction({
    id: "player.toggleMute",
    label: "Mute",
    shortcut: "m",
    run: () => playerClient.setMuted(!player.settings?.muted),
  });
  registerAction({
    id: "player.volumeUp",
    label: "Volume up",
    shortcut: "ctrl+arrowup",
    run: () => player.nudgeVolume(VOLUME_STEP),
  });
  registerAction({
    id: "player.volumeDown",
    label: "Volume down",
    shortcut: "ctrl+arrowdown",
    run: () => player.nudgeVolume(-VOLUME_STEP),
  });
  registerAction({
    id: "player.setGain",
    label: "ReplayGain",
    run: ({ replayGain, preampDb }) => playerClient.setGain(replayGain, preampDb),
  });

  registerAction({
    id: "player.setEq",
    label: "Equalizer",
    run: ({ enabled, bands }) => playerClient.setEq(enabled, bands),
  });
  registerAction({
    id: "player.saveEqPreset",
    label: "Save preset",
    run: async ({ name, bands }) => {
      await playerClient.saveEqPreset(name, bands);
      app.notify(`Saved the preset ${name.trim()}`);
    },
  });
  registerAction({
    id: "player.deleteEqPreset",
    label: "Delete preset",
    run: async ({ name }) => {
      await playerClient.deleteEqPreset(name);
      app.notify(`Deleted the preset ${name}`);
    },
  });

  registerAction({
    id: "queue.togglePanel",
    label: "Show or hide the queue",
    shortcut: "ctrl+q",
    run: () => app.setQueueOpen(!app.queueOpen),
  });
  registerAction({
    id: "queue.add",
    label: "Add to queue",
    run: async ({ trackIds }) => {
      await playerClient.addToQueue(trackIds);
      app.notify(`Added ${countOf(trackIds.length, "track")} to the queue`);
    },
  });
  registerAction({
    id: "queue.playNext",
    label: "Play next",
    run: async ({ trackIds }) => {
      await playerClient.playNext(trackIds);
      app.notify(`${countOf(trackIds.length, "track")} will play next`);
    },
  });
  registerAction({
    id: "queue.play",
    label: "Play this entry",
    run: ({ index }) => playerClient.playQueueEntry(index),
  });
  registerAction({
    id: "queue.remove",
    label: "Remove",
    run: ({ positions }) => playerClient.removeFromQueue(positions),
  });
  registerAction({
    id: "queue.move",
    label: "Move in queue",
    run: ({ positions, before }) => playerClient.moveInQueue(positions, before),
  });
  registerAction({ id: "queue.clear", label: "Clear", run: () => playerClient.clearQueue() });
  registerAction({
    id: "queue.save",
    label: "Save as playlist",
    run: async ({ name }) => {
      const playlist = await playerClient.saveQueue(name);
      app.notify(`Saved the queue as ${playlist.name}`);
    },
  });
}
