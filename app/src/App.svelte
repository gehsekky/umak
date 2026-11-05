<script lang="ts">
  import { registerAppActions } from "./lib/actions/app";
  import { registerView } from "./lib/shell/regions";
  import Shell from "./lib/shell/Shell.svelte";
  import FoldersView from "./lib/views/FoldersView.svelte";
  import LibraryView from "./lib/views/LibraryView.svelte";
  import PlaylistView from "./lib/views/PlaylistView.svelte";
  import Sidebar from "./lib/views/Sidebar.svelte";
  import StatusBar from "./lib/views/StatusBar.svelte";
  import TransportBar from "./lib/views/TransportBar.svelte";

  registerAppActions();
  registerView("sidebar", { id: "navigation", component: Sidebar });
  registerView("main", { id: "library", component: LibraryView });
  registerView("main", { id: "playlist", component: PlaylistView });
  registerView("main", { id: "folders", component: FoldersView });
  registerView("transport", { id: "transport", component: TransportBar });
  registerView("status", { id: "status", component: StatusBar });
</script>

<Shell />
