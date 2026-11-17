use std::sync::Mutex;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, CheckMenuItemBuilder, Menu, MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Wry};

use crate::runner::{TrayState, TraySummary};

pub const OPEN: &str = "open";
pub const SYNC_ALL: &str = "sync_all";
pub const PAUSE: &str = "pause";
pub const QUIT: &str = "quit";
pub const PAIR: &str = "pair:";

#[cfg(target_os = "macos")]
const GLYPHS: [&[u8]; 3] = [
    include_bytes!("../icons/tray-idle.png"),
    include_bytes!("../icons/tray-busy.png"),
    include_bytes!("../icons/tray-alert.png"),
];
#[cfg(not(target_os = "macos"))]
const GLYPHS: [&[u8]; 3] = [
    include_bytes!("../icons/tray-color-idle.png"),
    include_bytes!("../icons/tray-color-busy.png"),
    include_bytes!("../icons/tray-color-alert.png"),
];

fn glyph(state: TrayState) -> Option<Image<'static>> {
    let bytes = match state {
        TrayState::Idle => GLYPHS[0],
        TrayState::Busy => GLYPHS[1],
        TrayState::Alert => GLYPHS[2],
    };
    Image::from_bytes(bytes).ok()
}

struct Parts {
    lines: Vec<(String, MenuItem<Wry>)>,
    pause: CheckMenuItem<Wry>,
    shown: Option<TraySummary>,
}

pub struct Tray {
    icon: TrayIcon<Wry>,
    parts: Mutex<Parts>,
}

fn build_menu(app: &AppHandle, summary: &TraySummary) -> tauri::Result<(Menu<Wry>, Parts)> {
    let mut menu = MenuBuilder::new(app);
    let mut lines = Vec::new();
    if summary.lines.is_empty() {
        menu = menu.item(&MenuItemBuilder::with_id("none", "No pairs set up yet").enabled(false).build(app)?);
    }
    for line in &summary.lines {
        let item = MenuItemBuilder::with_id(format!("{PAIR}{}", line.id), &line.text).build(app)?;
        menu = menu.item(&item);
        lines.push((line.id.clone(), item));
    }
    let pause = CheckMenuItemBuilder::with_id(PAUSE, "Pause automatic syncing").checked(summary.paused).build(app)?;
    let menu = menu
        .separator()
        .text(SYNC_ALL, "Sync all now")
        .item(&pause)
        .separator()
        .text(OPEN, "Open Dupli")
        .text(QUIT, "Quit Dupli")
        .build()?;
    Ok((menu, Parts { lines, pause, shown: None }))
}

impl Tray {
    pub fn new(app: &AppHandle, summary: &TraySummary) -> tauri::Result<Tray> {
        let (menu, parts) = build_menu(app, summary)?;
        let mut builder = TrayIconBuilder::with_id("dupli").menu(&menu).tooltip("Dupli");
        if let Some(icon) = glyph(summary.state) {
            builder = builder.icon(icon);
        }
        #[cfg(target_os = "macos")]
        {
            builder = builder.icon_as_template(true).show_menu_on_left_click(true);
        }
        #[cfg(not(target_os = "macos"))]
        {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            builder = builder.show_menu_on_left_click(false).on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                    crate::show_window(tray.app_handle(), None);
                }
            });
        }
        let icon = builder.build(app)?;
        let tray = Tray { icon, parts: Mutex::new(parts) };
        tray.update(app, summary);
        Ok(tray)
    }

    // Change the menu only when the summary changes.
    pub fn update(&self, app: &AppHandle, summary: &TraySummary) {
        let mut parts = self.parts.lock().unwrap_or_else(|e| e.into_inner());
        if parts.shown.as_ref() == Some(summary) {
            return;
        }
        let same_pairs = parts.lines.len() == summary.lines.len()
            && parts.lines.iter().zip(&summary.lines).all(|((id, _), line)| *id == line.id);
        if same_pairs {
            for ((_, item), line) in parts.lines.iter().zip(&summary.lines) {
                let _ = item.set_text(&line.text);
            }
            let _ = parts.pause.set_checked(summary.paused);
        } else if let Ok((menu, fresh)) = build_menu(app, summary) {
            let _ = self.icon.set_menu(Some(menu));
            *parts = fresh;
        }
        if parts.shown.as_ref().map(|s| s.state) != Some(summary.state) {
            let _ = self.icon.set_icon(glyph(summary.state));
            #[cfg(target_os = "macos")]
            let _ = self.icon.set_icon_as_template(true);
        }
        let tip = match summary.state {
            TrayState::Busy => "Dupli is syncing",
            TrayState::Alert => "Dupli needs a look",
            TrayState::Idle if summary.paused => "Dupli (automatic syncing paused)",
            TrayState::Idle => "Dupli",
        };
        let _ = self.icon.set_tooltip(Some(tip));
        parts.shown = Some(summary.clone());
    }
}
