//! Ortak yardımcılar: hata türü, yollar, süreç başlatma, zaman ve ad biçimleri.
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static EN: AtomicBool = AtomicBool::new(false);

/// Arayüz dili İngilizce mi (mesajlar bu dile göre üretilir).
pub fn is_en() -> bool {
    EN.load(Ordering::Relaxed)
}

pub fn set_lang(lang: &str) {
    EN.store(lang == "en", Ordering::Relaxed);
}

/// Türkçe ve İngilizce biçim dizgesinden etkin dildekini üretir.
#[macro_export]
macro_rules! l {
    ($tr:literal, $en:literal $(, $a:expr)* $(,)?) => {
        if $crate::util::is_en() { format!($en $(, $a)*) } else { format!($tr $(, $a)*) }
    };
}

/// İş akışı hatası: iptal ayrı tutulur, diğerleri kullanıcıya gösterilen mesajdır.
#[derive(Debug)]
pub enum Fail {
    Cancelled,
    Msg(String),
}

pub type Res<T> = Result<T, Fail>;

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Msg(s)
    }
}
impl From<&str> for Fail {
    fn from(s: &str) -> Self {
        Fail::Msg(s.to_string())
    }
}
impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::Msg(e.to_string())
    }
}
impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Cancelled => write!(f, "{}", l!("İptal edildi", "Cancelled")),
            Fail::Msg(m) => write!(f, "{m}"),
        }
    }
}

pub struct Paths {
    pub data: PathBuf,
    pub player: PathBuf,
    pub player_conf: PathBuf,
    pub ffmpeg: PathBuf, // pakete gömülü ffmpeg/ffprobe klasörü
}

static PATHS: OnceLock<Paths> = OnceLock::new();

/// exe'nin klasöründen yukarı doğru "player" (eski kurulumlarda "oynatici") klasörünü arar;
/// bulunan klasör kök sayılır. Veriler kökteki "data" klasöründe durur.
pub fn paths() -> &'static Paths {
    PATHS.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_default();
        let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        let found = exe_dir.ancestors().find_map(|d| {
            ["player", "oynatici"].iter().map(|n| d.join(n)).find(|p| p.is_dir()).map(|p| (d.to_path_buf(), p))
        });
        let (root, player) = found.unwrap_or_else(|| (exe_dir.clone(), exe_dir.join("player")));
        let player_conf = ["config", "ayarlar"].iter().map(|n| player.join(n)).find(|p| p.is_dir()).unwrap_or_else(|| player.join("config"));
        let env = std::env::var("TOBASU_DATA").or_else(|_| std::env::var("ANIME_VERI")).unwrap_or_default();
        let data = if !env.is_empty() {
            PathBuf::from(env)
        } else if root.join("uygulama").join("veri").is_dir() {
            root.join("uygulama").join("veri") // eski Anime İndirici kurulumu
        } else {
            root.join("data")
        };
        let _ = std::fs::create_dir_all(&data);
        Paths { player, player_conf, ffmpeg: root.join("ffmpeg"), data }
    })
}

pub fn log(msg: &str) {
    let p = paths().data.join("uygulama.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{msg}");
    }
}

pub const NO_WINDOW: u32 = 0x0800_0000;
pub const DETACHED: u32 = 0x0000_0008;

/// Konsol penceresi açmayan komut. ffmpeg/ffprobe önce exe'nin yanındaki ffmpeg klasöründe aranır.
pub fn cmd(prog: &str) -> Command {
    let bundled = paths().ffmpeg.join(format!("{prog}.exe"));
    let mut c = if matches!(prog, "ffmpeg" | "ffprobe") && bundled.is_file() { Command::new(bundled) } else { Command::new(prog) };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(NO_WINDOW);
    }
    c
}

pub fn fmt_t(sec: f64) -> String {
    let s = sec.round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Python round(x, n) yeterli yakınlıkta.
pub fn round_to(x: f64, n: i32) -> f64 {
    let m = 10f64.powi(n);
    (x * m).round() / m
}

/// '1:30-3:00' -> (90.0, 180.0)
pub fn parse_range(text: &str) -> Option<(f64, f64)> {
    let t = text.trim();
    let (a, b) = t.split_once(['-', '–'])?;
    let sec = |p: &str| -> Option<f64> {
        let p = p.trim();
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.') {
            return None;
        }
        let mut v = 0.0;
        for part in p.split(':') {
            v = v * 60.0 + part.parse::<f64>().ok()?;
        }
        Some(v)
    };
    let (s, e) = (sec(a)?, sec(b)?);
    (e > s).then_some((s, e))
}

pub fn safe_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') && (*c as u32) >= 0x20)
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.');
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let out: String = joined.chars().take(120).collect();
    if out.is_empty() { "isimsiz".into() } else { out }
}

pub enum Arg<'a> {
    Int(i64),
    Str(&'a str),
}

/// Python str.format'ın ad şablonu için gereken alt kümesi: {ad}, {ad:02}, {ad:>5}, {{ }}.
pub fn format_tpl(tpl: &str, args: &[(&str, Arg)]) -> Option<String> {
    let mut out = String::new();
    let mut it = tpl.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' if it.peek() == Some(&'{') => {
                it.next();
                out.push('{');
            }
            '}' if it.peek() == Some(&'}') => {
                it.next();
                out.push('}');
            }
            '}' => return None,
            '{' => {
                let mut field = String::new();
                loop {
                    match it.next()? {
                        '}' => break,
                        '{' => return None,
                        ch => field.push(ch),
                    }
                }
                let (name, spec) = field.split_once(':').unwrap_or((field.as_str(), ""));
                let arg = &args.iter().find(|(n, _)| *n == name)?.1;
                out.push_str(&apply_spec(arg, spec)?);
            }
            _ => out.push(c),
        }
    }
    Some(out)
}

fn apply_spec(arg: &Arg, spec: &str) -> Option<String> {
    let mut s = spec;
    let mut align = None;
    let mut fill = ' ';
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 && matches!(chars[1], '<' | '>' | '^') {
        fill = chars[0];
        align = Some(chars[1]);
        s = &s[chars[0].len_utf8() + 1..];
    } else if !chars.is_empty() && matches!(chars[0], '<' | '>' | '^') {
        align = Some(chars[0]);
        s = &s[1..];
    }
    let mut zero = false;
    if s.starts_with('0') && align.is_none() {
        zero = true;
        s = &s[1..];
    }
    let s = s.strip_suffix('d').or_else(|| s.strip_suffix('s')).unwrap_or(s);
    let width: usize = if s.is_empty() { 0 } else { s.parse().ok()? };
    let (text, is_num) = match arg {
        Arg::Int(n) => (n.to_string(), true),
        Arg::Str(t) => {
            if zero {
                return None;
            }
            (t.to_string(), false)
        }
    };
    let len = text.chars().count();
    if len >= width {
        return Some(text);
    }
    let pad = width - len;
    if zero && is_num {
        let (sign, digits) = if let Some(d) = text.strip_prefix('-') { ("-", d) } else { ("", text.as_str()) };
        return Some(format!("{sign}{}{digits}", "0".repeat(pad)));
    }
    let align = align.unwrap_or(if is_num { '>' } else { '<' });
    let f = |n: usize| fill.to_string().repeat(n);
    Some(match align {
        '>' => format!("{}{text}", f(pad)),
        '^' => format!("{}{text}{}", f(pad / 2), f(pad - pad / 2)),
        _ => format!("{text}{}", f(pad)),
    })
}

pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Mutlak yol, ayraçlar Windows biçiminde.
pub fn abspath(p: &str) -> String {
    let pb = PathBuf::from(p);
    let abs = if pb.is_absolute() {
        pb
    } else {
        std::env::current_dir().unwrap_or_default().join(pb)
    };
    path_str(&abs).replace('/', "\\")
}

pub fn exists(p: &str) -> bool {
    !p.is_empty() && Path::new(p).exists()
}

/// Son satırı döner (ffmpeg hata mesajı için).
pub fn last_line(s: &str) -> String {
    s.trim().lines().last().unwrap_or("?").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tpl() {
        let a = [("anime", Arg::Str("Frieren")), ("s", Arg::Int(1)), ("b", Arg::Int(3)), ("e", Arg::Int(3)), ("title", Arg::Str(""))];
        assert_eq!(format_tpl("{anime} S{s}B{b}", &a).unwrap(), "Frieren S1B3");
        assert_eq!(format_tpl("{anime} S{s:02}E{b:02}", &a).unwrap(), "Frieren S01E03");
        assert!(format_tpl("{x}", &a).is_none());
        assert!(format_tpl("{anime", &a).is_none());
    }

    #[test]
    fn range() {
        assert_eq!(parse_range("1:30-3:00"), Some((90.0, 180.0)));
        assert_eq!(parse_range(" 90 – 100.5 "), Some((90.0, 100.5)));
        assert_eq!(parse_range("3:00-1:00"), None);
        assert_eq!(parse_range("abc"), None);
    }

    #[test]
    fn safe() {
        assert_eq!(safe_name(" a:b?  c. "), "ab c");
        assert_eq!(safe_name("..."), "isimsiz");
    }
}
