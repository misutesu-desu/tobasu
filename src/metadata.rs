//! Anime arama, sezon zinciri ve bölüm adları (AniList GraphQL + Jikan/MyAnimeList).
use fancy_regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::store::{Anime, Season};

const ANILIST: &str = "https://graphql.anilist.co";
const JIKAN: &str = "https://api.jikan.moe/v4";
const UA: &str = concat!("Tobasu/", env!("CARGO_PKG_VERSION"));
const SEASON_FORMATS: &[&str] = &["TV", "TV_SHORT", "ONA"];

static LAST: Mutex<Option<Instant>> = Mutex::new(None);
static JIKAN_DOWN_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

fn throttle(gap: f64) {
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = *last {
        let next = t + Duration::from_secs_f64(gap);
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
    *last = Some(Instant::now());
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent(UA).build()
}

fn anilist(query: &str, vars: Value) -> Result<Value, String> {
    let a = agent();
    for attempt in 0..4 {
        throttle(0.7);
        let r = a.post(ANILIST).set("Accept", "application/json").send_json(json!({"query": query, "variables": vars}));
        match r {
            Ok(resp) => {
                let v: Value = resp.into_json().map_err(|e| crate::l!("AniList yanıtı okunamadı: {e}", "Could not read the AniList response: {e}"))?;
                return Ok(v["data"].clone());
            }
            Err(ureq::Error::Status(429, resp)) => {
                let wait: u64 = resp.header("Retry-After").and_then(|h| h.parse().ok()).unwrap_or(5);
                std::thread::sleep(Duration::from_secs(wait + 1));
            }
            Err(ureq::Error::Status(code, _)) => return Err(crate::l!("AniList {code} döndü", "AniList returned {code}")),
            Err(e) => {
                if attempt == 3 {
                    return Err(crate::l!("AniList'e ulaşılamadı: {e}", "Could not reach AniList: {e}"));
                }
            }
        }
    }
    Err(crate::l!("AniList çok fazla istek hatası verdi, biraz sonra dene", "AniList is rate limiting, try again a bit later"))
}

const SEARCH_Q: &str = "query ($q: String) { Page(perPage: 15) { media(search: $q, type: ANIME, sort: SEARCH_MATCH) {
  id idMal format episodes seasonYear status title { romaji english } coverImage { medium } } } }";

const MEDIA_Q: &str = "query ($id: Int) { Media(id: $id, type: ANIME) {
  id idMal format episodes seasonYear title { romaji english } coverImage { large }
  streamingEpisodes { title }
  relations { edges { relationType node { id type format seasonYear title { romaji } } } } } }";

fn title(m: &Value) -> String {
    let t = &m["title"];
    t["english"].as_str().or_else(|| t["romaji"].as_str()).map(str::to_string).unwrap_or_else(|| m["id"].to_string())
}

pub fn search(q: &str) -> Result<Value, String> {
    let data = anilist(SEARCH_Q, json!({"q": q}))?;
    let list = data["Page"]["media"].as_array().cloned().unwrap_or_default();
    Ok(Value::Array(
        list.iter()
            .map(|m| {
                json!({"id": m["id"], "title": title(m), "romaji": m["title"]["romaji"], "format": m["format"],
                       "episodes": m["episodes"], "year": m["seasonYear"], "status": m["status"],
                       "image": m["coverImage"]["medium"]})
            })
            .collect(),
    ))
}

fn media(id: i64, cache: &mut HashMap<i64, Value>) -> Result<Value, String> {
    if !cache.contains_key(&id) {
        let v = anilist(MEDIA_Q, json!({"id": id}))?["Media"].clone();
        cache.insert(id, v);
    }
    Ok(cache[&id].clone())
}

/// rel ilişkisindeki en uygun anime düğümü (TV formatları öncelikli).
fn next(m: &Value, rel: &str) -> Option<i64> {
    let mut nodes: Vec<&Value> = m["relations"]["edges"]
        .as_array()?
        .iter()
        .filter(|e| e["relationType"] == rel && e["node"]["type"] == "ANIME")
        .map(|e| &e["node"])
        .collect();
    nodes.sort_by_key(|n| (!SEASON_FORMATS.contains(&n["format"].as_str().unwrap_or("")), n["seasonYear"].as_i64().unwrap_or(0)));
    nodes.first().and_then(|n| n["id"].as_i64())
}

fn streaming_titles(m: &Value) -> BTreeMap<i64, String> {
    let re = Regex::new(r"(?i)^\s*(?:Episode|Ep\.?)\s*(\d+)\s*[-:–]\s*(.+)$").unwrap();
    let mut out = BTreeMap::new();
    for se in m["streamingEpisodes"].as_array().into_iter().flatten() {
        if let Ok(Some(c)) = re.captures(se["title"].as_str().unwrap_or("")) {
            if let Ok(n) = c[1].parse() {
                out.insert(n, c[2].trim().to_string());
            }
        }
    }
    out
}

fn jikan_titles(mal: i64) -> BTreeMap<i64, String> {
    let mut titles = BTreeMap::new();
    let a = ureq::AgentBuilder::new().timeout(Duration::from_secs(8)).user_agent(UA).build();
    let mut page = 1;
    while page < 30 {
        if JIKAN_DOWN_UNTIL.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| Instant::now() < t) {
            break;
        }
        throttle(0.45);
        let r = a.get(&format!("{JIKAN}/anime/{mal}/episodes")).query("page", &page.to_string()).call();
        let j: Value = match r {
            Ok(resp) => match resp.into_json() {
                Ok(v) => v,
                Err(_) => {
                    *JIKAN_DOWN_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + Duration::from_secs(300));
                    break;
                }
            },
            Err(ureq::Error::Status(429, _)) => {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            Err(ureq::Error::Status(_, _)) => break,
            Err(_) => {
                *JIKAN_DOWN_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + Duration::from_secs(300));
                break;
            }
        };
        for e in j["data"].as_array().into_iter().flatten() {
            if let (Some(t), Some(n)) = (e["title"].as_str(), e["mal_id"].as_i64()) {
                if !t.is_empty() {
                    titles.insert(n, t.to_string());
                }
            }
        }
        if !j["pagination"]["has_next_page"].as_bool().unwrap_or(false) {
            break;
        }
        page += 1;
    }
    titles
}

/// Seçilen kayıttan başlayıp önceki/sonraki sezonları bulur.
pub fn build_anime(anilist_id: i64) -> Result<Anime, String> {
    let mut cache = HashMap::new();
    let start = media(anilist_id, &mut cache)?;
    // İlk sezona kadar geri git
    let mut root = start.clone();
    let mut seen: HashSet<i64> = [start["id"].as_i64().unwrap_or(0)].into();
    for _ in 0..15 {
        match next(&root, "PREQUEL") {
            Some(p) if !seen.contains(&p) => {
                seen.insert(p);
                root = media(p, &mut cache)?;
            }
            _ => break,
        }
    }
    let mut chain = Vec::new();
    let mut cur = root.clone();
    let mut seen: HashSet<i64> = [root["id"].as_i64().unwrap_or(0)].into();
    for _ in 0..25 {
        if SEASON_FORMATS.contains(&cur["format"].as_str().unwrap_or("")) || cur["id"].as_i64() == Some(anilist_id) {
            chain.push(cur.clone());
        }
        match next(&cur, "SEQUEL") {
            Some(n) if !seen.contains(&n) => {
                seen.insert(n);
                cur = media(n, &mut cache)?;
            }
            _ => break,
        }
    }
    if !chain.iter().any(|m| m["id"].as_i64() == Some(anilist_id)) {
        chain = vec![start];
    }
    let mut seasons = Vec::new();
    for (i, m) in chain.iter().enumerate() {
        let mut titles = m["idMal"].as_i64().map(jikan_titles).unwrap_or_default();
        if titles.is_empty() {
            titles = streaming_titles(m);
        }
        let count = m["episodes"].as_i64().unwrap_or_else(|| titles.keys().max().copied().unwrap_or(0));
        seasons.push(Season {
            n: i as i64 + 1,
            anilist_id: m["id"].as_i64(),
            title: title(m),
            year: m["seasonYear"].as_i64(),
            count,
            episodes: titles.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        });
    }
    let first = &chain[0];
    Ok(Anime {
        id: format!("al{}", first["id"]),
        name: title(first),
        image: first["coverImage"]["large"].as_str().map(str::to_string),
        seasons,
    })
}
