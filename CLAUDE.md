# Umak

A local-first desktop music library manager, tagger and player, built with Tauri, Rust and Svelte. The full plan is in `docs/PLAN.md`; read it when a task needs more than this file.

## Current phase

Phase 1: Library foundation. See "Phase 1 brief" in `docs/PLAN.md`. Work one phase at a time; each ends with the exit criteria under Milestones.

## Hard rules

- Crates under `crates/` never depend on Tauri or UI code. Only `app/` depends on them.
- One transport-neutral command layer: every action is a typed request or a state event. Tauri IPC is the first adapter; HTTP comes later.
- Tracks, albums and artists use stable opaque IDs, never file paths.
- Paths are stored relative to a library root.
- Per-user data (ratings, play counts, play history, playlists) carries a user id.
- The audio thread never blocks, allocates or takes locks.
- Music files change only through the safe tag-write path (temp file, verify, replace) or a user-requested rename or move.
- The user's real music collection (path in `CLAUDE.local.md`) is scanned read-only; tag-writing tests run on copies.
- Library code is built for maximum speed. About 30k tracks is the target to verify, not a design limit.
- UI colors, fonts and spacing come only from theme tokens. UI actions go through the action registry, and the UI reaches the player only through the player client interface.
- Build the seams in the "Extension points" section of `docs/PLAN.md` as surrounding code is written, but not the features behind them. Public repository: keep machine-specific paths and hostnames in `CLAUDE.local.md`.

## Layout

```
crates/umak-core/     library, database, scanner, tag model
crates/umak-audio/    engine, playlist, EQ, plugin host
crates/umak-play/     lightweight player executable
crates/umak-scan/     headless scan and benchmark tool
crates/umak-convert/  FFmpeg job runner (later phase)
crates/umak-server/   OpenSubsonic (later phase)
app/                  Tauri shell (umak-app) and Svelte UI
docs/                 PLAN.md and architecture notes
```

## Build and test

No code exists yet. Add the build, test and benchmark commands here when the workspace is created.
