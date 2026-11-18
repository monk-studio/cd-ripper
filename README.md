# CDR

> **Insert a CD. Get a perfect copy in your own library.**

CDR is a CD ripper for macOS. Insert a disc and it finds the album on
MusicBrainz, reads every track, checks it against AccurateRip and files tagged
FLAC into your music library, then tells Jellyfin to pick it up. You don't
need to know about read offsets or C2 errors to get a perfect copy.

## Features

- **Verified rips**: every track is checked against AccurateRip, and the
  album gets a verdict: Perfect, Verified, or which tracks to check.
- **Fast**: reads at the drive's full speed and only rereads when a track
  can't be confirmed. "Read every track twice" is there for discs AccurateRip
  doesn't know.
- **Read offset handled for you**: from AccurateRip's list of drives, or worked
  out from the discs you rip.
- **Your library, open files**: FLAC with standard tags and cover art, named
  like MusicBrainz Picard does (`Artist/Album (Year)/01 - Title.flac`), with
  a `rip.log` you can check later.
- **Jellyfin**: rescans its music libraries after each rip. The API key is
  kept in your Keychain.
- **Pick the right release** when MusicBrainz knows several pressings, or type
  the tags in for a disc it doesn't know.
- English and Chinese.

## Install

With Homebrew (macOS 14+):

```sh
brew tap monk-studio/cd-ripper https://gitlab.com/monk-studio/cd-ripper
brew install --cask monk-studio/cd-ripper/cdr
```

Or download the DMG from
[Releases](https://gitlab.com/monk-studio/cd-ripper/-/releases). It is signed
and notarized.

You'll need a USB CD, DVD or Blu-ray drive: Macs don't have one built in.
CDR is tested on the LG BP71N; other drives work too.

On first use, macOS asks whether CDR may read removable volumes; allow it.
If macOS opens the Music app when you insert a CD, CDR tells you how to turn
that off, since it would read the disc at the same time.

## From source

Requires macOS 14+ and Rust.

```sh
cd crates/cdr-app && cargo run     # the app
cargo run --release -p cdr-cli -- --out ~/Music/Library   # the command-line ripper
```

`cdr --help` lists the CLI's options (offset, template, secure mode, Jellyfin
and Navidrome).

## Layout

```
crates/cdr-core   reading, verification, tagging, library output (shared)
crates/cdr-cli    the `cdr` command
crates/cdr-app    the Mac app (NSH)
docs/vision.md    what CDR promises
docs/spec.md      what the ripper does and why
release.sh        notarized build, GitLab release, Homebrew cask
Casks/cdr.rb      Homebrew cask (this repo is also the tap)
```
