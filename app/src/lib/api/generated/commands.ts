// Generated from the commands! list in umak-core. Do not edit.
import type { AddRoot } from "./AddRoot";
import type { AppInfo } from "./AppInfo";
import type { CancelJob } from "./CancelJob";
import type { CancelResult } from "./CancelResult";
import type { GetAppInfo } from "./GetAppInfo";
import type { JobList } from "./JobList";
import type { JobRef } from "./JobRef";
import type { LibraryRoot } from "./LibraryRoot";
import type { ListJobs } from "./ListJobs";
import type { ListRoots } from "./ListRoots";
import type { RootList } from "./RootList";
import type { StartScan } from "./StartScan";

export type CommandMap = {
  "app.info": { args: GetAppInfo; output: AppInfo };
  "library.roots.list": { args: ListRoots; output: RootList };
  "library.roots.add": { args: AddRoot; output: LibraryRoot };
  "library.scan.start": { args: StartScan; output: JobRef };
  "jobs.list": { args: ListJobs; output: JobList };
  "jobs.cancel": { args: CancelJob; output: CancelResult };
};
