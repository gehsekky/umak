-- Batches of tag changes: previewed, then written, and able to be undone.
-- - An undo is a batch of its own, pointing at the batch it undoes through undoes_id.
-- - A snapshot holds what the batch's write replaces, exactly as the file held it. It is both
--   how a write knows the file is as previewed and how an undo puts it back.
-- - restore is set on an undo: the snapshot it puts back. Other batches write changes.after.

CREATE TABLE tag_edits (
    id          INTEGER PRIMARY KEY,
    public_id   TEXT NOT NULL UNIQUE,
    undoes_id   INTEGER REFERENCES tag_edits (id) ON DELETE SET NULL,
    status      TEXT NOT NULL
                CHECK (status IN ('previewing', 'ready', 'writing', 'done', 'canceled',
                                  'interrupted')),
    unchanged   INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    finished_at INTEGER
) STRICT;

CREATE INDEX idx_tag_edits_status ON tag_edits (status);
CREATE INDEX idx_tag_edits_undoes ON tag_edits (undoes_id) WHERE undoes_id IS NOT NULL;

CREATE TABLE tag_edit_items (
    edit_id  INTEGER NOT NULL REFERENCES tag_edits (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    track_id INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    status   TEXT NOT NULL
             CHECK (status IN ('pending', 'blocked', 'writing', 'written', 'failed', 'skipped',
                               'unsure')),
    message  TEXT,
    changes  TEXT NOT NULL,
    snapshot TEXT,
    restore  TEXT,
    PRIMARY KEY (edit_id, position)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_tag_edit_items_track ON tag_edit_items (track_id);
CREATE INDEX idx_tag_edit_items_status ON tag_edit_items (status);
