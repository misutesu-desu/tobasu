# Tobasu

Tobasu (飛ばす, "to skip") is a Windows app for watching an anime folder you already have on disk. It finds the opening and ending of every episode by comparing their audio, then plays the season in mpv and skips them for you.

It does not download anything. Point it at a folder, it matches the files to seasons and episodes, and you press play.

[Türkçe](#türkçe)

## What it does

- Finds OP/ED ranges offline. A 13-episode season takes about 3 seconds on my machine. Wrong range? Type `1:30-3:00` into the table and your value wins.
- Plays the season in mpv with OP and ED marked on the seek bar, skipped automatically if you want. Optionally skips the "next episode" preview after the ED too.
- Remembers where you stopped in each episode and shows a **Continue** button.
- Renames files with a template like `{anime} - S{s:02}E{b:02} - {title}` (undoable).
- Exports copies with OP/ED cut out, or joins a whole season into one video with one chapter per episode. Subtitles are shifted to match the cuts and mkv fonts are kept.
- Turkish and English interface.

## Install

1. Download `Tobasu-<version>-windows-x64.zip` from [Releases](https://github.com/misutesu-desu/tobasu/releases/latest) and extract it anywhere.
2. Make sure `ffmpeg` and `ffprobe` are on your PATH. If they aren't: `winget install Gyan.FFmpeg`
3. Run `Tobasu.exe`.

The zip includes mpv. The window uses WebView2, which ships with Windows 11 and current Windows 10.

The build is not code-signed yet, so SmartScreen may warn you the first time.

## Using it

1. **+ Add anime**, type the name, Enter. Nothing is fetched from the internet unless you ask for episode titles from AniList/MyAnimeList (optional and slow).
2. **Browse…** to the folder, then **Match**. Season numbers are read from file and folder names (`S02E03`, `2nd Season`, `Season 2`, `2. Sezon`). Analysis starts on its own.
3. Press **▶** on an episode to play from there to the end of the season.

### Player keys

| Key | |
|---|---|
| `s` | toggle auto skip |
| `S` | toggle preview skip |
| `Enter` | skip the OP/ED you're in, otherwise next episode |
| `Ctrl+1` … `Ctrl+4` | speed 1x / 2x / 2.5x / 3x |
| `<` `>` | previous / next episode |

The last speed you used is kept between episodes and sessions.

## How OP/ED detection works

For the first and last 6 minutes of each episode, Tobasu builds a 32-bit audio fingerprint every 0.1 s (Haitsma–Kalker style). Two episodes are compared frame by frame; a long diagonal run of matching frames is music they share, which is the OP or ED. Each episode is compared with its 4 nearest episodes and the median of the agreeing matches is used.

This needs at least 2 episodes of the same season. Episodes whose OP or ED is different from the rest of the season (special endings, first episodes without an OP) can come out wrong or empty; fix those by hand.

## Export modes

- **Fast** (default): cuts without re-encoding, about a second per episode. Cut points snap to the nearest keyframe, so up to 2 s of the OP/ED end can remain.
- **Exact**: frame-accurate. Decoding and encoding stay on the GPU (CUDA + NVENC); falls back to x264 if NVENC isn't available.

## Where things are stored

Everything lives next to the exe in `data/`: the library, watch progress, and fingerprint cache. Nothing is sent anywhere except the optional AniList/Jikan lookups.

## Building

Needs Rust (MSVC toolchain).

```bash
cargo build --release
```

The exe looks for a `player` folder next to it or in a parent folder, so running `target\release\Tobasu.exe` from the repo works if you put mpv in `player\mpv\`.

## License

GPL-3.0-or-later. The release zip also contains mpv and uosc; see [THIRD-PARTY.md](THIRD-PARTY.md).

## Türkçe

Tobasu, bilgisayarındaki anime klasörünü izlemek için bir Windows uygulaması. Bölümlerin seslerini karşılaştırıp her bölümün OP ve ED'sini bulur, sezonu mpv'de oynatır ve bunları senin yerine atlar. İnternetten bir şey indirmez.

**Kurulum:** [Releases](https://github.com/misutesu-desu/tobasu/releases/latest) sayfasından zip'i indir ve aç. `ffmpeg` PATH'te olmalı (`winget install Gyan.FFmpeg`). `Tobasu.exe`'yi çalıştır. Arayüz dili ilk açılışta sistem diline göre seçilir, Ayarlar'dan değiştirilebilir.

**Kullanım:** **+ Anime ekle** ile adı yaz, **Gözat…** ile klasörü seç, **Eşleştir**'e bas. Analiz kendiliğinden başlar. Satırdaki **▶** o bölümden sezon sonuna kadar oynatır. Yanlış bulunan aralığı tabloda `1:30-3:00` diye düzeltebilirsin.

Oynatıcıda `s` otomatik atlamayı, `S` bölüm sonu özetini atlamayı açıp kapatır; `Enter` içinde bulunduğun OP/ED'yi hemen geçer.
