// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Tray icon event wiring.
//!
//! tray-icon recommends forwarding tray/menu events through the winit event loop so the
//! application wakes promptly. We also call Win32 foreground helpers synchronously inside
//! the tray handlers while the user's tray click is still active.

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use std::sync::{Once, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ShowMainWindow,
    Quit,
}

struct TrayMenuIds {
    show: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
}

static TRAY_MENU_IDS: RwLock<Option<TrayMenuIds>> = RwLock::new(None);
static INSTALL_ONCE: Once = Once::new();

pub fn set_menu_ids(show: tray_icon::menu::MenuId, quit: tray_icon::menu::MenuId) {
    if let Ok(mut guard) = TRAY_MENU_IDS.write() {
        *guard = Some(TrayMenuIds { show, quit });
    }
}

pub fn clear_menu_ids() {
    if let Ok(mut guard) = TRAY_MENU_IDS.write() {
        *guard = None;
    }
}

/// Install global tray/menu handlers once and return the command receiver for [`logic`].
pub fn install_tray_event_handlers(wake_ctx: egui::Context) -> Receiver<TrayCommand> {
    let (tx, rx) = unbounded();
    INSTALL_ONCE.call_once(|| {
        install_tray_icon_handler(wake_ctx.clone(), tx.clone());
        install_tray_menu_handler(wake_ctx, tx);
    });
    rx
}

fn install_tray_icon_handler(wake_ctx: egui::Context, tx: Sender<TrayCommand>) {
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        if let tray_icon::TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
            ..
        } = event
        {
            crate::ipc::force_foreground_if_visible();
            let _ = tx.send(TrayCommand::ShowMainWindow);
            wake_ctx.request_repaint();
        }
    }));
}

fn install_tray_menu_handler(wake_ctx: egui::Context, tx: Sender<TrayCommand>) {
    tray_icon::menu::MenuEvent::set_event_handler(Some(
        move |event: tray_icon::menu::MenuEvent| {
        let cmd = TRAY_MENU_IDS.read().ok().and_then(|guard| {
            let ids = guard.as_ref()?;
            if event.id == ids.show {
                Some(TrayCommand::ShowMainWindow)
            } else if event.id == ids.quit {
                Some(TrayCommand::Quit)
            } else {
                None
            }
        });
        let Some(cmd) = cmd else {
            return;
        };
        if cmd == TrayCommand::ShowMainWindow {
            crate::ipc::force_foreground_if_visible();
        }
        let _ = tx.send(cmd);
        wake_ctx.request_repaint();
        },
    ));
}
