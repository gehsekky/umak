// Generated from the commands! list in umak-core. Do not edit.
import type { AddRoot } from "./AddRoot";
import type { AppInfo } from "./AppInfo";
import type { GetAppInfo } from "./GetAppInfo";
import type { LibraryRoot } from "./LibraryRoot";
import type { ListRoots } from "./ListRoots";
import type { RootList } from "./RootList";

export type CommandMap = {
  "app.info": { args: GetAppInfo; output: AppInfo };
  "library.roots.list": { args: ListRoots; output: RootList };
  "library.roots.add": { args: AddRoot; output: LibraryRoot };
};
