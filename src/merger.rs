//! İşlenmiş bölümleri tek videoda birleştirir: her bölüm bir chapter, altyazılar kaydırılıp
//! tek dosyada birleştirilir, mkv font ekleri toplanır. Biçimi farklı bölümler önce referans
//! biçime NVENC ile dönüştürülür; aynı biçimdekiler yeniden kodlanmadan eklenir.
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

use crate::cutter::{self, streams};
use crate::subs;
use crate::util::{abspath, cmd, exists, last_line, path_str, Fail, Res};

pub struct Item {
    pub file: String,
    pub sub_file: Option<String>,
    pub chapter: String,
}

#[derive(Clone, PartialEq)]
struct Sig {
    vcodec: Option<String>,
    profile: Option<String>,
    w: i64,
    h: i64,
    pix: Option<String>,
    fps: Option<String>,
    n_audio: usize,
    audio: Vec<(Option<String>, Option<String>, i64)>,
}

fn st(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn sig(info: &Value, all_audio: bool) -> Sig {
    let v = streams(info, "video").into_iter().next().unwrap_or(Value::Null);
    let mut a = streams(info, "audio");
    if !all_audio {
        a.truncate(1);
    }
    Sig {
        vcodec: st(&v["codec_name"]),
        profile: st(&v["profile"]),
        w: v["width"].as_i64().unwrap_or(0),
        h: v["height"].as_i64().unwrap_or(0),
        pix: st(&v["pix_fmt"]),
        fps: st(&v["r_frame_rate"]),
        n_audio: a.len(),
        audio: a.iter().map(|x| (st(&x["codec_name"]), st(&x["sample_rate"]), x["channels"].as_i64().unwrap_or(0))).collect(),
    }
}

fn normalize(src: &str, dst: &str, r: &Sig, quality: i64) -> Res<()> {
    let enc = if r.vcodec.as_deref() == Some("hevc") { "hevc_nvenc" } else { "h264_nvenc" };
    let fps = r.fps.clone().unwrap_or_else(|| "24000/1001".into());
    let vf = format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,fps={fps},format=yuv420p", w = r.w, h = r.h);
    let mut c: Vec<String> = ["-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-i", src, "-map", "0:v:0", "-vf", &vf]
        .iter()
        .map(|x| x.to_string())
        .collect();
    c.extend(cutter::encoder_args(enc, quality));
    if r.vcodec.as_deref() == Some("h264") {
        if let Some(p) = &r.profile {
            c.extend(["-profile:v".into(), if p.to_lowercase().contains("high") { "high" } else { "main" }.into()]);
        }
    }
    for k in 0..r.n_audio {
        c.extend(["-map".into(), format!("0:a:{k}")]);
    }
    if let Some(a0) = r.audio.first() {
        c.extend(["-c:a", "aac", "-b:a", "192k", "-ar"].map(String::from));
        c.push(a0.1.clone().unwrap_or_else(|| "48000".into()));
        c.extend(["-ac".into(), a0.2.to_string()]);
    }
    c.extend(["-f".into(), "matroska".into(), dst.into()]);
    let (rc, err) = cutter::run(&c, 0.0, &|_| {})?;
    if rc != 0 {
        return Err(Fail::Msg(crate::l!("Bölüm dönüştürülemedi: {}", "Could not convert an episode: {}", last_line(&err))));
    }
    Ok(())
}

fn meta_escape(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if matches!(ch, '\\' | '=' | ';' | '#' | '\n') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn font_mime(name: &str) -> &'static str {
    match Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("ttf") | Some("ttc") => "application/x-truetype-font",
        Some("otf") => "application/vnd.ms-opentype",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn most_common<T: Clone + PartialEq>(v: &[T]) -> T {
    let mut best = (0, 0);
    for (i, x) in v.iter().enumerate() {
        let n = v.iter().filter(|y| *y == x).count();
        if n > best.1 {
            best = (i, n);
        }
    }
    v[best.0].clone()
}

/// items sırayla birleştirilir. Çıktı yolunu döner.
#[allow(clippy::too_many_arguments)]
pub fn merge(items: &[Item], dst_base: &str, title: &str, container_pref: &str, quality: i64, workdir: &str,
             status: &dyn Fn(&str), progress: &dyn Fn(f64)) -> Res<String> {
    let _ = std::fs::remove_dir_all(workdir);
    std::fs::create_dir_all(workdir)?;
    let infos: Vec<Value> = std::thread::scope(|sc| {
        let hs: Vec<_> = items.iter().map(|it| sc.spawn(|| cutter::probe(&it.file))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or(Value::Null)).collect()
    });
    let counts: Vec<usize> = infos.iter().map(|i| streams(i, "audio").len()).collect();
    let all_audio = counts.iter().all(|c| *c == counts[0]);
    let sigs: Vec<Sig> = infos.iter().map(|i| sig(i, all_audio)).collect();
    let mut r = most_common(&sigs);
    if !matches!(r.vcodec.as_deref(), Some("h264") | Some("hevc")) {
        r.vcodec = Some("h264".into());
        r.profile = Some("High".into());
    }
    let mut bad: Vec<usize> = (0..sigs.len()).filter(|&i| sigs[i] != r).collect();
    if !bad.is_empty()
        && (r.pix.as_deref() != Some("yuv420p")
            || !matches!(r.profile.as_deref(), Some("High") | Some("Main"))
            || r.audio.iter().any(|a| a.0.as_deref() != Some("aac")))
    {
        // Dönüştürülen bölümler 8 bit + AAC olacağı için referans da öyle olmalı;
        // aksi halde herkes dönüştürülür
        r.profile = Some(if r.vcodec.as_deref() == Some("h264") { "High" } else { "Main" }.into());
        r.pix = Some("yuv420p".into());
        for a in r.audio.iter_mut() {
            a.0 = Some("aac".into());
        }
        bad = (0..sigs.len()).filter(|&i| sigs[i] != r).collect();
    }
    let mut files = Vec::new();
    for (n, (it, sg)) in items.iter().zip(&sigs).enumerate() {
        if *sg == r {
            files.push(it.file.clone());
            continue;
        }
        let k = bad.iter().position(|&b| b == n).unwrap_or(0) + 1;
        status(&crate::l!("Biçimi farklı bölüm dönüştürülüyor ({k}/{})", "Converting episode with a different format ({k}/{})", bad.len()));
        let out = format!("{workdir}\\norm_{n:03}.mkv");
        normalize(&it.file, &out, &r, quality)?;
        files.push(out);
        progress(0.5 * k as f64 / bad.len() as f64);
    }

    let durs: Vec<f64> = files
        .iter()
        .map(|f| cutter::probe(f)["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0))
        .collect();
    status(&crate::l!("Altyazı ve chapter'lar hazırlanıyor", "Preparing subtitles and chapters"));
    let meta = format!("{workdir}\\chapters.txt");
    let mut m = format!(";FFMETADATA1\ntitle={}\n", meta_escape(title));
    let mut t = 0.0;
    for (it, d) in items.iter().zip(&durs) {
        m += &format!("[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle={}\n", (t * 1000.0) as i64, ((t + d) * 1000.0) as i64, meta_escape(&it.chapter));
        t += d;
    }
    std::fs::write(&meta, m)?;

    let has_sub = |it: &Item| it.sub_file.as_deref().is_some_and(exists);
    let mut sub_path = None;
    if items.iter().any(has_sub) {
        let mut parts = Vec::new();
        let mut off = 0.0;
        for (it, d) in items.iter().zip(&durs) {
            if has_sub(it) {
                parts.push((subs::parse(it.sub_file.as_deref().unwrap())?, off));
            }
            off += d;
        }
        let merged = subs::merge(&parts);
        let p = format!("{workdir}\\altyazi{}", subs::ext_for(&merged));
        subs::write(&merged, &p)?;
        sub_path = Some(p);
    }

    let mut fonts: BTreeMap<String, String> = BTreeMap::new();
    for (n, (it, info)) in items.iter().zip(&infos).enumerate() {
        if streams(info, "attachment").is_empty() {
            continue;
        }
        let d = format!("{workdir}\\ek_{n:03}");
        std::fs::create_dir_all(&d)?;
        let _ = cmd("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-dump_attachment:t", "", "-i", &abspath(&it.file)])
            .current_dir(&d)
            .output();
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                fonts.entry(e.file_name().to_string_lossy().to_lowercase()).or_insert_with(|| path_str(&e.path()));
            }
        }
    }

    let mut container = cutter::container_for(container_pref, sub_path.is_some(), !fonts.is_empty());
    if container == "mp4" && !matches!(r.vcodec.as_deref(), Some("h264") | Some("hevc")) {
        container = "mkv".into();
    }
    let dst = format!("{dst_base}.{container}");
    let tmp = format!("{dst_base}.part.{container}");
    if let Some(parent) = Path::new(&dst).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lst = format!("{workdir}\\liste.txt");
    let body: String = files.iter().map(|p| format!("file '{}'\n", abspath(p).replace('\'', "'\\''"))).collect();
    std::fs::write(&lst, body)?;
    let mut c: Vec<String> = ["-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-progress", "pipe:1", "-nostats", "-f", "concat",
                              "-safe", "0", "-i", &lst, "-i", &meta].iter().map(|x| x.to_string()).collect();
    if let Some(sp) = &sub_path {
        c.extend(["-i".into(), sp.clone()]);
    }
    c.extend(["-map".into(), "0:v:0".into()]);
    for k in 0..r.n_audio {
        c.extend(["-map".into(), format!("0:a:{k}")]);
    }
    c.extend(["-c:v", "copy", "-c:a", "copy", "-map_metadata", "1", "-map_chapters", "1"].map(String::from));
    if sub_path.is_some() {
        c.extend(["-map", "2:0", "-c:s", if container == "mp4" { "mov_text" } else { "copy" }, "-metadata:s:s:0", "language=tur",
                  "-disposition:s:0", "default"].map(String::from));
    }
    if container == "mkv" {
        for (i, (name, path)) in fonts.iter().enumerate() {
            let fname = Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            c.extend(["-attach".into(), path.clone(), format!("-metadata:s:t:{i}"), format!("mimetype={}", font_mime(name)),
                      format!("-metadata:s:t:{i}"), format!("filename={fname}")]);
        }
    } else {
        c.extend(["-movflags".into(), "+faststart".into()]);
    }
    c.extend(["-f".into(), if container == "mkv" { "matroska" } else { "mp4" }.into(), tmp.clone()]);
    status(&crate::l!("Bölümler birleştiriliyor", "Joining episodes"));
    let base = if bad.is_empty() { 0.0 } else { 0.5 };
    let (rc, err) = cutter::run(&c, durs.iter().sum(), &|v| progress(base + (1.0 - base) * v))?;
    let _ = std::fs::remove_dir_all(workdir);
    if rc != 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err(Fail::Msg(crate::l!("Birleştirme hatası: {}", "Join error: {}", last_line(&err))));
    }
    let _ = std::fs::remove_file(&dst);
    std::fs::rename(&tmp, &dst)?;
    Ok(dst)
}
