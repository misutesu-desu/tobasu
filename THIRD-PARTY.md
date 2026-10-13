# Third-party software

The release zip bundles the following. Tobasu itself is GPL-3.0-or-later.

## mpv

`player/mpv/` is a Windows build of [mpv](https://mpv.io) (`v0.41.0-1150-g88eb9f2e7`) made by [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake). This build is licensed under GPL-2.0-or-later because it links FFmpeg built with GPL components.

Source code:
- mpv: https://github.com/mpv-player/mpv/tree/88eb9f2e7
- build scripts and the versions of every library used: https://github.com/shinchiro/mpv-winbuild-cmake

The license text is in `player/mpv/LICENSE.txt`.

## uosc

`player/config/scripts/uosc/` and `player/config/fonts/uosc_*` are [uosc](https://github.com/tomasklaen/uosc) 5.13.0 by Tomas Klaen, under LGPL-2.1. The license text is in `player/config/scripts/uosc/LICENSE`. Source: https://github.com/tomasklaen/uosc/tree/5.13.0

## Rust crates

`Tobasu.exe` is built from the crates listed in `Cargo.lock` (wry, tao, rayon, realfft, serde, ureq and others). They are under MIT and/or Apache-2.0 licenses; see each crate on https://crates.io.

## FFmpeg

FFmpeg is not bundled. Tobasu runs the `ffmpeg` and `ffprobe` found on your PATH.
