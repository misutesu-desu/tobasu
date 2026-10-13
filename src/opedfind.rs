//! Aynı sezondaki bölümlerin seslerini karşılaştırarak OP/ED aralığını bulur.
//!
//! Her bölümün ilk ve son birkaç dakikası için Haitsma-Kalker tarzı 32 bitlik ses parmak izi
//! çıkarılır (0.1 sn'lik kareler, her kare tek bir u32). İki kare arasındaki Hamming uzaklığı
//! tek bir popcount'tur; köşegenler boyunca uzun süre benzer kalan bölge ortak müziktir.
use rayon::prelude::*;
use realfft::RealFftPlanner;
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::util::cmd;

const SR: usize = 8000;
const NFFT: usize = 2048;
const HOP: usize = 800; // 0.1 sn
const FRAME_SEC: f64 = HOP as f64 / SR as f64;
const WINDOW_SEC: f64 = 360.0; // OP için ilk 6 dk, ED için son 6 dk aranır
const MIN_FRAMES: usize = 200; // 20 sn
const MIN_LEN: f64 = 20.0;
const MAX_LEN: f64 = 180.0;
const HAM_THRESH: u32 = 11; // 32 bitten en fazla bu kadar farklı kare "eşleşir"
const SMOOTH: usize = 21; // kare (~2 sn) çoğunluk penceresi

pub struct Fp {
    pub dur: f64,
    pub ed_off: f64,
    pub op: Vec<u32>,
    pub ed: Vec<u32>,
}

pub type Range = (f64, f64);

pub fn probe_duration(path: &str) -> f64 {
    let out = cmd("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1", path])
        .output();
    out.ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn decode(path: &str, start: f64, dur: f64) -> Vec<f32> {
    let child = cmd("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-ss", &format!("{start:.3}"), "-t",
               &format!("{dur:.3}"), "-i", path, "-vn", "-ac", "1", "-ar", &SR.to_string(), "-f", "s16le", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut buf = Vec::new();
    if let Ok(mut c) = child {
        if let Some(mut so) = c.stdout.take() {
            let _ = so.read_to_end(&mut buf);
        }
        let _ = c.wait();
    }
    buf.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
}

struct Bands {
    window: Vec<f64>,
    ranges: Vec<(usize, usize)>,
}

fn bands() -> &'static Bands {
    static B: std::sync::OnceLock<Bands> = std::sync::OnceLock::new();
    B.get_or_init(|| {
        let (lo, hi) = (300f64.log10(), 2000f64.log10());
        let edges: Vec<f64> = (0..34).map(|i| 10f64.powf(lo + (hi - lo) * i as f64 / 33.0)).collect();
        let bin = |k: usize| k as f64 * SR as f64 / NFFT as f64;
        let ranges = (0..33)
            .map(|i| {
                let a = (0..=NFFT / 2).find(|&k| bin(k) >= edges[i]).unwrap_or(NFFT / 2 + 1);
                let b = (0..=NFFT / 2).find(|&k| bin(k) >= edges[i + 1]).unwrap_or(NFFT / 2 + 1);
                (a, b)
            })
            .collect();
        // numpy.hanning: 0.5 - 0.5 cos(2πn/(N-1))
        let window = (0..NFFT)
            .map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (NFFT - 1) as f64).cos())
            .collect();
        Bands { window, ranges }
    })
}

fn bits(x: &[f32]) -> Vec<u32> {
    if x.len() < NFFT + HOP * 3 {
        return Vec::new();
    }
    let n = 1 + (x.len() - NFFT) / HOP;
    let b = bands();
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(NFFT);
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut prev = [0f64; 32];
    let mut out = Vec::with_capacity(n.saturating_sub(1));
    for f in 0..n {
        let frame = &x[f * HOP..f * HOP + NFFT];
        for (i, v) in input.iter_mut().enumerate() {
            *v = frame[i] as f64 * b.window[i];
        }
        let _ = fft.process(&mut input, &mut spec);
        let mut energy = [0f64; 33];
        for (j, &(a, z)) in b.ranges.iter().enumerate() {
            energy[j] = spec[a..z].iter().map(|c| c.re * c.re + c.im * c.im).sum();
        }
        let mut d = [0f64; 32];
        for j in 0..32 {
            d[j] = energy[j] - energy[j + 1];
        }
        if f > 0 {
            let mut w = 0u32;
            for j in 0..32 {
                if d[j] - prev[j] > 0.0 {
                    w |= 1 << j;
                }
            }
            out.push(w);
        }
        prev = d;
    }
    out
}

pub fn fingerprint(path: &str) -> Fp {
    let dur = probe_duration(path);
    let op_len = if dur > 0.0 { WINDOW_SEC.min(dur * 0.45) } else { WINDOW_SEC };
    let ed_off = if dur > 0.0 { (dur - WINDOW_SEC).max(0.0).max(dur * 0.55) } else { 0.0 };
    // İki kod çözme aynı anda: ffmpeg süreçleri paralel çalışır
    let (op, ed) = std::thread::scope(|sc| {
        let ed = sc.spawn(|| if dur > 0.0 { bits(&decode(path, ed_off, dur - ed_off)) } else { Vec::new() });
        let op = bits(&decode(path, 0.0, op_len));
        (op, ed.join().unwrap_or_default())
    });
    Fp { dur, ed_off, op, ed }
}

// Önbellek dosyası: "AIFP1" + dur + ed_off + n_op + n_ed + u32 dizileri (little endian)
pub fn save_fp(fp: &Fp, path: &Path) -> std::io::Result<()> {
    let mut b = Vec::with_capacity(32 + 4 * (fp.op.len() + fp.ed.len()));
    b.extend_from_slice(b"AIFP1");
    b.extend_from_slice(&fp.dur.to_le_bytes());
    b.extend_from_slice(&fp.ed_off.to_le_bytes());
    b.extend_from_slice(&(fp.op.len() as u32).to_le_bytes());
    b.extend_from_slice(&(fp.ed.len() as u32).to_le_bytes());
    for w in fp.op.iter().chain(fp.ed.iter()) {
        b.extend_from_slice(&w.to_le_bytes());
    }
    std::fs::write(path, b)
}

pub fn load_fp(path: &Path) -> Option<Fp> {
    let b = std::fs::read(path).ok()?;
    if b.len() < 29 || &b[..5] != b"AIFP1" {
        return None;
    }
    let f64_at = |i: usize| f64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let (dur, ed_off) = (f64_at(5), f64_at(13));
    let (no, ne) = (u32_at(21) as usize, u32_at(25) as usize);
    if b.len() != 29 + 4 * (no + ne) {
        return None;
    }
    let words: Vec<u32> = (0..no + ne).map(|k| u32_at(29 + 4 * k)).collect();
    Some(Fp { dur, ed_off, op: words[..no].to_vec(), ed: words[no..].to_vec() })
}

/// Ortak en uzun bölümün hem a'daki hem b'deki (başlangıç, bitiş) aralığı.
pub fn match_pair(a: &[u32], b: &[u32]) -> Option<(Range, Range)> {
    let (na, nb) = (a.len() as i64, b.len() as i64);
    if (na as usize) < SMOOTH || (nb as usize) < SMOOTH {
        return None;
    }
    let mf = MIN_FRAMES as i64;
    let half = SMOOTH / 2;
    let mut best = (0usize, 0usize, 0usize); // (uzunluk, a_başlangıç, b_başlangıç)
    let mut d: Vec<u8> = Vec::new();
    let mut cs: Vec<u32> = Vec::new();
    for off in -(na - mf)..=(nb - mf) {
        let r0 = (-off).max(0) as usize;
        let c0 = off.max(0) as usize;
        let len = (na as usize - r0.min(na as usize)).min(nb as usize - c0.min(nb as usize));
        if len == 0 {
            continue;
        }
        let (ar, br) = (&a[r0..r0 + len], &b[c0..c0 + len]);
        d.clear();
        d.extend(ar.iter().zip(br).map(|(x, y)| ((x ^ y).count_ones() <= HAM_THRESH) as u8));
        // Yeterince eşleşme içermeyen köşegen baştan elenir
        let total: u32 = d.iter().map(|&v| v as u32).sum();
        if (total as f64) < MIN_FRAMES as f64 * 0.5 {
            continue;
        }
        // Ortalanmış SMOOTH pencerede çoğunluk; en uzun "true" dizisi
        cs.clear();
        cs.push(0);
        let mut acc = 0u32;
        for &v in &d {
            acc += v as u32;
            cs.push(acc);
        }
        let (mut run, mut best_run, mut best_end) = (0usize, 0usize, 0usize);
        for k in 0..len {
            let lo = k.saturating_sub(half);
            let hi = (k + half + 1).min(len);
            let on = (cs[hi] - cs[lo]) * 2 >= SMOOTH as u32;
            if on {
                run += 1;
                if run > best_run {
                    best_run = run;
                    best_end = k;
                }
            } else {
                run = 0;
            }
        }
        if best_run > best.0 {
            let first = best_end + 1 - best_run;
            best = (best_run, first + r0, first + c0);
        }
    }
    let (ln, sa, sb) = best;
    let secs = ln as f64 * FRAME_SEC;
    if secs < MIN_LEN || secs > MAX_LEN {
        return None;
    }
    Some((
        (sa as f64 * FRAME_SEC, (sa + ln) as f64 * FRAME_SEC),
        (sb as f64 * FRAME_SEC, (sb + ln) as f64 * FRAME_SEC),
    ))
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

fn consensus(cands: Vec<Option<Range>>) -> Option<Range> {
    let mut c: Vec<Range> = cands.into_iter().flatten().collect();
    if c.is_empty() {
        return None;
    }
    c.sort_by(|x, y| (y.1 - y.0).total_cmp(&(x.1 - x.0))); // kararlı: eşitlerde sıra korunur
    let top = c[0];
    let group: Vec<Range> =
        c.into_iter().filter(|r| r.1.min(top.1) - r.0.max(top.0) > 0.5 * (top.1 - top.0)).collect();
    Some((median(group.iter().map(|r| r.0).collect()), median(group.iter().map(|r| r.1).collect())))
}

#[derive(Default, Clone)]
pub struct Detected {
    pub op: Option<Range>,
    pub ed: Option<Range>,
}

/// items: sezondaki tüm dosyalar (anahtar, bölüm_no, parmak izi), bölüm sırasıyla.
/// Her hedef en yakın max_refs bölümle karşılaştırılır; ortak çiftler bir kez ve paralel hesaplanır.
pub fn detect_season(
    items: &[(String, i64, &Fp)],
    targets: &[String],
    max_refs: usize,
    cancel: &AtomicBool,
) -> HashMap<String, Detected> {
    let by_key: HashMap<&str, (i64, &Fp)> = items.iter().map(|(k, n, f)| (k.as_str(), (*n, *f))).collect();
    let mut refs: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut pairs: BTreeSet<(&str, &str)> = BTreeSet::new();
    for t in targets {
        let Some(&(tn, _)) = by_key.get(t.as_str()) else { continue };
        let mut others: Vec<&str> = items.iter().map(|(k, _, _)| k.as_str()).filter(|k| *k != t).collect();
        others.sort_by_key(|k| (by_key[k].0 - tn).abs()); // kararlı sıralama
        others.truncate(max_refs);
        for &r in &others {
            pairs.insert(if t.as_str() < r { (t.as_str(), r) } else { (r, t.as_str()) });
        }
        refs.insert(t.as_str(), others);
    }
    let pairs: Vec<(&str, &str)> = pairs.into_iter().collect();
    let res: HashMap<(&str, &str), (Option<(Range, Range)>, Option<(Range, Range)>)> = pairs
        .par_iter()
        .map(|&p| {
            if cancel.load(Ordering::Relaxed) {
                return (p, (None, None));
            }
            let (a, b) = (by_key[p.0].1, by_key[p.1].1);
            (p, (match_pair(&a.op, &b.op), match_pair(&a.ed, &b.ed)))
        })
        .collect();
    let mut out = HashMap::new();
    for t in targets {
        let Some(rs) = refs.get(t.as_str()) else { continue };
        let fp = by_key[t.as_str()].1;
        let (mut ops, mut eds) = (Vec::new(), Vec::new());
        for &r in rs {
            let p = if t.as_str() < r { (t.as_str(), r) } else { (r, t.as_str()) };
            let first = p.0 == t.as_str();
            let (op, ed) = res.get(&p).cloned().unwrap_or((None, None));
            ops.push(op.map(|m| if first { m.0 } else { m.1 }));
            eds.push(ed.map(|m| {
                let s = if first { m.0 } else { m.1 };
                (s.0 + fp.ed_off, s.1 + fp.ed_off)
            }));
        }
        out.insert(t.clone(), Detected { op: consensus(ops), ed: consensus(eds) });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<u32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u32
            })
            .collect()
    }

    #[test]
    fn finds_shared_segment() {
        let song = noise(900, 99); // 90 sn ortak müzik
        let mut a = noise(3600, 1);
        let mut b = noise(3600, 2);
        a[300..1200].copy_from_slice(&song);
        b[1000..1900].copy_from_slice(&song);
        let ((sa, ea), (sb, eb)) = match_pair(&a, &b).unwrap();
        assert!((sa - 30.0).abs() < 1.5 && (ea - 120.0).abs() < 1.5, "{sa} {ea}");
        assert!((sb - 100.0).abs() < 1.5 && (eb - 190.0).abs() < 1.5, "{sb} {eb}");
    }

    #[test]
    fn rejects_unrelated() {
        assert!(match_pair(&noise(3000, 5), &noise(3000, 6)).is_none());
    }

    #[test]
    fn consensus_median() {
        let r = consensus(vec![Some((10.0, 100.0)), None, Some((12.0, 101.0)), Some((500.0, 510.0))]).unwrap();
        assert_eq!(r, (11.0, 100.5));
    }
}
