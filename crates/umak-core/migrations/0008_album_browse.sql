-- Orders for the album grid, beside idx_albums_browse, so a page walks an index instead of
-- sorting every album.

CREATE INDEX idx_albums_title ON albums (sort_title, album_artist_sort, id);
CREATE INDEX idx_albums_year ON albums (year, album_artist_sort, sort_title, id);

-- Finds the albums of matching tracks without reading each track's row, which halves the
-- time when a search matches most of the library.
CREATE INDEX idx_tracks_album_of ON tracks (id, album_id);
