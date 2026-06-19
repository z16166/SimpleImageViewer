// Directory tree navigation UI drawing.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossbeam_channel::Sender;
use eframe::egui;
use rust_i18n::t;

use crate::app::ImageViewerApp;
use crate::app::RootRedrawWake;
use crate::directory_tree_places::KnownFolderEntry;
use crate::directory_tree_places::types::KnownFolderKind;
use crate::loader::preview_aspect_matches_logical;
use crate::path_location::is_unc_path;
use crate::theme::ThemePalette;
use crate::ui::osd::{format_file_modified, format_file_size};

use super::sort::{compare_image_list_sort_keys, file_name_sort_key, image_list_sort_indicator};
use super::{
    DIRECTORY_TREE_COL_MODIFIED_MIN_WIDTH, DIRECTORY_TREE_COL_MODIFIED_WIDTH,
    DIRECTORY_TREE_COL_NAME_MIN_WIDTH, DIRECTORY_TREE_COL_SIZE_MIN_WIDTH,
    DIRECTORY_TREE_COL_SIZE_WIDTH, DIRECTORY_TREE_COL_THUMB_WIDTH,
    DIRECTORY_TREE_EMBEDDED_MIN_WIDTH, DIRECTORY_TREE_EXPAND_ICON_WIDTH,
    DIRECTORY_TREE_FOLDER_ICON_WIDTH, DIRECTORY_TREE_HEADER_HEIGHT,
    DIRECTORY_TREE_IMAGE_ROW_HEIGHT, DIRECTORY_TREE_INDENT, DIRECTORY_TREE_LEFT_MAX_WIDTH_RATIO,
    DIRECTORY_TREE_LEFT_MIN_WIDTH, DIRECTORY_TREE_RIGHT_MIN_WIDTH, DIRECTORY_TREE_ROW_HEIGHT,
    DIRECTORY_TREE_SPLITTER_GRAB_WIDTH, DirectoryTreeCommand, DirectoryTreeFileRow,
    DirectoryTreeNode, DirectoryTreeState, ImageListSortColumn, is_network_tree_path,
    is_places_sentinel_path, is_this_pc_tree_path, network_tree_path, this_pc_tree_path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectoryTreeNodeIcon {
    Folder,
    ThisPc,
    Network,
    Drive,
    KnownFolder(KnownFolderKind),
}

fn known_folder_kind_for_tree_path(
    state: &DirectoryTreeState,
    path: &Path,
) -> Option<KnownFolderKind> {
    state
        .known_folders
        .iter()
        .find(|entry| entry.tree_path == path)
        .map(|entry| entry.kind)
}

fn is_places_drive_root(state: &DirectoryTreeState, path: &Path) -> bool {
    state
        .nodes
        .get(&this_pc_tree_path())
        .is_some_and(|node| node.children.iter().any(|child| child.as_path() == path))
}

pub(super) fn directory_tree_node_icon(
    state: &DirectoryTreeState,
    path: &Path,
) -> DirectoryTreeNodeIcon {
    if is_this_pc_tree_path(path) {
        return DirectoryTreeNodeIcon::ThisPc;
    }
    if is_network_tree_path(path) {
        return DirectoryTreeNodeIcon::Network;
    }
    if let Some(kind) = known_folder_kind_for_tree_path(state, path) {
        return DirectoryTreeNodeIcon::KnownFolder(kind);
    }
    if is_places_drive_root(state, path) {
        return DirectoryTreeNodeIcon::Drive;
    }
    DirectoryTreeNodeIcon::Folder
}

fn directory_tree_node_expandable(node: &DirectoryTreeNode, path: &Path) -> bool {
    if is_places_sentinel_path(path) {
        return true;
    }
    node.loading || !node.children_loaded || !node.children.is_empty()
}

fn paint_tree_expand_chevron(ui: &mut egui::Ui, expanded: bool, response: &egui::Response) {
    let stroke = egui::Stroke::new(
        1.15_f32,
        ui.visuals()
            .widgets
            .noninteractive
            .fg_stroke
            .color
            .gamma_multiply(0.72),
    );
    let center = response.rect.center();
    let half = 3.5;
    if expanded {
        ui.painter().line_segment(
            [
                center + egui::vec2(-half, -half * 0.35),
                center + egui::vec2(0.0, half * 0.75),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                center + egui::vec2(0.0, half * 0.75),
                center + egui::vec2(half, -half * 0.35),
            ],
            stroke,
        );
    } else {
        ui.painter().line_segment(
            [
                center + egui::vec2(-half * 0.55, -half),
                center + egui::vec2(half * 0.75, 0.0),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                center + egui::vec2(half * 0.75, 0.0),
                center + egui::vec2(-half * 0.55, half),
            ],
            stroke,
        );
    }
}

fn paint_directory_tree_node_icon(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    icon: DirectoryTreeNodeIcon,
    palette: &ThemePalette,
) {
    let size = rect.width().min(rect.height()) * 0.78;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.15_f32, palette.text_normal.gamma_multiply(0.88));
    let accent = palette.accent2;
    let fill = accent.gamma_multiply(0.82);
    let soft_fill = accent.gamma_multiply(0.28);

    match icon {
        DirectoryTreeNodeIcon::Folder => {
            let body = egui::Rect::from_min_max(
                icon_rect.left_bottom() + egui::vec2(0.0, -icon_rect.height() * 0.52),
                icon_rect.right_bottom(),
            );
            let tab = egui::Rect::from_min_max(
                icon_rect.left_top() + egui::vec2(0.0, icon_rect.height() * 0.18),
                icon_rect.left_top()
                    + egui::vec2(icon_rect.width() * 0.56, icon_rect.height() * 0.46),
            );
            painter.rect(body, 1.5, soft_fill, stroke, egui::StrokeKind::Inside);
            painter.rect(tab, 1.0, fill, stroke, egui::StrokeKind::Inside);
        }
        DirectoryTreeNodeIcon::ThisPc => {
            let screen = egui::Rect::from_center_size(
                icon_rect.center() + egui::vec2(0.0, -icon_rect.height() * 0.08),
                egui::vec2(icon_rect.width() * 0.88, icon_rect.height() * 0.58),
            );
            painter.rect(screen, 1.5, soft_fill, stroke, egui::StrokeKind::Inside);
            let stand_w = icon_rect.width() * 0.34;
            painter.line_segment(
                [
                    screen.center_bottom() + egui::vec2(-stand_w * 0.5, 0.0),
                    screen.center_bottom() + egui::vec2(stand_w * 0.5, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    screen.center_bottom(),
                    icon_rect.center_bottom() + egui::vec2(0.0, -1.0),
                ],
                stroke,
            );
        }
        DirectoryTreeNodeIcon::Network => {
            let center = icon_rect.center();
            let radius = icon_rect.width() * 0.34;
            painter.circle_stroke(center, radius, stroke);
            painter.line_segment(
                [
                    center + egui::vec2(-radius, 0.0),
                    center + egui::vec2(radius, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(0.0, -radius),
                    center + egui::vec2(0.0, radius),
                ],
                stroke,
            );
            painter.circle_filled(center, 1.6, fill);
        }
        DirectoryTreeNodeIcon::Drive => {
            let body = egui::Rect::from_center_size(
                icon_rect.center() + egui::vec2(0.0, icon_rect.height() * 0.06),
                egui::vec2(icon_rect.width() * 0.82, icon_rect.height() * 0.52),
            );
            painter.rect(body, 2.0, soft_fill, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    body.left_center() + egui::vec2(body.width() * 0.18, 0.0),
                    body.right_center() + egui::vec2(-body.width() * 0.18, 0.0),
                ],
                stroke,
            );
        }
        DirectoryTreeNodeIcon::KnownFolder(kind) => match kind {
            KnownFolderKind::Documents => {
                let page = egui::Rect::from_center_size(
                    icon_rect.center(),
                    egui::vec2(icon_rect.width() * 0.72, icon_rect.height() * 0.86),
                );
                painter.rect(page, 1.5, soft_fill, stroke, egui::StrokeKind::Inside);
                for offset in [0.22, 0.38, 0.54] {
                    painter.line_segment(
                        [
                            page.left_center()
                                + egui::vec2(page.width() * 0.18, -page.height() * offset),
                            page.right_center()
                                + egui::vec2(-page.width() * 0.18, -page.height() * offset),
                        ],
                        stroke,
                    );
                }
            }
            KnownFolderKind::Pictures => {
                let frame = egui::Rect::from_center_size(
                    icon_rect.center(),
                    egui::vec2(icon_rect.width() * 0.82, icon_rect.height() * 0.72),
                );
                painter.rect(frame, 1.5, soft_fill, stroke, egui::StrokeKind::Inside);
                let hill = [
                    frame.left_bottom() + egui::vec2(frame.width() * 0.08, -frame.height() * 0.18),
                    frame.center_bottom()
                        + egui::vec2(-frame.width() * 0.08, -frame.height() * 0.42),
                    frame.right_bottom()
                        + egui::vec2(-frame.width() * 0.08, -frame.height() * 0.18),
                ];
                painter.add(egui::Shape::closed_line(hill.to_vec(), stroke));
                painter.circle_filled(
                    frame.right_top() + egui::vec2(-frame.width() * 0.22, frame.height() * 0.22),
                    1.5,
                    fill,
                );
            }
            KnownFolderKind::Music => {
                let center = icon_rect.center();
                painter.circle_stroke(
                    center + egui::vec2(-icon_rect.width() * 0.12, icon_rect.height() * 0.16),
                    icon_rect.width() * 0.14,
                    stroke,
                );
                painter.line_segment(
                    [
                        center + egui::vec2(icon_rect.width() * 0.02, -icon_rect.height() * 0.18),
                        center + egui::vec2(icon_rect.width() * 0.28, -icon_rect.height() * 0.34),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        center + egui::vec2(icon_rect.width() * 0.28, -icon_rect.height() * 0.34),
                        center + egui::vec2(icon_rect.width() * 0.28, icon_rect.height() * 0.24),
                    ],
                    stroke,
                );
            }
            KnownFolderKind::Videos => {
                let frame = egui::Rect::from_center_size(
                    icon_rect.center(),
                    egui::vec2(icon_rect.width() * 0.82, icon_rect.height() * 0.62),
                );
                painter.rect(frame, 1.5, soft_fill, stroke, egui::StrokeKind::Inside);
                let play = [
                    frame.center() + egui::vec2(-frame.width() * 0.12, -frame.height() * 0.18),
                    frame.center() + egui::vec2(-frame.width() * 0.12, frame.height() * 0.18),
                    frame.center() + egui::vec2(frame.width() * 0.18, 0.0),
                ];
                painter.add(egui::Shape::convex_polygon(
                    play.to_vec(),
                    fill,
                    egui::Stroke::NONE,
                ));
            }
            KnownFolderKind::Downloads => {
                let tray = egui::Rect::from_center_size(
                    icon_rect.center() + egui::vec2(0.0, icon_rect.height() * 0.16),
                    egui::vec2(icon_rect.width() * 0.78, icon_rect.height() * 0.34),
                );
                painter.rect(tray, 1.5, soft_fill, stroke, egui::StrokeKind::Inside);
                painter.line_segment(
                    [
                        icon_rect.center_top() + egui::vec2(0.0, icon_rect.height() * 0.08),
                        icon_rect.center() + egui::vec2(0.0, icon_rect.height() * 0.08),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        icon_rect.center() + egui::vec2(0.0, icon_rect.height() * 0.08),
                        icon_rect.center()
                            + egui::vec2(-icon_rect.width() * 0.16, -icon_rect.height() * 0.02),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        icon_rect.center() + egui::vec2(0.0, icon_rect.height() * 0.08),
                        icon_rect.center()
                            + egui::vec2(icon_rect.width() * 0.16, -icon_rect.height() * 0.02),
                    ],
                    stroke,
                );
            }
            KnownFolderKind::Desktop => {
                let screen = egui::Rect::from_center_size(
                    icon_rect.center() + egui::vec2(0.0, -icon_rect.height() * 0.08),
                    egui::vec2(icon_rect.width() * 0.82, icon_rect.height() * 0.54),
                );
                painter.rect(screen, 1.5, soft_fill, stroke, egui::StrokeKind::Inside);
            }
            KnownFolderKind::Profile => {
                let head = icon_rect.center() + egui::vec2(0.0, -icon_rect.height() * 0.12);
                painter.circle_stroke(head, icon_rect.width() * 0.16, stroke);
                painter.circle_stroke(
                    head + egui::vec2(0.0, icon_rect.height() * 0.24),
                    icon_rect.width() * 0.24,
                    stroke,
                );
            }
        },
    }
}

fn paint_tree_expand_icon(ui: &mut egui::Ui, expanded: bool, response: &egui::Response) {
    paint_tree_expand_chevron(ui, expanded, response);
}

fn paint_tree_folder_icon(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    icon: DirectoryTreeNodeIcon,
    palette: &ThemePalette,
) {
    paint_directory_tree_node_icon(ui, rect, icon, palette);
}

fn directory_tree_row_selected_fill(palette: &ThemePalette) -> egui::Color32 {
    if palette.is_dark {
        egui::Color32::from_gray(78)
    } else {
        egui::Color32::from_rgba_unmultiplied(
            palette.accent2.r(),
            palette.accent2.g(),
            palette.accent2.b(),
            30,
        )
    }
}

fn directory_tree_row_selected_text(palette: &ThemePalette) -> egui::Color32 {
    if palette.is_dark {
        egui::Color32::from_gray(210)
    } else {
        palette.accent2
    }
}

fn paint_directory_tree_folder_name(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    selected: bool,
    hovered: bool,
    name: &str,
    palette: &ThemePalette,
) {
    if selected {
        ui.painter()
            .rect_filled(rect, 3.0, directory_tree_row_selected_fill(palette));
    } else if hovered {
        ui.painter().rect_filled(rect, 3.0, palette.widget_hover);
    }
    let text_color = if selected {
        directory_tree_row_selected_text(palette)
    } else {
        palette.text_normal
    };
    let font = egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
    ui.painter().text(
        rect.left_center() + egui::vec2(4.0, 0.0),
        egui::Align2::LEFT_CENTER,
        name,
        font,
        text_color,
    );
}

pub(super) fn image_list_interaction_enabled(state: &DirectoryTreeState) -> bool {
    !state.scanning && !state.image_list_reordering
}

pub(super) fn image_list_sorting_available(state: &DirectoryTreeState) -> bool {
    image_list_interaction_enabled(state) && !state.image_rows.is_empty()
}

pub(super) fn draw_directory_tree_window(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
    embedded: bool,
) {
    ui.visuals_mut().button_frame = false;
    ui.visuals_mut().override_text_color = Some(palette.text_normal);
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, palette.panel_bg);
    draw_directory_tree_top_panels(
        ui,
        state,
        command_tx,
        root_wake,
        palette,
        egui::vec2(ui.available_width(), ui.available_height()),
        embedded,
    );
}

pub(super) fn directory_tree_left_panel_width_limits(viewport_width: f32) -> (f32, f32) {
    let viewport_width = viewport_width.max(0.0);
    let layout_cap =
        (viewport_width - DIRECTORY_TREE_SPLITTER_GRAB_WIDTH - DIRECTORY_TREE_RIGHT_MIN_WIDTH)
            .max(0.0);
    let max_left = (viewport_width * DIRECTORY_TREE_LEFT_MAX_WIDTH_RATIO).min(layout_cap);
    let min_left = DIRECTORY_TREE_LEFT_MIN_WIDTH.min(max_left);
    (min_left, max_left.max(min_left))
}

pub(super) fn clamp_directory_tree_left_panel_width(width: f32, viewport_width: f32) -> f32 {
    let (min_left, max_left) = directory_tree_left_panel_width_limits(viewport_width);
    width.clamp(min_left, max_left)
}

pub(super) fn directory_tree_panel_layout(
    left_panel_width: f32,
    image_list_panel_width: f32,
    viewport_width: f32,
) -> (f32, f32) {
    let splitter_w = DIRECTORY_TREE_SPLITTER_GRAB_WIDTH;
    let min_list = DIRECTORY_TREE_RIGHT_MIN_WIDTH;
    let (min_left, max_left) = directory_tree_left_panel_width_limits(viewport_width);

    if viewport_width <= splitter_w {
        return (0.0, 0.0);
    }

    let available = viewport_width - splitter_w;
    let mut left_w = left_panel_width.clamp(min_left, max_left);
    let mut list_w = image_list_panel_width.max(min_list);

    if left_w + list_w > available {
        list_w = (available - left_w).max(min_list);
    } else if left_w + list_w < available {
        list_w = available - left_w;
    }

    list_w = list_w.clamp(min_list, (available - min_left).max(0.0));
    left_w = (available - list_w).clamp(min_left, max_left);
    list_w = available - left_w;

    (left_w, list_w)
}

fn draw_directory_tree_top_panels(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
    panel_size: egui::Vec2,
    embedded: bool,
) {
    let viewport_height = panel_size.y;
    let viewport_width = panel_size.x;
    let (left_w, list_w) = directory_tree_panel_layout(
        state.left_panel_width,
        state.image_list_panel_width,
        viewport_width,
    );
    let splitter_w = DIRECTORY_TREE_SPLITTER_GRAB_WIDTH;
    let right_w = list_w;

    let origin = ui.cursor().min;
    let left_rect = egui::Rect::from_min_size(origin, egui::vec2(left_w, viewport_height));
    let splitter_rect = egui::Rect::from_min_size(
        origin + egui::vec2(left_w, 0.0),
        egui::vec2(splitter_w, viewport_height),
    );
    let right_rect = egui::Rect::from_min_size(
        origin + egui::vec2(left_w + splitter_w, 0.0),
        egui::vec2(right_w, viewport_height),
    );

    ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
        ui.set_clip_rect(left_rect);
        ui.set_width(left_w);
        draw_folder_panel(ui, state, command_tx, root_wake, palette);
    });

    ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
        ui.set_clip_rect(right_rect);
        ui.set_width(right_w);
        draw_image_file_list(ui, state, command_tx, palette, embedded);
    });

    let splitter_id = ui.id().with("directory_tree_splitter");
    let splitter_response = ui.interact(splitter_rect, splitter_id, egui::Sense::drag());
    if splitter_response.dragged() {
        state.left_panel_width = clamp_directory_tree_left_panel_width(
            state.left_panel_width + splitter_response.drag_delta().x,
            viewport_width,
        );
        state.panel_layout_dirty = true;
        ui.ctx().request_repaint();
    }
    if splitter_response.hovered() || splitter_response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    let splitter_stroke = if splitter_response.dragged() {
        ui.style().visuals.widgets.active.fg_stroke
    } else if splitter_response.hovered() {
        ui.style().visuals.widgets.hovered.fg_stroke
    } else {
        ui.style().visuals.widgets.noninteractive.bg_stroke
    };
    ui.painter().vline(
        splitter_rect.center().x,
        splitter_rect.y_range(),
        splitter_stroke,
    );
    if embedded {
        state.embedded_nav_panel_width = viewport_width;
    }
}

pub(super) fn preview_texture_contain_rect(
    cell: egui::Rect,
    texture_width: f32,
    texture_height: f32,
) -> egui::Rect {
    if texture_width <= 0.0 || texture_height <= 0.0 {
        return cell;
    }
    let scale = (cell.width() / texture_width).min(cell.height() / texture_height);
    let size = egui::vec2(texture_width * scale, texture_height * scale);
    let offset = (cell.size() - size) * 0.5;
    egui::Rect::from_min_size(cell.min + offset, size)
}

fn paint_image_list_thumbnail(
    painter: &egui::Painter,
    palette: &ThemePalette,
    thumb_rect: egui::Rect,
    texture: Option<&egui::TextureHandle>,
    logical_size: Option<(u32, u32)>,
) {
    let inner = thumb_rect.shrink(2.0);
    let mut drew_texture = false;
    if let Some(texture) = texture {
        let tex_size = texture.size();
        let texture_w = tex_size[0] as f32;
        let texture_h = tex_size[1] as f32;
        let aspect_ok = logical_size.is_none_or(|(logical_w, logical_h)| {
            preview_aspect_matches_logical(texture_w as u32, texture_h as u32, logical_w, logical_h)
        });
        if aspect_ok && texture_w > 0.0 && texture_h > 0.0 {
            painter.rect_filled(inner, 1.0, palette.widget_bg);
            let image_rect = preview_texture_contain_rect(inner, texture_w, texture_h);
            painter.image(
                texture.id(),
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            drew_texture = true;
        }
    }
    if !drew_texture {
        painter.rect_filled(inner, 1.0, palette.widget_bg);
    }
}

fn draw_folder_panel(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
) {
    let scroll_to_selected = state.scroll_folder_to_selected;
    directory_tree_scroll_area("directory_tree_folders", ui, |ui| {
        if !state.places_loaded {
            if state.places_loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(t!("directory_tree.places_loading"));
                });
            } else if let Some(err) = &state.places_load_error {
                ui.label(egui::RichText::new(err.as_str()).color(ui.visuals().error_fg_color));
            } else if !state.workers_available {
                ui.label(t!("directory_tree.workers_unavailable"));
            }
            return;
        }
        let mut scrolled = false;
        for entry in &state.known_folders {
            scrolled |= draw_directory_node(
                ui,
                state,
                command_tx,
                root_wake,
                palette,
                &entry.tree_path,
                0,
                scroll_to_selected,
            );
        }
        scrolled |= draw_directory_node(
            ui,
            state,
            command_tx,
            root_wake,
            palette,
            &this_pc_tree_path(),
            0,
            scroll_to_selected,
        );
        if state.network_visible {
            scrolled |= draw_directory_node(
                ui,
                state,
                command_tx,
                root_wake,
                palette,
                &network_tree_path(),
                0,
                scroll_to_selected,
            );
        }
        if scrolled {
            state.scroll_folder_to_selected = false;
        }
    });
}

fn directory_tree_scroll_area(
    id_salt: &'static str,
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let scroll_height = ui.available_height();
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .auto_shrink([false, false])
        .max_height(scroll_height)
        .show(ui, add_contents);
}

fn draw_directory_node(
    ui: &mut egui::Ui,
    state: &DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
    path: &Path,
    depth: usize,
    scroll_to_selected: bool,
) -> bool {
    let Some(node) = state.nodes.get(path).cloned() else {
        return false;
    };

    let icon = directory_tree_node_icon(state, path);
    let expandable = directory_tree_node_expandable(&node, path);

    let mut scrolled = false;

    let row_width = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(row_width, DIRECTORY_TREE_ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(row_width);
            ui.add_space(depth as f32 * DIRECTORY_TREE_INDENT);

            if node.loading {
                ui.add_sized(
                    [DIRECTORY_TREE_EXPAND_ICON_WIDTH, DIRECTORY_TREE_ROW_HEIGHT],
                    egui::Spinner::new().size(DIRECTORY_TREE_ROW_HEIGHT * 0.55),
                );
            } else if expandable {
                let expand_response = ui.allocate_response(
                    egui::vec2(DIRECTORY_TREE_EXPAND_ICON_WIDTH, DIRECTORY_TREE_ROW_HEIGHT),
                    egui::Sense::click(),
                );
                paint_tree_expand_icon(ui, node.expanded, &expand_response);
                if expand_response.clicked() {
                    let _ =
                        command_tx.send(DirectoryTreeCommand::ToggleExpanded(path.to_path_buf()));
                }
            } else {
                ui.add_space(DIRECTORY_TREE_EXPAND_ICON_WIDTH);
            }

            let folder_rect = ui.allocate_exact_size(
                egui::vec2(DIRECTORY_TREE_FOLDER_ICON_WIDTH, DIRECTORY_TREE_ROW_HEIGHT),
                egui::Sense::hover(),
            );
            paint_tree_folder_icon(ui, folder_rect.0, icon, palette);

            let selected = state
                .selected_dir
                .as_deref()
                .is_some_and(|selected| selected == node.browse_path.as_path());
            let name_width = ui.available_width().max(1.0);
            let (name_rect, name_response) = ui.allocate_exact_size(
                egui::vec2(name_width, DIRECTORY_TREE_ROW_HEIGHT),
                egui::Sense::click(),
            );
            paint_directory_tree_folder_name(
                ui,
                name_rect,
                selected,
                name_response.hovered(),
                node.display_name.as_str(),
                palette,
            );
            let name_response = name_response.on_hover_text(node.browse_path.to_string_lossy());
            if scroll_to_selected && selected {
                name_response.scroll_to_me(Some(egui::Align::Center));
                scrolled = true;
            }
            if name_response.clicked() {
                if is_places_sentinel_path(path) {
                    let _ =
                        command_tx.send(DirectoryTreeCommand::ToggleExpanded(path.to_path_buf()));
                } else {
                    let browse_path = node.browse_path.clone();
                    let _ = command_tx.send(DirectoryTreeCommand::SelectDirectory(browse_path));
                }
                if let Some(wake) = root_wake {
                    wake();
                }
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                ui.ctx().request_repaint();
            }
        },
    );

    if let Some(error) = node.error.as_deref() {
        ui.horizontal(|ui| {
            ui.add_space((depth + 1) as f32 * DIRECTORY_TREE_INDENT);
            ui.label(
                egui::RichText::new(t!("directory_tree.read_failed", err = error).to_string())
                    .color(ui.visuals().error_fg_color),
            );
        });
    }

    if node.expanded {
        for child in node.children {
            scrolled |= draw_directory_node(
                ui,
                state,
                command_tx,
                root_wake,
                palette,
                &child,
                depth + 1,
                scroll_to_selected,
            );
        }
    }

    scrolled
}

fn draw_image_file_list(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    palette: &ThemePalette,
    embedded: bool,
) {
    let panel_rect = ui.max_rect();
    let list_focus_id = ui.id().with("directory_tree_image_list");
    let list_enabled = !state.scanning || !state.image_rows.is_empty();
    if list_enabled {
        let panel_response = ui.interact(panel_rect, list_focus_id, egui::Sense::click());
        if panel_response.clicked() {
            panel_response.request_focus();
            state.image_list_keyboard_active = true;
        }
    }

    if state.image_rows.is_empty() && !state.scanning {
        ui.label(egui::RichText::new(t!("directory_tree.no_images")).weak());
        return;
    }

    let status_height = if state.scanning && state.image_rows.is_empty() {
        DIRECTORY_TREE_ROW_HEIGHT
    } else {
        0.0
    };
    let row_height = DIRECTORY_TREE_IMAGE_ROW_HEIGHT;
    let row_spacing = ui.spacing().item_spacing.y;
    let row_height_with_spacing = row_height + row_spacing;
    let body_font = egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
    state.ensure_image_list_column_widths(
        ui.painter(),
        &body_font,
        &t!("directory_tree.col_size"),
        &t!("directory_tree.col_modified"),
    );
    let column_layout = image_list_column_layout(
        ui.available_width(),
        ui.spacing().item_spacing.x,
        state.image_list_col_size_w,
        state.image_list_col_modified_w,
    );

    draw_image_details_header(ui, state, &column_layout, palette, command_tx);

    let interaction_enabled = image_list_interaction_enabled(state);
    let viewport_height = (ui.available_height() - status_height).max(row_height_with_spacing);

    if interaction_enabled {
        try_handle_image_list_arrow_keys(ui, state, list_focus_id, command_tx, embedded);
    }

    let mut pending_scroll_offset = None;
    if list_enabled && state.scroll_image_list_to_current && !state.image_rows.is_empty() {
        pending_scroll_offset = min_scroll_offset_to_show_row(
            state.current_index,
            row_height_with_spacing,
            row_height,
            viewport_height,
            state.image_list_scroll_offset_y,
        )
        .map(|offset| offset.max(0.0));
        state.scroll_image_list_to_current = false;
    }

    ui.add_enabled_ui(list_enabled && interaction_enabled, |ui| {
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("directory_tree_images")
            .auto_shrink([false, false])
            .max_height(viewport_height);

        if let Some(offset) = pending_scroll_offset {
            scroll = scroll.vertical_scroll_offset(offset);
        }

        let total_rows = state.image_rows.len();
        let current_index = state.current_index;
        let scroll_output = scroll.show_rows(ui, row_height, total_rows, |ui, row_range| {
            state.image_list_visible_row_range = Some((row_range.start, row_range.end));
            for row_index in row_range {
                let Some(row) = state.image_rows.get(row_index) else {
                    continue;
                };
                let clicked = draw_image_details_row(
                    ui,
                    row,
                    row_index,
                    row_index == current_index,
                    &column_layout,
                    &body_font,
                    state.preview_textures.get(&row_index),
                    state.preview_logical_sizes.get(&row_index).copied(),
                    command_tx,
                    list_enabled && interaction_enabled,
                    palette,
                );
                if clicked {
                    ui.memory_mut(|mem| mem.request_focus(list_focus_id));
                    state.image_list_keyboard_active = true;
                }
            }
        });
        state.image_list_scroll_offset_y = scroll_output.state.offset.y;
    });

    if state.scanning && state.image_rows.is_empty() {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), status_height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spinner();
                ui.label(egui::RichText::new(state.scan_status.as_str()).weak());
            },
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ImageListColumnLayout {
    pub(super) size_w: f32,
    pub(super) modified_w: f32,
}

const IMAGE_LIST_COL_CELL_PADDING: f32 = 4.0;
/// Fixed `YYYY/MM/DD HH:MM:SS` cell sample; all modified cells use this format.
const IMAGE_LIST_MODIFIED_CELL_SAMPLE: &str = "2000/01/01 00:00:00";

pub(super) fn measure_image_list_content_column_widths(
    painter: &egui::Painter,
    body_font: &egui::FontId,
    header_size: &str,
    header_modified: &str,
    rows: &[DirectoryTreeFileRow],
) -> (f32, f32) {
    let measure = |text: &str| {
        painter
            .layout_no_wrap(
                text.to_owned(),
                body_font.clone(),
                egui::Color32::PLACEHOLDER,
            )
            .size()
            .x
    };
    let mut size_w = measure(header_size);
    for row in rows {
        size_w = size_w.max(measure(&format_file_size(row.size_bytes)));
    }
    let modified_w = measure(header_modified)
        .max(measure(IMAGE_LIST_MODIFIED_CELL_SAMPLE))
        .max(measure("-"));
    (
        size_w + IMAGE_LIST_COL_CELL_PADDING,
        modified_w + IMAGE_LIST_COL_CELL_PADDING,
    )
}

pub(super) fn image_list_column_layout(
    row_width: f32,
    spacing_x: f32,
    ideal_size_w: f32,
    ideal_modified_w: f32,
) -> ImageListColumnLayout {
    let thumb_w = DIRECTORY_TREE_COL_THUMB_WIDTH;
    let gutters = spacing_x * 4.0;
    let ideal_fixed =
        thumb_w + ideal_size_w + ideal_modified_w + gutters + DIRECTORY_TREE_COL_NAME_MIN_WIDTH;
    if row_width >= ideal_fixed {
        return ImageListColumnLayout {
            size_w: ideal_size_w,
            modified_w: ideal_modified_w,
        };
    }

    let available_for_right_cols =
        (row_width - gutters - thumb_w - DIRECTORY_TREE_COL_NAME_MIN_WIDTH).max(0.0);
    let mut modified_w = (available_for_right_cols * 0.62).clamp(
        DIRECTORY_TREE_COL_MODIFIED_MIN_WIDTH.min(available_for_right_cols),
        ideal_modified_w,
    );
    let mut size_w = (available_for_right_cols - modified_w).clamp(0.0, ideal_size_w);
    if size_w < DIRECTORY_TREE_COL_SIZE_MIN_WIDTH && available_for_right_cols > 0.0 {
        size_w = available_for_right_cols
            .min(ideal_size_w)
            .min(DIRECTORY_TREE_COL_SIZE_MIN_WIDTH);
        modified_w = (available_for_right_cols - size_w).max(0.0);
    }
    ImageListColumnLayout { size_w, modified_w }
}

pub(super) fn image_list_thumb_column(row_rect: egui::Rect, spacing_x: f32) -> egui::Rect {
    let left = row_rect.left() + spacing_x;
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(left + DIRECTORY_TREE_COL_THUMB_WIDTH, row_rect.bottom()),
    )
}

pub(super) fn image_list_modified_column(
    row_rect: egui::Rect,
    columns: &ImageListColumnLayout,
    spacing_x: f32,
) -> egui::Rect {
    let right = row_rect.right() - spacing_x;
    let left = (right - columns.modified_w).max(row_rect.left());
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(right, row_rect.bottom()),
    )
}

pub(super) fn image_list_size_column(
    row_rect: egui::Rect,
    columns: &ImageListColumnLayout,
    spacing_x: f32,
) -> egui::Rect {
    let modified = image_list_modified_column(row_rect, columns, spacing_x);
    let right = (modified.left() - spacing_x).max(row_rect.left());
    let left = (right - columns.size_w).max(row_rect.left());
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(right, row_rect.bottom()),
    )
}

pub(super) fn image_list_name_column(
    row_rect: egui::Rect,
    columns: &ImageListColumnLayout,
    spacing_x: f32,
) -> egui::Rect {
    let thumb = image_list_thumb_column(row_rect, spacing_x);
    let size = image_list_size_column(row_rect, columns, spacing_x);
    let left = thumb.right() + spacing_x;
    let right = (size.left() - spacing_x).max(left);
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(right, row_rect.bottom()),
    )
}

fn paint_clipped_galley(
    painter: &egui::Painter,
    galley: std::sync::Arc<egui::Galley>,
    column: egui::Rect,
    color: egui::Color32,
    halign: egui::Align,
) {
    let x = match halign {
        egui::Align::RIGHT => column.right() - galley.size().x,
        egui::Align::Center => column.center().x - galley.size().x * 0.5,
        _ => column.left(),
    };
    let y = column.center().y - galley.size().y * 0.5;
    painter
        .with_clip_rect(column)
        .galley(egui::pos2(x, y), galley, color);
}

fn truncate_single_line_text(
    painter: &egui::Painter,
    text: &str,
    font: &egui::FontId,
    max_width: f32,
) -> String {
    let measure = |value: &str| {
        painter
            .layout_no_wrap(value.to_string(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    if max_width <= 0.0 {
        return String::from('…');
    }
    if measure(text) <= max_width {
        return text.to_string();
    }
    let mut lo = 0usize;
    let mut hi = text.chars().count();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let mut candidate = text.chars().take(mid).collect::<String>();
        candidate.push('…');
        if measure(&candidate) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return String::from('…');
    }
    let mut out = text.chars().take(lo).collect::<String>();
    out.push('…');
    out
}

fn draw_image_details_header(
    ui: &mut egui::Ui,
    state: &DirectoryTreeState,
    columns: &ImageListColumnLayout,
    palette: &ThemePalette,
    command_tx: &Sender<DirectoryTreeCommand>,
) {
    let header_width = ui.available_width();
    let header_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(header_width, DIRECTORY_TREE_HEADER_HEIGHT),
    );
    ui.allocate_exact_size(
        egui::vec2(header_width, DIRECTORY_TREE_HEADER_HEIGHT),
        egui::Sense::hover(),
    );
    let spacing_x = ui.spacing().item_spacing.x;
    let header_font =
        egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
    let weak = palette.text_muted;
    let sorting_enabled = image_list_sorting_available(state);

    let paint_header =
        |column: ImageListSortColumn, label: String, rect: egui::Rect, halign: egui::Align| {
            let text = format!("{}{}", label, image_list_sort_indicator(column, state));
            let galley = ui.painter().layout_no_wrap(text, header_font.clone(), weak);
            paint_clipped_galley(ui.painter(), galley, rect, weak, halign);
            if sorting_enabled {
                let response = ui.interact(
                    rect,
                    ui.id().with(("image_list_sort", column)),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    let _ = command_tx.send(DirectoryTreeCommand::SortImageList(column));
                }
            }
        };

    paint_header(
        ImageListSortColumn::Name,
        t!("directory_tree.col_name").to_string(),
        image_list_name_column(header_rect, columns, spacing_x),
        egui::Align::LEFT,
    );
    paint_header(
        ImageListSortColumn::Size,
        t!("directory_tree.col_size").to_string(),
        image_list_size_column(header_rect, columns, spacing_x),
        egui::Align::RIGHT,
    );
    paint_header(
        ImageListSortColumn::Modified,
        t!("directory_tree.col_modified").to_string(),
        image_list_modified_column(header_rect, columns, spacing_x),
        egui::Align::LEFT,
    );
    ui.separator();
}

pub(super) fn min_scroll_offset_to_show_row(
    row_index: usize,
    row_height_with_spacing: f32,
    row_height: f32,
    viewport_height: f32,
    scroll_offset_y: f32,
) -> Option<f32> {
    let row_top = row_index as f32 * row_height_with_spacing;
    let row_bottom = row_top + row_height;
    let view_top = scroll_offset_y;
    let view_bottom = scroll_offset_y + viewport_height;

    if row_top >= view_top && row_bottom <= view_bottom {
        return None;
    }
    if row_top < view_top {
        return Some(row_top);
    }
    if row_bottom > view_bottom {
        return Some(row_bottom - viewport_height);
    }
    None
}

pub(super) fn wrapped_image_list_index(current: usize, delta: i32, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let next = match delta {
        1 => (current + 1) % len,
        -1 => {
            if current == 0 {
                len - 1
            } else {
                current - 1
            }
        }
        _ => return None,
    };
    if next == current { None } else { Some(next) }
}

fn try_handle_image_list_arrow_keys(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    list_focus_id: egui::Id,
    command_tx: &Sender<DirectoryTreeCommand>,
    embedded: bool,
) {
    if !ImageViewerApp::directory_tree_list_accepts_keyboard_input(ui.ctx(), embedded) {
        return;
    }

    let list_has_focus = ui.memory(|mem| mem.has_focus(list_focus_id));
    if !(state.image_list_keyboard_active || list_has_focus)
        || state.image_rows.is_empty()
        || !image_list_interaction_enabled(state)
    {
        return;
    }

    let current = state.current_index;
    let len = state.image_rows.len();
    let mut next = None;
    ui.input(|input| {
        if input.key_pressed(egui::Key::ArrowDown) {
            next = wrapped_image_list_index(current, 1, len);
        } else if input.key_pressed(egui::Key::ArrowUp) {
            next = wrapped_image_list_index(current, -1, len);
        }
    });
    let Some(index) = next else {
        return;
    };

    ui.input_mut(|input| {
        input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
        input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
    });
    ui.memory_mut(|mem| mem.request_focus(list_focus_id));
    state.image_list_keyboard_active = true;
    state.current_index = index;
    state.scroll_image_list_to_current = true;
    let _ = command_tx.send(DirectoryTreeCommand::SelectImage(index));
}

fn draw_image_details_row(
    ui: &mut egui::Ui,
    row: &DirectoryTreeFileRow,
    row_index: usize,
    selected: bool,
    columns: &ImageListColumnLayout,
    body_font: &egui::FontId,
    texture: Option<&egui::TextureHandle>,
    logical_size: Option<(u32, u32)>,
    command_tx: &Sender<DirectoryTreeCommand>,
    list_enabled: bool,
    palette: &ThemePalette,
) -> bool {
    let row_width = ui.available_width();
    let (row_rect, response) = ui.allocate_exact_size(
        egui::vec2(row_width, DIRECTORY_TREE_IMAGE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(row_rect) {
        if selected {
            ui.painter()
                .rect_filled(row_rect, 0.0, directory_tree_row_selected_fill(palette));
        } else if response.hovered() {
            ui.painter()
                .rect_filled(row_rect, 0.0, palette.widget_hover);
        }

        let spacing_x = ui.spacing().item_spacing.x;
        let thumb_column = image_list_thumb_column(row_rect, spacing_x);
        paint_image_list_thumbnail(ui.painter(), palette, thumb_column, texture, logical_size);

        let text_color = if selected {
            directory_tree_row_selected_text(palette)
        } else {
            palette.text_normal
        };
        let size_text = format_file_size(row.size_bytes);
        let modified_text = row
            .modified_unix
            .map(format_file_modified)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| String::from("-"));

        let name_column = image_list_name_column(row_rect, columns, spacing_x);
        let size_column = image_list_size_column(row_rect, columns, spacing_x);
        let modified_column = image_list_modified_column(row_rect, columns, spacing_x);

        let name_text =
            truncate_single_line_text(ui.painter(), &row.name, &body_font, name_column.width());
        let name_galley = ui
            .painter()
            .layout_no_wrap(name_text, body_font.clone(), text_color);
        paint_clipped_galley(
            ui.painter(),
            name_galley,
            name_column,
            text_color,
            egui::Align::LEFT,
        );

        let size_galley = ui
            .painter()
            .layout_no_wrap(size_text, body_font.clone(), text_color);
        paint_clipped_galley(
            ui.painter(),
            size_galley,
            size_column,
            text_color,
            egui::Align::RIGHT,
        );

        let modified_galley =
            ui.painter()
                .layout_no_wrap(modified_text, body_font.clone(), text_color);
        paint_clipped_galley(
            ui.painter(),
            modified_galley,
            modified_column,
            text_color,
            egui::Align::LEFT,
        );
    }

    if list_enabled && response.clicked() {
        let _ = command_tx.send(DirectoryTreeCommand::SelectImage(row_index));
        return true;
    }
    response.on_hover_text(row.path.to_string_lossy());
    false
}

pub(super) fn directory_display_name(path: &Path) -> String {
    if is_places_sentinel_path(path) {
        return String::new();
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

pub(super) fn should_expand_this_pc_for_path(
    selected: &Path,
    known_folders: &[KnownFolderEntry],
) -> bool {
    if is_unc_path(selected) {
        return false;
    }
    if known_folders.iter().any(|entry| {
        selected == entry.filesystem_path.as_path() || selected.starts_with(&entry.filesystem_path)
    }) {
        return false;
    }
    let Some(root) = volume_root_for_path(selected) else {
        return false;
    };
    #[cfg(windows)]
    {
        let _ = root;
        return true;
    }
    #[cfg(not(windows))]
    {
        root.components().count() > 1 || root.as_os_str() == "/"
    }
}

pub(super) fn filesystem_ancestor_chain(target: &Path) -> Vec<PathBuf> {
    if let Some(root) = volume_root_for_path(target) {
        if target == root.as_path() {
            return vec![root];
        }
        let mut chain = vec![root.clone()];
        if let Ok(relative) = target.strip_prefix(&root) {
            let mut current = root;
            for component in relative.components() {
                current.push(component);
                chain.push(current.clone());
            }
        } else {
            chain.push(target.to_path_buf());
        }
        return chain;
    }

    let mut chain = vec![target.to_path_buf()];
    let mut current = target.to_path_buf();
    while current.pop() {
        chain.push(current.clone());
    }
    chain.reverse();
    chain
}

fn volume_root_for_path(path: &Path) -> Option<PathBuf> {
    if let Some(share_root) = unc_share_root(path) {
        return Some(share_root);
    }

    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        let bytes = text.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return Some(PathBuf::from(format!("{}:\\", bytes[0] as char)));
        }
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(rest) = path.strip_prefix("/Volumes") {
            if let Some(name) = rest.components().next() {
                return Some(PathBuf::from("/Volumes").join(name.as_os_str()));
            }
        }
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        for prefix in ["/media", "/mnt"] {
            if let Ok(rest) = path.strip_prefix(prefix) {
                if let Some(name) = rest.components().next() {
                    return Some(PathBuf::from(prefix).join(name.as_os_str()));
                }
            }
        }
        if path.has_root() {
            return Some(PathBuf::from("/"));
        }
        return None;
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    path.parent().map(|parent| parent.to_path_buf())
}

pub(super) fn unc_share_root(path: &Path) -> Option<PathBuf> {
    if !is_unc_path(path) {
        return None;
    }
    let text = path.to_string_lossy();
    let trimmed = text.trim_start_matches(r"\\").trim_start_matches("//");
    let mut parts = trimmed.split(['\\', '/']).filter(|part| !part.is_empty());
    let server = parts.next()?;
    let share = parts.next()?;
    Some(PathBuf::from(format!(r"\\{server}\{share}")))
}

pub(super) fn unc_share_display_name(share_root: &Path) -> String {
    let text = share_root.to_string_lossy();
    text.trim_start_matches(r"\\")
        .trim_start_matches("//")
        .to_string()
}

pub(super) fn directory_ancestor_chain(root: &Path, target: &Path) -> Vec<PathBuf> {
    if target == root {
        return vec![root.to_path_buf()];
    }
    if !target.starts_with(root) {
        return vec![target.to_path_buf()];
    }

    let mut chain = vec![root.to_path_buf()];
    if let Ok(relative) = target.strip_prefix(root) {
        let mut current = root.to_path_buf();
        for component in relative.components() {
            current.push(component);
            chain.push(current.clone());
        }
    }
    chain
}
