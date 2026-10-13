//! Kütüphane (animeler, sezonlar, bölümler) ve ayarlar. Python sürümüyle aynı kutuphane.json.
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, MutexGuard};

use crate::util::{now_secs, paths};

pub const DEFAULT_TEMPLATE: &str = "{anime} S{s}B{b}";

pub const ST_NOFILE: &str = "dosya yok";
pub const ST_READY: &str = "hazır";
pub const ST_ANALYZED: &str = "analiz edildi";
pub const ST_QUEUED: &str = "kuyrukta";
pub const ST_PROCESSING: &str = "işleniyor";
pub const ST_DONE: &str = "dışa aktarıldı";
pub const ST_ERROR: &str = "hata";

pub fn busy(status: &str) -> bool {
    status == ST_QUEUED || status == ST_PROCESSING
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub out_dir: String,
    pub name_template: String,
    pub auto_skip: bool,
    pub skip_preview: bool,
    pub mpv_path: String,
    pub remove_oped: bool,
    pub cut_mode: String,
    pub encoder: String,
    pub quality: i64,
    pub container: String,
    pub merge_delete_eps: bool,
    pub lang: String, // "tr" | "en" | "" (arayüz ilk açılışta sistem diline göre seçer)
}

impl Default for Settings {
    fn default() -> Self {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        Settings {
            out_dir: format!("{home}\\Videos\\Anime"),
            name_template: DEFAULT_TEMPLATE.into(),
            auto_skip: true,
            skip_preview: false,
            mpv_path: String::new(),
            remove_oped: true,
            cut_mode: "fast".into(),
            encoder: "h264_nvenc".into(),
            quality: 21,
            container: "auto".into(),
            merge_delete_eps: false,
            lang: String::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Season {
    pub n: i64,
    pub title: String,
    pub count: i64,
    pub episodes: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anilist_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Anime {
    pub id: String,
    pub name: String,
    pub image: Option<String>,
    pub seasons: Vec<Season>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Skip {
    pub start: f64,
    pub end: f64,
    pub src: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Ep {
    pub id: String,
    pub anime: String,
    pub season: i64,
    pub episode: i64,
    pub src: String,
    pub sub_src: String,
    pub status: String,
    pub progress: f64,
    pub message: String,
    pub skip: BTreeMap<String, Skip>,
    pub analyzed: bool,
    pub out_file: String,
    pub sub_file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orig_src: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dur: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<f64>,
}

impl Ep {
    /// Yalnızca elle girilmiş OP/ED zamanları.
    pub fn manual_skip(&self) -> BTreeMap<String, Skip> {
        self.skip.iter().filter(|(_, v)| v.src == "elle").map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Merge {
    pub status: String,
    pub progress: f64,
    pub message: String,
    pub file: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Saved {
    settings: Settings,
    animes: IndexMap<String, Anime>,
    eps: IndexMap<String, Ep>,
}

#[derive(Default)]
pub struct Data {
    pub settings: Settings,
    pub animes: IndexMap<String, Anime>,
    pub eps: IndexMap<String, Ep>,
    pub merges: IndexMap<String, Merge>,
    pub version: u64,
}

pub fn ep_id(aid: &str, season: i64, episode: i64) -> String {
    format!("{aid}/{season}/{episode}")
}

impl Data {
    /// Bölüm kaydı yoksa oluşturur.
    pub fn ensure_ep(&mut self, aid: &str, season: i64, episode: i64) -> &mut Ep {
        let eid = ep_id(aid, season, episode);
        self.eps.entry(eid.clone()).or_insert_with(|| Ep {
            id: eid,
            anime: aid.into(),
            season,
            episode,
            status: ST_NOFILE.into(),
            ..Default::default()
        })
    }

    pub fn season_eps(&self, aid: &str, season: i64) -> Vec<Ep> {
        let mut v: Vec<Ep> = self.eps.values().filter(|e| e.anime == aid && e.season == season).cloned().collect();
        v.sort_by_key(|e| e.episode);
        v
    }

    pub fn anime_eps(&self, aid: &str) -> Vec<Ep> {
        let mut v: Vec<Ep> = self.eps.values().filter(|e| e.anime == aid).cloned().collect();
        v.sort_by_key(|e| (e.season, e.episode));
        v
    }
}

pub struct Store {
    data: Mutex<Data>,
    pub changed: Condvar,
}

impl Store {
    pub fn load() -> Store {
        let mut d = Data::default();
        let file = paths().data.join("kutuphane.json");
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Ok(s) = serde_json::from_str::<Saved>(&text) {
                d.settings = s.settings;
                d.animes = s.animes;
                d.eps = s.eps;
            }
        }
        crate::util::set_lang(&d.settings.lang);
        for ep in d.eps.values_mut() {
            if ep.status == "bitti" {
                ep.status = ST_DONE.into();
            }
            if busy(&ep.status) {
                ep.status = if ep.src.is_empty() { ST_NOFILE } else { ST_READY }.into();
                ep.message = crate::l!("Uygulama kapandı, yeniden başlat", "The app was closed, start again");
            }
            ep.progress = 0.0;
        }
        Store { data: Mutex::new(d), changed: Condvar::new() }
    }

    pub fn lock(&self) -> MutexGuard<'_, Data> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Değişikliği bekleyen arayüz bağlantılarına haber verir.
    pub fn touch(&self, d: &mut Data) {
        d.version += 1;
        self.changed.notify_all();
    }

    /// Kilit tutulurken diske yazar.
    pub fn save_locked(&self, d: &mut Data) {
        self.touch(d);
        let saved = SavedRef { settings: &d.settings, animes: &d.animes, eps: &d.eps };
        let file = paths().data.join("kutuphane.json");
        let tmp = file.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string_pretty(&saved) {
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
        }
    }

    pub fn save(&self) {
        let mut d = self.lock();
        self.save_locked(&mut d);
    }

    pub fn get(&self, eid: &str) -> Option<Ep> {
        self.lock().eps.get(eid).cloned()
    }

    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    /// Bölümü değiştirir; save=false ise yalnızca arayüze bildirir (ilerleme gibi sık güncellemeler).
    pub fn update(&self, eid: &str, save: bool, f: impl FnOnce(&mut Ep)) -> Option<Ep> {
        let mut d = self.lock();
        let ep = d.eps.get_mut(eid)?;
        f(ep);
        ep.updated = Some(now_secs());
        let out = ep.clone();
        if save { self.save_locked(&mut d) } else { self.touch(&mut d) }
        Some(out)
    }

    pub fn set_merge(&self, aid: &str, f: impl FnOnce(&mut Merge)) {
        let mut d = self.lock();
        f(d.merges.entry(aid.to_string()).or_default());
        self.touch(&mut d);
    }

    pub fn add_anime(&self, mut anime: Anime) {
        let mut d = self.lock();
        if let Some(old) = d.animes.get(&anime.id) {
            if !old.name.is_empty() {
                anime.name = old.name.clone();
            }
        }
        let aid = anime.id.clone();
        let counts: Vec<(i64, i64)> = anime.seasons.iter().map(|s| (s.n, s.count)).collect();
        d.animes.insert(aid.clone(), anime);
        for (n, c) in counts {
            for e in 1..=c.max(0) {
                d.ensure_ep(&aid, n, e);
            }
        }
        self.save_locked(&mut d);
    }
}

#[derive(Serialize)]
struct SavedRef<'a> {
    settings: &'a Settings,
    animes: &'a IndexMap<String, Anime>,
    eps: &'a IndexMap<String, Ep>,
}
