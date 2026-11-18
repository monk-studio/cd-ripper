# CDR

## Vision

"Insert a CD. Get a perfect copy in your own library." `docs/vision.md` says
what that promises (accurate, fast, intuitive) and how to use it to decide
unclear choices.

## Behaviour spec

`docs/spec.md` says what the ripper does and why: reading, verification,
offset, verdicts, output, logs, external services, known limitations. Read it
before changing behaviour, and update it in the same commit when you do.

## Releasing the Mac app

`./release.sh` releases the version in `crates/cdr-app/nsh.json` (bump it
first). It runs `asc publish-web cdr --skip-upload` (the `asc` CLI,
`~/@monk-studio/asc`), which packages the app with `nsh package`, signs it
with Developer ID and the hardened runtime, and notarizes and staples the app
and its DMG with the App Store Connect API key from `asc login`. Then it
publishes the DMG as GitHub release `v<version>` (`gh`) and updates
`Casks/cdr.rb` in the Homebrew tap, github.com/monk-studio/homebrew-tap,
checked out at `../homebrew-tap`. The repo is public on GitHub
(github.com/monk-studio/cd-ripper); use `gh` for it.

The bundle id is `com.monk-studio.cdr`; don't change it after a release,
since permissions and settings are tied to it.

## Reference sources

Other rippers' source, for studying how they solve the same problems. Read
for ideas only: XLD's files say "All rights reserved", so copy no code.

- **XLD 20250302** (build 157.2): `~/oss/sourceforge/xld`. The same version
  as the XLD.app we benchmark against. Ripping is in `XLD/XLDCDDABackend.c`
  (drive I/O), `XLD/XLDSecureRipperEngine.m` (secure mode) and
  `XLD/XLDController.m` (offset list lookup).
- **CUETools** (CTDB client): `~/oss/github/gchudov/cuetools.net`. The CTDB
  lookup protocol is in `CUETools.CTDB/CUEToolsDB.cs`.
