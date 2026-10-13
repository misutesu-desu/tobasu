//! Uygulama mantığı: analiz, dışa aktarma, birleştirme, oynatma, yeniden adlandırma ve API.
use crate::l;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::store::*;
use crate::util::*;
use crate::{cutter, matcher, merger, metadata, opedfind, subs};

const MAX_REFS: usize = 4;

pub enum Task {
    Analyze(Vec<String>),
    Export(Vec<String>),
    Merge(String, Vec<String>),
}

const ANALIZ: usize = 0;
const AKTAR: usize = 1;

pub struct App {
    pub store: Store,
    pub cancel: AtomicBool,
    queues: [Mutex<Sender<Task>>; 2],
    pending: [AtomicUsize; 2],
    idle: [AtomicBool; 2],
    season_locks: Mutex<HashMap<(String, i64), Arc<Mutex<()>>>>,
    pick_lock: Mutex<()>,
    watch: Mutex<(Option<SystemTime>, Map<String, Value>)>,
    mpv_cache: Mutex<Option<(Instant, String, Option<String>)>>,
    fp_pool: rayon::ThreadPool,
}

static APP: OnceLock<App> = OnceLock::new();

pub fn app() -> &'static App {
    APP.get().expect("app")
}

/// Depoyu yükler ve iş kuyruklarını başlatır.
pub fn init() {
    let (tx_a, rx_a) = channel();
    let (tx_e, rx_e) = channel();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let a = App {
        store: Store::load(),
        cancel: AtomicBool::new(false),
        queues: [Mutex::new(tx_a), Mutex::new(tx_e)],
        pending: [AtomicUsize::new(0), AtomicUsize::new(0)],
        idle: [AtomicBool::new(true), AtomicBool::new(true)],
        season_locks: Mutex::new(HashMap::new()),
        pick_lock: Mutex::new(()),
        watch: Mutex::new((None, Map::new())),
        mpv_cache: Mutex::new(None),
        // Her parmak izi iki ffmpeg süreci açar; çekirdek sayısına göre aynı anda birkaç dosya
        fp_pool: rayon::ThreadPoolBuilder::new().num_threads((cores / 2).clamp(2, 6)).build().expect("pool"),
    };
    if APP.set(a).is_err() {
        return;
    }
    spawn_worker(ANALIZ, rx_a);
    spawn_worker(AKTAR, rx_e);
}

fn check_cancel() -> Res<()> {
    if app().cancel.load(Ordering::SeqCst) { Err(Fail::Cancelled) } else { Ok(()) }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- adlar

fn anime_of(aid: &str) -> Option<Anime> {
    app().store.lock().animes.get(aid).cloned()
}

fn ep_title(anime: &Anime, season: i64, episode: i64) -> String {
    anime.seasons.iter().find(|s| s.n == season).and_then(|s| s.episodes.get(&episode.to_string())).cloned().unwrap_or_default()
}

fn format_name(template: &str, anime_name: &str, season: i64, episode: i64, title: &str) -> String {
    let args = [("anime", Arg::Str(anime_name)), ("s", Arg::Int(season)), ("b", Arg::Int(episode)), ("e", Arg::Int(episode)), ("title", Arg::Str(title))];
    let name = format_tpl(template, &args).unwrap_or_else(|| format_tpl(DEFAULT_TEMPLATE, &args).unwrap_or_default());
    // Bölüm adı boşsa sonda kalan ayraçları temizle
    let name = name.trim().trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '_' | '.'));
    safe_name(name)
}

fn ep_name_t(ep: &Ep, anime: &Anime, with_anime: bool, template: &str) -> String {
    let name = if with_anime { if anime.name.is_empty() { ep.anime.as_str() } else { anime.name.as_str() } } else { "" };
    let title = ep_title(anime, ep.season, ep.episode);
    let tpl = if template.is_empty() { DEFAULT_TEMPLATE } else { template };
    let out = format_name(tpl, name, ep.season, ep.episode, &title);
    if with_anime { out } else { out.trim_matches(|c| c == ' ' || c == '-').to_string() }
}

fn ep_name(ep: &Ep, anime: &Anime, with_anime: bool) -> String {
    let tpl = app().store.lock().settings.name_template.clone();
    ep_name_t(ep, anime, with_anime, &tpl)
}

fn out_base(ep: &Ep, anime: &Anime, out_dir: &str) -> String {
    let name = safe_name(if anime.name.is_empty() { &ep.anime } else { &anime.name });
    path_str(&Path::new(out_dir).join(name).join(format!("Season {:02}", ep.season)).join(ep_name(ep, anime, true)))
}

/// Kuyruktan çıkarılan bölümün gerçek durumu.
fn rest_status(ep: &Ep) -> &'static str {
    if ep.src.is_empty() {
        ST_NOFILE
    } else if exists(&ep.out_file) {
        ST_DONE
    } else if ep.analyzed {
        ST_ANALYZED
    } else {
        ST_READY
    }
}

fn summary(skip: &BTreeMap<String, Skip>, n_refs: usize) -> String {
    let parts: Vec<String> = ["op", "ed"]
        .iter()
        .filter_map(|k| skip.get(*k).map(|s| format!("{} {}–{}{}", k.to_uppercase(), fmt_t(s.start), fmt_t(s.end), if s.src == "elle" { l!(" (elle)", " (manual)") } else { String::new() })))
        .collect();
    if !parts.is_empty() {
        parts.join(", ")
    } else if n_refs == 0 {
        l!("Karşılaştırma için aynı sezondan en az 2 dosya gerekli", "At least 2 files from the same season are needed to compare")
    } else {
        l!("OP/ED bulunamadı", "No OP/ED found")
    }
}

// ---------------------------------------------------------------- analiz

/// Dosya içeriğine bağlı anahtar: yeniden adlandırınca önbellek geçersiz olmaz.
fn fp_path(src: &str) -> std::io::Result<PathBuf> {
    use std::io::Read;
    let md = std::fs::metadata(src)?;
    let mtime = md.modified()?.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut h = sha1_smol::Sha1::new();
    h.update(format!("{}|{}|", md.len(), mtime).as_bytes());
    let mut buf = Vec::with_capacity(1 << 20);
    std::fs::File::open(src)?.take(1 << 20).read_to_end(&mut buf)?;
    h.update(&buf);
    Ok(paths().data.join("parmakizi").join(format!("{}.fp", h.digest())))
}

fn get_fp(src: &str) -> Res<opedfind::Fp> {
    let path = fp_path(src)?;
    if let Some(fp) = opedfind::load_fp(&path) {
        return Ok(fp);
    }
    let fp = opedfind::fingerprint(src);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("{:?}.tmp", std::thread::current().id()).replace(['(', ')'], ""));
    opedfind::save_fp(&fp, &tmp)?;
    let _ = std::fs::rename(&tmp, &path);
    Ok(fp)
}

/// Sezondaki tüm dosyaların parmak izini (paralel) çıkarır, hedef bölümlerin OP/ED'sini bulur.
fn analyze_season(aid: &str, season: i64, target_ids: &[String]) -> Res<()> {
    let a = app();
    let sl = lock(&a.season_locks).entry((aid.to_string(), season)).or_default().clone();
    let _g = lock(&sl);
    let eps: Vec<Ep> = a.store.lock().season_eps(aid, season).into_iter().filter(|e| exists(&e.src)).collect();
    let targets: Vec<String> = target_ids.iter().filter(|t| eps.iter().any(|e| &e.id == *t)).cloned().collect();
    for t in &targets {
        a.store.update(t, false, |e| {
            e.status = ST_PROCESSING.into();
            e.progress = 0.0;
            e.message = l!("Ses parmak izi çıkarılıyor", "Fingerprinting audio");
        });
    }
    let done = AtomicUsize::new(0);
    let fps: Vec<Res<opedfind::Fp>> = a.fp_pool.install(|| {
        use rayon::prelude::*;
        eps.par_iter()
            .map(|e| {
                check_cancel()?;
                let fp = get_fp(&e.src)?;
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                for t in &targets {
                    a.store.update(t, false, |x| x.progress = round_to(0.8 * n as f64 / eps.len() as f64, 3));
                }
                Ok(fp)
            })
            .collect()
    });
    let fps: Vec<opedfind::Fp> = fps.into_iter().collect::<Res<_>>()?;
    check_cancel()?;
    for t in &targets {
        a.store.update(t, false, |e| e.message = l!("OP/ED aranıyor", "Looking for OP/ED"));
    }
    let items: Vec<(String, i64, &opedfind::Fp)> = eps.iter().zip(&fps).map(|(e, f)| (e.id.clone(), e.episode, f)).collect();
    let need: Vec<String> = targets.iter().filter(|t| a.store.get(t).is_some_and(|e| e.manual_skip().len() < 2)).cloned().collect();
    let det = if items.len() > 1 && !need.is_empty() {
        opedfind::detect_season(&items, &need, MAX_REFS, &a.cancel)
    } else {
        HashMap::new()
    };
    check_cancel()?;
    let n_refs = items.len().saturating_sub(1);
    for t in &targets {
        let Some(ep) = a.store.get(t) else { continue };
        let mut skip = ep.manual_skip();
        let d = det.get(t).cloned().unwrap_or_default();
        for (k, r) in [("op", d.op), ("ed", d.ed)] {
            if let (false, Some(r)) = (skip.contains_key(k), r) {
                skip.insert(k.into(), Skip { start: round_to(r.0, 2), end: round_to(r.1, 2), src: "ses".into() });
            }
        }
        let dur = eps.iter().zip(&fps).find(|(e, _)| &e.id == t).map(|(_, f)| f.dur);
        a.store.update(t, false, |e| {
            e.skip = skip.clone();
            e.analyzed = true;
            e.progress = 1.0;
            e.dur = dur;
            e.status = rest_status(e).into();
            e.message = summary(&skip, n_refs);
        });
    }
    a.store.save();
    Ok(())
}

fn run_analyze(ids: Vec<String>) {
    let a = app();
    let mut groups: BTreeMap<(String, i64), Vec<String>> = BTreeMap::new();
    for i in ids {
        if let Some(ep) = a.store.get(&i) {
            if ep.status == ST_QUEUED {
                groups.entry((ep.anime, ep.season)).or_default().push(i);
            }
        }
    }
    for ((aid, season), tids) in groups {
        if a.cancel.load(Ordering::SeqCst) {
            break;
        }
        match analyze_season(&aid, season, &tids) {
            Ok(()) => {}
            Err(Fail::Cancelled) => break,
            Err(Fail::Msg(m)) => {
                log(&format!("[analiz hatası] {aid} S{season}: {m}"));
                for t in &tids {
                    a.store.update(t, true, |e| {
                        e.status = ST_ERROR.into();
                        e.progress = 0.0;
                        e.message = m.chars().take(300).collect();
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------- dışa aktarma

fn work_dir() -> PathBuf {
    paths().data.join("gecici")
}

/// Kaynak altyazıyı (yanındaki dosya ya da gömülü) kesime göre kaydırıp kaydeder.
fn prepare_sub(ep: &Ep, keep: &[(f64, f64)]) -> Res<Option<String>> {
    let id_name = safe_name(&ep.id.replace('/', "_"));
    let mut tmp = None;
    let src_sub = if exists(&ep.sub_src) {
        ep.sub_src.clone()
    } else {
        let Some((idx, codec)) = cutter::pick_sub_stream(&cutter::probe(&ep.src)) else { return Ok(None) };
        std::fs::create_dir_all(work_dir())?;
        let base = path_str(&work_dir().join(format!("cikar_{id_name}")));
        let Some(p) = cutter::extract_sub(&ep.src, idx, &codec, &base) else { return Ok(None) };
        tmp = Some(p.clone());
        p
    };
    let parsed = subs::parse(&src_sub);
    if let Some(t) = &tmp {
        let _ = std::fs::remove_file(t);
    }
    let sub = subs::cut(parsed?, keep);
    let dir = paths().data.join("altyazi");
    std::fs::create_dir_all(&dir)?;
    let path = path_str(&dir.join(format!("{id_name}{}", subs::ext_for(&sub))));
    subs::write(&sub, &path)?;
    Ok(Some(path))
}

fn progress_setter(eid: &str) -> impl Fn(f64) + '_ {
    let last = std::cell::Cell::new(-1.0f64);
    move |v: f64| {
        if v - last.get() >= 0.005 || v >= 1.0 {
            last.set(v);
            app().store.update(eid, false, |e| e.progress = round_to(v, 4));
        }
    }
}

fn export_one(eid: &str, force_encode: bool) -> Res<()> {
    let a = app();
    let mut ep = match a.store.get(eid) {
        Some(e) if exists(&e.src) => e,
        _ => {
            a.store.update(eid, true, |e| {
                e.status = ST_ERROR.into();
                e.message = l!("Kaynak dosya bulunamadı", "Source file not found");
            });
            return Ok(());
        }
    };
    let s = a.store.settings();
    if s.remove_oped && !ep.analyzed {
        a.store.update(eid, true, |e| e.status = ST_QUEUED.into());
        analyze_season(&ep.anime, ep.season, &[eid.to_string()])?;
        ep = a.store.get(eid).ok_or(l!("bölüm yok", "episode not found"))?;
    }
    check_cancel()?;
    let anime = anime_of(&ep.anime).unwrap_or_default();
    a.store.update(eid, true, |e| {
        e.status = ST_PROCESSING.into();
        e.progress = 0.0;
        e.message = l!("Hazırlanıyor", "Preparing");
    });
    let dur = opedfind::probe_duration(&ep.src);
    if dur <= 0.0 {
        return Err(l!("Video süresi okunamadı, dosya bozuk olabilir", "Could not read the video duration, the file may be damaged").into());
    }
    let mut cuts = Vec::new();
    if s.remove_oped {
        for k in ["op", "ed"] {
            if let Some(r) = ep.skip.get(k) {
                let mut b = r.end.min(dur);
                if k == "ed" && s.skip_preview {
                    b = dur; // ED'den sonraki özet/fragman da gitsin
                }
                if 0.0 <= r.start && r.start < b {
                    cuts.push((r.start, b));
                }
            }
        }
    }
    let mut keep = if cuts.is_empty() { vec![(0.0, dur)] } else { cutter::keep_segments(dur, &cuts) };
    let mut fast = None;
    if !cuts.is_empty() && s.cut_mode == "fast" && !force_encode {
        fast = cutter::plan_fast(&ep.src, &keep);
        if let Some(f) = &fast {
            keep = f.clone();
        }
    }
    a.store.update(eid, true, |e| e.message = l!("Altyazı hazırlanıyor", "Preparing subtitles"));
    let sub_file = prepare_sub(&ep, &keep)?;
    a.store.update(eid, true, |e| e.message = if cuts.is_empty() { l!("Kaydediliyor", "Saving") } else { l!("Kesiliyor", "Cutting") });
    let base = out_base(&ep, &anime, &s.out_dir);
    let wd = path_str(&work_dir().join(format!("parca_{:?}", std::thread::current().id()).replace(['(', ')', ' '], "")));
    let job = cutter::CutJob {
        src: &ep.src,
        dst_base: &base,
        duration: dur,
        cuts: &cuts,
        encoder: &s.encoder,
        quality: s.quality,
        container_pref: &s.container,
        sub: sub_file.as_deref(),
        force_encode,
        fast_segs: fast.as_deref(),
        workdir: &wd,
    };
    let (dst, mut used) = cutter::cut(&job, &progress_setter(eid))?;
    if !ep.out_file.is_empty() && ep.out_file != dst && exists(&ep.out_file) {
        let _ = std::fs::remove_file(&ep.out_file);
    }
    let mut msg = if s.remove_oped { summary(&ep.skip, 1) } else { l!("OP/ED silme kapalı", "OP/ED removal is off") };
    if !cuts.is_empty() && s.cut_mode == "fast" && fast.is_none() && !force_encode {
        used += &l!(" (kesimde anahtar kare yok, kodlandı)", " (no keyframe at the cut, re-encoded)");
    }
    msg += &format!(" · {used}{}", if sub_file.is_some() { l!(" · altyazı", " · subtitles") } else { String::new() });
    a.store.update(eid, true, |e| {
        e.status = ST_DONE.into();
        e.progress = 1.0;
        e.out_file = dst.clone();
        e.sub_file = sub_file.clone().unwrap_or_default();
        e.message = msg;
    });
    log(&format!("[dışa aktarıldı] {eid} -> {dst}"));
    Ok(())
}

fn run_export(ids: &[String], force_encode: bool) {
    let a = app();
    let mut order: Vec<Ep> = ids.iter().filter_map(|i| a.store.get(i)).collect();
    order.sort_by(|x, y| (&x.anime, x.season, x.episode).cmp(&(&y.anime, y.season, y.episode)));
    for ep in order {
        if a.cancel.load(Ordering::SeqCst) {
            break;
        }
        if a.store.get(&ep.id).map(|e| e.status) != Some(ST_QUEUED.into()) {
            continue;
        }
        match export_one(&ep.id, force_encode) {
            Ok(()) => {}
            Err(Fail::Cancelled) => break,
            Err(Fail::Msg(m)) => {
                if a.cancel.load(Ordering::SeqCst) {
                    break;
                }
                log(&format!("[dışa aktarma hatası] {}: {m}", ep.id));
                a.store.update(&ep.id, true, |e| {
                    e.status = ST_ERROR.into();
                    e.progress = 0.0;
                    e.message = m.chars().take(300).collect();
                });
            }
        }
    }
}

fn merge_label(aid: &str, eps: &[Ep]) -> String {
    let anime = anime_of(aid).unwrap_or_default();
    let mut seasons: Vec<i64> = eps.iter().map(|e| e.season).collect();
    seasons.sort();
    seasons.dedup();
    let all_eps: Vec<Ep> = app().store.lock().anime_eps(aid).into_iter().filter(|e| !e.src.is_empty()).collect();
    let n_seasons = if anime.seasons.is_empty() { seasons.len() } else { anime.seasons.len() };
    if eps.len() == all_eps.len() && seasons.len() == n_seasons {
        return if seasons.len() > 1 { l!("Tüm Sezonlar", "All Seasons") } else { format!("S{}", seasons[0]) };
    }
    if seasons.len() == 1 && eps.len() == all_eps.iter().filter(|e| e.season == seasons[0]).count() {
        return format!("S{}", seasons[0]);
    }
    format!("{} - {}", ep_name(&eps[0], &anime, false), ep_name(&eps[eps.len() - 1], &anime, false))
}

fn run_merge(aid: &str, ids: &[String]) {
    let a = app();
    a.store.set_merge(aid, |m| {
        m.status = ST_PROCESSING.into();
        m.progress = 0.0;
        m.message = l!("Bölümler dışa aktarılıyor", "Exporting episodes");
        m.file = String::new();
    });
    let todo: Vec<String> = ids.iter().filter(|i| a.store.get(i).is_some_and(|e| e.status == ST_QUEUED)).cloned().collect();
    // Hızlı modda bölümler aynı kaynaktan kopyalandığı için birleştirme de kopyalama olur
    let s = a.store.settings();
    run_export(&todo, s.cut_mode != "fast");
    if a.cancel.load(Ordering::SeqCst) {
        a.store.set_merge(aid, |m| {
            m.status = "iptal".into();
            m.message = l!("İptal edildi", "Cancelled");
        });
        return;
    }
    let mut eps: Vec<Ep> = ids.iter().filter_map(|i| a.store.get(i)).collect();
    eps.sort_by_key(|e| (e.season, e.episode));
    eps.retain(|e| e.status == ST_DONE && exists(&e.out_file));
    if eps.is_empty() {
        a.store.set_merge(aid, |m| {
            m.status = ST_ERROR.into();
            m.message = l!("Birleştirilecek bölüm yok", "No episodes to join");
        });
        return;
    }
    let anime = anime_of(aid).unwrap_or_default();
    let name = safe_name(if anime.name.is_empty() { aid } else { &anime.name });
    let label = merge_label(aid, &eps);
    let items: Vec<merger::Item> = eps
        .iter()
        .map(|e| merger::Item {
            file: e.out_file.clone(),
            sub_file: (!e.sub_file.is_empty()).then(|| e.sub_file.clone()),
            chapter: ep_name(e, &anime, false),
        })
        .collect();
    let dst_base = path_str(&Path::new(&s.out_dir).join(&name).join(format!("{name} {label}")));
    let title = format!("{} {label}", if anime.name.is_empty() { &name } else { &anime.name });
    let res = merger::merge(
        &items,
        &dst_base,
        &title,
        &s.container,
        s.quality,
        &path_str(&work_dir().join("birlestir")),
        &|msg| a.store.set_merge(aid, |m| m.message = msg.to_string()),
        &|v| a.store.set_merge(aid, |m| m.progress = round_to(v, 4)),
    );
    let dst = match res {
        Ok(d) => d,
        Err(e) => {
            log(&format!("[birleştirme hatası] {aid}: {e}"));
            let msg: String = e.to_string().chars().take(300).collect();
            a.store.set_merge(aid, |m| {
                m.status = ST_ERROR.into();
                m.message = msg;
            });
            return;
        }
    };
    if s.merge_delete_eps {
        for e in &eps {
            if std::fs::remove_file(&e.out_file).is_ok() {
                a.store.update(&e.id, true, |x| {
                    x.out_file = String::new();
                    x.status = rest_status(x).into();
                    x.message = format!("{} · tek videoya eklendi", e.message);
                });
            }
        }
    }
    let n = eps.len();
    a.store.set_merge(aid, |m| {
        m.status = ST_DONE.into();
        m.progress = 1.0;
        m.file = dst.clone();
        m.message = l!("{n} bölüm birleştirildi", "Joined {n} episodes");
    });
    log(&format!("[tek video] {dst}"));
}

// ---------------------------------------------------------------- iş kuyrukları

fn spawn_worker(kind: usize, rx: Receiver<Task>) {
    std::thread::spawn(move || {
        let a = app();
        for task in rx {
            a.idle[kind].store(false, Ordering::SeqCst);
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match task {
                Task::Analyze(ids) => run_analyze(ids),
                Task::Export(ids) => run_export(&ids, false),
                Task::Merge(aid, ids) => run_merge(&aid, &ids),
            }));
            if r.is_err() {
                log("[hata] iş sırasında beklenmeyen hata");
            }
            a.idle[kind].store(true, Ordering::SeqCst);
            if a.pending[kind].fetch_sub(1, Ordering::SeqCst) == 1 {
                // İptal edilen işlerden kuyrukta kalanları serbest bırak
                let job = if kind == ANALIZ { "analiz" } else { "aktar" };
                let cancelled = a.cancel.load(Ordering::SeqCst);
                let mut d = a.store.lock();
                for ep in d.eps.values_mut() {
                    if ep.status == ST_QUEUED && ep.job.as_deref() == Some(job) {
                        ep.status = rest_status(ep).into();
                        if cancelled {
                            ep.message = l!("İptal edildi", "Cancelled");
                        }
                    }
                }
                let all_idle = (0..2).all(|k| a.idle[k].load(Ordering::SeqCst) && a.pending[k].load(Ordering::SeqCst) == 0);
                if all_idle {
                    a.cancel.store(false, Ordering::SeqCst);
                }
                a.store.save_locked(&mut d);
            }
        }
    });
}

fn submit(kind: usize, t: Task) {
    let a = app();
    a.pending[kind].fetch_add(1, Ordering::SeqCst);
    let _ = lock(&a.queues[kind]).send(t);
}

fn queue_eps(ids: &[String], job: &str) -> Vec<String> {
    let a = app();
    let mut ok = Vec::new();
    let mut d = a.store.lock();
    for i in ids {
        if let Some(ep) = d.eps.get_mut(i) {
            if !ep.src.is_empty() && !busy(&ep.status) {
                ep.status = ST_QUEUED.into();
                ep.progress = 0.0;
                ep.job = Some(job.into());
                ep.message = if job == "analiz" { l!("Analiz sırasında", "Waiting for analysis") } else { l!("Sırada", "Queued") };
                ok.push(i.clone());
            }
        }
    }
    a.store.save_locked(&mut d);
    ok
}

fn start_analyze(ids: &[String]) -> usize {
    let ok = queue_eps(ids, "analiz");
    let n = ok.len();
    if n > 0 {
        submit(ANALIZ, Task::Analyze(ok));
    }
    n
}

// ---------------------------------------------------------------- oynatıcı

fn find_mpv_uncached(setting: &str) -> Option<String> {
    let la = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let mut cands = vec![
        setting.to_string(),
        path_str(&paths().player.join("mpv").join("mpv.exe")),
        path_str(&paths().player.join("mpv.exe")),
    ];
    if let Ok(path) = std::env::var("PATH") {
        for d in std::env::split_paths(&path) {
            cands.push(path_str(&d.join("mpv.exe")));
        }
    }
    cands.push(format!("{la}\\Microsoft\\WinGet\\Links\\mpv.exe"));
    if let Ok(rd) = std::fs::read_dir(format!("{la}\\Microsoft\\WinGet\\Packages")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().to_lowercase().starts_with("shinchiro.mpv") {
                cands.push(path_str(&e.path().join("mpv.exe")));
            }
        }
    }
    cands.into_iter().find(|c| !c.is_empty() && Path::new(c).is_file())
}

fn find_mpv() -> Option<String> {
    let a = app();
    let setting = a.store.lock().settings.mpv_path.clone();
    let mut c = lock(&a.mpv_cache);
    if let Some((t, s, r)) = c.as_ref() {
        if *s == setting && t.elapsed() < Duration::from_secs(5) {
            return r.clone();
        }
    }
    let r = find_mpv_uncached(&setting);
    *c = Some((Instant::now(), setting, r.clone()));
    r
}

fn play(aid: &str, ids: &[String]) -> Result<usize, String> {
    let mpv = find_mpv().ok_or_else(|| l!("mpv bulunamadı. player\\mpv klasörüne kur ya da Ayarlar'dan mpv.exe yolunu gir.", "mpv not found. Put it in the player\\mpv folder or set the mpv.exe path in Settings."))?;
    let a = app();
    let anime = anime_of(aid).unwrap_or_default();
    let (eps, s) = {
        let d = a.store.lock();
        (d.anime_eps(aid).into_iter().filter(|e| exists(&e.src)).collect::<Vec<_>>(), d.settings.clone())
    };
    if eps.is_empty() {
        return Err(l!("Oynatılacak dosya yok", "No files to play"));
    }
    let mut chosen: Vec<&Ep> = eps.iter().filter(|e| ids.contains(&e.id)).collect();
    if chosen.is_empty() {
        chosen = eps.iter().collect();
    }
    // Seçim tek bölümse oradan sezon sonuna kadar oynat
    if chosen.len() == 1 {
        let st = (chosen[0].season, chosen[0].episode);
        chosen = eps.iter().filter(|e| (e.season, e.episode) >= st).collect();
    }
    let mut data = Map::new();
    for e in &eps {
        let mut info = Map::new();
        info.insert("id".into(), json!(e.id));
        info.insert("title".into(), json!(ep_name_t(e, &anime, true, &s.name_template)));
        for k in ["op", "ed"] {
            if let Some(r) = e.skip.get(k) {
                info.insert(k.into(), json!([r.start, r.end]));
            }
        }
        data.insert(abspath(&e.src), Value::Object(info));
    }
    let dd = &paths().data;
    let data_file = dd.join("oynatici.json");
    std::fs::write(&data_file, Value::Object(data).to_string()).map_err(|e| e.to_string())?;
    let playlist = dd.join("oynatma_listesi.m3u8");
    let mut pl = String::from("#EXTM3U\n");
    for e in &chosen {
        pl += &format!("#EXTINF:-1,{}\n{}\n", ep_name_t(e, &anime, true, &s.name_template), abspath(&e.src));
    }
    std::fs::write(&playlist, pl).map_err(|e| e.to_string())?;
    let yn = |b: bool| if b { "yes" } else { "no" };
    let mut c = std::process::Command::new(&mpv);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(DETACHED);
    }
    c.arg(format!("--config-dir={}", path_str(&paths().player_conf)))
        .arg(format!("--playlist={}", path_str(&playlist)))
        .arg(format!("--script-opts-append=tobasu-veri={}", path_str(&data_file)))
        .arg(format!("--script-opts-append=tobasu-izleme={}", path_str(&dd.join("izleme.json"))))
        .arg(format!("--script-opts-append=tobasu-oto={}", yn(s.auto_skip)))
        .arg(format!("--script-opts-append=tobasu-sonu={}", yn(s.skip_preview)))
        .arg(format!("--script-opts-append=tobasu-dil={}", if is_en() { "en" } else { "tr" }))
        .args(if is_en() { vec!["--slang=eng,en,tur,tr", "--alang=jpn,ja,eng,en"] } else { vec![] })
        .current_dir(Path::new(&mpv).parent().unwrap_or(Path::new(".")));
    c.spawn().map_err(|e| l!("mpv başlatılamadı: {e}", "Could not start mpv: {e}"))?;
    Ok(chosen.len())
}

pub fn watch_mtime() -> Option<SystemTime> {
    std::fs::metadata(paths().data.join("izleme.json")).and_then(|m| m.modified()).ok()
}

/// Oynatıcının yazdığı izleme kayıtları; dosya o an yazılıyorsa önceki okuma kullanılır.
fn load_watch() -> Map<String, Value> {
    let a = app();
    let mut w = lock(&a.watch);
    let m = watch_mtime();
    if m.is_some() && m != w.0 {
        if let Ok(text) = std::fs::read_to_string(paths().data.join("izleme.json")) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                w.0 = m;
                w.1 = v["eps"].as_object().cloned().unwrap_or_default();
            }
        }
    }
    w.1.clone()
}

// ---------------------------------------------------------------- yeniden adlandırma

fn rename_files(ids: &[String], undo: bool) -> (usize, Vec<String>) {
    let a = app();
    let (mut done, mut errors) = (0, Vec::new());
    let mut d = a.store.lock();
    let tpl = d.settings.name_template.clone();
    for i in ids {
        let Some(ep) = d.eps.get(i).cloned() else { continue };
        if ep.src.is_empty() || busy(&ep.status) || (undo && ep.orig_src.is_none()) {
            continue;
        }
        let src = Path::new(&ep.src);
        let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let new = if undo {
            ep.orig_src.clone().unwrap_or_default()
        } else {
            let anime = d.animes.get(&ep.anime).cloned().unwrap_or_default();
            path_str(&src.with_file_name(format!("{}{ext}", ep_name_t(&ep, &anime, true, &tpl))))
        };
        if new.to_lowercase() == ep.src.to_lowercase() {
            continue;
        }
        let newp = Path::new(&new);
        let base = |p: &Path| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        if newp.exists() {
            errors.push(format!("{} zaten var", base(newp)));
            continue;
        }
        if let Err(e) = std::fs::rename(src, newp) {
            errors.push(format!("{}: {e}", base(src)));
            continue;
        }
        let e = d.eps.get_mut(i).unwrap();
        if exists(&ep.sub_src) {
            let sp = Path::new(&ep.sub_src);
            let sname = base(sp);
            if sname.to_lowercase().starts_with(&stem.to_lowercase()) {
                let new_stem = newp.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let new_sub = sp.with_file_name(format!("{new_stem}{}", &sname[stem.len()..]));
                if !new_sub.exists() && std::fs::rename(sp, &new_sub).is_ok() {
                    e.sub_src = path_str(&new_sub);
                }
            }
        }
        if undo {
            e.orig_src = None;
        } else if e.orig_src.is_none() {
            e.orig_src = Some(ep.src.clone());
        }
        e.src = new;
        done += 1;
    }
    a.store.save_locked(&mut d);
    (done, errors)
}

// ---------------------------------------------------------------- API

pub type ApiRes = Result<(u16, Value), String>;

fn s_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn int_of(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn ids_of(body: &Value) -> Vec<String> {
    body["ids"].as_array().map(|a| a.iter().map(s_of).collect()).unwrap_or_default()
}

pub fn api_search(q: &str) -> ApiRes {
    let q = q.trim();
    if q.chars().count() < 2 {
        return Ok((400, json!({"error": l!("En az 2 harf yaz", "Type at least 2 letters")})));
    }
    Ok((200, json!({"results": metadata::search(q)?})))
}

fn api_add(body: &Value) -> ApiRes {
    let id = int_of(&body["anilist_id"]).ok_or(l!("anilist_id eksik", "anilist_id is missing"))?;
    let anime = metadata::build_anime(id)?;
    let aid = anime.id.clone();
    app().store.add_anime(anime);
    Ok((200, json!({"id": aid})))
}

fn api_manual(body: &Value) -> ApiRes {
    let name = s_of(&body["name"]).split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Ok((400, json!({"error": l!("Anime adı boş", "Anime name is empty")})));
    }
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(60).collect();
    let aid = if slug.is_empty() {
        let h = sha1_smol::Sha1::from(name.as_bytes()).digest().to_string();
        format!("el-{}", &h[..10])
    } else {
        format!("el-{slug}")
    };
    let a = app();
    if !a.store.lock().animes.contains_key(&aid) {
        let n = int_of(&body["seasons"]).unwrap_or(1).clamp(1, 30);
        a.store.add_anime(Anime {
            id: aid.clone(),
            name,
            image: None,
            seasons: (1..=n).map(|i| Season { n: i, ..Default::default() }).collect(),
        });
    }
    Ok((200, json!({"id": aid})))
}

fn api_anime_edit(body: &Value) -> ApiRes {
    let a = app();
    let aid = s_of(&body["id"]);
    let mut d = a.store.lock();
    if !d.animes.contains_key(&aid) {
        return Ok((404, json!({"error": l!("anime yok", "anime not found")})));
    }
    if body["remove"].as_bool().unwrap_or(false) {
        if d.eps.values().any(|e| e.anime == aid && busy(&e.status)) {
            return Ok((409, json!({"error": l!("İşlem sürerken kaldırılamaz", "Cannot remove while a job is running")})));
        }
        d.animes.shift_remove(&aid);
        d.eps.retain(|_, e| e.anime != aid);
    } else {
        let name = s_of(&body["name"]).trim().chars().take(120).collect::<String>();
        if !name.is_empty() {
            d.animes.get_mut(&aid).unwrap().name = name;
        }
    }
    a.store.save_locked(&mut d);
    Ok((200, json!({"ok": true})))
}

fn api_pickdir(_: &Value) -> ApiRes {
    let _g = lock(&app().pick_lock);
    let p = rfd::FileDialog::new().set_title(l!("Bölümlerin olduğu klasörü seç", "Choose the folder with the episodes")).pick_folder();
    Ok((200, json!({"path": p.map(|p| path_str(&p)).unwrap_or_default()})))
}

fn api_assign(body: &Value) -> ApiRes {
    let a = app();
    let aid = s_of(&body["anime"]);
    let path = s_of(&body["path"]).trim().trim_matches('"').to_string();
    let season = match &body["season"] {
        Value::Null => None,
        v if matches!(s_of(v).as_str(), "" | "all") => None,
        v => Some(int_of(v).ok_or(l!("sezon hatalı", "invalid season"))?),
    };
    let Some(anime) = anime_of(&aid) else { return Ok((404, json!({"error": l!("anime yok", "anime not found")}))) };
    if !Path::new(&path).is_dir() {
        return Ok((400, json!({"error": l!("Klasör bulunamadı: {path}", "Folder not found: {path}")})));
    }
    let counts: BTreeMap<i64, i64> = anime.seasons.iter().map(|s| (s.n, s.count)).collect();
    let (found, unmatched) = matcher::assign(&path, &counts, season);
    let redo: Vec<String> = {
        let mut d = a.store.lock();
        let mut changed = Vec::new();
        for (&(s, e), f) in &found {
            let an = d.animes.get_mut(&aid).unwrap();
            if !an.seasons.iter().any(|x| x.n == s) {
                an.seasons.push(Season { n: s, ..Default::default() });
            }
            let ep = d.ensure_ep(&aid, s, e);
            if busy(&ep.status) || ep.src == *f {
                continue;
            }
            ep.skip = ep.manual_skip();
            ep.src = f.clone();
            ep.sub_src = matcher::find_sub(f).unwrap_or_default();
            ep.status = ST_READY.into();
            ep.analyzed = false;
            ep.message = String::new();
            ep.orig_src = None;
            changed.push(ep.id.clone());
        }
        d.animes.get_mut(&aid).unwrap().seasons.sort_by_key(|s| s.n);
        // Yeni dosyalar sezondaki diğerlerinin sonucunu da etkiler; o sezonları baştan analiz et
        let seasons: Vec<i64> = changed.iter().filter_map(|i| d.eps.get(i)).map(|e| e.season).collect();
        let redo = d
            .eps
            .values()
            .filter(|e| e.anime == aid && seasons.contains(&e.season) && !e.src.is_empty() && !busy(&e.status))
            .map(|e| e.id.clone())
            .collect();
        a.store.save_locked(&mut d);
        redo
    };
    start_analyze(&redo);
    let names: Vec<String> = unmatched.iter().map(|u| Path::new(u).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()).collect();
    Ok((200, json!({"matched": found.len(), "unmatched": names})))
}

fn api_action(body: &Value) -> ApiRes {
    let a = app();
    let ids = ids_of(body);
    let aid = s_of(&body["anime"]);
    match body["action"].as_str().unwrap_or("") {
        "analyze" => Ok((200, json!({"count": start_analyze(&ids)}))),
        "export" => {
            let ok = queue_eps(&ids, "aktar");
            let n = ok.len();
            if n > 0 {
                submit(AKTAR, Task::Export(ok));
            }
            Ok((200, json!({"count": n})))
        }
        "merge" => {
            let eps: Vec<Ep> = ids.iter().filter_map(|i| a.store.get(i)).filter(|e| e.anime == aid && !e.src.is_empty()).collect();
            if eps.len() < 2 {
                return Ok((400, json!({"error": l!("Tek video için dosyası olan en az 2 bölüm seç", "Select at least 2 episodes with files to make a single video")})));
            }
            if a.store.lock().merges.get(&aid).is_some_and(|m| busy(&m.status)) {
                return Ok((409, json!({"error": l!("Bu anime için birleştirme zaten sürüyor", "A join is already running for this anime")})));
            }
            let need: Vec<String> = eps.iter().filter(|e| e.status != ST_DONE || !exists(&e.out_file)).map(|e| e.id.clone()).collect();
            queue_eps(&need, "aktar");
            a.store.set_merge(&aid, |m| {
                m.status = ST_QUEUED.into();
                m.progress = 0.0;
                m.message = l!("Sırada", "Queued");
                m.file = String::new();
            });
            let n = eps.len();
            submit(AKTAR, Task::Merge(aid, eps.into_iter().map(|e| e.id).collect()));
            Ok((200, json!({"count": n})))
        }
        "play" => Ok((200, json!({"count": play(&aid, &ids)?}))),
        act @ ("rename" | "rename_undo") => {
            let (n, errors) = rename_files(&ids, act == "rename_undo");
            Ok((200, json!({"count": n, "errors": errors})))
        }
        "cancel" => {
            a.cancel.store(true, Ordering::SeqCst);
            cutter::kill_all();
            Ok((200, json!({"ok": true})))
        }
        "unassign" => {
            let mut d = a.store.lock();
            for i in &ids {
                if let Some(ep) = d.eps.get_mut(i) {
                    if !busy(&ep.status) {
                        ep.src = String::new();
                        ep.sub_src = String::new();
                        ep.status = ST_NOFILE.into();
                        ep.analyzed = false;
                        ep.message = String::new();
                    }
                }
            }
            a.store.save_locked(&mut d);
            Ok((200, json!({"ok": true})))
        }
        _ => Ok((400, json!({"error": l!("bilinmeyen işlem", "unknown action")}))),
    }
}

fn api_edit(body: &Value) -> ApiRes {
    let a = app();
    let eid = s_of(&body["id"]);
    let Some(ep) = a.store.get(&eid) else { return Ok((404, json!({"error": l!("bölüm yok", "episode not found")}))) };
    let mut skip = ep.skip.clone();
    for k in ["op", "ed"] {
        if body.get(k).is_none() {
            continue;
        }
        let txt = s_of(&body[k]).trim().to_string();
        if txt.is_empty() {
            skip.remove(k);
            continue;
        }
        let Some((s, e)) = parse_range(&txt) else {
            return Ok((400, json!({"error": l!("{} zamanı anlaşılamadı, örnek: 1:30-3:00", "Could not read the {} time, example: 1:30-3:00", k.to_uppercase())})));
        };
        skip.insert(k.into(), Skip { start: s, end: e, src: "elle".into() });
    }
    let msg = summary(&skip, 1);
    a.store.update(&eid, true, |e| {
        e.skip = skip;
        e.message = msg;
    });
    Ok((200, json!({"ok": true})))
}

fn api_settings(body: &Value) -> ApiRes {
    let a = app();
    let mut d = a.store.lock();
    let s = &mut d.settings;
    let out = s_of(&body["out_dir"]).trim().to_string();
    if !out.is_empty() {
        let expanded = if let Some(rest) = out.strip_prefix('~') { format!("{}{rest}", std::env::var("USERPROFILE").unwrap_or_default()) } else { out };
        s.out_dir = abspath(&expanded);
    }
    if body.get("name_template").is_some() {
        let mut tpl = s_of(&body["name_template"]).trim().to_string();
        if tpl.is_empty() {
            tpl = DEFAULT_TEMPLATE.into();
        }
        let test = [("anime", Arg::Str("A")), ("s", Arg::Int(1)), ("b", Arg::Int(1)), ("e", Arg::Int(1)), ("title", Arg::Str("T"))];
        if format_tpl(&tpl, &test).is_none() {
            return Ok((400, json!({"error": l!("Ad şablonu hatalı. Kullanılabilir: {{anime}} {{s}} {{b}} {{title}}, örnek: {{anime}} S{{s:02}}E{{b:02}}", "Invalid name template. Available: {{anime}} {{s}} {{b}} {{title}}, example: {{anime}} S{{s:02}}E{{b:02}}")})));
        }
        s.name_template = tpl;
    }
    if body.get("mpv_path").is_some() {
        s.mpv_path = s_of(&body["mpv_path"]).trim().trim_matches('"').to_string();
    }
    let flag = |k: &str| body.get(k).map(|v| v.as_bool().unwrap_or(false));
    if let Some(v) = flag("remove_oped") { s.remove_oped = v; }
    if let Some(v) = flag("merge_delete_eps") { s.merge_delete_eps = v; }
    if let Some(v) = flag("auto_skip") { s.auto_skip = v; }
    if let Some(v) = flag("skip_preview") { s.skip_preview = v; }
    let pick = |k: &str, allowed: &[&str]| body[k].as_str().filter(|v| allowed.contains(v)).map(str::to_string);
    if let Some(v) = pick("cut_mode", &["fast", "exact"]) { s.cut_mode = v; }
    if let Some(v) = pick("encoder", &["h264_nvenc", "hevc_nvenc", "libx264"]) { s.encoder = v; }
    if let Some(v) = pick("container", &["auto", "mkv", "mp4"]) { s.container = v; }
    if let Some(v) = pick("lang", &["tr", "en"]) {
        set_lang(&v);
        // Varsayılan ad şablonu dile uyar: Türkçe S1B1, İngilizce S1E1
        let en_default = "{anime} S{s}E{b}";
        if v == "en" && s.name_template == DEFAULT_TEMPLATE {
            s.name_template = en_default.into();
        } else if v == "tr" && s.name_template == en_default {
            s.name_template = DEFAULT_TEMPLATE.into();
        }
        s.lang = v;
    }
    if body.get("quality").is_some() {
        s.quality = int_of(&body["quality"]).ok_or(l!("Kalite sayı olmalı", "Quality must be a number"))?.clamp(10, 40);
    }
    a.store.save_locked(&mut d);
    Ok((200, json!({"ok": true})))
}

/// Arayüzün tüm durumu (ayarlar, kütüphane, bölümler + adlar ve izleme kayıtları).
pub fn api_state() -> Value {
    let a = app();
    let mpv = find_mpv().is_some();
    let watch = load_watch();
    let d = a.store.lock();
    let tpl = d.settings.name_template.clone();
    let empty = Anime::default();
    let eps: Vec<Value> = d
        .eps
        .values()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
            let name = match d.animes.get(&e.anime) {
                Some(an) => ep_name_t(e, an, true, &tpl),
                None => {
                    let _ = &empty;
                    String::new()
                }
            };
            if let Value::Object(m) = &mut v {
                m.insert("name".into(), json!(name));
                if let Some(w) = watch.get(&e.id).filter(|w| w.is_object()) {
                    m.insert("watch".into(), w.clone());
                }
            }
            v
        })
        .collect();
    json!({"settings": d.settings, "animes": d.animes, "eps": eps, "merges": d.merges, "mpv": mpv})
}

pub fn api_post(path: &str, body: &Value) -> Option<ApiRes> {
    Some(match path {
        "/api/add" => api_add(body),
        "/api/manual" => api_manual(body),
        "/api/anime" => api_anime_edit(body),
        "/api/pickdir" => api_pickdir(body),
        "/api/assign" => api_assign(body),
        "/api/action" => api_action(body),
        "/api/edit" => api_edit(body),
        "/api/settings" => api_settings(body),
        _ => return None,
    })
}

/// Uygulama kapanırken: çalışan ffmpeg'leri durdur, kaydet.
pub fn shutdown() {
    if let Some(a) = APP.get() {
        a.cancel.store(true, Ordering::SeqCst);
        cutter::kill_all();
        a.store.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let anime = Anime {
            id: "x".into(),
            name: "Frieren".into(),
            image: None,
            seasons: vec![Season { n: 1, episodes: [("2".to_string(), "Yolculuk".to_string())].into(), ..Default::default() }],
        };
        let ep = Ep { anime: "x".into(), season: 1, episode: 2, ..Default::default() };
        assert_eq!(ep_name_t(&ep, &anime, true, "{anime} S{s}B{b}"), "Frieren S1B2");
        assert_eq!(ep_name_t(&ep, &anime, true, "{anime} - S{s:02}E{b:02} - {title}"), "Frieren - S01E02 - Yolculuk");
        let ep3 = Ep { episode: 3, ..ep.clone() };
        assert_eq!(ep_name_t(&ep3, &anime, true, "{anime} - S{s:02}E{b:02} - {title}"), "Frieren - S01E03");
        assert_eq!(ep_name_t(&ep, &anime, false, "{anime} - S{s:02}E{b:02}"), "S01E02");
    }
}
