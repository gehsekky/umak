// The app's actions. Buttons and shortcuts run these by ID, through the registry.

import { call } from "../api/client";
import { countOf } from "../lists/format";
import { pickExportFile, pickExportTarget, pickFolder } from "../platform/dialog";
import { playerClient } from "../player/client";
import { player } from "../player/state.svelte";
import { app } from "../state/app.svelte";
import { onActionError, registerAction } from "./registry";

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
    label: "Cancel scan",
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
  registerAction({ id: "player.next", label: "Next", run: () => playerClient.control("next") });
  registerAction({
    id: "player.previous",
    label: "Previous",
    run: () => playerClient.control("previous"),
  });
  registerAction({ id: "player.seek", label: "Seek", run: ({ ms }) => playerClient.seek(ms) });

  registerAction({
    id: "player.setVolume",
    label: "Volume",
    run: ({ volume }) => playerClient.setVolume(volume),
  });
  registerAction({
    id: "player.setGain",
    label: "ReplayGain",
    run: ({ replayGain, preampDb }) => playerClient.setGain(replayGain, preampDb),
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
