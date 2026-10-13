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

`ffmpeg/` is FFmpeg `n9.0.2-27-g7e93014c5b`, the shared GPL build from [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds), under GPL-3.0-or-later. The license text is in `ffmpeg/LICENSE.txt`.

Source code:
- FFmpeg: https://github.com/FFmpeg/FFmpeg/tree/7e93014c5b
- build scripts and library versions: https://github.com/BtbN/FFmpeg-Builds
