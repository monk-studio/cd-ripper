# CDR

## Behaviour spec

`docs/spec.md` says what the ripper does and why: reading, verification,
offset, verdicts, output, logs, external services, known limitations. Read it
before changing behaviour, and update it in the same commit when you do.

## Releasing the Mac app

`asc publish-web cdr` (the `asc` CLI, `~/@monk-studio/asc`) packages the app
with `nsh package`, signs it with Developer ID and the hardened runtime,
notarizes and staples the app and its DMG with the App Store Connect API key
from `asc login`, and uploads the DMG to the CDN
(`cdn.monk-studio.com/cdr/downloads/`). `--skip-upload` stops after the
notarized DMG. The bundle id is `com.monk-studio.cdr`; don't change it after a
release, since permissions and settings are tied to it.

## Reference sources

Other rippers' source, for studying how they solve the same problems. Read
for ideas only: XLD's files say "All rights reserved", so copy no code.

- **XLD 20250302** (build 157.2): `~/oss/sourceforge/xld`. The same version
  as the XLD.app we benchmark against. Ripping is in `XLD/XLDCDDABackend.c`
  (drive I/O), `XLD/XLDSecureRipperEngine.m` (secure mode) and
  `XLD/XLDController.m` (offset list lookup).
- **CUETools** (CTDB client): `~/oss/github/gchudov/cuetools.net`. The CTDB
  lookup protocol is in `CUETools.CTDB/CUEToolsDB.cs`.
