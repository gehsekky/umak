<script lang="ts">
  import { registerAppActions } from "./lib/actions/app";
  import { registerView } from "./lib/shell/regions";
  import Shell from "./lib/shell/Shell.svelte";
  import FoldersView from "./lib/views/FoldersView.svelte";
  import ConversionsView from "./lib/views/ConversionsView.svelte";
  import ConvertDialog from "./lib/views/ConvertDialog.svelte";
  import ImportView from "./lib/views/ImportView.svelte";
  import LibraryView from "./lib/views/LibraryView.svelte";
  import PlayerBar from "./lib/views/PlayerBar.svelte";
  import PlaylistView from "./lib/views/PlaylistView.svelte";
  import QueueView from "./lib/views/QueueView.svelte";
  import SettingsView from "./lib/views/SettingsView.svelte";
  import Sidebar from "./lib/views/Sidebar.svelte";
  import StatusBar from "./lib/views/StatusBar.svelte";
  import TagEditorView from "./lib/views/TagEditorView.svelte";
  import DuplicatesView from "./lib/views/DuplicatesView.svelte";
  import SmartEditorView from "./lib/views/SmartEditorView.svelte";
  import TagHistoryView from "./lib/views/TagHistoryView.svelte";
  import TagReviewView from "./lib/views/TagReviewView.svelte";

  registerAppActions();
  registerView("sidebar", { id: "navigation", component: Sidebar });
  registerView("main", { id: "library", component: LibraryView });
  registerView("main", { id: "playlist", component: PlaylistView });
  registerView("main", { id: "folders", component: FoldersView });
  registerView("main", { id: "import", component: ImportView });
  registerView("main", { id: "settings", component: SettingsView });
  registerView("main", { id: "tagEditor", component: TagEditorView });
  registerView("main", { id: "tagReview", component: TagReviewView });
  registerView("main", { id: "tagEdits", component: TagHistoryView });
  registerView("main", { id: "duplicates", component: DuplicatesView });
  registerView("main", { id: "smartEditor", component: SmartEditorView });
  registerView("main", { id: "conversions", component: ConversionsView });
  registerView("queue", { id: "queue", component: QueueView });
  registerView("transport", { id: "player", component: PlayerBar });
  registerView("status", { id: "status", component: StatusBar });
  registerView("overlay", { id: "convert", component: ConvertDialog });
</script>

<Shell />
