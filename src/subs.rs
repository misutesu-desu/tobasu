//! ASS/SRT altyazıları kesilen videoya göre kaydırır ve bölümleri tek dosyada birleştirir.
use fancy_regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

const DEFAULT_STYLE: &str = "Style: Default,Arial,52,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2.5,1,2,60,60,40,1";
const STYLE_FORMAT: &str = "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding";

fn ass_header() -> String {
    format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\n{STYLE_FORMAT}\n{DEFAULT_STYLE}\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
    )
}

#[derive(Clone, PartialEq)]
pub enum Kind {
    Ass,
    Srt,
}

#[derive(Clone)]
pub enum Payload {
    Fields(Vec<String>), // ASS olay alanları
    Text(String),        // SRT metni
}

#[derive(Clone)]
pub struct Event {
    pub start: f64,
    pub end: f64,
    pub typ: String, // Dialogue | Comment
    pub payload: Payload,
}

#[derive(Clone)]
pub struct Sub {
    pub kind: Kind,
    header: Vec<String>,
    style_format: String,
    styles: Vec<String>,
    trailer: Vec<String>,
    event_format: Vec<String>,
    pub events: Vec<Event>,
}

impl Sub {
    fn new(kind: Kind) -> Sub {
        Sub { kind, header: vec![], style_format: String::new(), styles: vec![], trailer: vec![], event_format: vec![], events: vec![] }
    }
}

fn read_text(path: &str) -> std::io::Result<String> {
    let raw = std::fs::read(path)?;
    if let Some(rest) = raw.strip_prefix(b"\xef\xbb\xbf") {
        if let Ok(t) = std::str::from_utf8(rest) {
            return Ok(t.to_string());
        }
    }
    if let Ok(t) = std::str::from_utf8(&raw) {
        return Ok(t.to_string());
    }
    if raw.starts_with(b"\xff\xfe") || raw.starts_with(b"\xfe\xff") {
        let enc = if raw[0] == 0xff { encoding_rs::UTF_16LE } else { encoding_rs::UTF_16BE };
        return Ok(enc.decode_with_bom_removal(&raw).0.into_owned());
    }
    // eski Türkçe altyazılar
    Ok(encoding_rs::WINDOWS_1254.decode_without_bom_handling(&raw).0.into_owned())
}

fn ass_time(t: &str) -> f64 {
    let parts: Vec<&str> = t.trim().split([':', '.', ',']).collect();
    if parts.len() != 4 {
        return 0.0;
    }
    let (Ok(h), Ok(m), Ok(s)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>(), parts[2].parse::<f64>()) else { return 0.0 };
    let frac = parts[3];
    let Ok(f) = frac.parse::<f64>() else { return 0.0 };
    h * 3600.0 + m * 60.0 + s + f / 10f64.powi(frac.len() as i32)
}

fn fmt_ass(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as i64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

fn fmt_srt(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as i64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3600000, ms / 60000 % 60, ms / 1000 % 60, ms % 1000)
}

pub fn parse(path: &str) -> std::io::Result<Sub> {
    let text = read_text(path)?.replace("\r\n", "\n").replace('\r', "\n");
    Ok(if text.contains("[Events]") || text.contains("[Script Info]") { parse_ass(&text) } else { parse_srt(&text) })
}

fn parse_ass(text: &str) -> Sub {
    let mut s = Sub::new(Kind::Ass);
    let mut section: Option<String> = None;
    for line in text.split('\n') {
        let st = line.trim();
        let low = st.to_lowercase();
        if st.starts_with('[') && st.ends_with(']') {
            section = Some(low.clone());
            if matches!(low.as_str(), "[v4+ styles]" | "[v4 styles]" | "[events]") {
                continue;
            }
        }
        match section.as_deref() {
            Some("[v4+ styles]") | Some("[v4 styles]") => {
                if low.starts_with("format:") {
                    s.style_format = st.to_string();
                } else if low.starts_with("style:") {
                    s.styles.push(st.to_string());
                }
                continue;
            }
            Some("[events]") => {
                if low.starts_with("format:") {
                    s.event_format = st[7..].split(',').map(|f| f.trim().to_lowercase()).collect();
                    continue;
                }
                let typ = if st.starts_with("Dialogue:") { "Dialogue" } else if st.starts_with("Comment:") { "Comment" } else { "" };
                if !typ.is_empty() && !s.event_format.is_empty() {
                    let rest = st[typ.len() + 1..].trim_start();
                    let n = s.event_format.len();
                    let fields: Vec<String> = rest.splitn(n, ',').map(str::to_string).collect();
                    if fields.len() == n {
                        let i_s = s.event_format.iter().position(|f| f == "start");
                        let i_e = s.event_format.iter().position(|f| f == "end");
                        if let (Some(i_s), Some(i_e)) = (i_s, i_e) {
                            s.events.push(Event {
                                start: ass_time(&fields[i_s]),
                                end: ass_time(&fields[i_e]),
                                typ: typ.into(),
                                payload: Payload::Fields(fields),
                            });
                        }
                    }
                }
                continue;
            }
            _ => {}
        }
        if section.as_deref().is_some_and(|x| x != "[script info]") {
            s.trailer.push(line.to_string());
        } else {
            s.header.push(line.to_string());
        }
    }
    while s.header.last().is_some_and(|l| l.trim().is_empty()) {
        s.header.pop();
    }
    if s.event_format.is_empty() {
        s.event_format = ["layer", "start", "end", "style", "name", "marginl", "marginr", "marginv", "effect", "text"]
            .iter()
            .map(|x| x.to_string())
            .collect();
    }
    s
}

fn srt_time_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(\d+):(\d{2}):(\d{2})[,.](\d{1,3})\s*-->\s*(\d+):(\d{2}):(\d{2})[,.](\d{1,3})").unwrap()
    })
}

fn parse_srt(text: &str) -> Sub {
    let mut s = Sub::new(Kind::Srt);
    let blank = Regex::new(r"\n\s*\n").unwrap();
    for block in blank.split(text.trim()).flatten() {
        let lines: Vec<&str> = block.split('\n').collect();
        for (i, ln) in lines.iter().take(3).enumerate() {
            if let Ok(Some(m)) = srt_time_re().captures(ln) {
                let g = |k: usize| m.get(k).unwrap().as_str();
                let num = |k: usize| g(k).parse::<f64>().unwrap_or(0.0);
                let st = num(1) * 3600.0 + num(2) * 60.0 + num(3) + num(4) / 10f64.powi(g(4).len() as i32);
                let en = num(5) * 3600.0 + num(6) * 60.0 + num(7) + num(8) / 10f64.powi(g(8).len() as i32);
                let body = lines[i + 1..].join("\n").trim().to_string();
                if !body.is_empty() {
                    s.events.push(Event { start: st, end: en, typ: "Dialogue".into(), payload: Payload::Text(body) });
                }
                break;
            }
        }
    }
    s
}

/// keep: korunan aralıklar. Olayları yeni zaman çizelgesine taşır.
pub fn cut(mut sub: Sub, keep: &[(f64, f64)]) -> Sub {
    let mut offsets = Vec::new();
    let mut acc = 0.0;
    for &(a, b) in keep {
        offsets.push((a, b, acc));
        acc += b - a;
    }
    let mut out = Vec::new();
    for ev in &sub.events {
        for &(a, b, off) in &offsets {
            let (s2, e2) = (ev.start.max(a), ev.end.min(b));
            if e2 - s2 > 0.04 {
                out.push(Event { start: s2 - a + off, end: e2 - a + off, ..ev.clone() });
            }
        }
    }
    sub.events = out;
    sub
}

fn srt_to_ass_text(t: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static ANY: OnceLock<Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| Regex::new(r"</?(i|b|u)>").unwrap());
    let any = ANY.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let t = tag.replace_all(t, |c: &fancy_regex::Captures<'_, str>| {
        let whole = c.get(0).unwrap().as_str();
        format!("{{\\{}{}}}", &c[1], if whole.as_bytes()[1] == b'/' { 0 } else { 1 })
    });
    any.replace_all(&t, "").replace('\n', "\\N")
}

fn capitalize(x: &str) -> String {
    let mut c = x.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

pub fn write(sub: &Sub, path: &str) -> std::io::Result<()> {
    let mut f = String::new();
    if sub.kind == Kind::Srt {
        let mut evs: Vec<&Event> = sub.events.iter().collect();
        evs.sort_by(|a, b| a.start.total_cmp(&b.start));
        for (i, e) in evs.iter().enumerate() {
            let body = match &e.payload {
                Payload::Text(t) => t.clone(),
                Payload::Fields(fs) => fs.last().cloned().unwrap_or_default(),
            };
            f += &format!("{}\n{} --> {}\n{}\n\n", i + 1, fmt_srt(e.start), fmt_srt(e.end), body);
        }
        return std::fs::write(path, f);
    }
    f += &(sub.header.join("\n") + "\n\n[V4+ Styles]\n");
    f += &(if sub.style_format.is_empty() { STYLE_FORMAT.to_string() } else { sub.style_format.clone() } + "\n");
    let styles = if sub.styles.is_empty() { vec![DEFAULT_STYLE.to_string()] } else { sub.styles.clone() };
    f += &(styles.join("\n") + "\n\n[Events]\n");
    let fmt: Vec<String> = sub
        .event_format
        .iter()
        .map(|x| if x.starts_with("margin") && x.len() == 7 { format!("Margin{}", x[6..].to_uppercase()) } else { capitalize(x) })
        .collect();
    f += &format!("Format: {}\n", fmt.join(", "));
    let i_s = sub.event_format.iter().position(|x| x == "start").unwrap_or(1);
    let i_e = sub.event_format.iter().position(|x| x == "end").unwrap_or(2);
    for e in &sub.events {
        if let Payload::Fields(fields) = &e.payload {
            let mut fields = fields.clone();
            fields[i_s] = fmt_ass(e.start);
            fields[i_e] = fmt_ass(e.end);
            f += &format!("{}: {}\n", e.typ, fields.join(","));
        }
    }
    if !sub.trailer.is_empty() {
        f += &format!("\n{}\n", sub.trailer.join("\n").trim());
    }
    std::fs::write(path, f)
}

fn as_ass(sub: &Sub) -> Sub {
    if sub.kind == Kind::Ass {
        return sub.clone();
    }
    let mut a = parse_ass(&ass_header());
    for e in &sub.events {
        let body = match &e.payload {
            Payload::Text(t) => t.clone(),
            Payload::Fields(fs) => fs.last().cloned().unwrap_or_default(),
        };
        let fields = ["0", "", "", "Default", "", "0", "0", "0", ""].iter().map(|s| s.to_string()).chain([srt_to_ass_text(&body)]).collect();
        a.events.push(Event { start: e.start, end: e.end, typ: e.typ.clone(), payload: Payload::Fields(fields) });
    }
    a
}

/// parts: (altyazı, başlangıç ofseti). Tümü SRT ise SRT, değilse ASS döner.
/// Farklı bölümlerde aynı adlı ama farklı tanımlı stiller yeniden adlandırılır.
pub fn merge(parts: &[(Sub, f64)]) -> Sub {
    if parts.iter().all(|(p, _)| p.kind == Kind::Srt) {
        let mut out = Sub::new(Kind::Srt);
        for (p, off) in parts {
            out.events.extend(p.events.iter().map(|e| Event { start: e.start + off, end: e.end + off, ..e.clone() }));
        }
        return out;
    }
    let base = as_ass(&parts[0].0);
    let mut out = parse_ass(&ass_header());
    out.header = base.header.clone();
    out.style_format = base.style_format.clone();
    out.trailer = vec![];
    let mut styles: Vec<(String, String)> = Vec::new(); // ad -> tanım (sıra korunur)
    for (idx, (p, off)) in parts.iter().enumerate() {
        let p = as_ass(p);
        let mut rename: HashMap<String, String> = HashMap::new();
        for line in &p.styles {
            let body = line[6..].trim();
            let (name, rest) = body.split_once(',').unwrap_or((body, ""));
            let mut name = name.to_string();
            if let Some((_, def)) = styles.iter().find(|(n, _)| *n == name) {
                if def != rest {
                    let new = format!("{}_B{}", name, idx + 1);
                    rename.insert(name.clone(), new.clone());
                    name = new;
                }
            }
            if !styles.iter().any(|(n, _)| *n == name) {
                styles.push((name, rest.to_string()));
            }
        }
        if p.styles.is_empty() && !styles.iter().any(|(n, _)| n == "Default") {
            let d = DEFAULT_STYLE[6..].trim();
            styles.push(("Default".into(), d.split_once(',').map(|x| x.1).unwrap_or("").to_string()));
        }
        for e in &p.events {
            let Payload::Fields(fields) = &e.payload else { continue };
            let mut d: HashMap<&str, String> = p.event_format.iter().map(String::as_str).zip(fields.iter().cloned()).collect();
            if let Some(st) = d.get("style").map(|s| s.trim_start_matches('*').to_string()) {
                if let Some(new) = rename.get(&st) {
                    d.insert("style", new.clone());
                }
            }
            let row = out
                .event_format
                .iter()
                .map(|k| d.get(k.as_str()).cloned().unwrap_or_else(|| if k.starts_with("margin") || k == "layer" { "0".into() } else { String::new() }))
                .collect();
            out.events.push(Event { start: e.start + off, end: e.end + off, typ: e.typ.clone(), payload: Payload::Fields(row) });
        }
    }
    out.styles = styles.into_iter().map(|(n, r)| format!("Style: {n},{r}")).collect();
    out
}

pub fn ext_for(sub: &Sub) -> &'static str {
    if sub.kind == Kind::Srt { ".srt" } else { ".ass" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_cut_and_ass_merge() {
        let s = parse_srt("1\n00:00:05,000 --> 00:00:07,500\n<i>Merhaba</i>\n\n2\n00:01:00,000 --> 00:01:02,000\nİkinci\n");
        assert_eq!(s.events.len(), 2);
        let c = cut(s, &[(0.0, 6.0), (60.0, 100.0)]);
        assert_eq!(c.events.len(), 2);
        assert!((c.events[1].start - 6.0).abs() < 1e-9);
        let m = merge(&[(c.clone(), 0.0), (c, 100.0)]);
        assert!(m.kind == Kind::Srt && m.events.len() == 4);
        let a = as_ass(&m);
        if let Payload::Fields(f) = &a.events[0].payload {
            assert_eq!(f[9], "{\\i1}Merhaba{\\i0}");
        } else {
            panic!()
        }
    }

    #[test]
    fn ass_roundtrip() {
        let t = "[Script Info]\nTitle: x\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.50,0:00:03.00,Default,,0,0,0,,a, b\n";
        let s = parse_ass(t);
        assert_eq!(s.events.len(), 1);
        assert!((s.events[0].start - 1.5).abs() < 1e-9);
        let p = std::env::temp_dir().join("ai_test.ass");
        write(&s, p.to_str().unwrap()).unwrap();
        let back = std::fs::read_to_string(&p).unwrap();
        assert!(back.contains("Dialogue: 0,0:00:01.50,0:00:03.00,Default,,0,0,0,,a, b"));
        assert!(back.contains("MarginL"));
    }
}
