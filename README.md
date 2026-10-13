# Tobasu

A player for anime you already have on disk. It finds the openings and endings and skips them.

You point it at a folder of episodes. It works out which file is which episode, compares the audio across the season to find where the OP and ED are, and plays the season in mpv with those parts skipped. It also remembers where you stopped.

Windows 10/11.

## Download

Get the zip from [Releases](https://github.com/misutesu-desu/tobasu/releases/latest), extract it and run `Tobasu.exe`. mpv and ffmpeg are in the zip, nothing else to install.

The exe isn't signed, so SmartScreen will probably complain the first time (More info → Run anyway).

## Usage

**+ Add anime**, type the name, then **Browse…** to the folder and **Match**. Analysis starts on its own; a 13-episode season took about 3 seconds on my PC. Press ▶ on an episode and it plays from there to the end of the season.

Season numbers are read from file and folder names (`S02E03`, `2nd Season`, `Season 2`). Episode titles can be pulled from AniList when you add the anime, but that's optional.

If an OP or ED comes out wrong, type the right range into the table, like `1:30-3:00`. Episodes with a different ending from the rest of the season are the usual suspects.

In the player:

| Key | |
|---|---|
| `s` | auto skip on/off |
| `S` | also skip the next-episode preview after the ED |
| `Enter` | skip the OP/ED you're in, or go to the next episode |
| `Ctrl+1` – `Ctrl+4` | 1x, 2x, 2.5x, 3x |

It can also rename the files with a template, export episodes with the OP/ED cut out, and join a season into one video with a chapter per episode. The interface is in English or Turkish.

## How detection works

The first and last 6 minutes of each episode get a 32-bit audio fingerprint every 0.1 s. When two episodes are compared frame by frame, a long run of matching frames is the song they share. Each episode is checked against its 4 nearest episodes and the median of the matches wins, so you need at least 2 episodes from the same season.

## Building

`cargo build --release` with the MSVC Rust toolchain. The exe looks for the `player` and `ffmpeg` folders next to it or in a parent folder and keeps its data in `data`. `scripts/package.ps1` makes the release zip.

## License

GPL-3.0-or-later. The bundled mpv, uosc and FFmpeg are listed in [THIRD-PARTY.md](THIRD-PARTY.md).
