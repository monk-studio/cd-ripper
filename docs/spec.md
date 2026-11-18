# CDR ripper specification

What CDR does when it rips a CD, and why. Every front end (the `cdr` CLI,
the desktop app, future Windows and Linux clients) behaves as described here,
because they all run the same pipeline in `cdr-core` (`album::rip`).

This document describes current behaviour, not plans. A commit that changes
behaviour updates the spec in the same commit. Decisions and measurements
behind the rules live in the GTD tickets (`CDR-n`), cited where useful.

## 1. Reading the disc

### 1.1 Drive access

- The disc is read through the platform's raw CD-DA interface, one sector
  (2352 bytes, 588 stereo samples) at a time, 32 sectors per request.
  On macOS: `DKIOCCDREAD` on `/dev/rdiskN`, which works while the system has
  the disc mounted.
- When the drive supports it, every read also returns C2 error pointers
  (2352 + 294 bytes per sector). Support is probed on first use.
- A request that fails is retried sector by sector, so one bad sector doesn't
  take its neighbours down.
- Sectors outside the audio area (lead-in, lead-out) can't be read. They are
  treated as silence and never flagged.
- Only one track is held in memory at a time.

### 1.2 Fast read first, verified by AccurateRip

Each track is read **once** at full speed. If its checksum matches
AccurateRip (v1 or v2, any pressing), the track is done.

*Why:* the drive's physical speed is the limit (on an LG BP71N: 7–10x near
the centre, 18x at the edge). A second read can't prove more than an
AccurateRip match already proves, and it doubles the time. Measured on the
same disc and drive: CDR 3:06, XLD's default secure mode 6:15, because XLD
always reads twice (CDR-4).

### 1.3 Secure mode

A track enters secure mode when AccurateRip doesn't confirm it (the disc
isn't in the database, or no pressing matches), or always with `--secure`.

1. **Flush the drive cache**: read 2048 sectors (4.8 MB) at whichever end of
   the disc is further from the track, so the next read comes from the disc
   surface and not the drive's cache.
2. **Second full read** of the track.
3. **Compare**: a sector is unsettled if the two reads differ, either read
   had C2 errors, or either read failed.
4. **Re-read rounds**, up to 8. Each round flushes the cache (at full speed),
   then re-reads every unsettled sector. A sector settles when a read
   **without** C2 errors is identical to any earlier read of it. Identical
   reads that both carry C2 errors don't count: drives can repeat the same
   interpolated data.
5. **Slow rounds**: from round 5 on, the drive is slowed to its slowest
   speed for the re-reads and set back to full speed after each round. The
   flush stays at full speed. Drives ignore speeds below their minimum, so
   CDR asks for 1x, 2x, 4x and 8x in turn and keeps the first one the drive
   reports as lower than before (an LG BP71N ignores 1x and accepts 4x).
6. **Suspicious sectors**: whatever is still unsettled keeps the reading seen
   most often (preferring clean reads) and is reported by its position in the
   track.

After secure mode the track's checksums are checked against AccurateRip again.

### 1.4 Read offset

The read offset belongs to the **drive**, and every rip's audio is aligned to
it: corrected sample `n` is raw sample `n + offset`.

The drive offset comes from, in order:

1. **Set by hand** for this rip (`--offset`).
2. **Remembered** for this drive in `drives.json` (a database lookup or an
   offset learned earlier), in CDR's data folder (macOS:
   `~/Library/Application Support/CDR`).
3. **AccurateRip's drive database** (`DriveOffsets.bin`), looked up by the
   drive's model. The result is remembered.
4. **This disc alone**, when nothing above knows the drive: the offset at
   which this disc matches AccurateRip (below). It may be the pressing's
   alignment rather than the drive's, and the log says so.

If none of these gives an offset, the rip stops and asks for one.

**Pressing offset.** For every disc in AccurateRip, CDR also finds where this
disc's pressing lines up with the database: it reads about 11 sectors around
sector 450 of a track and searches ±4 sectors for the database's
offset-finding checksums, track by track until one matches. A disc can match
several pressings; the offset with the most AccurateRip confidence behind it
wins (nearest to 0 on a tie). On 鋼の錬金術師 the first match found was a
pressing with confidence 4–5 and the best one, 42 samples away, had 96. This
is evidence about the drive, not the drive offset itself.

**Learning.** Each disc's pressing offset is recorded for the drive. Once 3 or
more discs agree on an offset, and they outnumber all other offsets seen,
it becomes the drive's learned offset. A single disc never overrides an offset
set by hand or taken from the drive database.

*Why:* a disc matching AccurateRip at some offset only shows how its
pressing lines up with the pressing in the database. The same LG BP71N gave
+6 on one disc and −36 on another; its real offset is +6 (AccurateRip's drive
database agrees). Aligning each disc to its database match shifts the
track boundaries away from the disc's own layout (CDR-9). XLD instead looks
the model up in a list frozen at its release; it doesn't know the BP71N and
silently uses 0, which left its files 6 samples off (CDR-4).

### 1.5 What is ripped

- Audio tracks only. On an enhanced CD the audio ends 11400 sectors before
  the data session.
- A track's pregap belongs to the track before it (the AccurateRip
  convention). The hidden track before track 1 (HTOA) is not ripped.

## 2. Verification

### 2.1 AccurateRip

- Disc IDs are computed from the audio tracks' start sectors and the audio
  lead-out; the CDDB ID uses all tracks.
- Checksums v1 and v2 skip the first 2939 samples of the first track and the
  last 2940 of the last track, as AccurateRip does.
- A track matches if either checksum equals any pressing's entry, first at
  the drive offset. If nothing matches there and the disc's pressing lines up
  at another offset, the checksums are also taken at that alignment (from a
  few extra sectors read around the track). A match there counts as accurate,
  **other pressing**; the audio is still written at the drive offset.
- The highest confidence among matches is reported.

### 2.2 Disc verdict

| Verdict | When |
|---|---|
| **Perfect** | Every track matches AccurateRip. |
| **Verified** | No track is suspicious, and the tracks AccurateRip didn't confirm read identically twice in secure mode. |
| **Check** | At least one track has suspicious positions. The verdict names the tracks. |

Per track the result is `accurate`, `accurate, other pressing`, `secure`,
`secure, not in AccurateRip's pressings` (the disc is in the database but this
track matched no pressing), or `SUSPICIOUS`.

## 3. Metadata

- **MusicBrainz**: the disc ID is computed as libdiscid does, from the audio
  session only. One request looks up the disc ID and falls back to a fuzzy
  match on the TOC. Results say which of the two they are.
- With several matching releases, the user chooses (CLI: `--release` or a
  prompt; app: a picker).
- A lookup that **fails** (network error, or a 503 after retries) is not
  treated as "unknown disc": when filing into a library, the rip stops. A disc
  MusicBrainz genuinely doesn't know is filed as unknown (§4.3).
- **Cover art** comes from the Cover Art Archive: the release's front cover at
  1200 px, else its release group's. A missing cover doesn't stop the rip.

## 4. Output

### 4.1 Audio files

- FLAC, 16-bit, 44.1 kHz, stereo, with the PCM MD5 in STREAMINFO.
- Fixed block size: STREAMINFO declares minimum = maximum block size
  (4096). Decoders use it to find sample positions, and `flac -t` flags
  every frame otherwise.
- Cover embedded as the front-cover picture.

### 4.2 Tags

Vorbis comments, named as MusicBrainz Picard names them, so library servers
group albums correctly. Empty values are left out.

| Tag | Value |
|---|---|
| `TITLE`, `ARTIST` | Track title and track artist credit |
| `ALBUM`, `ALBUMARTIST` | Release title and release artist credit |
| `TRACKNUMBER`, `TRACKTOTAL`, `TOTALTRACKS` | Position and count of audio tracks |
| `DISCNUMBER`, `DISCTOTAL`, `TOTALDISCS` | Medium position and count |
| `DATE`, `RELEASECOUNTRY`, `LABEL`, `CATALOGNUMBER`, `BARCODE` | Release details |
| `MUSICBRAINZ_TRACKID` | Recording MBID (Picard's naming) |
| `MUSICBRAINZ_RELEASETRACKID` | Track MBID |
| `MUSICBRAINZ_ALBUMID`, `MUSICBRAINZ_RELEASEGROUPID` | Release and release group |
| `MUSICBRAINZ_ARTISTID`, `MUSICBRAINZ_ALBUMARTISTID` | One value per credited artist |
| `MUSICBRAINZ_DISCID` | Always, even without a release |
| `COMPILATION=1` | When the release artist is Various Artists |

Without a release only the track numbering and the disc ID are written.

### 4.3 Library layout

- Default template: `{albumartist}/{album} ({year})/{discprefix}{track} - {title}`,
  plus `.flac`. `{discprefix}` is `2-` on multi-disc releases, so all discs
  share one album folder. Other placeholders: `{artist}`, `{disc}`.
- Unknown disc: `Unknown Artist/Unknown Album [<MusicBrainz disc ID>]/NN - Track NN.flac`.
- Every folder or file name is made safe for any filesystem a library may
  live on: `/ \ : * ? " < > |` and control characters become `_` (in values,
  so a title can't create a folder); trailing dots and spaces and leading dots
  are removed; Windows reserved names (`CON`, `COM1`, …) get a `_`; names are
  cut to 240 bytes on a character boundary; brackets left empty by a missing
  value are removed.
- `cover.jpg` (or `.png`), `rip.log` and `rip.json` go into the album folder.

### 4.4 Writing into the library

- All destinations are decided **before** reading. If any track already
  exists, the rip stops before touching the disc (CLI: `--overwrite` to
  replace).
- Files are encoded in a local temporary folder, then copied into the library
  as a hidden `.<name>.part` file and renamed into place. The copy is retried
  3 times (1 s, then 2 s apart); a final failure leaves no partial file.

*Why:* libraries usually live on network shares, and their servers scan
folders while we write. They should only ever see finished files.

### 4.5 Library servers

After filing, optionally:

- **Jellyfin**: rescan every library of type *music* (not films or shows).
  Authenticated with an API key. The Mac app only offers Jellyfin for now.
- **Navidrome**: Subsonic `startScan`, with salted-token authentication (the
  password never travels). Only admin users may start scans.

A failed rescan is reported but doesn't fail the rip: the files are already
in place.

## 5. Rip log

Every filed album gets two records of the same facts.

- **`rip.log`**, for people: tool and date (UTC), drive, read offset and where
  it came from, the pressing offset when it differs, mode, release, disc IDs,
  AccurateRip pressings; one line per track (start, length, CRC v1/v2,
  AccurateRip match, with `@offset` for another pressing, reads, C2, result);
  suspicious positions; each file's audio MD5; the verdict.
- **`rip.json`**, for tools, with `"format": 1`. Fields are only ever added;
  a change in meaning gets a new format number. `offset_source` is one of
  `manual`, `accurate-rip-drives`, `learned`, `this-disc`;
  `pressing_offset` (disc, and per AccurateRip match) is set when a pressing
  lines up elsewhere.
- The audio MD5 is the FLAC STREAMINFO MD5, so a file can be checked against
  the log years later.
- Positions are written `m:ss.ff` (minutes, seconds, CD frames of 1/75 s).
- Reads: `1`, `2`, `2+Ns` (N sectors re-read), `2+Ns slow` (some re-reads
  at low speed).

## 6. External services

| Service | Rules |
|---|---|
| All | User-Agent `CDR/<version> (+https://monk-studio.com)`. No personal data. |
| AccurateRip | Agreed with Illustrate (2026-11-10, CDR-2): free while CDR is donation-funded, a $250–500/year licence if sold. Identify via User-Agent, cache where possible, **never submit**. The drive database is cached for 30 days. |
| MusicBrainz | One request per disc. On 503, back off 2, 4 and 8 s. |
| Cover Art Archive | Thumbnails only (1200 px); originals can be tens of MB. |
| CTDB | Not used yet; awaiting the maintainer's reply (CDR-2). |

## 7. Known limitations

- Enhanced-CD AccurateRip disc IDs are unverified on a real disc.
- No cache yet for AccurateRip disc results or MusicBrainz lookups
  (AccurateRip asks for one); only its drive database is cached.
- A drive missing from AccurateRip's drive database needs 3 agreeing discs
  before its offset is learned; until then each disc stands in for it.
- Only one pressing offset is tried per disc (the first track that reveals
  one).
- The cache flush runs even on drives without an audio cache (about 7% of
  secure-mode time on a BP71N).
- HTOA, CD-Text, ISRC and pre-emphasis are not handled; pre-emphasis is only
  shown in the TOC (CDR-5).
- A track that AccurateRip knows but no pressing matches is still "Verified"
  if it reads identically twice. That covers both an unknown pressing and a
  drive that misreads consistently.
- Peak memory is 670–820 MB, mostly FLAC encoding copies (CDR-12).
- Secure mode's behaviour on damaged discs is covered by simulated-fault tests
  only.

## 8. Mac app settings

Settings (⌘,) holds what most users set once. Each setting is saved as it
changes and applies to the next rip.

- **Library**: the library folder, and the file-name template (§4.3). An
  empty template means the default; the hint shows where a sample track
  would go. Until a library is chosen, the main window offers to choose one
  too.
- **Ripping**: *Read every track twice* (secure mode, §1.3), off by default;
  and the read offset. Empty means automatic (§1.4, steps 2–4); a number is
  the offset set by hand (step 1). Anything else, or beyond ±3000 samples,
  is refused, and Rip won't start until it is fixed.
- **Servers**: a Jellyfin address and API key. The key is kept in the macOS
  Keychain (service `com.monk-studio.cdr`, account `jellyfin`), never in the
  app's settings file. *Check Connection* lists the server's music libraries
  without scanning them. Once both are set, every finished rip (any verdict)
  asks Jellyfin to rescan (§4.5); the result shows in the footer.

*Why:* the defaults are right for most discs (see `docs/vision.md`), so
these stay out of the main window.
