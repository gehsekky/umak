// The one draft the app works on.

import { call } from "../api/client";
import { TagDraft } from "./draft.svelte";

export const tagDraft = new TagDraft(
  async (trackIds) => (await call("tags.read", { trackIds })).tracks,
);
