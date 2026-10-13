//! Klasördeki video dosyalarını sezon/bölüm numaralarıyla eşleştirir.
use fancy_regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::util::path_str;

const VIDEO_EXT: &[&str] = &["mkv", "mp4", "avi", "webm", "m4v", "ts", "mov", "wmv", "flv"];
const SUB_EXT: &[&str] = &["ass", "ssa", "srt"];
const TR_TAGS: &[&str] = &["tr", "tur", "turkish", "türkçe", "turkce"];

struct Rx {
    se: Regex,
    season_ep: Regex,
    ep_word: Regex,
    ep_tr: Regex,
    dash: Regex,
    dir_season: Regex,
    season_hint: Regex,
    noise: Regex,
    tech: Regex,
    nums: Regex,
}

fn rx() -> &'static Rx {
    static R: OnceLock<Rx> = OnceLock::new();
    R.get_or_init(|| {
        let r = |s: &str| Regex::new(s).expect("regex");
        Rx {
            se: r(r"(?i)(?<![A-Za-z0-9])S(\d{1,2})\s*[._-]?\s*[EB](\d{1,4})(?!\d)"),
            season_ep: r(r"(?i)(?:Season|Sezon)\s*[._-]?\s*(\d{1,2}).*?(?:Episode|Ep|Bölüm|Bolum|B)\s*[._-]?\s*(\d{1,4})"),
            ep_word: r(r"(?i)(?:Episode|Ep|Bölüm|Bolum|Böl)\s*[._-]?\s*(\d{1,4})(?!\d)"),
            ep_tr: r(r"(?i)(?<!\d)(\d{1,4})\s*\.?\s*(?:Bölüm|Bolum)"),
            dash: r(r"\s[-–]\s+(\d{1,4})(?:v\d)?(?!\d)"),
            dir_season: r(r"(?i)(?:Season|Sezon|\bS)\s*[._-]?\s*(\d{1,2})\b|(\d{1,2})\s*\.?\s*Sezon"),
            season_hint: r(r"(?i)(\d{1,2})(?:st|nd|rd|th)\s+Season|Season\s*(\d{1,2})|(\d{1,2})\s*\.?\s*Sezon"),
            noise: r(r"\[[^\]]*\]|\([^)]*\)|\{[^}]*\}"),
            tech: r(r"(?i)\b(\d{3,4}p|[xh]\.?26[45]|10bit|8bit|hevc|avc|aac|flac|ddp?\d?\.?\d?|web-?dl|webrip|bluray|bd|dual|multi)\b"),
            nums: r(r"(?<![\dx])(\d{1,4})(?![\dp])"),
        }
    })
}

fn group_int(c: &fancy_regex::Captures<'_, str>, i: usize) -> Option<i64> {
    c.get(i).and_then(|m| m.as_str().parse().ok())
}

/// Dosya adından (sezon, bölüm).
pub fn parse_name(stem: &str) -> (Option<i64>, Option<i64>) {
    let r = rx();
    for re in [&r.se, &r.season_ep] {
        if let Ok(Some(c)) = re.captures(stem) {
            return (group_int(&c, 1), group_int(&c, 2));
        }
    }
    let no_noise = r.noise.replace_all(&stem.replace('_', " "), " ").into_owned();
    let mut clean = r.tech.replace_all(&no_noise, " ").into_owned();
    let mut hint = None;
    if let Ok(Some(c)) = r.season_hint.captures(clean.as_str()) {
        hint = (1..=3).find_map(|i| group_int(&c, i));
        let m = c.get(0).unwrap();
        clean = format!("{} {}", &clean[..m.start()], &clean[m.end()..]);
    }
    for re in [&r.ep_word, &r.ep_tr, &r.dash] {
        if let Ok(Some(c)) = re.captures(clean.as_str()) {
            return (hint, group_int(&c, 1));
        }
    }
    let nums: Vec<i64> = r
        .nums
        .captures_iter(clean.as_str())
        .filter_map(|c| c.ok().and_then(|c| group_int(&c, 1)))
        .filter(|n| !(1900..=2099).contains(n))
        .collect();
    (hint, nums.last().copied())
}

/// Alt klasör adında sezon bilgisi varsa döner ('Season 2', 'S02', '2. Sezon').
fn dir_season(path: &Path, root: &Path) -> Option<i64> {
    let rel = path.parent()?.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    for part in parts.iter().rev() {
        if let Ok(Some(c)) = rx().dir_season.captures(part.as_str()) {
            return group_int(&c, 1).or_else(|| group_int(&c, 2));
        }
    }
    None
}

fn ext_lower(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Videonun yanındaki altyazı dosyası; Türkçe etiketli olan öncelikli.
pub fn find_sub(video: &str) -> Option<String> {
    let vp = Path::new(video);
    let dir = vp.parent()?;
    let stem = vp.file_stem()?.to_string_lossy().to_lowercase();
    let mut cands = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        let ext = ext_lower(&p);
        if !SUB_EXT.contains(&ext.as_str()) {
            continue;
        }
        let base = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let Some(rest) = base.strip_prefix(&stem) else { continue };
        let tag = rest.trim_matches(|c| matches!(c, ' ' | '.' | '_' | '-')).to_string();
        let lang_like = (2..=3).contains(&tag.chars().count()) && tag.chars().all(|c| c.is_ascii_lowercase());
        if !tag.is_empty() && !TR_TAGS.contains(&tag.as_str()) && !lang_like {
            continue;
        }
        let rank = if TR_TAGS.contains(&tag.as_str()) { 0 } else if tag.is_empty() { 1 } else { 2 };
        cands.push((rank, (ext == "srt") as u8, path_str(&p)));
    }
    cands.sort();
    cands.into_iter().next().map(|c| c.2)
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Text(String),
    Num(usize, String), // basamak sayısı + baştaki sıfırlar atılmış hali: sayısal sıralama
}

fn natural_key(p: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digit = false;
    let flush = |cur: &mut String, digit: bool, out: &mut Vec<Key>| {
        if digit {
            let t = cur.trim_start_matches('0').to_string();
            out.push(Key::Num(t.len(), t));
        } else {
            out.push(Key::Text(cur.to_lowercase()));
        }
        cur.clear();
    };
    for c in p.chars() {
        let d = c.is_ascii_digit();
        if d != digit {
            flush(&mut cur, digit, &mut out);
            digit = d;
        }
        cur.push(c);
    }
    flush(&mut cur, digit, &mut out);
    out
}

fn scan(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if !name.starts_with('.') {
                    stack.push(p);
                }
            } else if VIDEO_EXT.contains(&ext_lower(&p).as_str()) {
                files.push(p);
            }
        }
    }
    files.sort_by_cached_key(|p| natural_key(&path_str(p)));
    files
}

/// root klasöründeki dosyaları eşleştirir. season: klasör tek bir sezona aitse onun numarası.
/// {(sezon, bölüm): dosya} ve eşleşmeyen dosyalar döner.
pub fn assign(root: &str, counts: &BTreeMap<i64, i64>, season: Option<i64>) -> (BTreeMap<(i64, i64), String>, Vec<String>) {
    let rootp = Path::new(root);
    let files = scan(rootp);
    let mut parsed: Vec<(String, Option<i64>, Option<i64>)> = files
        .iter()
        .map(|f| {
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let (mut s, e) = parse_name(&stem);
            if season.is_some() {
                s = season;
            } else if s.is_none() {
                s = dir_season(f, rootp);
            }
            (path_str(f), s, e)
        })
        .collect();
    // Hiç numara çıkmadıysa sıraya göre dağıt
    if !parsed.is_empty() && parsed.iter().all(|p| p.2.is_none()) {
        for (i, p) in parsed.iter_mut().enumerate() {
            p.2 = Some(i as i64 + 1);
        }
    }
    let mut result = BTreeMap::new();
    let mut unmatched = Vec::new();
    for (f, s, e) in parsed {
        let Some(mut e) = e else {
            unmatched.push(f);
            continue;
        };
        let s = match s {
            Some(s) => s,
            None => {
                let mut s = 1;
                // Mutlak numaralandırma: 1. sezon 12 bölümse 13 -> S02E01
                let c1 = counts.get(&1).copied().unwrap_or(0);
                if season.is_none() && c1 > 0 && e > c1 {
                    let mut rest = e;
                    for (&n, &c) in counts {
                        if c > 0 && rest > c {
                            rest -= c;
                            continue;
                        }
                        s = n;
                        e = rest;
                        break;
                    }
                }
                s
            }
        };
        if result.contains_key(&(s, e)) {
            unmatched.push(f);
        } else {
            result.insert((s, e), f);
        }
    }
    (result, unmatched)
}

#[cfg(test)]
mod tests {
    use super::parse_name;

    #[test]
    fn names() {
        assert_eq!(parse_name("[Sub] Frieren - S02E03 [1080p]"), (Some(2), Some(3)));
        assert_eq!(parse_name("[SubsPlease] Sousou no Frieren - 07 (1080p) [ABCD]"), (None, Some(7)));
        assert_eq!(parse_name("Frieren 2nd Season - 05"), (Some(2), Some(5)));
        assert_eq!(parse_name("Frieren 12. Bölüm"), (None, Some(12)));
        assert_eq!(parse_name("Show Season 3 Episode 10"), (Some(3), Some(10)));
        assert_eq!(parse_name("Anime 2023 x264 08"), (None, Some(8)));
    }
}
