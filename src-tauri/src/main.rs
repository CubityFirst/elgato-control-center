// Always a windowed (GUI) program: never show a console, even in debug builds.
#![windows_subsystem = "windows"]

mod autostart;
mod lights;
mod store;

use lights::{Light, Patch};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use store::Lights;
use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Position, Size, WebviewWindow,
    WindowEvent,
};

const POPOVER_WIDTH: f64 = 320.0;
const TRAY_ID: &str = "main";
const WINDOW_LABEL: &str = "popover";
const SCAN_TIME: Duration = Duration::from_millis(2500);

#[derive(Clone, Copy, Debug)]
struct TrayRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// What the tray currently shows, so unchanged updates can be skipped.
#[derive(Clone, PartialEq)]
struct TrayLook {
    any_on: bool,
    dim: bool,
    light_taskbar: bool,
    tooltip: String,
}

struct AppState {
    lights: Mutex<Lights>,
    /// Bumped by every write to a light; reads that began before a write must not undo it.
    writes: AtomicU64,
    tray_rect: Mutex<Option<TrayRect>>,
    tray_look: Mutex<Option<TrayLook>>,
    hidden_at: Mutex<Option<Instant>>,
    config_path: PathBuf,
}

impl AppState {
    fn write_count(&self) -> u64 {
        self.writes.load(Ordering::SeqCst)
    }

    fn next_write(&self) -> u64 {
        self.writes.fetch_add(1, Ordering::SeqCst) + 1
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    lights: Vec<Light>,
}

// ---------- state helpers ----------

fn snapshot(app: &AppHandle) -> Vec<Light> {
    let state = app.state::<AppState>();
    let map = state.lights.lock().unwrap();
    let mut v: Vec<Light> = map.values().cloned().collect();
    v.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.ip.cmp(&b.ip))
    });
    v
}

fn save(app: &AppHandle) {
    let state = app.state::<AppState>();
    let saved = Saved {
        lights: snapshot(app),
    };
    if let Some(dir) = state.config_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&saved) {
        let _ = std::fs::write(&state.config_path, json);
    }
}

fn load(path: &PathBuf) -> Lights {
    let mut map = Lights::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(saved) = serde_json::from_str::<Saved>(&text) {
            for l in saved.lights {
                store::load(&mut map, l);
            }
        }
    }
    map
}

/// Pushes the current list to the UI and refreshes the tray icon.
fn broadcast(app: &AppHandle) {
    let list = snapshot(app);
    update_tray(app, &list);
    let _ = app.emit("lights", &list);
}

/// Saves, tells the UI and returns the new list.
fn finish(app: &AppHandle) -> Vec<Light> {
    save(app);
    broadcast(app);
    snapshot(app)
}

/// Reads a light's state. Accessory info (serial, name, product) is fetched only when
/// `with_info` is set, something is missing, or the light was offline, since that is when
/// its address may have changed hands. If a different light answers there, `l` is offline.
fn refresh_one(l: &mut Light, with_info: bool) {
    if with_info || !l.reachable || l.serial.is_empty() || l.name.trim().is_empty() {
        let info = match lights::fetch_info(&l.ip, l.port) {
            Ok(info) => info,
            Err(_) => {
                l.reachable = false;
                return;
            }
        };
        if !l.serial.is_empty() && !info.serial.is_empty() && info.serial != l.serial {
            l.reachable = false;
            return;
        }
        if !info.serial.is_empty() {
            l.serial = info.serial;
        }
        if !info.name.trim().is_empty() {
            l.name = info.name;
        }
        if !info.product.trim().is_empty() {
            l.product = info.product;
        }
    }
    match lights::fetch_state(&l.ip, l.port) {
        Ok(s) => {
            l.reachable = true;
            l.on = s.on;
            l.brightness = s.brightness;
            l.temperature = s.temperature;
        }
        Err(_) => l.reachable = false,
    }
}

fn probe_all(list: &mut [Light], with_info: bool) {
    std::thread::scope(|s| {
        for l in list.iter_mut() {
            s.spawn(move || refresh_one(l, with_info));
        }
    });
}

/// Re-reads state from every known light in parallel.
fn do_refresh(app: &AppHandle) -> Vec<Light> {
    let state = app.state::<AppState>();
    let since = state.write_count();
    let mut list: Vec<Light> = state.lights.lock().unwrap().values().cloned().collect();
    probe_all(&mut list, false);
    {
        let mut map = state.lights.lock().unwrap();
        for l in list {
            store::merge_known(&mut map, l, since);
        }
    }
    finish(app)
}

/// Browses mDNS, then probes every light found plus every known light that wasn't.
fn do_rescan(app: &AppHandle) -> Vec<Light> {
    let _ = app.emit("scanning", true);
    let found = lights::discover(SCAN_TIME);
    let state = app.state::<AppState>();
    let since = state.write_count();
    let mut known: Vec<Light> = state
        .lights
        .lock()
        .unwrap()
        .values()
        .filter(|l| !found.iter().any(|d| d.ip == l.ip))
        .cloned()
        .collect();
    let mut seen: Vec<Light> = found
        .into_iter()
        .map(|d| {
            let mut l = Light::new(d.ip, d.port, d.name, false);
            l.product = d.product;
            l
        })
        .collect();
    std::thread::scope(|s| {
        s.spawn(|| probe_all(&mut known, true));
        s.spawn(|| probe_all(&mut seen, true));
    });
    {
        let mut map = state.lights.lock().unwrap();
        for l in known {
            store::merge_known(&mut map, l, since);
        }
        for l in seen {
            store::upsert(&mut map, l, since);
        }
    }
    let list = finish(app);
    let _ = app.emit("scanning", false);
    list
}

// ---------- tray icon ----------

#[cfg(windows)]
fn taskbar_is_light() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("SystemUsesLightTheme"))
        .is_ok_and(|v| v == 1)
}

#[cfg(not(windows))]
fn taskbar_is_light() -> bool {
    false
}

/// 32x32 anti-aliased disc (on) or ring (off). `alpha` dims the icon when nothing is known.
fn tray_image(on: bool, alpha: f32, dark_glyph: bool) -> Image<'static> {
    const N: usize = 32;
    const SS: usize = 4;
    let (cx, cy) = (N as f32 / 2.0, N as f32 / 2.0);
    let outer = 11.0f32;
    let inner = 7.5f32;
    let shade: u8 = if dark_glyph { 20 } else { 245 };
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let mut hit = 0usize;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                    if d <= outer && (on || d >= inner) {
                        hit += 1;
                    }
                }
            }
            let cov = hit as f32 / (SS * SS) as f32 * alpha;
            let i = (y * N + x) * 4;
            rgba[i] = shade;
            rgba[i + 1] = shade;
            rgba[i + 2] = shade;
            rgba[i + 3] = (cov * 255.0).round() as u8;
        }
    }
    Image::new_owned(rgba, N as u32, N as u32)
}

fn update_tray(app: &AppHandle, list: &[Light]) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let reachable: Vec<&Light> = list.iter().filter(|l| l.reachable).collect();
    let on = reachable.iter().filter(|l| l.on).count();
    let look = TrayLook {
        any_on: on > 0,
        dim: reachable.is_empty(),
        light_taskbar: taskbar_is_light(),
        tooltip: if reachable.is_empty() {
            "Elgato Lights - no lights found".to_string()
        } else {
            format!("Elgato Lights - {on} of {} on", reachable.len())
        },
    };
    let state = app.state::<AppState>();
    let mut shown = state.tray_look.lock().unwrap();
    if shown.as_ref() == Some(&look) {
        return;
    }
    let alpha = if look.dim { 0.45 } else { 1.0 };
    let _ = tray.set_icon(Some(tray_image(look.any_on, alpha, look.light_taskbar)));
    let _ = tray.set_tooltip(Some(&look.tooltip));
    *shown = Some(look);
}

// ---------- popover ----------

fn popover(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(WINDOW_LABEL)
}

fn position_popover(app: &AppHandle, win: &WebviewWindow, logical_height: Option<f64>) {
    let state = app.state::<AppState>();
    let rect = *state.tray_rect.lock().unwrap();

    let monitor = rect
        .and_then(|r| {
            app.monitor_from_point(
                r.x as f64 + r.w as f64 / 2.0,
                r.y as f64 + r.h as f64 / 2.0,
            )
            .ok()
            .flatten()
        })
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };

    let scale = monitor.scale_factor();
    let wa = monitor.work_area();
    let (wa_x, wa_y) = (wa.position.x, wa.position.y);
    let (wa_w, wa_h) = (wa.size.width as i32, wa.size.height as i32);

    let w = (POPOVER_WIDTH * scale).round() as i32;
    let h = match logical_height {
        Some(lh) => (lh * scale).round() as i32,
        None => win.outer_size().map(|s| s.height as i32).unwrap_or(240),
    };
    let margin = (6.0 * scale).round() as i32;

    let (mut x, mut y) = match rect {
        Some(r) => {
            let x = r.x + r.w / 2 - w / 2;
            // Taskbar at the top of the screen? Open below the icon instead.
            let y = if r.y < wa_y + wa_h / 2 {
                r.y + r.h + margin
            } else {
                r.y - h - margin
            };
            (x, y)
        }
        None => (wa_x + wa_w - w - margin, wa_y + wa_h - h - margin),
    };
    x = x.clamp(wa_x, (wa_x + wa_w - w).max(wa_x));
    y = y.clamp(wa_y, (wa_y + wa_h - h).max(wa_y));

    let _ = win.set_position(Position::Physical(PhysicalPosition::new(x, y)));
}

fn show_popover(app: &AppHandle) {
    let Some(win) = popover(app) else {
        return;
    };
    position_popover(app, &win, None);
    let _ = win.show();
    let _ = win.set_focus();
    let _ = app.emit("popover-shown", ());
}

fn hide_popover(app: &AppHandle) {
    if let Some(win) = popover(app) {
        let _ = win.hide();
        *app.state::<AppState>().hidden_at.lock().unwrap() = Some(Instant::now());
    }
}

fn toggle_popover(app: &AppHandle) {
    let Some(win) = popover(app) else {
        return;
    };
    if win.is_visible().unwrap_or(false) {
        hide_popover(app);
        return;
    }
    // Clicking the tray while open first blurs (hides) the window, then delivers the click.
    let just_hidden = app
        .state::<AppState>()
        .hidden_at
        .lock()
        .unwrap()
        .map(|t| t.elapsed() < Duration::from_millis(300))
        .unwrap_or(false);
    if !just_hidden {
        show_popover(app);
    }
}

fn remember_tray_rect(app: &AppHandle, rect: &tauri::Rect) {
    let scale = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0);
    let (x, y) = match rect.position {
        Position::Physical(p) => (p.x, p.y),
        Position::Logical(p) => ((p.x * scale) as i32, (p.y * scale) as i32),
    };
    let (w, h) = match rect.size {
        Size::Physical(s) => (s.width as i32, s.height as i32),
        Size::Logical(s) => ((s.width * scale) as i32, (s.height * scale) as i32),
    };
    *app.state::<AppState>().tray_rect.lock().unwrap() = Some(TrayRect { x, y, w, h });
}

// ---------- commands ----------

#[tauri::command]
async fn list_lights(app: AppHandle) -> Result<Vec<Light>, String> {
    Ok(snapshot(&app))
}

#[tauri::command]
async fn refresh(app: AppHandle) -> Result<Vec<Light>, String> {
    tauri::async_runtime::spawn_blocking(move || do_refresh(&app))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn rescan(app: AppHandle) -> Result<Vec<Light>, String> {
    tauri::async_runtime::spawn_blocking(move || do_rescan(&app))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_light(app: AppHandle, ip: String) -> Result<Vec<Light>, String> {
    let ip = ip.trim().to_string();
    let (host, port) = match ip.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse::<u16>().map_err(|e| e.to_string())?)
        }
        _ => (ip.clone(), lights::DEFAULT_PORT),
    };
    if host.is_empty() {
        return Err("Enter an IP address".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let since = state.write_count();
        let mut light = Light::new(host.clone(), port, String::new(), true);
        refresh_one(&mut light, true);
        if !light.reachable {
            return Err(format!("No Elgato light answered at {host}:{port}"));
        }
        store::upsert(&mut state.lights.lock().unwrap(), light, since);
        Ok(finish(&app))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn remove_light(app: AppHandle, id: String) -> Result<Vec<Light>, String> {
    app.state::<AppState>().lights.lock().unwrap().remove(&id);
    Ok(finish(&app))
}

#[tauri::command]
async fn set_light(app: AppHandle, id: String, patch: Patch) -> Result<Light, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (ip, port) = state
            .lights
            .lock()
            .unwrap()
            .get(&id)
            .map(|l| (l.ip.clone(), l.port))
            .ok_or("Unknown light")?;
        let result = lights::put_state(&ip, port, &patch);
        let light = {
            let mut map = state.lights.lock().unwrap();
            let l = map.get_mut(&id).ok_or("Unknown light")?;
            match &result {
                Ok(s) => {
                    l.reachable = true;
                    l.on = s.on;
                    l.brightness = s.brightness;
                    l.temperature = s.temperature;
                }
                Err(_) => l.reachable = false,
            }
            l.rev = state.next_write();
            l.clone()
        };
        let list = snapshot(&app);
        update_tray(&app, &list);
        result.map(|_| light)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_all(app: AppHandle, on: bool) -> Result<Vec<Light>, String> {
    tauri::async_runtime::spawn_blocking(move || do_set_all(&app, on))
        .await
        .map_err(|e| e.to_string())
}

fn do_set_all(app: &AppHandle, on: bool) -> Vec<Light> {
    let state = app.state::<AppState>();
    let mut list: Vec<Light> = state.lights.lock().unwrap().values().cloned().collect();
    let patch = Patch {
        on: Some(on),
        ..Default::default()
    };
    std::thread::scope(|s| {
        for l in list.iter_mut() {
            let patch = patch.clone();
            s.spawn(move || match lights::put_state(&l.ip, l.port, &patch) {
                Ok(st) => {
                    l.reachable = true;
                    l.on = st.on;
                    l.brightness = st.brightness;
                    l.temperature = st.temperature;
                }
                Err(_) => l.reachable = false,
            });
        }
    });
    {
        let mut map = state.lights.lock().unwrap();
        for l in list {
            // Skip lights forgotten, or moved to another address, while the requests ran.
            if let Some(cur) = map.get_mut(&l.id).filter(|cur| cur.ip == l.ip) {
                cur.reachable = l.reachable;
                cur.on = l.on;
                cur.brightness = l.brightness;
                cur.temperature = l.temperature;
                cur.rev = state.next_write();
            }
        }
    }
    broadcast(app);
    snapshot(app)
}

#[tauri::command]
fn resize_popover(app: AppHandle, height: f64) {
    let Some(win) = popover(&app) else {
        return;
    };
    let height = height.clamp(120.0, 640.0);
    let _ = win.set_size(Size::Logical(LogicalSize::new(POPOVER_WIDTH, height)));
    position_popover(&app, &win, Some(height));
}

#[tauri::command]
fn close_popover(app: AppHandle) {
    hide_popover(&app);
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

// ---------- app ----------

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_popover(app);
        }))
        .invoke_handler(tauri::generate_handler![
            list_lights,
            refresh,
            rescan,
            add_light,
            remove_light,
            set_light,
            set_all,
            resize_popover,
            close_popover,
            quit
        ])
        .setup(|app| {
            let config_path = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::temp_dir())
                .join("lights.json");
            app.manage(AppState {
                lights: Mutex::new(load(&config_path)),
                writes: AtomicU64::new(0),
                tray_rect: Mutex::new(None),
                tray_look: Mutex::new(None),
                hidden_at: Mutex::new(None),
                config_path,
            });
            autostart::sync_path();

            let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
            let all_on = MenuItem::with_id(app, "all_on", "All lights on", true, None::<&str>)?;
            let all_off = MenuItem::with_id(app, "all_off", "All lights off", true, None::<&str>)?;
            let rescan = MenuItem::with_id(app, "rescan", "Rescan network", true, None::<&str>)?;
            let start_with_windows = CheckMenuItem::with_id(
                app,
                "autostart",
                "Start with Windows",
                true,
                autostart::is_enabled(),
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &open,
                    &PredefinedMenuItem::separator(app)?,
                    &all_on,
                    &all_off,
                    &rescan,
                    &PredefinedMenuItem::separator(app)?,
                    &start_with_windows,
                    &quit,
                ],
            )?;

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(tray_image(false, 0.45, taskbar_is_light()))
                .tooltip("Elgato Lights")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| {
                    let app = app.clone();
                    match event.id().as_ref() {
                        "open" => show_popover(&app),
                        "all_on" => {
                            std::thread::spawn(move || do_set_all(&app, true));
                        }
                        "all_off" => {
                            std::thread::spawn(move || do_set_all(&app, false));
                        }
                        "rescan" => {
                            std::thread::spawn(move || do_rescan(&app));
                        }
                        "autostart" => {
                            // Read back from the registry rather than trusting the menu's own toggle.
                            let _ = autostart::set_enabled(!autostart::is_enabled());
                            let _ = start_with_windows.set_checked(autostart::is_enabled());
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    let app = tray.app_handle();
                    match event {
                        TrayIconEvent::Click {
                            rect,
                            button,
                            button_state,
                            ..
                        } => {
                            remember_tray_rect(app, &rect);
                            if button_state != MouseButtonState::Up {
                                return;
                            }
                            match button {
                                MouseButton::Left => toggle_popover(app),
                                MouseButton::Middle => {
                                    let app = app.clone();
                                    std::thread::spawn(move || {
                                        let any_on =
                                            snapshot(&app).iter().any(|l| l.reachable && l.on);
                                        do_set_all(&app, !any_on);
                                    });
                                }
                                MouseButton::Right => {}
                            }
                        }
                        TrayIconEvent::DoubleClick { rect, .. } => remember_tray_rect(app, &rect),
                        _ => {}
                    }
                })
                .build(app)?;

            if let Some(win) = popover(app.handle()) {
                let handle = app.handle().clone();
                win.on_window_event(move |e| {
                    if let WindowEvent::Focused(false) = e {
                        hide_popover(&handle);
                    }
                });
            }

            // Initial discovery in the background so the tray appears immediately.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                do_rescan(&handle);
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running application");
}
