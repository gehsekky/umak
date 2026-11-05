// The app's actions. Buttons and shortcuts run these by ID, through the registry.

import { call } from "../api/client";
import { countOf } from "../lists/format";
import { pickFolder } from "../platform/dialog";
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
}
