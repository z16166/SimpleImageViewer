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

use crate::app::ImageViewerApp;
use crate::settings::{
    DirectoryTreeListPreviewSize, DirectoryTreeNavStyle, PairedJpegHandling, PairedJpegPrimaryKind,
};
use crate::ui::utils::{
    path_display_box, settings_card, stable_selectable_value, styled_button, themed_labeled_toggle,
};
use eframe::egui::{self, RichText};
use rust_i18n::t;

const PAIRED_RAW_JPEG_COMBO_WIDTH: f32 = 180.0;
const DIRECTORY_TREE_NAV_STYLE_COMBO_WIDTH: f32 = 180.0;
const DIRECTORY_TREE_PREVIEW_SIZE_COMBO_WIDTH: f32 = 120.0;

pub(super) fn draw_library_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, open_dir: &mut bool) {
    ui.vertical(|ui| {
        draw_library_controls(app, ui, open_dir);
    });
}

fn draw_library_controls(app: &mut ImageViewerApp, ui: &mut egui::Ui, open_dir: &mut bool) {
    // ThemePalette is Copy; take by value once so closures can borrow app mutably.
    let palette = app.cached_palette;
    settings_card(ui, &palette, t!("section.directory"), |ui| {
        let dir_full = app
            .current_browse_directory()
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        let dir_empty = app.current_browse_directory().is_none();
        let dir_label = if dir_empty {
            t!("label.no_dir").to_string()
        } else {
            dir_full.clone().unwrap_or_default()
        };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if styled_button(ui, t!("btn.pick"), &palette).clicked() {
                    *open_dir = true;
                }
                ui.add_space(4.0);
                if styled_button(ui, t!("btn.refresh"), &palette).clicked() {
                    app.start_refresh_file_list();
                }

                let box_w = (ui.available_width() - 16.0).max(20.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let resp = path_display_box(ui, &dir_label, dir_empty, box_w, &palette);
                    if let Some(full) = &dir_full {
                        resp.on_hover_text(full.as_str());
                    }
                });
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("library.images")).color(palette.text_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(app.image_files.len().to_string());
            });
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("library.scan_status")).color(palette.text_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.horizontal(|ui| {
                    if app.scanning {
                        ui.spinner();
                        // Borrow status text; avoid cloning the String every frame.
                        ui.label(app.status_message.as_str());
                    } else if app.current_browse_directory().is_some() {
                        ui.label(t!("library.scan_idle").as_ref());
                    } else {
                        ui.label(t!("library.scan_no_directory").as_ref());
                    }
                });
            });
        });

        ui.add_space(4.0);
        let old_tree_nav = app.settings.show_directory_tree_nav;
        if themed_labeled_toggle(
            ui,
            &mut app.settings.show_directory_tree_nav,
            t!("label.show_directory_tree_nav"),
            &palette,
        )
        .changed()
        {
            if app.settings.show_directory_tree_nav {
                app.show_directory_tree_nav(ui.ctx());
            } else {
                app.hide_directory_tree_nav(ui.ctx());
            }
            if old_tree_nav != app.settings.show_directory_tree_nav {
                app.queue_save();
            }
        }
        if app.auto_hidden_directory_tree_nav && app.settings.show_directory_tree_nav {
            ui.label(egui::RichText::new(t!("label.directory_tree_nav_session_hidden")).weak());
        }

        let old_tree_nav_style = app.settings.directory_tree_nav_style;
        ui.add_enabled_ui(app.settings.show_directory_tree_nav, |ui| {
            ui.horizontal(|ui| {
                ui.label(t!("label.directory_tree_nav_style"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("directory_tree_nav_style_combo")
                        .width(DIRECTORY_TREE_NAV_STYLE_COMBO_WIDTH)
                        .selected_text(app.settings.directory_tree_nav_style.label())
                        .show_ui(ui, |ui| {
                            ui.set_min_width(DIRECTORY_TREE_NAV_STYLE_COMBO_WIDTH);
                            stable_selectable_value(
                                ui,
                                &mut app.settings.directory_tree_nav_style,
                                DirectoryTreeNavStyle::Embedded,
                                DirectoryTreeNavStyle::Embedded.label(),
                            );
                            stable_selectable_value(
                                ui,
                                &mut app.settings.directory_tree_nav_style,
                                DirectoryTreeNavStyle::Detached,
                                DirectoryTreeNavStyle::Detached.label(),
                            );
                        });
                });
            });
        });
        if old_tree_nav_style != app.settings.directory_tree_nav_style {
            app.on_directory_tree_nav_style_changed(
                ui.ctx(),
                old_tree_nav_style == DirectoryTreeNavStyle::Detached,
            );
            app.queue_save();
        }

        let old_list_previews = app.settings.directory_tree_show_list_previews;
        let old_preview_size = app.settings.directory_tree_list_preview_size;
        ui.add_enabled_ui(app.settings.show_directory_tree_nav, |ui| {
            if themed_labeled_toggle(
                ui,
                &mut app.settings.directory_tree_show_list_previews,
                t!("label.directory_tree_show_list_previews"),
                &palette,
            )
            .changed()
                && old_list_previews != app.settings.directory_tree_show_list_previews
            {
                app.on_directory_tree_list_preview_settings_changed(ui.ctx());
            }

            ui.add_enabled_ui(app.settings.directory_tree_show_list_previews, |ui| {
                ui.horizontal(|ui| {
                    ui.label(t!("label.directory_tree_list_preview_size"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("directory_tree_list_preview_size_combo")
                            .width(DIRECTORY_TREE_PREVIEW_SIZE_COMBO_WIDTH)
                            .selected_text(app.settings.directory_tree_list_preview_size.label())
                            .show_ui(ui, |ui| {
                                ui.set_min_width(DIRECTORY_TREE_PREVIEW_SIZE_COMBO_WIDTH);
                                for size in [
                                    DirectoryTreeListPreviewSize::Small,
                                    DirectoryTreeListPreviewSize::Medium,
                                    DirectoryTreeListPreviewSize::Large,
                                ] {
                                    stable_selectable_value(
                                        ui,
                                        &mut app.settings.directory_tree_list_preview_size,
                                        size,
                                        size.label(),
                                    );
                                }
                            });
                    });
                });
            });
            if old_preview_size != app.settings.directory_tree_list_preview_size {
                app.on_directory_tree_list_preview_settings_changed(ui.ctx());
            }
        });

        let old_recursive = app.settings.recursive;
        if app.directory_tree_settings_active() {
            ui.add_enabled_ui(false, |ui| {
                let mut recursive = false;
                themed_labeled_toggle(ui, &mut recursive, t!("label.recursive_scan"), &palette);
            });
            ui.label(RichText::new(t!("directory_tree.recursive_disabled")).weak());
        } else if themed_labeled_toggle(
            ui,
            &mut app.settings.recursive,
            t!("label.recursive_scan"),
            &palette,
        )
        .changed()
        {
            if !old_recursive && app.settings.recursive {
                app.settings.recursive = false;
                app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                    crate::ui::dialogs::confirm::State::recursive_scan(
                        t!("win.confirm_recursive_title").to_string(),
                        t!("win.confirm_recursive_msg").to_string(),
                    ),
                ));
            } else if old_recursive && !app.settings.recursive {
                if let Some(dir) = app.current_browse_directory() {
                    app.reload_current_browse_directory(dir);
                }
                app.queue_save();
            }
        }

        if themed_labeled_toggle(
            ui,
            &mut app.settings.preload,
            t!("label.enable_preload"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }

        if themed_labeled_toggle(
            ui,
            &mut app.settings.keep_gallery_dir_on_double_click,
            t!("label.keep_gallery_dir_on_double_click"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }

        let old_pair_handling = app.settings.paired_raw_jpeg_handling;
        ui.horizontal(|ui| {
            ui.label(t!("label.paired_raw_jpeg_handling"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("paired_raw_jpeg_handling_combo")
                    .width(PAIRED_RAW_JPEG_COMBO_WIDTH)
                    .selected_text(
                        app.settings
                            .paired_raw_jpeg_handling
                            .label(PairedJpegPrimaryKind::Raw),
                    )
                    .show_ui(ui, |ui| {
                        ui.set_min_width(PAIRED_RAW_JPEG_COMBO_WIDTH);
                        stable_selectable_value(
                            ui,
                            &mut app.settings.paired_raw_jpeg_handling,
                            PairedJpegHandling::ShowBoth,
                            PairedJpegHandling::ShowBoth.label(PairedJpegPrimaryKind::Raw),
                        );
                        stable_selectable_value(
                            ui,
                            &mut app.settings.paired_raw_jpeg_handling,
                            PairedJpegHandling::SkipPrimary,
                            PairedJpegHandling::SkipPrimary.label(PairedJpegPrimaryKind::Raw),
                        );
                        stable_selectable_value(
                            ui,
                            &mut app.settings.paired_raw_jpeg_handling,
                            PairedJpegHandling::SkipJpeg,
                            PairedJpegHandling::SkipJpeg.label(PairedJpegPrimaryKind::Raw),
                        );
                    });
            });
        });
        if old_pair_handling != app.settings.paired_raw_jpeg_handling {
            if let Some(dir) = app.current_browse_directory() {
                app.reload_current_browse_directory(dir);
            }
            app.queue_save();
        }

        let old_psd_pair = app.settings.paired_psd_jpeg_handling;
        ui.horizontal(|ui| {
            ui.label(t!("label.paired_psd_jpeg_handling"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("paired_psd_jpeg_handling_combo")
                    .width(PAIRED_RAW_JPEG_COMBO_WIDTH)
                    .selected_text(
                        app.settings
                            .paired_psd_jpeg_handling
                            .label(PairedJpegPrimaryKind::Psd),
                    )
                    .show_ui(ui, |ui| {
                        ui.set_min_width(PAIRED_RAW_JPEG_COMBO_WIDTH);
                        stable_selectable_value(
                            ui,
                            &mut app.settings.paired_psd_jpeg_handling,
                            PairedJpegHandling::ShowBoth,
                            PairedJpegHandling::ShowBoth.label(PairedJpegPrimaryKind::Psd),
                        );
                        stable_selectable_value(
                            ui,
                            &mut app.settings.paired_psd_jpeg_handling,
                            PairedJpegHandling::SkipPrimary,
                            PairedJpegHandling::SkipPrimary.label(PairedJpegPrimaryKind::Psd),
                        );
                        stable_selectable_value(
                            ui,
                            &mut app.settings.paired_psd_jpeg_handling,
                            PairedJpegHandling::SkipJpeg,
                            PairedJpegHandling::SkipJpeg.label(PairedJpegPrimaryKind::Psd),
                        );
                    });
            });
        });
        if old_psd_pair != app.settings.paired_psd_jpeg_handling {
            if let Some(dir) = app.current_browse_directory() {
                app.reload_current_browse_directory(dir);
            }
            app.queue_save();
        }

        if themed_labeled_toggle(
            ui,
            &mut app.settings.resume_last_image,
            t!("label.resume_last"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }
    });
}
