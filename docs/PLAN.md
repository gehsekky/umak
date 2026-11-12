# Umak: Project Plan

## Goals and requirements

Build a local-first music player and library manager that stays fast at about 30,000 tracks, edits tags in every common format, and serves its library to remote clients.

**Pain points this app is built to fix**

- Weak library management: slow scans, manual rescans, no duplicate handling.
- A poor player UI: hidden queues, clunky lists, no keyboard workflow.
- Dated technology: no gapless playback, no loudness normalization, no modern OS integration.
- Incomplete tagging: missing formats, multi-value fields and custom tags.

**Must-have capabilities**

- Library: folder scanning, duplicate detection, smart playlists, batch editing. File watching follows after the MVP.
- Tagging: read and write ID3v2.3/2.4, Vorbis comments, MP4 atoms, APE, RIFF/AIFF, and ASF where feasible.
- Player: one player playlist, gapless playback, ReplayGain, an equalizer and limiter, OS media controls.
- Remote access: an open API so existing client apps can connect, planned for a later phase. Navidrome covers this in the meantime.
- Conversion: transcode between formats, singly or in batches.
- Visualizations: real-time spectrum and waveform displays.
- Scale: about 30k tracks with no visible lag in browsing, search or scrolling. This is a performance target, not a design limit; library functions are built for the maximum speed the filesystem and OS allow.

## Prior art and scope

Existing projects cover parts of this plan but none covers all of it, so version one targets the unserved part: a desktop library manager, tagger and player with a customizable UI. The server is deferred, and the architecture keeps it easy to add.

| Project | Covers | Gap for us |
| --- | --- | --- |
| [Navidrome](https://www.navidrome.org) (Go, GPL 3.0) | OpenSubsonic server, built-in web UI, smart playlists, scrobbling | New themes need a source change and rebuild; no tag editing or conversion that we know of |
| [Feishin](https://github.com/jeffvli/feishin) | Desktop client for Navidrome, Jellyfin and OpenSubsonic; JSON and CSS themes; local tag editor; visualizer | A client, not a library manager |
| [Psysonic](https://github.com/Psysonic/psysonic) (Rust, Tauri, React) | Theme store, keybind remapping | Navidrome-first client |
| [Strawberry](https://flathub.org/apps/org.strawberrymusicplayer.strawberry) (C++, Qt) | Tag editing, MusicBrainz lookups, smart playlists, EQ, transcoder, bit-perfect on Linux | Dated look; Windows builds tied to a Patreon subscription |
| MusicBee, foobar2000 | Library management, tagging, conversion; skins or a UI editor | Windows only; MusicBee is closed source |
| RustMusic, Musicat (Tauri) | Players with tagging, gapless playback, smart playlists | Early stage |

Source: web search on 2026-11-03, not yet checked against each project's own site.

**Scope decisions**

- No server in version one. Navidrome on the NAS can provide remote listening with existing apps in the meantime, once the music is copied there.
- Build the desktop app first: library, tagging, player, EQ, conversion, visualizations.
- Keep the architecture server-ready (see Remote media server).
- Themes are JSON tokens plus optional CSS files, modeled on Feishin's approach.
- Licensing: Navidrome and Feishin are GPL. Studying their designs is fine; copying their code would make this app GPL too. Umak is MIT-licensed for now and developed in a public repository, so dependencies must stay compatible with that.

## Platforms and tech stack

Use Tauri with a Rust core and a web-tech UI. One codebase targets Windows and Linux first, with macOS and a web client possible later.

| Layer | Choice | Why |
| --- | --- | --- |
| Shell | Tauri | Small installs, native OS integration, web UI |
| Core language | Rust | Fast scanning, safe concurrency, strong audio crates |
| Database | SQLite via rusqlite, FTS5 for search | Handles 30k tracks easily, runs embedded |
| Tags | Lofty | Broad format coverage, read and write |
| Decode | Symphonia | Pure-Rust decoders for common formats; coverage to verify (see Risks) |
| Output | cpal | Cross-platform audio output |
| Conversion | FFmpeg (bundled sidecar) | Covers every codec and container |
| Server | axum | HTTP layer for OpenSubsonic (later phase) |
| UI | Svelte 5, TypeScript, Vite | Faster and lighter than React in published benchmarks, with an active community; virtualized lists keep large views fast |
| OS controls | SMTC (Windows), MPRIS (Linux) | Media keys and lock-screen controls |

**Platform order**

1. Windows and Linux desktop through Tauri.
2. Headless server mode (later phase): the same core runs without a UI on a NAS or home server.
3. Web client later, and only if the existing OpenSubsonic clients don't satisfy; it would reuse the UI against the server API.
4. macOS when needed; Tauri supports it, so it costs little.

**Deferred decisions**

- Exclusive-mode or bit-perfect output on Windows (WASAPI exclusive) needs a platform-specific output path beyond cpal defaults.
- Mobile apps are not planned; Subsonic-compatible mobile clients can connect to Navidrome if it is set up, and to our own server once it exists.

## Architecture

A single Rust core owns the library, player, conversion and visualizer. The desktop UI and remote clients both reach it through one command layer, so each feature is built once.

```
Desktop UI (Tauri webview)            Remote clients (Symfonium, Tempo, Feishin)
        |  commands and events                 |  OpenSubsonic over HTTP (later)
        v                                      v
+------------------------------ Rust core ----------------------------------+
|  Command and event layer          OpenSubsonic server (later phase)        |
|              \_____________________________/                              |
|  Library and tags | Player engine | Conversion jobs | Visualizer           |
+----------------------------------------------------------------------------+
        |  reads and writes                    |  reads files, writes tags
        v                                      v
  SQLite database                        Music files on disk
```

The UI sends commands and receives state events; remote clients will use the HTTP server once it is added. Both read the same SQLite library, and music files change only when you edit tags or ask to rename or move them.

**Suggested Rust workspace**

- `umak-core`: library, database, scanner, tag model.
- `umak-audio`: decoding, output, playlist and playback state, effects and plugin host, FFT tap.
- `umak-play`: a thin test tool around `umak-audio` that takes typed player commands over stdin and reports events over stdout.
- `umak-scan`: a headless binary that runs the core without a UI.
- `umak-convert`: FFmpeg job runner.
- `umak-server`: axum, OpenSubsonic and the management API (later phase).
- `umak-app`: the Tauri shell that wires the UI to the core.

**Process model**

The core runs the audio engine on its own real-time thread and the scanner on a worker pool. The UI never touches audio or the database directly; it only sends commands and renders state. The same core compiles to a headless binary for server-only use.

In phase 1 the audio engine ran in the separate `umak-play` process. Since phase 2 it runs in-process behind the `PlayerBackend` interface, on its own threads, and the UI did not change. Its events are handled on a separate thread, because the handlers use the database and must never hold up decoding. `umak-play` stays as a test tool. The player protocol lives in `umak-audio` without its `engine` feature, and `umak-core` turns the engine on only through its own `engine` feature, so headless tools build no audio code.

**Repository layout: one monorepo**

The project is a single Git repository containing a Cargo workspace, not separate repositories. The crates change together, and one repository gives one commit, one build and one test run. A crate is split out later only if it becomes useful on its own.

```
umak/
  Cargo.toml        workspace
  crates/
    umak-core/      library, database, scanner, tag model
    umak-audio/     engine, playlist, EQ, plugin host
    umak-play/      player test tool around the engine
    umak-scan/      headless scan and benchmark tool
    umak-convert/   FFmpeg job runner
    umak-server/    OpenSubsonic (later)
  app/              Tauri shell (umak-app) and web UI
  docs/             PLAN.md and architecture notes
  CLAUDE.md
```

The rule that protects the design: the crates under `crates/` never depend on Tauri or UI code. Only `app/` depends on them.

**How the UI talks to the core**

A Tauri app is a Rust process plus a webview that renders the UI, connected by built-in IPC. Four mechanisms are used:

- Commands: the UI calls one IPC entry point with a command name and JSON arguments, and `umak-core` runs the matching typed command and returns a JSON result. This is the "send a command" half of the command layer. TypeScript types are generated from the Rust types, and a test fails if the committed copies go stale.
- Events: Rust emits named events such as playback state or scan progress, and the UI subscribes. This avoids polling.
- Channels: ordered streams for high-frequency or large data, such as visualizer frames and long result sets.
- A custom URI scheme: binary data like album art is served through it and used as an ordinary image source, never through JSON.

Shared objects, such as the database pool and the audio engine handle, live in Tauri's managed state. Tauri 2 requires a capabilities file that grants each window access to specific commands. The traffic stays on the machine and uses no network port.

The audio engine runs on its own real-time thread. Commands only send it messages, and it reports back as events, so the UI can never stall playback. Tauri IPC is the first adapter of the command layer; HTTP is the later one.

## Library and tagging

The library engine is the heart of the app. It never modifies music files unless you edit tags or ask to rename or move them, and the database can always be rebuilt from the files.

**Scanner**

- Walk folders and read tags with Lofty, tuning concurrency per storage device. Parallel reads can slow a hard drive, so benchmark rather than assume more threads help. On the hard drive holding the collection, 4 threads was fastest (12.8 ms per cold MP3, against 17.7 with 1 thread and 13.3 with 8).
- Ignore playlist files (`.m3u`, `.m3u8`). Playlists are created and stored in the app.
- Skip unchanged files by comparing modified time and size, so rescans are near-instant.
- Changes made outside the app appear at the next rescan, which the person starts. A file watcher comes after the MVP (see "After the MVP").
- Detect moved files by a fingerprint of sampled audio data, which tag edits don't change and which never needs a full read of a large file. Track IDs, ratings, play counts and playlist entries survive a reorganization. The fingerprint hashes the audio length and the first 64 KiB of audio: that costs about 1 ms per file, while each extra sample adds a disk seek of about 7 ms. Across 21,387 real files, all 84 groups sharing a fingerprint held byte-identical audio.
- Mark files that disappear as missing instead of deleting their rows, so playlist entries survive until the file returns.

**Tag model**

- Tags are the source of truth. Values are stored as written and never split or derived; a value that needs interpreting is a tagging problem to report and fix in the file.
- Treat artists, genres and composers as multi-value fields, using only the separate values a file actually stores.
- Separate album artist from track artist, and support compilation flags.
- Keep disc and track numbers, year and original date, ReplayGain values, and MusicBrainz IDs.
- Preserve unknown and custom tags in a raw key-value store, so nothing is lost on write.
- Embedded art is extracted once to a thumbnail cache on disk.

**Album grouping and compilations**

The scanner assigns each track to an album using the first rule that applies. Grouping only affects the database and never writes to music files.

1. MusicBrainz album ID, when present.
2. Album artist tag, title and folder.
3. Compilation flag with no album artist: album artist becomes Various Artists.
4. No usable tags: tracks with the same album title in the same folder are judged together. An artist on at least half the tracks becomes the album artist; otherwise the album is an inferred compilation under Various Artists.

- Folders named like `CD1` or `Disc 2` count as their parent folder when grouping.
- Spelling variants such as "VA" and "Various" map to the reserved Various Artists row.
- Each track keeps its own artists, so compilations appear once under Various Artists and in an "Appears on" section on each artist's page.
- The 50% threshold is a starting guess. `umak-scan` reports how many albums took each rule and lists inferred compilations, so it can be tuned against real files.

**Tag editing**

- Batch edit across any selection, with a preview of every change before it is written.
- Write back in each file's native format; never convert tag formats silently.
- Write safely: temp file, verify, then replace, so a crash cannot corrupt a file. The check reads the edited copy back: the edited fields must hold the new values, every other tag item must be unchanged, and a hash of the whole audio must match.
- ID3v2 tags are edited frame by frame, so frames that are not being edited keep their exact bytes. The tag library is not used for this, because when writing ID3v2.3 it discards frames such as TSOP and TSOT and writes URL frames in the wrong encoding. It still reads all formats and writes FLAC and MP4.
- An ID3v1 or APE tag beside the main tag is kept in step, so another player does not find the old values there.
- Undo history for tag changes. What a write replaces is saved exactly as the file held it, not through the tag model, which would turn "2001-05-03" into 2001 and lose the rest. An undo is a batch like any other, with its own preview, and leaves alone any file changed since.
- A write goes ahead only if what it replaces is what the preview saw.
- The tag editor starts from what the files hold, read a page at a time behind a form that is already usable, because the library fills gaps for display: a missing title shows the file's name. A field the person did not touch is never written. Each value has its own box, so nothing is split on a separator.

**Cleanup tools**

- Duplicate finder by fingerprint and by tag match. It only reports: the app never deletes a music file, so copies are removed in a file manager and a rescan follows. A tag match needs the same title, artist, album and track number, because the same song played twice in a concert otherwise looks like a copy: in the real library that was 194 of 217 groups.
- Rename and move files from tag patterns.
- Identify untagged files with AcoustID fingerprinting and MusicBrainz lookups.
- Smart playlists defined by rules, such as FLAC, rated 4 or higher, not played in six months. A rule is stored and worked out each time, so the list follows the library. Playing one copies the list as it stands into the player playlist.

## Database design

SQLite in WAL mode, accessed through rusqlite, with FTS5 for search. At 30,000 tracks the database is small; lag comes from schema and query design, not the engine.

| Table | Holds | Key notes |
| --- | --- | --- |
| library_roots | Music folders | Name and path on this machine |
| users | Accounts | A single default user until the server adds more |
| tracks | One row per file | Stable ID, root-relative path, size, mtime, duration, codec, bitrate, fingerprint, missing flag, plus sort and display copies so browsing needs no joins |
| track_user_data | Per-user track data | User id, track id, rating (0 to 10, shown as half-stars), play count |
| albums | Album entities | Grouping key, album artist, year, compilation source (none, tagged or inferred), art reference |
| artists | Artist entities | Name, sort name, MusicBrainz ID; a reserved row for Various Artists |
| genres | Genre names | Referenced by track_genres |
| track_artists, album_artists, track_genres | Join tables | Multi-value tags, with role (artist, composer) on track_artists |
| raw_tags | Unmapped tags | Preserves custom and unknown fields per track |
| playlists, playlist_items | Saved playlists and the player playlist | Per-user, ordered items; smart playlists store a rule; a reserved row per user holds the player playlist and position |
| play_history | Listens | User id and timestamp per play, feeds scrobbling and smart rules. A play is half a track or four minutes of actual listening, the rule scrobbling services use |
| tracks_fts | FTS5 index | Title, artist, album, genre for instant search |
| jobs | Conversion and scan jobs | Status, progress, errors |
| settings | App settings | Key and JSON value, with typed access in Rust |

**Performance rules**

- Index sort and filter columns, plus composite indexes such as (album_artist, album, disc, track).
- Enable WAL so the UI reads while the scanner writes.
- Batch scanner inserts into transactions of a few thousand rows.
- Page results by position and virtualize lists in the UI. A virtualized list must jump to any row when its scrollbar is dragged, which keyset pagination cannot do. An offset page costs under 2 ms at the 95th percentile on 21,387 tracks, because it walks an index without reading table rows.
- Search walks the sort index and stops once a page is full, instead of sorting every match. That took the 95th percentile from 11 ms to under 3 ms.
- Cache resized album art on disk, not in the database.
- Use one writer connection and a small pool of readers.
- Design for the maximum speed available, not just for the target below: the remaining cost of any library operation should be filesystem and OS overhead, not our code.

**Targets to verify with a 30k-track test library**

- Search-as-you-type returns within a few milliseconds per keystroke.
- Opening any list view shows results with no perceptible delay.
- A rescan with no changes finishes in seconds.
- The database stays around 40 MB, as measured at 30k tracks.

Phase 1 results are in `docs/benchmarks-phase1.md`.

**Where the database lives**

- On the local disk of whichever machine runs the app, in the OS app-data folder that Tauri provides. The thumbnail and waveform caches sit beside it.
- Never on the NAS share and never in the music folder. SQLite file locking is unreliable over network filesystems, and WAL mode requires every connection to be on the same machine.
- The music files are on a local hard drive for now, and the first library roots are the collection's Albums and phish folders. The database stores paths relative to a library root, so the files can move to the NAS later.
- When the server is added later, the database lives on the NAS's own local disk (an SSD volume if available), and only the server process opens it. Desktop apps then use the API.
- Moving to the server later means copying the database and pointing it at the music folder. Stable IDs and relative paths mean no re-import.

**Backups**

The database can be rebuilt from the files, except for ratings, play counts, play history, playlists and settings. Back those up with SQLite's online backup API on a schedule, writing dated copies to a different location than the database. Also support exporting playlists and ratings to a plain file as a second safety net. The database is the only home for playlists, because playlist files are not imported.

**Export and import between installations**

- Export writes one versioned JSON file holding library roots, each track's stable ID, root-relative path and fingerprint, per-user data, playlists and settings. It leaves out tags and caches, which are rebuilt from the files.
- Import on another installation asks where each root lives on that machine, then matches tracks by stable ID, then relative path, then fingerprint.
- Tracks with no matching file are kept and marked missing, and import reports them. They show the file name as their title until a later scan finds the file and reads its tags. Existing data is kept on a conflict, unless the imported field is the only value.
- A track an import finds for the first time takes its exported ID and added date, so IDs stay stable across installations. A track already in the library keeps its own. Importing the same file again changes nothing.
- The format carries a version number, and new versions of the app can read older files.

## Player and playlist model

The player has exactly one playlist, the player playlist, and playback always reads from it. Every way of playing something is an operation on that list.

| User action | What happens to the player playlist |
| --- | --- |
| Play a single song | Replace the list with that one song, then start playback |
| Play an existing playlist | Load the playlist's tracks into the list, then start playback |
| Play an album, artist or search result | Load those tracks into the list, then start playback |
| Add to queue | Append the track or tracks to the end of the list |
| Play next | Insert after the current track |
| Press Play with nothing loaded | Resume the last session's list and position |
| Remove the current track | The track that followed it takes over, or playback stops if none did |
| Clear the list | Playback stops |

The player playlist is persistent and editable: reorder, remove, clear, or save it as a regular playlist. Loading a saved playlist copies its tracks, so edits to the player list never change the saved one unless you save back.

**Playback engine (Rust)**

- Decode with Symphonia on a dedicated thread, output through cpal. Stream rather than load whole files, since single tracks can reach hundreds of megabytes.
- Gapless playback by pre-decoding the next track and joining at the sample boundary.
- Optional crossfade, ReplayGain and EBU R128 loudness normalization, and a gain-free bit-perfect mode.
- Shuffle and repeat modes live in the engine, so the UI cannot drift from what actually plays.
- A single source of truth: the UI sends commands and subscribes to state events.

**System integration**

- Media keys and lock-screen controls through SMTC on Windows and MPRIS on Linux.
- Scrobbling to ListenBrainz and Last.fm, driven by play history.
- Synced lyrics from LRC files or embedded tags.

## Equalizer and DSP

The engine includes an equalizer as one stage of a fixed signal chain, so every effect is applied once, in a known order, on the audio thread.

**Signal chain**

1. Decode (Symphonia).
2. Resample to the output rate if needed (rubato).
3. Preamp and ReplayGain.
4. Equalizer.
5. Plugin slots: the user's ordered chain of effect plugins (see Plugin system and effects).
6. Limiter, to catch peaks created by boosted bands and plugins.
7. Dither when reducing bit depth.
8. Output (cpal). The visualizer tap sits here, so the display matches what you hear.

**Equalizer design**

- Each band is a biquad filter: peaking, low shelf, high shelf, high-pass or low-pass.
- Offer a 10-band graphic EQ and a parametric EQ (frequency, gain, Q per band) built from the same filters.
- Process in 32-bit or 64-bit float internally.
- Reduce the preamp automatically when bands are boosted, to avoid clipping.
- Send new filter coefficients to the audio thread without locks, and smooth changes to avoid clicks.
- Bit-perfect mode bypasses the whole DSP chain, and the UI says so.

**Useful crates:** `biquad` for filters, `fundsp` and `dasp` for general DSP, `rubato` for resampling, `rustfft` for FFT-based filtering.

**Features to add**

- Saved presets, including per-device presets.
- Import of headphone correction profiles such as AutoEq.
- An optional linear-phase mode using FFT filtering, at the cost of added latency.

## Plugin system and effects

The engine hosts third-party effects through plugin slots in the signal chain. Built-in effects use the same internal interface, so plugin support is an adapter on top of it, not a rewrite.

**Plugin kinds**

- Effect plugins process samples in the chain: EQ, compressor, reverb, crossfeed, tube, tape and vinyl saturation, speaker and room correction.
- Source plugins (later) decode formats by emulation, such as chiptune, SID and tracker music.

**Formats**

| Format | License | Plan |
| --- | --- | --- |
| CLAP | MIT | First. Host it through the clack crate |
| VST3 | MIT (SDK 3.8.0, October 2025) | Second. Likely needs bindings or a C++ shim; to be verified |
| LV2 | ISC / GPL | Later, only if Linux users ask |
| WAM | MIT | Only for a future web client |
| AU, AAX | Proprietary | Skip; AU only if macOS is added |

**Hosting rules**

- Run untrusted plugins in a separate process with shared-memory audio buffers, so a crash mutes that plugin, not playback. The clack documentation itself recommends extra process isolation.
- Let users build an ordered chain with bypass, wet/dry mix, presets, and per-device or per-genre chains.
- Report and compensate plugin latency, so seek position, the visualizer and gapless joins stay correct.
- Negotiate sample rate, channel layout and float format with each plugin.
- Expose parameters as data so the UI can draw generic controls. Plugins with their own GUI open in separate floating windows.
- Bit-perfect mode bypasses all plugins.
- Tell users that third-party plugins run native code. The future server never loads plugins on behalf of remote users.

**Amp and gear emulation**

From cheapest to most realistic:

1. Saturation and waveshaping with oversampling, for tube and transformer warmth.
2. Convolution with impulse response files, for cabinets, headphones and rooms, using partitioned FFT.
3. Circuit modeling, such as wave digital filters, for faithful analog designs.
4. Neural amp models, in the style of Neural Amp Modeler. Licensing and Rust integration options still need checking.

Two caveats. Guitar amp models expect a guitar signal, so on full mixes the better targets are hi-fi character: tube warmth, headphone correction, tape and vinyl, speaker cabinets. And the result is an interpretation, not a restoration, so it must toggle on and off for easy A/B comparison.

Playback has no live-monitoring deadline, so we can accept added latency, heavy oversampling and ahead-of-time processing.

**Staging**

1. With the player phase: define the internal effect interface and ship the EQ and limiter on it.
2. After phase 5: add the CLAP host with out-of-process isolation.
3. Then: add VST3 hosting, a built-in convolution (IR) effect and a saturation stage.
4. Later: add a neural model loader and the source-plugin interface for emulated formats.

## Remote media server (planned for later)

The server is deferred past version one. When it is built, implement OpenSubsonic, the open extension of the Subsonic API, so existing clients such as Symfonium, Tempo, Feishin and Supersonic work on day one. Navidrome and Gonic already serve the same ecosystem.

**Server design**

- axum HTTP server inside the same Rust core, so the desktop app and the server share one library.
- Run embedded (desktop app serves while open) or headless (a daemon on a NAS or home server).
- Endpoints for browsing, search, streaming, cover art, playlists, ratings, scrobbling and lyrics.
- Range requests for seeking, plus on-the-fly transcoding to a client-requested format and bitrate.
- Token and salt authentication per the spec, with per-user accounts and TLS through a reverse proxy.

**Beyond the spec: a private management API**

Subsonic is mostly read-oriented, so tag editing, duplicate cleanup, library settings and conversion jobs live in a separate authenticated API. The desktop UI reaches the same operations through the command layer over IPC; this API exposes them to remote tools, such as a future web admin page.

**Design principle**

The desktop UI talks to the core through the same command layer the server uses, so there is one interface to build and test instead of two.

**Server-ready design rules (apply from phase 1)**

1. One command layer. Every action (play, search, edit tags, convert) is a typed request or a state event, independent of transport. Tauri IPC is the first adapter; HTTP is the later one.
2. The core crate has no Tauri or UI dependency. A small headless binary that runs the core without a UI is built in phase 1 to keep this honest.
3. Stable opaque IDs for tracks, albums and artists, never file paths. OpenSubsonic clients cache these IDs.
4. Paths are stored relative to a library root. The same database then works whether files are at a Windows network share or a NAS volume path.
5. Per-user data (ratings, play history, playlists) carries a user id from day one, with a single default user. Adding accounts later needs no migration.
6. Playback reads from a source abstraction: a local file now, an HTTP stream later. This also lets the desktop app play from a remote server.
7. The transcoder is a reusable service built in the conversion phase, so the server can reuse it for streaming.
8. Long operations are cancelable jobs that report progress as events, so any client can show them.
9. Library reads run on a separate reader pool from writes, so concurrent clients never block the scanner.

## Agent and smart-home control (nice to have)

An AI agent or home hub could run Umak as one more client of the command layer, without screen control. This is not scheduled. It is deferred because it would not make development or testing faster: core logic is tested directly through the command layer, and the app is tested end to end through its debug port.

**What it needs**

- A background service that runs the core without a window, with the desktop app as one of its clients. This shares its work with the headless server mode.
- A local API over HTTP, with a live event stream, generated from the `commands!` list and listening only on this machine by default.
- A description of each command at runtime: its name, purpose and input and output formats.
- An MCP server on top of the API, and a Home Assistant integration if that becomes the hub.
- Per-client tokens and permissions, with reading and playback allowed by default and file changes needing separate permission. Bulk changes preview first and can be undone, and every action is logged with its client.

**Missing controls**

Volume, output device choice, queue editing, shuffle and repeat, browsing by album, artist and genre, filtered search, reading and setting ratings and play counts, and removing library folders. Some are planned for other phases. An agent needs each one as a command.

**Rooms**

Umak has one player and one output. In order of size, the options are one zone, one player per local sound device, and streaming to devices such as Sonos or Chromecast.

**Keep these open when building other phases**

- Keep the player behind the `PlayerBackend` interface when the engine moves in-process in phase 2, so it can move to a service later without changing callers.
- Add new player state as commands and events, not as UI-only state.

## Conversion and visualization

**Conversion**

- Bundle FFmpeg as a sidecar process and drive it from a job queue, so conversions never block playback or the UI.
- Presets for common targets (FLAC, MP3, AAC, Opus), plus custom codec, bitrate and sample-rate settings.
- Carry tags and embedded art across, and optionally apply ReplayGain during conversion.
- Batch jobs with progress, cancel and retry, stored in the jobs table.
- Output to a chosen folder, with an option to add converted files to the library.
- The same transcoder backs the server's on-the-fly streaming.

**Visualization**

- Tap the audio at the output stage and run an FFT in Rust, then send spectrum frames to the UI at a steady frame rate.
- Start with a spectrum analyzer, an oscilloscope and a waveform seek bar; waveforms are pre-computed once per track and cached.
- Render in the UI with Canvas or WebGL, and keep rendering off the audio thread so playback never glitches.
- Later option: Butterchurn gives Milkdrop-style presets in the web UI.
- Visualizers are plugins behind a small interface: they receive frames and draw to a canvas.

## UI plan

The interface is a thin, fast layer over the core. Every list is virtualized, and every action is reachable from the keyboard.

| View | Purpose |
| --- | --- |
| Player bar | Transport, seek bar with waveform, volume, now-playing art |
| Queue panel | The player playlist, always visible, drag to reorder |
| Library browser | Album grid and track list, with customizable columns and sort |
| Search | Instant results across tracks, albums and artists |
| Tag editor | Batch edit with a preview of changes before writing |
| Playlists | Manual and smart playlists, with the rule editor |
| Conversion | Job queue with progress |
| Visualizer | Full-screen or docked spectrum and waveform views |
| Settings | Library folders, server, audio output, plugins |

**UX principles**

- A command palette (Ctrl+K) reaches any action or library item.
- The queue is a first-class panel, not a hidden playlist.
- Context menus offer the same verbs everywhere: play, play next, add to queue, edit tags, convert.
- Light and dark themes, plus user-defined themes through CSS variables.
- Responsive layout, so the same UI can later serve a web client.

## Extension points

Each phase should plug into the last without a large refactor. Features arrive in their phase, but the seam a feature will plug into is built when the surrounding code is first written.

| Later feature | Seam built from phase 1 | Not built until its phase |
| --- | --- | --- |
| Theming | Every color, font and spacing value comes from one token set exposed as CSS variables, through one theme loader. A default theme and a small test theme prove it works | Theme picker, user themes, custom CSS files |
| Full player UI | The UI talks to one player client interface, using typed commands and state events | Player bar, queue panel, waveform seek bar |
| Gapless, ReplayGain, crossfade | The playlist, decoder and output are separate stages, and decoding reads from a source interface | The features themselves |
| EQ and plugins | A fixed signal chain with an effect interface, empty in phase 1 | Filters, limiter, plugin hosting |
| Visualizers | A tap point at the output stage | FFT and rendering |
| Scrobbling | Plays are recorded by the services' rule into an append-only play history, so a sender can keep a cursor | The sender, deferred from phase 5 |
| Command palette, keybindings, context menus | UI actions live in one registry, each with an ID, a label and a handler | Palette UI, remapping |
| Layout changes | Views register into named regions of the app shell | Docking, customizable layouts |
| Tag editing | The tag model covers multi-value fields and raw tags | Tag writing and undo |
| File watching | The scanner processes a set of changed paths, so a watcher can feed it | The watcher, after the MVP |
| Conversion, long tasks | Long operations are cancelable jobs that report progress | FFmpeg runner |
| Later settings | One typed, versioned settings store, built in the settings round after phase 5 | Most of the settings |
| Later schema and export changes | Numbered migrations and a versioned export format | The changes |
| Server | The server-ready design rules | axum and OpenSubsonic |

A seam belongs here only if it costs little now and would be expensive to add later, and phase 1 code or a test must exercise it. Anything speculative beyond that waits for its phase.

## Development environment and Claude Code workflow

**Working name**

Umak is the working name. It is the Yale-style romanization of the Korean word for music; the official romanization is Eumak. In November 2026 the name was unclaimed on crates.io and npm. Trademark searches have not been done, so settle the name before a packaged release. Crates carry the `umak-` prefix, so a rename is mechanical.

**Where to work**

- The main checkout is on the Windows side. The main target is Windows: WASAPI audio, SMTC media controls, the Windows webview and the NAS share all behave differently inside WSL.
- Keep a second clone inside WSL's Ubuntu filesystem (under `~/`) for building and testing the Linux target and the headless core. Do not work on the Windows checkout from `/mnt/c`; it is slow in both directions.
- Treat a remote repository (GitHub or similar) as the source of truth, with both checkouts as clones.
- Add a `.gitattributes` file at the start with `* text=auto eol=lf` to avoid line-ending problems between the two systems.
- Install Rust through rustup with the MSVC build tools, plus Node. Check Tauri's prerequisites page for the exact requirements.
- The 30k-track test library is the real Albums and phish folders (21,387 tracks in November 2026), scanned read-only, plus about 8,600 synthetic tracks kept outside the music folder. Tag-writing tests run on copies. Machine-specific paths live in the gitignored `CLAUDE.local.md`.
- The NAS holds no music yet. To use Navidrome for remote listening before our server exists, copy the music there first.

**Using Claude Code**

- This plan becomes `docs/PLAN.md` in the repository.
- `CLAUDE.md` at the repository root stays short, under about 200 lines, and refers to `docs/PLAN.md` by path instead of importing it, so the full plan loads only when needed.
- `CLAUDE.md` holds the hard rules: crates never depend on Tauri or UI code; one transport-neutral command layer; opaque IDs, never paths; library-root-relative paths; a user id on per-user data; the audio thread never blocks, allocates or takes locks; music files change only through the safe tag-write path (temp file, verify, replace) or a user-requested rename or move.
- `CLAUDE.md` also holds the build and test commands, the crate layout, and the current phase.
- Work one phase at a time. Each phase ends with the exit criteria listed under Milestones.

**Phase 1 brief**

Phase 1 builds the basic library features and a lightweight player, plus the seams listed under Extension points.

1. Cargo workspace with `umak-core`, `umak-audio`, `umak-play` and `umak-scan`, plus the Tauri and Svelte shell in `app/`. `umak-convert` and `umak-server` wait for their phases.
2. SQLite schema with migrations, covering every table under Database design.
3. A command layer with typed commands and events, independent of transport, with TypeScript types generated from Rust.
4. Library import: choose folders, then scan with Lofty, tuned concurrency, sampled fingerprints, missing-file handling and a skip for unchanged files by modified time and size. Check early whether the fingerprint's extra disk seeks dominate first-import time.
5. `umak-scan`, a headless binary that scans a folder and prints statistics, proving the core runs without the UI.
6. Library UI: a virtualized track list, search, and playlists that can be created, edited and deleted.
7. `umak-play` and a minimal engine: streaming decode with Symphonia, resampling to the output rate, cpal output, and play, pause, stop, next, previous and seek. Playing a song or playlist replaces the player playlist. The app shows only minimal transport controls.
8. Export and import between installations.
9. A benchmark on the real library's I/O and a 30k-track database, including cold-cache runs and the raw cost of listing and reading files, so time in our code is separated from filesystem and OS overhead.

Exit criteria:

- Library import works on the real library, with results reported against raw disk cost.
- At 30k tracks, search per keystroke and fetching a page of a list each take 10 ms or less at the 95th percentile in the core.
- A rescan with no changes takes 5 seconds or less with a warm cache.
- Browsing and scrolling show no visible lag.
- An export imported into a clean second installation restores its playlists and per-user data, and reports any missing files.
- Single songs and playlists play through `umak-play` in MP3, FLAC and M4A, including 24-bit/192 kHz files.
- A test theme can restyle the app by changing tokens only.

**Phase 2 brief**

Goal: the real player. One list, gapless, at a consistent volume, with an EQ, and controls in the app. Build order:

1. In-process engine behind `PlayerBackend`, replacing the `umak-play` process. The UI does not change.
2. Gapless playback: decode the next track early and join at the sample boundary, honoring encoder delay and padding. The position and current track change exactly at the join.
3. Queue operations and the queue panel: add to queue, play next, reorder, remove, clear, and save as a playlist. The engine takes edits to the list without restarting playback.
4. Volume as a saved setting, then preamp and ReplayGain from the stored tag values.
5. EQ and limiter as effects in the fixed signal chain: a 10-band graphic EQ with saved presets, new coefficients sent to the audio thread without locks, and changes smoothed to avoid clicks.
6. The player bar: transport, seek, volume, and now-playing text.

Gapless results, November 2026:

- Encoder silence comes off using the LAME tag for MP3 and the iTunes tag for AAC in MP4. In a sample of the library, 397 of 400 tagged MP3s and all 25 sampled tagged AAC files decode to the exact length their tags state. FLAC, ALAC and WAV hold no encoder silence.
- About 4,300 of 15,000 MP3s and 27 of 203 AAC files have no such tag, and a LAME tag with a bad checksum is ignored. These play with their silence, since nothing says how much there is.
- Tracks at the same rate share one resampler, so a join through it matches the unsplit audio to within 0.001 of full scale. At the output rate the join is identical to the unsplit audio, sample for sample.
- Across 12 real joins in live recordings that run across tracks, in MP3 and FLAC, the audio has at most 1 silent frame near the join and no step beyond what the music itself has.

Edits reach the engine as the new list plus where each entry came from, so the engine maps its own position and an edit cannot race a track change. Playback is untouched unless the edit changes audio already queued, which can only be discarded along with the queued part of the current track, so that track then resumes from where it was. Edits on a 30,000-entry list take 28 to 69 ms.

ReplayGain results, November 2026: gain changes on the audio thread at the exact frame a track starts, through a lock-free queue of track levels, so settings apply at once and joins stay sample-accurate. Off is the default, because only 1,395 of 21,387 tracks in the library (6.5%) have ReplayGain values and they average about -5 dB, so turning it on leaves untagged tracks louder than tagged ones. Analyzing the library with EBU R128 would give every track a value.

The fixed chain runs on the audio thread as: ReplayGain and preamp, equalizer, plugin slots, limiter, then volume. The limiter has no lookahead, so it adds no latency and does not hold back the end of a track; it reacts within the frame that goes over and recovers over about 100 ms. With ReplayGain off, the equalizer flat or off, and no plugins, both stages are bypassed and samples are left exactly as decoded.

Keyboard: Space plays or pauses, Ctrl+Left and Ctrl+Right change track, Shift+Left and Shift+Right skip 5 seconds, Ctrl+Up and Ctrl+Down change the volume, M mutes, and Ctrl+Q shows or hides the queue. In the queue, Delete removes and Alt+Up and Alt+Down move. Arrow chords stay with text boxes and sliders when they hold focus.

Not in this phase unless asked for: crossfade, EBU R128 analysis, parametric EQ, AutoEq import, linear-phase mode, device selection, shuffle and repeat, album art, a waveform seek bar, and WASAPI exclusive output.

Exit criteria:

- Single songs, albums and saved playlists play through one list.
- Joins are gapless: captured output across a join has no added or missing samples, including MP3 with encoder delay and across sample rates.
- ReplayGain and the EQ change the output by the expected amount in measured tests, and the limiter keeps boosted audio under full scale.
- The audio thread still never allocates, with the EQ and limiter running.
- No underruns during a large import, checked with the health log.
- A listening check confirms it sounds right.

**Phase 3 brief**

Goal: fix the library from inside the app. Tag edits are safe and reversible, and duplicates and rule-based playlists are easy. Build order:

1. The safe tag writer, for MP3, FLAC and M4A, tested against a corpus of copies of real files.
2. Edit planning: a preview of every change before it is written, batch writes as a cancelable job, library updates after each write, and undo history.
3. The tag editor UI: batch editing across any selection, with the preview.
4. The duplicate finder, by fingerprint and by tag match.
5. Ratings and play counts, recorded as they happen, then smart playlists with a rule editor.

Not in this phase unless asked for: the file watcher, which moved to after the MVP, renaming and moving files from tag patterns, AcoustID and MusicBrainz lookups, embedded art editing, and loudness analysis.

Exit criteria:

- All target tag formats read and write cleanly: MP3 with ID3v2.3 and ID3v2.4, FLAC, and AAC and ALAC in M4A, checked against copies of real files by a comparison independent of the tag library.
- A batch edit shows every change first, can be canceled, and can be undone.
- A failed write leaves the file exactly as it was.
- Ratings and play counts survive an export and import.

**Phase 4 brief**

Goal: convert files in batches without disturbing playback, with tags and art carried across exactly. Build order:

1. `umak-convert`: find FFmpeg (beside the app first, then on the path), check its version and encoders, and run one conversion as a child process with progress read from FFmpeg and cancel by ending the process. No Tauri, like the other crates.
2. Presets and output: FLAC, MP3 (V0 and 320 kbps), AAC (256 kbps) and Opus (160 kbps), plus custom codec, bitrate, sample rate and bit depth. Output goes to a chosen folder, mirroring each file's folders under its library root, and never over a source file.
3. Tags and art: FFmpeg writes audio only, and the core's tag writer then copies the source's tags and embedded art onto the new file, so values arrive as written instead of through FFmpeg's metadata mapping. Fields the target format cannot hold are reported, not dropped silently.
4. Batch jobs: a conversion job in the jobs table, several files at once within a CPU limit, progress, cancel and retry of failed files. Canceling or a failure leaves no partial files. An option adds the converted files to the library.
5. The UI: Convert as an action on any selection, a dialog for preset, folder and options, and a jobs panel with progress.

Not in this phase unless asked for: applying ReplayGain during conversion, converting in place (replacing originals), naming output from tag patterns, and on-the-fly transcoding for the server.

Exit criteria:

- A batch converts to each preset with tags and embedded art preserved, checked by reading the output independently of FFmpeg.
- Source files are never changed.
- Cancel and failures leave no partial output, and failed files can be retried.
- Playback has no underruns while a batch runs, checked with the health log.

Found while building step 1, November 2026: FFmpeg's AAC output marks its encoder delay with an MP4 edit list, not the iTunes tag the engine reads, so the engine reads the edit list as well since step 3. MP3 output carries a LAME tag and decodes to the exact source length. The engine cannot play Opus, so Opus output is for other players and devices.

Step 2, November 2026: sample rate and bit depth are limits, not targets, so files already at or under them are left alone. A file above the rate limit comes down to a lower rate that divides its own evenly where one fits, so 176.4 kHz goes to 44.1 kHz rather than 48 kHz, and MP3 output is held to 48 kHz and AAC to 96 kHz without being asked. Bit depth is reduced with triangular dither, and lossy sources converted to a lossless format come out at 16 bits. Output names keep the source's folders and name under its library root with the new extension, compared without regard to case. A file already at the output path is left alone and skipped, so converting again into the same folder carries on where a batch stopped, and two sources that would land in the same place are both held back. Planning all 21,387 tracks of the library takes about 0.22 s.

Step 3, November 2026: tags are copied onto the converted file before it is moved into place. Between files with the same kind of tag the tag is copied whole: ID3v2 frame for frame in the same version, and Vorbis comments and MP4 atoms item for item. Between kinds, each field goes to its counterpart, fields of the user's own go across by name, and a field the target has no place for goes across under the name it had where that name means something. Fields that describe the old file's encoding (encoder name and settings, iTunSMPB) are left behind, since they would be wrong for the new file. The result is read back and compared, and whatever did not arrive is reported by the name it had in the source: in a sample of real files, Apple store atoms, ID3-only frames such as PRIV, GEOB and MCDI, URL frames and comments with a description. The engine now also reads an MP4 edit list for AAC encoder delay, which is where FFmpeg records it, and trims the last packet by its timestamps, so converted AAC plays to the exact source length. No file in the library decodes to a different length than before.

Step 4, November 2026: a conversion batch is a job of its own kind. It only reads the library, so it runs beside scans and tag edits, one batch at a time. What becomes of each file is recorded as it finishes, so a batch lists every file's outcome and the fields its copy lacks, and a retry converts the failed and unreached files again as a new batch with the same settings. Files convert several at once, half the processor's threads by default, with FFmpeg at below-normal priority. Cancel ends FFmpeg and removes its partial output; files left waiting when the app stops are marked failed at the next start, and their partial output is removed then. Adding to the library waits until no other job is changing it, then scans the output folder into the root it is in, or adds the folder as a root. Opus is refused there, since the player cannot play it. Converting 300 random real tracks to MP3 V0, six at a time, took 225 seconds, and playback on the real device ran through it with no underruns, its longest wait between callbacks 13 ms. Real files also showed what the tag copy now handles: ID3v2 tags stacked one after another (78 MP3s in the library) go across merged, as the library reads them; pictures MP4 has no type for, such as WebP, are left out and reported instead of failing the file; a rating kept as text goes across by name; and empty fields are no longer reported.

Step 5, November 2026: Convert is an action on a selection in the library and in playlists. Its dialog offers the presets or a custom codec and quality, limits on sample rate and bit depth, the folder, how many files at once and adding to the library, and plans the batch as the choices change, so it shows how many files will convert, which are already in the folder, lossy sources and an example output path before anything starts. The last choices are offered again. The Conversions view lists recent batches with live progress, cancel, retry, and each batch's problem files with FFmpeg's message or the fields left out. The status bar shows a running batch beside any library job, since the two run at once. Dialogs open in an overlay region of the shell, and keys pressed in one do not reach shortcuts.

After step 5, November 2026: the tag editor writes to files with stacked ID3v2 tags. An edit goes into the first tag and takes the field out of the later ones, every tag keeps its version and the frames not edited, and an undo puts each frame back in the tag it came from. Padding stays in the first tag, so the later ones are only as long as their frames, since programs looking for the audio skip one tag and then search only so far. The fingerprint's audio region now starts after every tag in a stack, so the fingerprints of the 78 such files change once, the next time they are fingerprinted. Editing and undoing all 78, and 300 random MP3s, passes the check after writing. The random run also showed that a track number written as two values, "5/8" then "5", kept the second as part of its total; only the first value counts now.

Phase 4 closed in November 2026 at the owner's go-ahead.

Development uses a locally installed FFmpeg. Shipping needs an LGPL build, chosen and reviewed with the installer, which waits until the project is further along.

Phase 2 closed in November 2026 at the owner's go-ahead. The listening check in its brief was not separately reported.

Phase 3 closed in November 2026 at the owner's go-ahead, after the owner tested the app.

**Phase 5 brief**

Goal: see the music as it plays, control the player from Windows, and install the app on another machine. Build order:

1. FFT pipeline: the audio thread copies its output into a ring buffer that never blocks or allocates, at the existing tap point. An analysis thread runs the FFT and sends spectrum and sample frames to the UI at a steady rate, only while a visualizer is open.
2. Visualizers: a spectrum analyzer and an oscilloscope, as plugins behind a small interface that receives frames and draws to a canvas, docked or full screen.
3. Waveform seek bar: each track's waveform is computed once in a background job and cached, then drawn in the player bar.
4. Windows media controls: media keys and the Windows media overlay and lock screen show what is playing and control playback, through SMTC.
5. Installer: a Windows installer from Tauri's bundler, with an LGPL build of FFmpeg beside the app and its license terms reviewed. Settle the name before any public release. Deferred in November 2026 at the owner's choice, until the project is further along; it is no longer part of this phase.

Not in this phase unless asked for: scrobbling, Milkdrop-style presets (Butterchurn), MPRIS on Linux, the command palette and context menus.

Scrobbling is left for later, at the owner's choice. Its hook is the play history: plays are recorded by the scrobbling services' own rule, rows are only ever added, and a play's track cannot be deleted while the play refers to it. A scrobbler can keep the id of the last play it sent and send the ones after it, which also covers plays made offline, with no change to how plays are recorded.

Exit criteria:

- Visualizers run smoothly while open, with no underruns in the health log.
- A track's waveform appears after it is first computed and comes from the cache after that.
- Media keys and the Windows overlay control playback and show the current track.
- Deferred with the installer: it installs and uninstalls cleanly on a clean Windows machine, and conversion works with the bundled FFmpeg.

Step 1, November 2026: the tap mixes the output to one channel and copies it into a ring buffer, after the equalizer, effects and limiter but before the volume, so visualizers show the music whatever it is played at, muted included. It does nothing while no visualizer watches, and drops samples rather than wait if the ring is full. An analysis thread runs a 4096-point FFT with a Hann window 60 times a second and sends a frame of 96 bands spaced evenly in pitch from 20 Hz to 20 kHz, in dBFS, plus the latest 1,024 samples for an oscilloscope. Silence sends one silent frame and then nothing, so a paused player sends no traffic. Frames are turned on with `player.setVisuals` and reach the UI as `visualFrame` events; the player client counts its watchers, so frames run while any visualizer is open. On Windows the thread waits with a high-resolution sleep, since parking waits in 15.6 ms timer ticks: on the real device frames came 16.6 ms apart at the median and 17.1 ms at the 99th percentile, with no underruns. The allocation test now runs with the tap turned on.

Step 2, November 2026: a visualizer plugin is an ID, a label and a factory for an object that receives frames and draws a picture into a canvas it is given, sized for the screen, with the theme's colors. The host owns the canvas, its size and the drawing loop: it draws once per screen refresh while frames arrive and stops two seconds after they end, and it takes frames only while the window is visible. Two plugins are built in. The spectrum shows as many bars as fit, each the loudest of its bands, which rise at once and fall at a steady rate, with peak marks that hold a moment, and frequency labels when there is room. The oscilloscope starts each picture where the wave rises through zero, so a steady tone stands still. A visualizer docks in a strip below the main view (V, or the button in the player bar) or fills the screen (F, or a double click), where it also shows what is playing; Escape leaves. Each place remembers its own plugin, and a button in its corner moves to the next. Visualizer colors and the dock's height are theme tokens. Both plugins were checked drawing synthetic frames in a browser; the band layout they assume is compared with the core's constants by a test.

Step 3, November 2026: a waveform is the whole file decoded once and summed up as 1,000 columns, each holding the loudest sample and the root mean square of its stretch of the track. Waveforms are kept in a `waveforms` table and stay current while the file's size and modification time are unchanged, or its audio fingerprint is, so a tag edit that leaves the audio alone keeps its waveform. A file that cannot be decoded keeps its failure until it changes; a missing file is reported without storing anything, so it is tried again when it comes back. `tracks.waveform` returns a stored waveform or starts computing one on a background thread, newest request first, and `waveformReady` follows. The UI asks for the playing track's waveform when the track changes. Decoding real files took 0.07 to 0.3 seconds each in a release build, so the next track is not computed ahead. The seek bar draws loudness solid and peaks faint, the played part in the accent color, with the range input over it, invisible, so pointer, keyboard and screen readers seek as before; with no waveform it draws a plain track. Its colors and height are theme tokens.

Step 4, November 2026: `umak-media` drives the System Media Transport Controls through the souvlaki crate, which also covers MPRIS on Linux. It follows the core's player events to show the title, artist, album, duration, state and position, and turns presses of the media keys and the overlay's buttons into the same commands the UI runs: play, pause, play or pause by the current state, next, previous, stop and seeking. The controls are told the position only when playback jumps, since they keep time themselves. They run on a thread of their own, and the app shell only hands them the window and runs their commands. Checked on the real system with a release build, its output muted: Windows listed the session, and pressing play, seek, pause and next through Windows' media session API played, moved and paused the player, with the overlay showing each track and position. Cover art is not shown yet, since the library does not extract it.

**Phase 5, continued: cover art and the album grid**

Added in November 2026 at the owner's request, with the server on hold. Goal: show each album's cover in a grid of the library and wherever a track shows. Build order:

1. Art cache: a track's art is the front cover in its tags, or the first picture if none is marked front, or else an image in its folder named `cover`, `folder` or `front`. Each image is resized to a grid thumbnail and a large size and kept on disk under a hash of the image, so an album sharing one cover costs one file; the database records only which image each track uses. An album's cover is that of its first track with one. Art is extracted on demand in the background, newest request first, and a job after each scan fills in the rest. A track's entry stays current while its file's size and time are unchanged.
2. Serving: a custom URL scheme in the app shell serves cached images by ID and size, so pictures never go through JSON. The server can later serve the same files.
3. Album grid: a library mode showing a virtualized grid of covers with album, artist and year, sorted and searchable like the track list. An album opens to its tracks, with the track list's actions.
4. Art where tracks show: the player bar, the full-screen visualizer, the Windows overlay and lock screen, and the tag editor, which shows a file's pictures read-only.

Not in this round unless asked for: editing or embedding pictures, fetching art online, and showing in the grid art that differs between tracks of one album. The player bar still shows a track's own picture.

Exit criteria:

- The whole library's art is cached, timed on the real collection, with no underruns if it runs during playback.
- The album grid scrolls smoothly across every album.
- Covers appear in each place in step 4, and albums without art show a placeholder.

Step 1, November 2026: the art cache follows the brief, except that filling it is not a job in the jobs panel but the same background thread as requests, as with waveforms: requests from the screen go first, and between them it works through tracks with no current entry. The app turns this on at start and it runs again after each scan, import or tag write; tools that only scan leave it off, and requests work either way. Pictures are kept at 256 and 1,024 pixels along the longer side, as JPEG, never enlarged, in an `art` folder beside the database, in subfolders by the first two characters of the hash. A damaged picture in the tags falls back to the folder's image, and a file that is missing is answered without storing anything. `tracks.art` returns a track's art or starts looking, and `artReady` lists the tracks done; `umak-scan art <database>` fills the cache and times it. On the real collection, from a fresh scan: 21,387 tracks in 135 seconds, 158 a second, with art in the tags of 21,326 tracks, in the folder of 44 and none for 17. The 1,274 distinct pictures take 183 MB, and 1,282 of the 1,283 albums have art. In 9 albums the tracks carry different pictures.

Step 2, November 2026: the app shell serves the art folder through a `umak-art` URL scheme, which the webview on Windows reaches as `http://umak-art.localhost/`. A picture's name is its hash and size, such as `<hash>-small`; the core turns a name into a file only if it is well formed and the file exists, so a request cannot reach outside the art folder. Files are read off the main thread and marked cacheable for good, since a name always shows the same picture. The security policy allows images, not other requests, from the scheme, and the UI builds URLs with `artUrl` in the API client. Checked in a release build on the real library: a track's picture loaded in the app's webview at both sizes, and an unknown name did not.

Step 3, November 2026: Albums is its own entry in the sidebar, beside Library, rather than a switch inside the track list. `library.albums.page` pages albums by artist, title or year, each walking an index, and searches like the track list: an album shows if any of its tracks matches. The matching albums are found once, through a covering index from track to album, and handed to the page as a list. `library.albums.get` returns an album with its tracks by disc and track. An album's cover is the cached art of its first track that has some. While the cache fills, an album with no cover yet is marked pending, and the grid asks `library.albums.findArt` for the next unchecked track of each album on screen, which the cache serves before the rest; on an empty cache, covers at the end of the grid appeared within 2 seconds. The grid fits as many tiles across as stay at least the theme's tile width, draws only the rows in view, and moves by arrow keys, Page Up and Down, Home and End; Enter or a click opens an album, and a button on the cover plays it. It keeps its search, order and place when an album is opened and closed. The album page shows the large cover, the album's details and buttons to play or queue it, above the track list with the same actions as the library, which are now one component. On the real library's 1,283 albums, a page takes 1.3 to 1.6 ms at the median and 2.1 ms at the 95th percentile, a search keystroke 5.3 ms at the 95th percentile, and an album with its tracks 0.15 ms. In a release build, scrolling from the first album to the last in 600 frames, and back, kept every frame at 16.7 ms, with covers loading as they came into view. Playing on the real device, muted, for three minutes while the cache filled from empty, logged no underruns.

Step 4, November 2026: the player bar shows the playing track's own picture beside its title, and the full-screen visualizer shows it large beside what is playing; the player client gives a track's art like its waveform, and asks again when `artReady` names the track. The album page uses the same cover component, which shows a placeholder when there is no picture or it fails to load. The system's media controls are given the large picture as a file, looked up on their own thread and again when `artReady` names the track playing. They keep the last picture they were given, so a track with no art gets a plain placeholder written once into the art folder. `tags.pictures` lists every picture in a file's tags, with its type, description, format, size and pixel size, storing each in the art folder so the tag editor can show it through the art URL scheme; the editor shows the pictures of the track whose fields were last chosen, or the first, read-only. Checked in a release build on the real library, its output muted: the cover appeared in the player bar, the full-screen visualizer and the tag editor, and Windows' media session held the same picture; a track with no art showed the placeholder in the bar and in Windows; and with the cache partly filled, a track not yet looked at showed its cover in the bar 0.3 seconds after it started, and Windows then held the real picture. Filling the whole cache from empty while the app played on the real device, muted, took 366 seconds for 21,387 tracks, with no underruns logged and playback still going at the end.

Phase 5 closed in November 2026 at the owner's go-ahead, after the owner checked the visualizers in the app. Scrobbling, the installer and the server remain deferred.

**After Phase 5: settings**

Added in November 2026 at the owner's request, on a `settings` branch merged into `main` when the round is done. Goal: make the app as configurable as is useful, with every setting in one place, applied at once, and carried by export and import. Build order:

1. Settings store: the "one typed, versioned settings store" from the extension points. Each setting is declared once in the core with its key, type, default and allowed values; the core checks values, stores them in the `settings` table as JSON, and announces changes with an event. Commands read all settings, set one and reset one or a group to defaults. The player's existing settings move onto it with their stored values kept. The Settings view becomes sections with a search box, each setting showing whether it differs from its default.
2. Appearance: choose the theme while the app runs; your own themes as JSON token files in a themes folder beside the database, checked against the token list, with a clear message for a bad one; text size and row density as scales on the theme's tokens; and the track list's columns, shown or hidden and in your order.
3. Keyboard shortcuts: every action in the registry can take a shortcut, or none. Recording a key shows any action that already has it, and saving takes it from that action. Each shortcut, or all of them, can go back to its default.
4. Playback: the output device, followed when it disappears by falling back to the default with a message; the skip length; how far into a track Previous restarts it instead of going back; when a play counts; the volume step; and whether to resume the queue and position at start, paused.
5. Library and behavior: the file types a scan reads and the folders it skips; the folder image names art looks for; the leading articles left out when sorting; what double-click and Enter do on a track (play from here, play the album, play next, add to queue); and the view the app opens on.

Not in this round unless asked for: crossfade, shuffle and repeat, which are features rather than settings; settings per user; internal tuning such as buffer, batch and analysis sizes; and syncing settings between machines beyond export and import.

Exit criteria:

- Every setting applies without a restart, except where its description says otherwise, and survives a restart.
- Export and import carry the settings, and a setting the importing app does not know is kept, not dropped.
- A bad value from any source, including an edited database or theme file, falls back to the default with a message, never a crash.
- The existing player settings keep their stored values through the move.

Step 1, November 2026: the core declares each setting once in `settings.rs`, with its section, label, help, kind (on or off, a number in a range with a step and unit, a choice among named values, or a control of its own such as the equalizer) and default. Values are stored as JSON; migration 9 quotes the values the player kept as plain text, and a value from before is still read by its meaning if one arrives in an old export. `settings.list` gives every definition and value, `settings.set` checks a value, bringing a number into range and refusing anything else the setting does not allow, and `settings.reset` puts any set of settings back to their defaults; each announces `settingsChanged` with the new values. A saved value that makes no sense reads as the default and carries a message saying why. The player keeps its own commands for the volume, ReplayGain and equalizer, now saving through the store, and a player setting changed through the store reaches a running engine before it is saved, as before. An import that brings settings has the player take them up at once. Settings that are set elsewhere, such as the volume and mute on the player bar, are declared but not listed, and the saved equalizer presets stay the player's own data beside the settings. The Settings view lists sections in the core's order, searches labels, help and choices, marks each setting changed from its default with a reset beside it, and resets a whole section; problems in settings not listed show at its top, and a message at start says when any saved value was not used. Checked in a release build on a copy of a database with the player's settings saved the old way: the volume, ReplayGain mode, preamp, equalizer bands and a saved preset all came through, a setting nobody declares was kept, a nonsense mute value was reported and reset, changes applied while a track played without interrupting it, and every value survived a restart.

Step 2, November 2026: the theme is a setting. Default and a new Light theme come with the app; your own are JSON files in a `themes` folder beside the database, which `appearance.themes` reads and makes if it is missing. A file gives a name, a base theme for the tokens it leaves out, and tokens; the UI checks it against its token list, sizes in pixels and colors and fonts with the browser's own CSS check, and lists every problem a bad file has, which cannot then be chosen. A chosen file that goes missing or goes bad falls back to the default theme with a message. Text size, from 80 to 150 percent, scales the font sizes, rows, sidebar, queue panel, album captions and seek bar; density, Compact, Normal or Roomy, scales the gaps and rows; pictures and the visualizer keep their sizes. The list, album grid, seek bar and visualizers read token values again when they change, and the last tokens applied are kept in the window's storage so a start shows them before the settings arrive. A new kind of setting, an order, holds some of its options in the user's order; the track list's columns use it, with disc and track and last played added to the columns there were, and the title always shown. Checked in a release build: a partial theme file and a broken one were listed, the broken one with its four problems; Light, 120 percent text and Compact applied at once; reordered columns showed in the library; a theme file taken away fell back to the default with a message; and every value survived a restart.

Step 3, November 2026: every action without arguments declares its default shortcut, or none, when it is registered; the few that only make sense on their own screen, such as a tag edit's Cancel and Done, cannot take one. One setting, `keys.shortcuts`, holds only the user's changes, by action ID, as a key or none; the core refuses a value giving one key to two actions, and keeps changes for actions this version does not know. A key the user gave an action wins over another action having it by default, so a default added by a later version cannot take a key the user chose. The Keyboard shortcuts section lists the actions under headings with their keys, and the settings search finds them by name. Changing one records the next key pressed, ignoring modifiers pressed alone and leaving Tab to move between controls; Escape cancels. If another action has the key, the row says which, and saving takes the key from it. Each shortcut can be cleared or reset, and resetting the section resets them all. Buttons' tooltips show the keys in use. Checked in a release build: giving M to Stop took it from Mute, after which M stopped instead of muting; a new Ctrl+2 opened Albums; resetting Mute took M back; clearing Space left Play or pause without a key; the shortcuts survived a restart; and resetting the section brought back every default.

Step 4, November 2026: the output device is a setting, empty for the system's default. The engine opens a device by name and falls back to the default for one that is not there; its `Ready` names the device opened, and the core says when that is not the one chosen. Changing the device replaces the engine and carries on from the same track and position, paused if it was paused; with nothing playing, a running engine is replaced at once so a missing device is reported then. When an output stops working, as when it is unplugged, the engine says so with `outputFailed` and the core replaces it on the chosen device or the default, carrying on and saying where playback moved; a second failure within ten seconds leaves playback stopped with a message instead of starting engine after engine. `player.outputDevices` lists the devices, and Settings offers them with the one playing through. A new Playback section holds the skip length, which the media controls' seek buttons also read; how far into a track Previous restarts it, now a command to the engine, where 0 always goes back; when a play counts, as a share of the track or a length of listening, whichever comes first; the volume step; and resuming the last queue at start, which loads it at its place, paused, through a paused load the engine now takes. Checked in a release build with the output muted: a device that does not exist fell back to the default with a message; switching to a second real device while playing carried on from the same place; switching while paused stayed paused; a 15-second skip, Previous at 0 and 30 seconds and a 20 percent volume step all behaved as set; and with resuming on, the app opened paused on the chosen device at the place it was left.

Step 5, November 2026: two new kinds of setting, several of a fixed set of options and a list of short text of the user's own, cover the library's rules. A scan reads the file types and the folders to skip from the settings, comparing both ignoring case, and changing either starts a scan; a type taken away leaves its tracks missing, with their ratings and plays kept. Art looks for folder images by the names in the settings, in their order, and changing them forgets what folders gave, or did not give, so they are looked through again. Sort values left out the articles in the settings, where a word ending in an apostrophe needs no space after it; sort tags always win. Since stored sort values do not say whether they came from a tag, a change of articles has the next whole-library scan read every file again, refreshing the artists' sort names as well, and the articles used are recorded so it happens once. Double-click and Enter on a track in the library, an album or a playlist play from there, play its album, play it next or add it to the queue, taking the tracks selected with it; and the app opens on the library, the albums, or the view it was left on, unless that was an editor or a playlist since deleted. Checked in a release build on a library of copied real files and generated ones: skipping a folder and taking a file type away left their tracks missing, and bringing the type back restored them; changing the articles read every file again once; a folder image named artwork was found once added to the names; each double-click choice did as it says; and the app opened on Conversions, where it was left, and then on Albums. Found and fixed after: Play next chosen within a moment of starting a new list went to the top of the queue, because the player waited for the engine to say which entry was playing; loading a list now sets that place at once.

## Milestones, risks and open questions

Build in six phases, each ending in something usable. The 30k-track test library exists from phase 1, so performance is checked continuously.

1. **Library foundation:** Tauri shell, Rust workspace, schema, library import, search, playlists, export and import between installations, a lightweight player executable, and the extension-point seams. Exit: see the phase 1 brief.
2. **Player:** the in-process engine, gapless playback and ReplayGain, the EQ and limiter, the player bar and queue panel. Exit: play single songs, albums and saved playlists through one list, with gapless joins.
3. **Tagging and management:** batch tag editing, duplicate finder, smart playlists. Exit: all target tag formats read and write cleanly.
4. **Conversion:** FFmpeg job queue and presets, built as a reusable service. Exit: batch conversion with tags and art preserved.
5. **Visualizations and polish:** FFT pipeline, visualizers, OS integration. Scrobbling and the installer are deferred; see the phase 5 brief. Exit: see the phase 5 brief.
6. **Server (later):** OpenSubsonic through axum on top of the existing command layer, auth, streaming, transcoding, and a headless mode for the NAS. Exit: two third-party clients connect and play.

**After the MVP:** a file watcher, so adds, moves, changes and deletes appear with no manual rescan. Network shares can miss change notifications, so network roots would also get a periodic rescan. The scanner already takes a set of changed paths, so a watcher can feed it.

Nice to have, not scheduled: agent and smart-home control. See "Agent and smart-home control".

**Risks**

| Risk | Mitigation |
| --- | --- |
| Tag-writing bugs damage files | Temp-file writes, verification, undo history, tests against a corpus of real files |
| Gapless playback edge cases | Test across formats and sample rates, with encoder-delay metadata handling |
| Decoder and tag library gaps | Symphonia may lack Opus, HE-AAC, WMA and Monkey's Audio, and Lofty may lack ASF. Verify current versions; the collection is almost entirely MP3, FLAC and M4A, so check the M4A codecs first |
| Bit-perfect output on Windows | Treat WASAPI exclusive mode as a separate, later output backend |
| FFmpeg licensing and bundling | Choose an LGPL build and review the license terms before shipping |
| Scope creep | Hold to the phase order; defer anything outside the must-have list |

**Open questions**

- [x] Which M4A codecs do the files use? The 274 M4A files in the library roots are 203 AAC and 71 ALAC, and all of them decode fully with Symphonia. Each decodes at the sample rate its tags state, so none is HE-AAC, which would decode at half the rate.
- [x] Should a single artist tag be split on `/`? No. Tags are the source of truth, so a value like `Dr. Dre/Eminem` stays one artist until the file is retagged. `umak-scan` lists these values as tagging problems.
- [ ] When the server is added, should it run inside the desktop app, headless on the NAS, or both?
- [ ] Is bit-perfect or exclusive-mode output a requirement from the start?
