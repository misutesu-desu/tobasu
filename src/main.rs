//! Tobasu: kendi anime arşivindeki bölümleri adlandırır, OP/ED'yi bulur, mpv tabanlı
//! oynatıcıda işaretleyip otomatik atlar; istersen OP/ED'yi silip dışa aktarır ya da tüm
//! bölümleri tek videoda birleştirir. Arayüz yerel pencerede (WebView2) açılır.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cutter;
mod matcher;
mod merger;
mod metadata;
mod opedfind;
mod server;
mod store;
mod subs;
mod util;

use std::net::TcpListener;

fn open_in_browser(url: &str) {
    let _ = util::cmd("cmd").args(["/c", "start", "", url]).spawn();
}

fn main() {
    let url = format!("http://127.0.0.1:{}", server::PORT);
    let headless = std::env::var("ANIME_NO_BROWSER").as_deref() == Ok("1");
    // Port doluysa uygulama zaten açıktır: yalnızca onun arayüzü açılır
    let listener = TcpListener::bind(("127.0.0.1", server::PORT)).ok();
    let running = listener.is_some();
    if let Some(l) = listener {
        app::init();
        util::log(&format!("Tobasu running: {url}"));
        std::thread::spawn(move || server::serve(l));
    }
    if headless {
        if running {
            loop {
                std::thread::park();
            }
        }
        return;
    }
    if let Err(e) = window(&url) {
        util::log(&format!("window failed ({e}), using the browser"));
        open_in_browser(&url);
        if running {
            loop {
                std::thread::park();
            }
        }
    }
    if running {
        app::shutdown();
    }
}

fn window(url: &str) -> Result<(), String> {
    use tao::dpi::LogicalSize;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoop};
    use tao::window::WindowBuilder;
    use wry::WebViewBuilder;

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Tobasu")
        .with_inner_size(LogicalSize::new(1280.0, 860.0))
        .with_min_inner_size(LogicalSize::new(800.0, 500.0))
        .with_background_color((20, 21, 26, 255))
        .build(&event_loop)
        .map_err(|e| e.to_string())?;
    let webview = WebViewBuilder::new()
        .with_url(url)
        .with_background_color((20, 21, 26, 255))
        .build(&window)
        .map_err(|e| e.to_string())?;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let _ = &webview;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            app::shutdown();
            *flow = ControlFlow::Exit;
        }
    });
}
