//! Yerel HTTP sunucusu: arayüz sayfası, JSON API ve durum akışı (Server-Sent Events).
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::app::{self, app};
use crate::util::log;

pub const PORT: u16 = 8765;
const UI: &str = include_str!("ui.html");
const TOKEN_HEADER: &str = "x-tobasu";

pub fn serve(listener: TcpListener) {
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || {
            let _ = stream.set_nodelay(true);
            if let Err(e) = handle(stream) {
                if e.kind() != std::io::ErrorKind::ConnectionAborted && e.kind() != std::io::ErrorKind::ConnectionReset {
                    log(&format!("[http] {e}"));
                }
            }
        });
    }
}

struct Req {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> &str {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("")
    }
}

fn read_req(stream: &TcpStream) -> std::io::Result<Option<Req>> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = target.split_once('?').map(|(p, q)| (p.to_string(), q.to_string())).unwrap_or((target, String::new()));
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
        if headers.len() > 100 {
            break;
        }
    }
    let len: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = Vec::new();
    if len > 0 {
        if len > 1024 * 1024 {
            return Ok(Some(Req { method, path, query, headers, body: b"\xff".to_vec() }));
        }
        body.resize(len, 0);
        r.read_exact(&mut body)?;
    }
    Ok(Some(Req { method, path, query, headers, body }))
}

fn send(mut s: &TcpStream, code: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

fn send_json(s: &TcpStream, code: u16, v: &Value) -> std::io::Result<()> {
    send(s, code, "application/json; charset=utf-8", v.to_string().as_bytes())
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(q: &str, key: &str) -> String {
    q.split('&')
        .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
        .find(|(k, _)| url_decode(k) == key)
        .map(|(_, v)| url_decode(v))
        .unwrap_or_default()
}

fn handle(stream: TcpStream) -> std::io::Result<()> {
    let Some(req) = read_req(&stream)? else { return Ok(()) };
    // DNS rebinding'e karşı yalnızca yerel adlar kabul edilir
    let host = req.header("host");
    if host != format!("127.0.0.1:{PORT}") && host != format!("localhost:{PORT}") {
        return send_json(&stream, 403, &json!({"error": "forbidden"}));
    }
    match req.method.as_str() {
        "GET" => match req.path.as_str() {
            "/" | "/index.html" => send(&stream, 200, "text/html; charset=utf-8", UI.as_bytes()),
            "/api/state" => send_json(&stream, 200, &app::api_state()),
            "/api/events" => events(stream),
            "/api/search" => respond(&stream, app::api_search(&query_param(&req.query, "q"))),
            _ => send_json(&stream, 404, &json!({"error": "not found"})),
        },
        "POST" => {
            // Özel başlık zorunlu: başka siteler tarayıcıdan bu API'ye istek atamaz
            if req.header(TOKEN_HEADER) != "1" {
                return send_json(&stream, 403, &json!({"error": "forbidden"}));
            }
            if req.body == b"\xff" {
                return send_json(&stream, 413, &json!({"error": "too large"}));
            }
            let body: Value = if req.body.is_empty() {
                json!({})
            } else {
                match serde_json::from_slice(&req.body) {
                    Ok(v) => v,
                    Err(_) => return send_json(&stream, 400, &json!({"error": "invalid JSON"})),
                }
            };
            let path = req.path.clone();
            let res = std::panic::catch_unwind(|| app::api_post(&path, &body));
            match res {
                Ok(Some(r)) => respond(&stream, r),
                Ok(None) => send_json(&stream, 404, &json!({"error": "not found"})),
                Err(_) => send_json(&stream, 500, &json!({"error": crate::l!("beklenmeyen hata", "unexpected error")})),
            }
        }
        _ => send_json(&stream, 404, &json!({"error": "not found"})),
    }
}

fn respond(s: &TcpStream, r: app::ApiRes) -> std::io::Result<()> {
    match r {
        Ok((code, v)) => send_json(s, code, &v),
        Err(e) => send_json(s, 400, &json!({"error": e})),
    }
}

/// Durum değişince tam durumu hemen gönderir (yoklama yok).
fn events(mut s: TcpStream) -> std::io::Result<()> {
    s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n")?;
    s.flush()?;
    let a = app();
    let (mut seen, mut seen_watch) = (u64::MAX, None);
    let mut last = Instant::now();
    loop {
        let v = {
            let d = a.store.lock();
            let (d, _) = a.store.changed.wait_timeout_while(d, Duration::from_secs(1), |d| d.version == seen).unwrap_or_else(|e| e.into_inner());
            d.version
        };
        let w = app::watch_mtime(); // oynatıcı izleme kaydını yazınca
        if v != seen || w != seen_watch {
            seen = v;
            seen_watch = w;
            last = Instant::now();
            let data = app::api_state().to_string();
            s.write_all(format!("data: {data}\n\n").as_bytes())?;
            s.flush()?;
            std::thread::sleep(Duration::from_millis(30)); // ilerleme yağmurunda en fazla ~30/sn
        } else if last.elapsed() > Duration::from_secs(15) {
            last = Instant::now();
            s.write_all(b": \n\n")?;
            s.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn decode() {
        assert_eq!(super::url_decode("Fr%C3%BCh+ling%"), "Früh ling%");
        assert_eq!(super::query_param("a=1&q=s%C3%B6z", "q"), "söz");
    }
}
