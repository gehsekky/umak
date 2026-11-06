<script lang="ts">
  import { registerAppActions } from "./lib/actions/app";
  import { registerView } from "./lib/shell/regions";
  import Shell from "./lib/shell/Shell.svelte";
  import FoldersView from "./lib/views/FoldersView.svelte";
  import ImportView from "./lib/views/ImportView.svelte";
  import LibraryView from "./lib/views/LibraryView.svelte";
  import PlayerBar from "./lib/views/PlayerBar.svelte";
  import PlaylistView from "./lib/views/PlaylistView.svelte";
  import QueueView from "./lib/views/QueueView.svelte";
  import SettingsView from "./lib/views/SettingsView.svelte";
  import Sidebar from "./lib/views/Sidebar.svelte";
  import StatusBar from "./lib/views/StatusBar.svelte";

  registerAppActions();
  registerView("sidebar", { id: "navigation", component: Sidebar });
  registerView("main", { id: "library", component: LibraryView });
  registerView("main", { id: "playlist", component: PlaylistView });
  registerView("main", { id: "folders", component: FoldersView });
  registerView("main", { id: "import", component: ImportView });
  registerView("main", { id: "settings", component: SettingsView });
  registerView("queue", { id: "queue", component: QueueView });
  registerView("transport", { id: "player", component: PlayerBar });
  registerView("status", { id: "status", component: StatusBar });
</script>

<Shell />
