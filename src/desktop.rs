//! The desktop shell: native windows (tao) with one web view each (wry), all sharing one profile and one event loop.
//! Any thread, artifact or page can pop out into its own window, like an email client; windows remember where they
//! were. The windows talk to the core over loopback HTTP like any other client, so a window can just as well show a
//! remote server: switching servers is navigating the window.

use crate::{core::CoreRef, os};
use serde_json::{json, Value};
use std::collections::HashMap;
use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    window::{Fullscreen, Icon, ResizeDirection, Window, WindowBuilder, WindowId},
};
#[cfg(windows)]
use tao::platform::windows::WindowBuilderExtWindows;
use wry::{NewWindowResponse, WebContext, WebView, WebViewBuilder};

pub enum Ev {
    Ipc(WindowId, String),
    Core(Value),
    Tray(String),
}

struct Win {
    window: Window,
    webview: WebView,
    key: String,
    main: bool,
}

struct Shell {
    core: CoreRef,
    proxy: EventLoopProxy<Ev>,
    ctx: WebContext,
    wins: HashMap<WindowId, Win>,
    /// The local server's address; windows may navigate there and to servers the person connected to.
    local: String,
    allowed: Vec<String>,
    icon: Option<Icon>,
    hidden_to_tray: bool,
    #[allow(dead_code)]
    tray: Option<tray_icon::TrayIcon>,
}

fn icon() -> Option<Icon> {
    Icon::from_rgba(include_bytes!("../assets/icon-64.rgba").to_vec(), 64, 64).ok()
}

fn places_path() -> std::path::PathBuf {
    os::data_dir().join("windows.json")
}

fn load_places() -> Value {
    std::fs::read_to_string(places_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}))
}

fn origin_of(url: &str) -> String {
    let after = url.split("://").nth(1).unwrap_or("");
    let host = after.split('/').next().unwrap_or("");
    format!("{}://{}", url.split("://").next().unwrap_or("http"), host)
}

impl Shell {
    fn auth_url(&self, next: &str) -> String {
        let enc = percent_encoding::utf8_percent_encode(next, percent_encoding::NON_ALPHANUMERIC);
        format!("{}/auth?token={}&next={}", self.local, self.core.local_token, enc)
    }

    fn open(&mut self, target: &EventLoopWindowTarget<Ev>, url: &str, key: &str, main: bool, place: Option<&Value>) {
        if let Some((_, w)) = self.wins.iter().find(|(_, w)| !key.is_empty() && w.key == key) {
            w.window.set_visible(true);
            w.window.set_minimized(false);
            w.window.set_focus();
            return;
        }
        let (w, h) = (place.and_then(|p| p["w"].as_f64()).unwrap_or(if main { 1440.0 } else { 980.0 }), place.and_then(|p| p["h"].as_f64()).unwrap_or(if main { 920.0 } else { 820.0 }));
        #[allow(unused_mut)]
        let mut wb = WindowBuilder::new()
            .with_title("Model Roamer")
            .with_window_icon(self.icon.clone())
            .with_decorations(cfg!(target_os = "macos"))
            .with_inner_size(LogicalSize::new(w, h))
            .with_min_inner_size(LogicalSize::new(560.0, 420.0))
            .with_maximized(place.and_then(|p| p["max"].as_bool()).unwrap_or(false));
        if let (Some(x), Some(y)) = (place.and_then(|p| p["x"].as_f64()), place.and_then(|p| p["y"].as_f64())) {
            // only if that spot is still on a screen
            let on_screen = target.available_monitors().any(|m| {
                let p = m.position().to_logical::<f64>(m.scale_factor());
                let s = m.size().to_logical::<f64>(m.scale_factor());
                x + 80.0 > p.x && x < p.x + s.width - 80.0 && y >= p.y - 10.0 && y < p.y + s.height - 60.0
            });
            if on_screen {
                wb = wb.with_position(LogicalPosition::new(x, y));
            }
        } else if !main {
            // cascade pop-outs from the window that opened them
            if let Some(src) = self.wins.values().find(|w| w.window.is_focused()).or_else(|| self.wins.values().next()) {
                if let Ok(p) = src.window.outer_position() {
                    let p = p.to_logical::<f64>(src.window.scale_factor());
                    wb = wb.with_position(LogicalPosition::new(p.x + 40.0, p.y + 40.0));
                }
            }
        }
        #[cfg(windows)]
        {
            wb = wb.with_undecorated_shadow(true);
        }
        let window = match wb.build(target) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("couldn't open a window: {e}");
                return;
            }
        };
        let id = window.id();
        let proxy = self.proxy.clone();
        let proxy2 = self.proxy.clone();
        let allowed = self.allowed.clone();
        let boot = format!(
            "window.__roamerDesktop = {};",
            json!({ "platform": os::os_name(), "version": env!("CARGO_PKG_VERSION"), "key": key, "main": main, "local": self.local })
        );
        let builder = WebViewBuilder::new_with_web_context(&mut self.ctx)
            .with_url(url)
            .with_background_color((10, 17, 19, 255))
            .with_devtools(true)
            .with_initialization_script(&boot)
            .with_ipc_handler(move |r| {
                let _ = proxy.send_event(Ev::Ipc(id, r.body().to_string()));
            })
            .with_navigation_handler(move |u| {
                // our servers load in the window; anything else opens in the person's browser
                if allowed.iter().any(|a| u.starts_with(a.as_str())) || u.starts_with("about:") || u.starts_with("data:") || u.starts_with("blob:") {
                    true
                } else {
                    let _ = proxy2.send_event(Ev::Ipc(id, json!({ "cmd": "external", "url": u }).to_string()));
                    false
                }
            })
            .with_new_window_req_handler(|u, _| {
                os::open_external(&u);
                NewWindowResponse::Deny
            });
        #[cfg(not(target_os = "linux"))]
        let webview = builder.build(&window);
        #[cfg(target_os = "linux")]
        let webview = {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;
            builder.build_gtk(window.default_vbox().expect("window layout"))
        };
        match webview {
            Ok(webview) => {
                self.wins.insert(id, Win { window, webview, key: key.to_string(), main });
            }
            Err(e) => tracing::error!("couldn't create a web view: {e}"),
        }
    }

    fn save_places(&self) {
        let mut v = load_places();
        let mut pops = vec![];
        for w in self.wins.values() {
            let sf = w.window.scale_factor();
            let max = w.window.is_maximized();
            let pos = w.window.outer_position().map(|p| p.to_logical::<f64>(sf)).ok();
            let size = w.window.inner_size().to_logical::<f64>(sf);
            let mut p = json!({ "max": max });
            if !max && !w.window.is_minimized() {
                if let Some(pos) = pos {
                    p["x"] = json!(pos.x);
                    p["y"] = json!(pos.y);
                }
                p["w"] = json!(size.width);
                p["h"] = json!(size.height);
            } else if let Some(old) = if w.main { v.get("main").cloned() } else { None } {
                for k in ["x", "y", "w", "h"] {
                    p[k] = old[k].clone();
                }
            }
            if w.main {
                v["main"] = p;
            } else if let Some(url) = w.webview.url().ok().filter(|u| u.starts_with(&self.local)) {
                p["url"] = json!(url.trim_start_matches(&self.local));
                p["key"] = json!(w.key);
                pops.push(p);
            }
        }
        v["popouts"] = json!(pops);
        let _ = os::write_atomic(&places_path(), v.to_string().as_bytes());
    }

    fn tell(&self, id: WindowId, v: Value) {
        if let Some(w) = self.wins.get(&id) {
            let _ = w.webview.evaluate_script(&format!("window.__native && window.__native({v})"));
        }
    }

    fn tell_all(&self, v: Value) {
        for id in self.wins.keys() {
            self.tell(*id, v.clone());
        }
    }

    fn agents_busy(&self) -> usize {
        self.core.running.lock().map(|r| r.values().filter(|x| !x.turn_id.is_empty()).count()).unwrap_or(0)
    }

    /// Closing a window. The last one: quit, or keep running in the tray while agents work.
    fn close(&mut self, id: WindowId, flow: &mut ControlFlow) {
        self.save_places();
        let Some(w) = self.wins.get(&id) else { return };
        let last = self.wins.values().filter(|w| w.window.is_visible()).count() <= 1;
        if last {
            let busy = self.agents_busy();
            if self.core.settings()["close_to_tray"] != false && (busy > 0 || self.core.settings()["server"]["share"] == true) && self.tray.is_some() {
                w.window.set_visible(false);
                self.hidden_to_tray = true;
                return;
            }
            *flow = ControlFlow::Exit;
            return;
        }
        if w.main {
            // the main window can go; the app stays while pop-outs are open
            w.window.set_visible(false);
            return;
        }
        self.wins.remove(&id);
    }

    fn show_main(&mut self, target: &EventLoopWindowTarget<Ev>) {
        self.hidden_to_tray = false;
        if let Some(w) = self.wins.values().find(|w| w.main) {
            w.window.set_visible(true);
            w.window.set_minimized(false);
            w.window.set_focus();
        } else {
            let url = self.auth_url("/");
            let place = load_places()["main"].clone();
            self.open(target, &url, "main", true, Some(&place));
        }
    }

    fn ipc(&mut self, target: &EventLoopWindowTarget<Ev>, id: WindowId, body: &str, flow: &mut ControlFlow) {
        let v: Value = serde_json::from_str(body).unwrap_or_default();
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        match s("cmd").as_str() {
            "win" => {
                let Some(w) = self.wins.get(&id) else { return };
                match s("op").as_str() {
                    "min" => w.window.set_minimized(true),
                    "max" => w.window.set_maximized(!w.window.is_maximized()),
                    "close" => self.close(id, flow),
                    "drag" => {
                        let _ = w.window.drag_window();
                    }
                    "fullscreen" => w.window.set_fullscreen(if w.window.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) }),
                    "top" => w.window.set_always_on_top(v["on"].as_bool().unwrap_or(false)),
                    op => {
                        let dir = match op.strip_prefix("resize:").unwrap_or("") {
                            "N" => Some(ResizeDirection::North),
                            "S" => Some(ResizeDirection::South),
                            "E" => Some(ResizeDirection::East),
                            "W" => Some(ResizeDirection::West),
                            "NE" => Some(ResizeDirection::NorthEast),
                            "NW" => Some(ResizeDirection::NorthWest),
                            "SE" => Some(ResizeDirection::SouthEast),
                            "SW" => Some(ResizeDirection::SouthWest),
                            _ => None,
                        };
                        if let Some(d) = dir {
                            let _ = w.window.drag_resize_window(d);
                        }
                    }
                }
                if let Some(w) = self.wins.get(&id) {
                    self.tell(id, json!({ "t": "win", "max": w.window.is_maximized(), "fs": w.window.fullscreen().is_some() }));
                }
            }
            "open" => {
                // a pop-out: the same server as the window asking, at the path given
                let src = self.wins.get(&id).and_then(|w| w.webview.url().ok()).unwrap_or_else(|| self.local.clone());
                let origin = origin_of(&src);
                let path = s("path");
                let url = if path.starts_with("http") { path } else { format!("{origin}{path}") };
                if !self.allowed.iter().any(|a| url.starts_with(a.as_str())) {
                    return;
                }
                let place = json!({ "w": v["w"], "h": v["h"] });
                self.open(target, &url, &s("key"), false, Some(&place));
            }
            "external" => {
                let u = s("url");
                if u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:") {
                    os::open_external(&u);
                }
            }
            "navigate" => {
                // switching servers: this window goes there; pop-outs of the old server close
                let url = s("url");
                let origin = origin_of(&url);
                if !self.allowed.contains(&origin) {
                    self.allowed.push(origin.clone());
                }
                let mains: Vec<WindowId> = self.wins.iter().filter(|(_, w)| w.main).map(|(k, _)| *k).collect();
                let others: Vec<WindowId> = self.wins.iter().filter(|(_, w)| !w.main).map(|(k, _)| *k).collect();
                for o in others {
                    self.wins.remove(&o);
                }
                for m in mains {
                    if let Some(w) = self.wins.get(&m) {
                        let _ = w.webview.load_url(&url);
                    }
                }
            }
            "home" => {
                let url = self.auth_url("/");
                let others: Vec<WindowId> = self.wins.iter().filter(|(_, w)| !w.main).map(|(k, _)| *k).collect();
                for o in others {
                    self.wins.remove(&o);
                }
                if let Some(w) = self.wins.values().find(|w| w.main) {
                    let _ = w.webview.load_url(&url);
                }
            }
            "title" => {
                if let Some(w) = self.wins.get(&id) {
                    w.window.set_title(&s("text"));
                }
            }
            "focus_main" => self.show_main(target),
            "devtools" => {
                if let Some(w) = self.wins.get(&id) {
                    w.webview.open_devtools();
                }
            }
            "zoom" => {
                if let Some(w) = self.wins.get(&id) {
                    let _ = w.webview.zoom(v["factor"].as_f64().unwrap_or(1.0).clamp(0.5, 2.5));
                }
            }
            "quit" => {
                self.save_places();
                *flow = ControlFlow::Exit;
            }
            "state" => {
                if let Some(w) = self.wins.get(&id) {
                    self.tell(id, json!({ "t": "win", "max": w.window.is_maximized(), "fs": w.window.fullscreen().is_some(), "windows": self.wins.len() }));
                }
            }
            _ => {}
        }
    }
}

fn make_tray(proxy: &EventLoopProxy<Ev>) -> Option<tray_icon::TrayIcon> {
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        TrayIconBuilder,
    };
    let menu = Menu::new();
    let open = MenuItem::with_id("open", "Open Model Roamer", true, None);
    let quit = MenuItem::with_id("quit", "Quit (stops running agents)", true, None);
    let _ = menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]);
    let rgba = include_bytes!("../assets/icon-64.rgba").to_vec();
    let icon = tray_icon::Icon::from_rgba(rgba, 64, 64).ok()?;
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = p.send_event(Ev::Tray(e.id().0.clone()));
    }));
    let p2 = proxy.clone();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |e: tray_icon::TrayIconEvent| {
        if let tray_icon::TrayIconEvent::DoubleClick { .. } = e {
            let _ = p2.send_event(Ev::Tray("open".into()));
        }
    }));
    TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Model Roamer").with_icon(icon).build().ok()
}

/// Run the windows until the person quits. `core` is already serving on loopback.
pub fn run(core: CoreRef, rt: tokio::runtime::Handle, first_path: Option<String>) {
    let event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    *core.desktop.lock().unwrap() = Some(tx);
    let p2 = proxy.clone();
    rt.spawn(async move {
        while let Some(v) = rx.recv().await {
            if p2.send_event(Ev::Core(v)).is_err() {
                break;
            }
        }
    });
    let local = core.base_url();
    let tray = make_tray(&proxy);
    let mut shell = Shell {
        core: core.clone(),
        proxy: proxy.clone(),
        ctx: WebContext::new(Some(os::data_dir().join("webview"))),
        wins: HashMap::new(),
        allowed: vec![local.clone()],
        local: local.clone(),
        icon: icon(),
        hidden_to_tray: false,
        tray,
    };
    // the servers this computer is connected to may load in its windows
    for c in core.db.all("SELECT url FROM connections", &[]).unwrap_or_default() {
        if let Some(u) = c["url"].as_str() {
            shell.allowed.push(origin_of(u));
        }
    }
    let places = load_places();
    let main_url = shell.auth_url(first_path.as_deref().unwrap_or("/"));
    shell.open(&event_loop, &main_url, "main", true, places.get("main"));
    if core.settings()["restore_windows"] != false {
        for p in places["popouts"].as_array().cloned().unwrap_or_default() {
            if let Some(path) = p["url"].as_str().filter(|u| u.starts_with('/')) {
                let url = shell.auth_url(path);
                shell.open(&event_loop, &url, p["key"].as_str().unwrap_or(""), false, Some(&p));
            }
        }
    }

    let mut last_save = std::time::Instant::now();
    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => shell.close(window_id, flow),
            Event::WindowEvent { window_id, event: WindowEvent::Resized(_) | WindowEvent::Moved(_), .. } => {
                if let Some(w) = shell.wins.get(&window_id) {
                    shell.tell(window_id, json!({ "t": "win", "max": w.window.is_maximized(), "fs": w.window.fullscreen().is_some() }));
                }
                if last_save.elapsed().as_secs() > 2 {
                    last_save = std::time::Instant::now();
                    shell.save_places();
                }
            }
            Event::WindowEvent { window_id, event: WindowEvent::Focused(true), .. } => shell.tell(window_id, json!({ "t": "focus" })),
            Event::UserEvent(Ev::Ipc(id, body)) => shell.ipc(target, id, &body, flow),
            Event::UserEvent(Ev::Core(v)) => match v["cmd"].as_str().unwrap_or("") {
                "open" => {
                    // a second launch, or a link: show the app
                    shell.show_main(target);
                    if let Some(path) = v["path"].as_str().filter(|p| p.starts_with('/')) {
                        let url = shell.auth_url(path);
                        shell.open(target, &url, v["key"].as_str().unwrap_or(""), false, None);
                    }
                }
                "quit" => {
                    shell.save_places();
                    *flow = ControlFlow::Exit;
                }
                "notify" => shell.tell_all(json!({ "t": "notify", "data": v["data"] })),
                _ => {}
            },
            Event::UserEvent(Ev::Tray(id)) => match id.as_str() {
                "open" => shell.show_main(target),
                "quit" => {
                    shell.save_places();
                    *flow = ControlFlow::Exit;
                }
                _ => {}
            },
            Event::LoopDestroyed => shell.save_places(),
            _ => {}
        }
    });
}
