# Umak

A local-first desktop music library manager, tagger and player, built with Tauri, Rust and Svelte. The full plan is in `docs/PLAN.md`; read it when a task needs more than this file.

## Current phase

Phase 4: Conversion. See "Phase 4 brief" in `docs/PLAN.md`. Phases 1 to 3 are complete. Work one phase at a time; each ends with the exit criteria under Milestones.

## Hard rules

- Crates under `crates/` never depend on Tauri or UI code. Only `app/` depends on them.
- One transport-neutral command layer: every action is a typed request or a state event. Tauri IPC is the first adapter; HTTP comes later.
- Tracks, albums and artists use stable opaque IDs, never file paths.
- Paths are stored relative to a library root.
- Per-user data (ratings, play counts, play history, playlists) carries a user id.
- The audio thread never blocks, allocates or takes locks.
- Tags are the source of truth. Store values as written; never split or derive from them. Report suspect values as tagging problems.
- Music files change only through the safe tag-write path (temp file, verify, replace) or a user-requested rename or move.
- The user's real music collection (path in `CLAUDE.local.md`) is scanned read-only; tag-writing tests run on copies.
- Library code is built for maximum speed. About 30k tracks is the target to verify, not a design limit.
- UI colors, fonts and spacing come only from theme tokens. UI actions go through the action registry, and the UI reaches the player only through the player client interface.
- The UI reaches the core only through `app/src/lib/api/client.ts`. Transport adapters only forward JSON and events, with no library logic of their own.
- Build the seams in the "Extension points" section of `docs/PLAN.md` as surrounding code is written, but not the features behind them. Public repository: keep machine-specific paths and hostnames in `CLAUDE.local.md`.

## Layout

```
crates/umak-core/     library, database, scanner, tag model
crates/umak-audio/    engine, playlist, EQ, plugin host
crates/umak-play/     player test tool around the engine
crates/umak-scan/     headless scan and benchmark tool
crates/umak-convert/  FFmpeg runner and conversion tool
crates/umak-server/   OpenSubsonic (later phase)
app/                  Tauri shell (umak-app) and Svelte UI
docs/                 PLAN.md and architecture notes
```

## Build and test

Run from the repository root:

- `cargo test --workspace`: Rust tests.
- `cargo run -p umak-scan -- <database> [folder]... [--threads N] [--samples N] [--no-scan] [--list N] [--json]`: scan folders into a database and print statistics, including album grouping and tagging problems. Keep test databases outside the repository.
- `cargo run --release -p umak-scan -- bench <database>`: time queries against the 10 ms target. `rawio <folder>...` times listing and reading with no parsing, and `generate <folder> <count>` writes synthetic tracks. Results so far are in `docs/benchmarks-phase1.md`.
- `cargo run -p umak-play -- <file>...`: play files. With no files it takes JSON commands on stdin. `--volume N` sets the gain, `--null-output` makes no sound, and `--check <file>...` decodes files without playing them.
- `UMAK_JOIN_FILES="a;b;c" cargo test --release -p umak-audio --test engine report_on_real_joins -- --ignored --nocapture`: measure the joins between real files.
- `cargo run -p umak-convert -- info` shows the FFmpeg found and its encoders; `cargo run -p umak-convert -- <input> <output> <encoding>` converts one file, with encodings such as `flac`, `mp3-v0` or `aac-256`. The converter looks for FFmpeg in `UMAK_FFMPEG`, beside the executable, then on the path. Its tests, and the core's `convert_tags` tests, need FFmpeg installed.
- `cargo test -p umak-core --features engine`: include the tests of the engine inside the core. `cargo test --workspace` already includes them, because the app turns the feature on.
- `UMAK_TAG_CORPUS=<folder> cargo test --release -p umak-core --test tag_corpus -- --ignored --nocapture`: write tags to real files and undo them, to find what fixtures cannot. It changes the files, so it needs a folder of copies holding a `.umak-tag-corpus` file.
- `cargo tree -p umak-core -e normal`: must not list Tauri. The same applies to `umak-audio`.

Run from `app/` (run `npm install` first):

- `npm run check`: type-check the UI.
- `npm test`: UI logic tests.
- `npx tauri dev`: run the app with hot reload.
- `npx tauri build --no-bundle`: build the release executable at `target/release/umak-app.exe`.
- Set `VITE_UMAK_THEME=test` when building to try the test theme.
- Set `UMAK_DATA_DIR` to run the app against another data folder, leaving the real database alone.
- Set `UMAK_PLAY_OUTPUT` to test the app without sound: `null` uses no device and `muted` runs the real device at zero gain.
- Underruns and device stalls are logged to `player-health.log` beside the database.

TypeScript stays on version 6 until `svelte-check` supports version 7.

## Adding a command

1. Define the argument and result structs in `crates/umak-core/src/api.rs`, implement `Command`, and list it in `commands!`. Results are named structs.
2. Regenerate the TypeScript types by running `cargo test -p umak-core` with `UMAK_UPDATE_BINDINGS=1` set, and commit `app/src/lib/api/generated/`.

Without step 2, `typescript_bindings_are_current` fails.
