//! OP/ED aralıklarını videodan çıkarır. Kod çözme CUDA, kodlama NVENC ile yapılır.
//! Tüm ses kanalları, (önceden kaydırılmış) altyazı ve mkv font ekleri korunur.
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};

use crate::util::{abspath, cmd, last_line, Fail, Res};

const MIN_SEG: f64 = 0.5;
const TEXT_SUBS: &[&str] = &["ass", "ssa", "subrip", "srt", "mov_text", "webvtt", "text"];
const TR_LANGS: &[&str] = &["tur", "tr", "turkish"];
// Hızlı modda parça başı anahtar kareye yaslanır: geriye (OP/ED'nin sonundan biraz kalır)
// en fazla SNAP_BACK, ileriye (içerikten biraz gider) en fazla SNAP_FWD saniye.
const SNAP_BACK: f64 = 2.0;
const SNAP_FWD: f64 = 0.7;
const GPU_PIX: &[&str] = &["yuv420p", "nv12", "yuvj420p"];

// ---------------------------------------------------------------- süreç takibi (iptal için)

static PROCS: Mutex<Vec<Arc<Mutex<Child>>>> = Mutex::new(Vec::new());

pub fn kill_all() {
    for p in PROCS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        let _ = p.lock().unwrap_or_else(|e| e.into_inner()).kill();
    }
}

/// ffmpeg'i çalıştırır; -progress çıktısından ilerleme bildirir. (çıkış kodu, stderr) döner.
pub fn run(args: &[String], total: f64, progress: &dyn Fn(f64)) -> Res<(i32, String)> {
    let mut child = cmd("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Fail::Msg(crate::l!("ffmpeg başlatılamadı (PATH'te mi?): {e}", "Could not start ffmpeg (is it on PATH?): {e}")))?;
    let stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let child = Arc::new(Mutex::new(child));
    PROCS.lock().unwrap_or_else(|e| e.into_inner()).push(child.clone());
    if let Some(out) = stdout {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(v) = line.strip_prefix("out_time_us=") {
                if total > 0.0 {
                    if let Ok(us) = v.trim().parse::<i64>() {
                        progress((us as f64 / 1e6 / total).clamp(0.0, 1.0));
                    }
                }
            }
        }
    }
    let code = child.lock().unwrap_or_else(|e| e.into_inner()).wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    PROCS.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| !Arc::ptr_eq(p, &child));
    Ok((code, err_thread.join().unwrap_or_default()))
}

// ---------------------------------------------------------------- ffprobe

pub fn probe(path: &str) -> Value {
    let out = cmd("ffprobe").args(["-v", "error", "-show_streams", "-show_format", "-of", "json", path]).output();
    out.ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_else(|| serde_json::json!({"streams": [], "format": {}}))
}

pub fn all_streams(info: &Value) -> Vec<Value> {
    info["streams"].as_array().cloned().unwrap_or_default()
}

pub fn streams(info: &Value, kind: &str) -> Vec<Value> {
    all_streams(info)
        .into_iter()
        .filter(|s| s["codec_type"] == kind)
        .filter(|s| kind != "video" || !truthy(&s["disposition"]["attached_pic"]))
        .collect()
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => false,
    }
}

/// Akışın tüm akışlar içindeki sırası (ffmpeg -map 0:N için).
fn abs_index(info: &Value, s: &Value) -> usize {
    all_streams(info).iter().position(|x| x == s).unwrap_or(0)
}

/// Türkçe etiketli metin altyazı öncelikli; (tür içi sıra, codec).
pub fn pick_sub_stream(info: &Value) -> Option<(usize, String)> {
    let mut subs: Vec<(usize, Value)> = streams(info, "subtitle")
        .into_iter()
        .enumerate()
        .filter(|(_, s)| TEXT_SUBS.contains(&s["codec_name"].as_str().unwrap_or("")))
        .collect();
    subs.sort_by_key(|(i, s)| {
        let lang = s["tags"]["language"].as_str().unwrap_or("").to_lowercase();
        (!TR_LANGS.contains(&lang.as_str()), !truthy(&s["disposition"]["default"]), *i)
    });
    subs.first().map(|(i, s)| (*i, s["codec_name"].as_str().unwrap_or("").to_string()))
}

pub fn extract_sub(src: &str, rel_index: usize, codec: &str, out_base: &str) -> Option<String> {
    let ass = codec == "ass" || codec == "ssa";
    let out = format!("{out_base}{}", if ass { ".ass" } else { ".srt" });
    let ok = cmd("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-i", src, "-map", &format!("0:s:{rel_index}"),
               "-c:s", if ass { "ass" } else { "srt" }, &out])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    (ok && std::path::Path::new(&out).exists()).then_some(out)
}

/// cuts: silinecek aralıklar. Kalan aralıkları döner.
pub fn keep_segments(duration: f64, cuts: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut c: Vec<(f64, f64)> = cuts.iter().filter(|(s, e)| e > s).map(|&(s, e)| (s.max(0.0), e.min(duration))).collect();
    c.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (s, e) in c {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let mut keep = Vec::new();
    let mut pos = 0.0;
    for (s, e) in merged {
        if s - pos >= MIN_SEG {
            keep.push((pos, s));
        }
        pos = e;
    }
    if duration - pos >= MIN_SEG {
        keep.push((pos, duration));
    }
    keep
}

/// t çevresindeki video anahtar karelerinin zamanları (çözmeden, paket bayraklarından).
fn keyframes_near(src: &str, t: f64) -> Vec<f64> {
    let w = SNAP_BACK + 2.0;
    let out = cmd("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-read_intervals", &format!("{:.3}%{:.3}", (t - w).max(0.0), t + w),
               "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0", src])
        .output();
    let Ok(o) = out else { return vec![] };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let mut p = l.split(',');
            let (ts, fl) = (p.next()?, p.next()?);
            if fl.contains('K') { ts.parse().ok() } else { None }
        })
        .collect()
}

/// Kopyalanacak her parçanın başını en yakın anahtar kareye yaslar.
/// Uygun anahtar kare yoksa None (bölüm kodlanarak kesilir).
pub fn plan_fast(src: &str, keep: &[(f64, f64)]) -> Option<Vec<(f64, f64)>> {
    // Parça başlarının anahtar kareleri paralel sorgulanır
    let kfs: Vec<Vec<f64>> = std::thread::scope(|sc| {
        let hs: Vec<_> = keep
            .iter()
            .map(|&(a, _)| sc.spawn(move || if a > 0.05 { keyframes_near(src, a) } else { vec![] }))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    let mut planned = Vec::new();
    for (&(mut a, b), kf) in keep.iter().zip(kfs) {
        if a > 0.05 {
            let near: Vec<f64> = kf.into_iter().filter(|k| (-SNAP_BACK..=SNAP_FWD).contains(&(k - a))).collect();
            if near.is_empty() {
                return None;
            }
            a = near.into_iter().min_by(|x, y| (x - a).abs().total_cmp(&(y - a).abs()))?;
        }
        if b - a >= MIN_SEG {
            planned.push((a, b));
        }
    }
    Some(planned)
}

fn s<T: ToString>(v: T) -> String {
    v.to_string()
}

fn base_args() -> Vec<String> {
    ["-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-progress", "pipe:1", "-nostats"].map(s).to_vec()
}

fn fmt_name(container: &str) -> &'static str {
    if container == "mkv" { "matroska" } else { "mp4" }
}

/// Concat demuxer'ın inpoint/outpoint'i ile aynı dosyadan parçaları kopyalar.
fn build_fast(src: &str, info: &Value, dst: &str, segs: &[(f64, f64)], sub: Option<&str>, container: &str, workdir: &str) -> Res<Vec<String>> {
    let lst = format!("{workdir}\\parcalar.txt");
    let path = abspath(src).replace('\'', "'\\''");
    let mut body = String::new();
    for (a, b) in segs {
        body += &format!("file '{path}'\ninpoint {a:.6}\noutpoint {b:.6}\n");
    }
    std::fs::write(&lst, body)?;
    let mut c = base_args();
    c.extend(["-f", "concat", "-safe", "0", "-i", &lst].map(s));
    let mut n = 1;
    if let Some(sp) = sub {
        c.extend(["-i".into(), sp.into()]);
        n += 1;
    }
    let attach = container == "mkv" && !streams(info, "attachment").is_empty();
    if attach {
        c.extend(["-i".into(), src.into()]);
    }
    c.extend(["-map", "0:v:0", "-map", "0:a?", "-c:v", "copy", "-c:a", "copy"].map(s));
    if sub.is_some() {
        c.extend(["-map", "1:0", "-c:s", if container == "mp4" { "mov_text" } else { "copy" }, "-metadata:s:s:0", "language=tur", "-disposition:s:0", "default"].map(s));
    }
    if attach {
        c.extend(["-map".into(), format!("{n}:t?"), "-c:t".into(), "copy".into()]);
    }
    if container == "mp4" {
        c.extend(["-movflags", "+faststart"].map(s));
    }
    c.extend(["-avoid_negative_ts", "make_zero", "-f", fmt_name(container), dst].map(s));
    Ok(c)
}

pub fn encoder_args(encoder: &str, quality: i64) -> Vec<String> {
    let q = quality.to_string();
    if encoder == "h264_nvenc" || encoder == "hevc_nvenc" {
        ["-c:v", encoder, "-preset", "p4", "-tune", "hq", "-rc", "vbr", "-cq", &q, "-b:v", "0", "-pix_fmt", "yuv420p"].map(s).to_vec()
    } else {
        ["-c:v", "libx264", "-preset", "veryfast", "-crf", &q, "-pix_fmt", "yuv420p"].map(s).to_vec()
    }
}

pub fn container_for(pref: &str, has_sub: bool, has_attach: bool) -> String {
    if pref == "mkv" || pref == "mp4" {
        return pref.into();
    }
    if has_sub || has_attach { "mkv".into() } else { "mp4".into() }
}

fn sub_and_attach_args(info: &Value, sub: Option<&str>, sub_input: usize, container: &str) -> Vec<String> {
    let mut a = Vec::new();
    if sub.is_some() {
        a.extend(["-map".to_string(), format!("{sub_input}:0")]);
        a.extend(["-c:s", if container == "mp4" { "mov_text" } else { "copy" }, "-metadata:s:s:0", "language=tur", "-disposition:s:0", "default"].map(s));
    }
    if container == "mkv" && !streams(info, "attachment").is_empty() {
        a.extend(["-map", "0:t?", "-c:t", "copy"].map(s));
    }
    a
}

/// gpu_frames: kareler çözümden kodlamaya kadar GPU belleğinde kalır (en hızlı yol).
#[allow(clippy::too_many_arguments)]
fn build_cut(src: &str, info: &Value, dst: &str, segs: &[(f64, f64)], encoder: &str, quality: i64, sub: Option<&str>, container: &str, gpu_frames: bool) -> Vec<String> {
    let v = streams(info, "video");
    let audios = streams(info, "audio");
    let vidx = v.first().map(|x| abs_index(info, x)).unwrap_or(0);
    let mut c = base_args();
    if encoder.ends_with("_nvenc") {
        c.extend(["-hwaccel", "cuda"].map(s));
        if gpu_frames {
            c.extend(["-hwaccel_output_format", "cuda"].map(s));
        }
    }
    c.extend(["-i".into(), src.into()]);
    if let Some(sp) = sub {
        c.extend(["-i".into(), sp.into()]);
    }
    let mut parts = Vec::new();
    let mut labels = String::new();
    for (i, (a, b)) in segs.iter().enumerate() {
        parts.push(format!("[0:{vidx}]trim=start={a:.3}:end={b:.3},setpts=PTS-STARTPTS[v{i}]"));
        labels += &format!("[v{i}]");
        for k in 0..audios.len() {
            parts.push(format!("[0:a:{k}]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS[a{i}_{k}]"));
            labels += &format!("[a{i}_{k}]");
        }
    }
    let outs: String = std::iter::once("[vo]".to_string()).chain((0..audios.len()).map(|k| format!("[ao{k}]"))).collect();
    parts.push(format!("{labels}concat=n={}:v=1:a={}{outs}", segs.len(), audios.len()));
    c.extend(["-filter_complex".into(), parts.join(";"), "-map".into(), "[vo]".into()]);
    for (k, a) in audios.iter().enumerate() {
        c.extend(["-map".into(), format!("[ao{k}]")]);
        if let Some(lang) = a["tags"]["language"].as_str() {
            c.extend([format!("-metadata:s:a:{k}"), format!("language={lang}")]);
        }
    }
    if !audios.is_empty() {
        c.extend(["-c:a", "aac", "-b:a", "192k"].map(s));
    }
    let mut enc = encoder_args(encoder, quality);
    if gpu_frames {
        enc.retain(|x| x != "-pix_fmt" && x != "yuv420p");
    }
    c.extend(enc);
    c.extend(sub_and_attach_args(info, sub, 1, container));
    if container == "mp4" {
        c.extend(["-movflags", "+faststart"].map(s));
    }
    c.extend(["-f", fmt_name(container), dst].map(s));
    c
}

fn build_copy(src: &str, info: &Value, dst: &str, sub: Option<&str>, container: &str) -> Vec<String> {
    let mut c = base_args();
    c.extend(["-i".into(), src.into()]);
    if let Some(sp) = sub {
        c.extend(["-i".into(), sp.into()]);
    }
    let v = streams(info, "video");
    let vi = v.first().map(|x| abs_index(info, x)).unwrap_or(0);
    c.extend(["-map".into(), format!("0:{vi}")]);
    c.extend(["-map", "0:a?", "-c:v", "copy", "-c:a", "copy"].map(s));
    c.extend(sub_and_attach_args(info, sub, 1, container));
    if container == "mp4" {
        c.extend(["-movflags", "+faststart"].map(s));
    }
    c.extend(["-f", fmt_name(container), dst].map(s));
    c
}

pub struct CutJob<'a> {
    pub src: &'a str,
    pub dst_base: &'a str,
    pub duration: f64,
    pub cuts: &'a [(f64, f64)],
    pub encoder: &'a str,
    pub quality: i64,
    pub container_pref: &'a str,
    pub sub: Option<&'a str>,
    pub force_encode: bool,
    pub fast_segs: Option<&'a [(f64, f64)]>,
    pub workdir: &'a str,
}

/// Kesip yazar. cuts boşsa (ve force_encode yoksa) yeniden kodlamadan kopyalar.
/// NVENC hata verirse CPU'ya düşer. (çıktı yolu, kullanılan kodlayıcı) döner.
pub fn cut(j: &CutJob, progress: &dyn Fn(f64)) -> Res<(String, String)> {
    let info = probe(j.src);
    let mut container = container_for(j.container_pref, j.sub.is_some(), !streams(&info, "attachment").is_empty());
    let mut dst = format!("{}.{container}", j.dst_base);
    if let Some(parent) = std::path::Path::new(&dst).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = format!("{}.part.{container}", j.dst_base);
    let segs = if j.cuts.is_empty() { vec![(0.0, j.duration)] } else { keep_segments(j.duration, j.cuts) };
    if segs.is_empty() {
        return Err(crate::l!("Kesimden sonra video kalmıyor, OP/ED zamanlarını kontrol et", "Nothing is left after cutting, check the OP/ED times").into());
    }
    let total: f64 = segs.iter().map(|(a, b)| b - a).sum();
    let used;
    let (mut rc, mut err);
    if let (false, Some(fast), false) = (j.cuts.is_empty(), j.fast_segs, j.force_encode) {
        used = crate::l!("hızlı (kopya)", "fast (copy)");
        std::fs::create_dir_all(j.workdir)?;
        let ft: f64 = fast.iter().map(|(a, b)| b - a).sum();
        (rc, err) = run(&build_fast(j.src, &info, &tmp, fast, j.sub, &container, j.workdir)?, ft, progress)?;
        if rc != 0 && container == "mp4" {
            container = "mkv".into();
            dst = format!("{}.mkv", j.dst_base);
            tmp = format!("{}.part.mkv", j.dst_base);
            (rc, err) = run(&build_fast(j.src, &info, &tmp, fast, j.sub, &container, j.workdir)?, ft, progress)?;
        }
    } else if j.cuts.is_empty() && !j.force_encode {
        used = crate::l!("kopya", "copy");
        (rc, err) = run(&build_copy(j.src, &info, &tmp, j.sub, &container), j.duration, progress)?;
        if rc != 0 && container == "mp4" {
            // Kaynağın kodeki mp4'e sığmıyorsa mkv'ye geç
            container = "mkv".into();
            dst = format!("{}.mkv", j.dst_base);
            tmp = format!("{}.part.mkv", j.dst_base);
            (rc, err) = run(&build_copy(j.src, &info, &tmp, j.sub, &container), j.duration, progress)?;
        }
    } else {
        let mut u = j.encoder.to_string();
        let v = streams(&info, "video");
        let gpu = j.encoder.ends_with("_nvenc") && v.first().is_some_and(|x| GPU_PIX.contains(&x["pix_fmt"].as_str().unwrap_or("")));
        (rc, err) = run(&build_cut(j.src, &info, &tmp, &segs, j.encoder, j.quality, j.sub, &container, gpu), total, progress)?;
        if rc != 0 && gpu {
            (rc, err) = run(&build_cut(j.src, &info, &tmp, &segs, j.encoder, j.quality, j.sub, &container, false), total, progress)?;
        }
        if rc != 0 && j.encoder != "libx264" {
            u = crate::l!("libx264 (NVENC başarısız)", "libx264 (NVENC failed)");
            (rc, err) = run(&build_cut(j.src, &info, &tmp, &segs, "libx264", j.quality, j.sub, &container, false), total, progress)?;
        }
        used = u;
    }
    if rc != 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err(Fail::Msg(crate::l!("ffmpeg hatası: {}", "ffmpeg error: {}", last_line(&err))));
    }
    let _ = std::fs::remove_file(&dst);
    std::fs::rename(&tmp, &dst)?;
    Ok((dst, used))
}

#[cfg(test)]
mod tests {
    use super::keep_segments;

    #[test]
    fn segments() {
        assert_eq!(keep_segments(100.0, &[(10.0, 20.0), (90.0, 100.0)]), vec![(0.0, 10.0), (20.0, 90.0)]);
        assert_eq!(keep_segments(100.0, &[(0.0, 20.0), (15.0, 30.0)]), vec![(30.0, 100.0)]);
        assert_eq!(keep_segments(100.0, &[(0.2, 99.8)]), Vec::<(f64, f64)>::new());
    }
}
