# Umak: Project Plan

## Goals and requirements

Build a local-first music player and library manager that stays fast at about 30,000 tracks, edits tags in every common format, and serves its library to remote clients.

**Pain points this app is built to fix**

- Weak library management: slow scans, manual rescans, no duplicate handling.
- A poor player UI: hidden queues, clunky lists, no keyboard workflow.
- Dated technology: no gapless playback, no loudness normalization, no modern OS integration.
- Incomplete tagging: missing formats, multi-value fields and custom tags.

**Must-have capabilities**

- Library: folder scanning, file watching, duplicate detection, smart playlists, batch editing.
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
- `umak-play`: a thin executable around `umak-audio` that takes typed player commands over stdin and reports events over stdout.
- `umak-scan`: a headless binary that runs the core without a UI.
- `umak-convert`: FFmpeg job runner.
- `umak-server`: axum, OpenSubsonic and the management API (later phase).
- `umak-app`: the Tauri shell that wires the UI to the core.

**Process model**

The core runs the audio engine on its own real-time thread and the scanner on a worker pool. The UI never touches audio or the database directly; it only sends commands and renders state. The same core compiles to a headless binary for server-only use.

In phase 1 the audio engine runs in the separate `umak-play` process, so the library can be built and tested with a minimal player. The app reaches it through a player client interface. Phase 2 adds an in-process implementation of that interface, and the UI does not change. `umak-play` stays as a test tool. The player protocol lives in `umak-audio` without its `engine` feature, so `umak-core` can speak it without building any audio code.

**Repository layout: one monorepo**

The project is a single Git repository containing a Cargo workspace, not separate repositories. The crates change together, and one repository gives one commit, one build and one test run. A crate is split out later only if it becomes useful on its own.

```
umak/
  Cargo.toml        workspace
  crates/
    umak-core/      library, database, scanner, tag model
    umak-audio/     engine, playlist, EQ, plugin host
    umak-play/      lightweight player executable
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

**Scanner and watcher**

- Walk folders and read tags with Lofty, tuning concurrency per storage device. Parallel reads can slow a hard drive, so benchmark rather than assume more threads help. On the hard drive holding the collection, 4 threads was fastest (12.8 ms per cold MP3, against 17.7 with 1 thread and 13.3 with 8).
- Ignore playlist files (`.m3u`, `.m3u8`). Playlists are created and stored in the app.
- Skip unchanged files by comparing modified time and size, so rescans are near-instant.
- Run a file watcher so adds, moves and deletes appear automatically, with no manual rescan. Network shares can miss change notifications, so network roots also get a periodic rescan.
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
- Write safely: temp file, verify, then replace, so a crash cannot corrupt a file.
- Undo history for tag changes.

**Cleanup tools**

- Duplicate finder by fingerprint and by tag match.
- Rename and move files from tag patterns.
- Identify untagged files with AcoustID fingerprinting and MusicBrainz lookups.
- Smart playlists defined by rules, such as FLAC, rated 4 or higher, not played in six months.

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
| play_history | Listens | User id and timestamp per play, feeds scrobbling and smart rules |
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
| Command palette, keybindings, context menus | UI actions live in one registry, each with an ID, a label and a handler | Palette UI, remapping |
| Layout changes | Views register into named regions of the app shell | Docking, customizable layouts |
| Tag editing | The tag model covers multi-value fields and raw tags | Tag writing and undo |
| File watching | The scanner processes a set of changed paths, so a watcher can feed it | The watcher |
| Conversion, long tasks | Long operations are cancelable jobs that report progress | FFmpeg runner |
| Later settings | One typed, versioned settings store | Most of the settings |
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

## Milestones, risks and open questions

Build in six phases, each ending in something usable. The 30k-track test library exists from phase 1, so performance is checked continuously.

1. **Library foundation:** Tauri shell, Rust workspace, schema, library import, search, playlists, export and import between installations, a lightweight player executable, and the extension-point seams. Exit: see the phase 1 brief.
2. **Player:** the in-process engine, gapless playback and ReplayGain, the EQ and limiter, the player bar and queue panel. Exit: play single songs, albums and saved playlists through one list, with gapless joins.
3. **Tagging and management:** batch tag editing, duplicate finder, smart playlists, file watcher. Exit: all target tag formats read and write cleanly.
4. **Conversion:** FFmpeg job queue and presets, built as a reusable service. Exit: batch conversion with tags and art preserved.
5. **Visualizations and polish:** FFT pipeline, visualizers, OS integration, scrobbling, installers.
6. **Server (later):** OpenSubsonic through axum on top of the existing command layer, auth, streaming, transcoding, and a headless mode for the NAS. Exit: two third-party clients connect and play.

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
