-- Batches of conversions. Each batch, and each retry of one, is a job of its own.
-- - conversions holds what the batch was asked to do, so a retry can do it again.
-- - conversion_files holds what became of each file. source_path and output_path stay readable
--   after a track leaves the library.

CREATE TABLE conversions (
    job_id         INTEGER PRIMARY KEY REFERENCES jobs (id) ON DELETE CASCADE,
    settings       TEXT NOT NULL,
    folder         TEXT NOT NULL,
    threads        INTEGER NOT NULL,
    add_to_library INTEGER NOT NULL
) STRICT;

CREATE TABLE conversion_files (
    job_id      INTEGER NOT NULL REFERENCES conversions (job_id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    track_id    INTEGER REFERENCES tracks (id) ON DELETE SET NULL,
    source_path TEXT NOT NULL,
    output_path TEXT NOT NULL,
    status      TEXT NOT NULL
                CHECK (status IN ('pending', 'converted', 'failed', 'canceled', 'exists',
                                  'sameOutput', 'missing')),
    message     TEXT,
    left_out    TEXT,
    PRIMARY KEY (job_id, position)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_conversion_files_track ON conversion_files (track_id);
CREATE INDEX idx_conversion_files_pending ON conversion_files (status) WHERE status = 'pending';
