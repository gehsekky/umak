# Phase 1 benchmarks

Measured in November 2026 on the phase 1 code. To repeat the query and raw disk numbers, use `umak-scan bench` and `umak-scan rawio`. A scan's disk and code split comes from its normal report.

## Setup

- Windows 11, 32 GB of RAM. Music on a 4 TB SATA hard drive; database on an NVMe SSD.
- 30,000 tracks: 21,387 real (15,015 MP3, 6,098 FLAC, 274 M4A, about 455 GiB) and 8,613 small synthetic MP3s from `umak-scan generate`.
- "Cold" means file data was dropped from the Windows cache first, by opening each file unbuffered. This needs no admin rights but leaves NTFS directory metadata cached, so the listing figures below are warm.

## Exit criteria

| Criterion | Target | Result |
| --- | --- | --- |
| Search per keystroke, p95 in the core | 10 ms or less | 4.6 to 4.8 ms across 3 runs of 991 keystrokes |
| A page of a list, any position, p95 in the core | 10 ms or less | 0.95 to 4.1 ms by sort, across 3 runs |
| Rescan with no changes, warm | 5 s or less | about 0.24 s of scan time, 0.3 to 0.4 s for the whole program |
| Browsing with no visible lag | none visible | steady 60 fps while scrolling; rows appear within 26 ms (p95) of a scrollbar jump |
| Library import, reported against raw disk cost | reported | see below |

Query times go through the full command layer, including JSON, and return 200 rows.

| Query | Median | p95 | p99 | Max |
| --- | --- | --- | --- | --- |
| List page by album | 1.5 ms | 2.9 ms | 3.1 ms | 3.5 ms |
| List page by artist | 1.4 ms | 2.6 ms | 2.9 ms | 3.6 ms |
| List page by title | 1.8 ms | 3.1 ms | 3.3 ms | 3.5 ms |
| List page by date added | 0.7 ms | 1.0 ms | 1.2 ms | 1.3 ms |
| Search, per keystroke | 3.3 ms | 4.8 ms | 5.3 ms | 6.6 ms |
| Search, deep in the results | 3.2 ms | 4.7 ms | 5.4 ms | 6.5 ms |

These are from the second of three runs. The first of the three shows occasional higher maximums, up to 21 ms, but its p95 values stay within 4.9 ms. The first page after opening the database took 7.6 to 10 ms.

## Browsing during a scan

The app stays responsive while a scan reads cold files from the hard drive and writes to the database, measured in the real window at 22,000 tracks with 7,869 being added:

- Scrolling: 60 fps, with a worst frame of 17.1 ms.
- Scrollbar jumps: p95 of 23 ms.
- Search: 19 to 34 ms from keystroke to screen.

## Playback during a scan

The player's sample queue stayed full and nothing stalled while a scan read cold files from the same hard drive. Measured on the real output device at zero gain, once as separate programs for 90 s on each of two cold FLAC files, and twice in the real app repeating a first import for 90 s:

- Underruns: none in any run.
- Longest wait between device callbacks: 13.5 ms, against a normal 10 ms.
- Queue: about 2 s throughout, once the track had started.

This measures what the player hands to Windows, so it cannot see a problem after that point. One real listening session on 2026-11-04 did stutter during a scan, and the cause is not yet known. The app now writes `player-health.log` beside its database when the engine sees an underrun or a stall.

## First import against raw disk cost

| Step, 30,000 files with cold data, 4 threads | Time |
| --- | --- |
| Whole import | 396 s |
| Listing folders (warm metadata) | 0.23 s |
| Database writes | 4.7 s |
| Raw baseline: reading 234 KB from the start of each file, no parsing | 210 s |
| The same raw read with warm data | 1.1 s |

The worker threads spent 1,539 s of about 1,568 s of thread time waiting on file opens, reads and seeks, so about 98% of the import is disk time and about 2% is parsing and hashing.

The gap between 396 s and the 210 s baseline is also mostly disk time. The import read 7.0 GB against the baseline's 5.2 GB, because it follows each format's layout, including reading the end of MP3 files for trailing tags, which adds seeks. Reducing those reads is the remaining opportunity if first-import speed ever matters; it runs only once per library.

## Fingerprints and threads

Measured on about 460 cold MP3s per run.

| Setting | Per file | Fingerprint part |
| --- | --- | --- |
| 1 thread, no fingerprint | 17.8 ms | none |
| 1 thread, 1 sample | 17.7 ms | 0.9 ms |
| 1 thread, 2 samples | 20.2 ms | 7.4 ms |
| 1 thread, 3 samples | 29.7 ms | 14.0 ms |
| 2 threads, 1 sample | 14.2 ms | |
| 4 threads, 1 sample | 12.8 ms | |
| 8 threads, 1 sample | 13.3 ms | |

## Database size

39.1 MB at 30,000 tracks, against the plan's estimate of 15 to 30 MB. Raw tags account for only about 3 MB of text. Size causes no measurable slowness, so nothing was changed.

## Not measured

- A listing with cold NTFS metadata, which needs a reboot or admin rights.
- A solid-state or network library. The 4-thread default was chosen on a hard drive.
