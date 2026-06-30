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

// Directory tree navigation UI drawing.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use eframe::egui;
use rust_i18n::t;

use crate::app::ImageViewerApp;
use crate::directory_tree_places::KnownFolderEntry;
use crate::directory_tree_places::types::KnownFolderKind;
use crate::loader::preview_aspect_matches_logical;
use crate::path_location::is_unc_path;
use crate::theme::ThemePalette;
use crate::ui::osd::FORMAT_FILE_SIZE_WIDTH_SAMPLES;

use super::view::{DirectoryTreeUiChrome, DirectoryTreeView};
use super::{
    DIRECTORY_TREE_COL_MODIFIED_MIN_WIDTH, DIRECTORY_TREE_COL_NAME_MIN_WIDTH,
    DIRECTORY_TREE_COL_SIZE_MIN_WIDTH, DIRECTORY_TREE_DOWNLOADS_TRAY_HEIGHT_RATIO,
    DIRECTORY_TREE_EXPAND_ICON_WIDTH, DIRECTORY_TREE_FOLDER_ICON_WIDTH,
    DIRECTORY_TREE_HEADER_HEIGHT, DIRECTORY_TREE_IMAGE_ROW_HEIGHT_COMPACT, DIRECTORY_TREE_INDENT,
    DIRECTORY_TREE_LEFT_MIN_WIDTH, DIRECTORY_TREE_NODE_ICON_DRAW_RATIO,
    DIRECTORY_TREE_RIGHT_MIN_WIDTH, DIRECTORY_TREE_ROW_HEIGHT, DIRECTORY_TREE_SPLITTER_GRAB_WIDTH,
    DIRECTORY_TREE_UI_STROKE_WIDTH, DirectoryTreeCommand, DirectoryTreeFileRow,
    DirectoryTreeListState, DirectoryTreeNode, ImageListSortColumn, is_network_namespace_path,
    is_places_sentinel_namespace_path, is_this_pc_namespace_path, network_namespace_path,
    send_directory_tree_command, this_pc_namespace_path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectoryTreeNodeIcon {
    Folder,
    ThisPc,
    Network,
    Drive,
    KnownFolder(KnownFolderKind),
}

pub(super) fn directory_tree_node_icon_fields(
    known_folders: &[KnownFolderEntry],
    nodes: &impl DirectoryTreeNodeLookup,
    path: &Path,
) -> DirectoryTreeNodeIcon {
    if is_this_pc_namespace_path(path) {
        return DirectoryTreeNodeIcon::ThisPc;
    }
    if is_network_namespace_path(path) {
        return DirectoryTreeNodeIcon::Network;
    }
    if let Some(kind) = known_folders
        .iter()
        .find(|entry| entry.namespace_path == path)
        .map(|entry| entry.kind)
    {
        return DirectoryTreeNodeIcon::KnownFolder(kind);
    }
    if super::namespace::is_mount_namespace_path(path) {
        return DirectoryTreeNodeIcon::Drive;
    }
    if super::namespace::is_network_share_namespace_path(path) {
        return DirectoryTreeNodeIcon::Network;
    }
    if nodes
        .get_node(&this_pc_namespace_path())
        .is_some_and(|node| node.children.iter().any(|child| child.as_path() == path))
    {
        return DirectoryTreeNodeIcon::Drive;
    }
    DirectoryTreeNodeIcon::Folder
}

#[derive(Default)]
struct DirectoryTreePanelLayoutDiag {
    last_embedded_width: Option<f32>,
    last_layout_left: Option<f32>,
    last_layout_list: Option<f32>,
    last_log_at: Option<Instant>,
}

static DIRECTORY_TREE_PANEL_LAYOUT_DIAG: OnceLock<Mutex<DirectoryTreePanelLayoutDiag>> =
    OnceLock::new();

fn maybe_log_directory_tree_panel_layout(
    embedded: bool,
    viewport_width: f32,
    layout_left_w: f32,
    layout_list_w: f32,
    stored_left_before: f32,
    stored_left_after: f32,
    splitter_dragged: bool,
    splitter_drag_delta_x: f32,
) {
    if !embedded {
        return;
    }

    const WIDTH_CHANGE_EPS: f32 = 2.0;
    const LEFT_CLAMP_EPS: f32 = 0.5;
    const IDLE_LOG_INTERVAL: Duration = Duration::from_millis(1000);
    const DRAG_LOG_INTERVAL: Duration = Duration::from_millis(250);

    let diag = DIRECTORY_TREE_PANEL_LAYOUT_DIAG
        .get_or_init(|| Mutex::new(DirectoryTreePanelLayoutDiag::default()));
    let Ok(mut diag) = diag.try_lock() else {
        return;
    };

    let now = Instant::now();
    let width_delta = diag
        .last_embedded_width
        .map(|prev| viewport_width - prev)
        .unwrap_or(0.0);
    let layout_left_delta = diag
        .last_layout_left
        .map(|prev| layout_left_w - prev)
        .unwrap_or(0.0);
    let layout_list_delta = diag
        .last_layout_list
        .map(|prev| layout_list_w - prev)
        .unwrap_or(0.0);
    let left_clamped = (stored_left_before - layout_left_w).abs() > LEFT_CLAMP_EPS;
    let width_changed = width_delta.abs() >= WIDTH_CHANGE_EPS;
    let interval = if splitter_dragged {
        DRAG_LOG_INTERVAL
    } else {
        IDLE_LOG_INTERVAL
    };
    let interval_elapsed = diag
        .last_log_at
        .map_or(true, |last| now.saturating_duration_since(last) >= interval);

    if interval_elapsed && (splitter_dragged || width_changed || left_clamped) {
        log::debug!(
            "[DirectoryTree][PanelDiag] embedded_w={:.1} d_w={:+.1} layout_left={:.1} d_left={:+.1} \
             layout_list={:.1} d_list={:+.1} stored_left={:.1}->{:.1} dragged={} drag_dx={:+.1} clamped={}",
            viewport_width,
            width_delta,
            layout_left_w,
            layout_left_delta,
            layout_list_w,
            layout_list_delta,
            stored_left_before,
            stored_left_after,
            splitter_dragged,
            splitter_drag_delta_x,
            left_clamped
        );
        diag.last_log_at = Some(now);
    }

    diag.last_embedded_width = Some(viewport_width);
    diag.last_layout_left = Some(layout_left_w);
    diag.last_layout_list = Some(layout_list_w);
}

pub(super) trait DirectoryTreeNodeLookup {
    fn get_node(&self, path: &Path) -> Option<&DirectoryTreeNode>;
}

impl DirectoryTreeNodeLookup for super::node_store::DirectoryTreeNodeArena {
    fn get_node(&self, path: &Path) -> Option<&DirectoryTreeNode> {
        self.get(path)
    }
}

impl DirectoryTreeNodeLookup
    for std::collections::HashMap<PathBuf, std::sync::Arc<DirectoryTreeNode>>
{
    fn get_node(&self, path: &Path) -> Option<&DirectoryTreeNode> {
        self.get(path).map(|node| node.as_ref())
    }
}

fn directory_tree_node_expandable(node: &DirectoryTreeNode, path: &Path) -> bool {
    if is_places_sentinel_namespace_path(path) {
        return true;
    }
    node.loading || !node.children_loaded || !node.children.is_empty()
}

fn paint_tree_expand_chevron(ui: &mut egui::Ui, expanded: bool, response: &egui::Response) {
    let stroke = egui::Stroke::new(
        DIRECTORY_TREE_UI_STROKE_WIDTH,
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
    let size = rect.width().min(rect.height()) * DIRECTORY_TREE_NODE_ICON_DRAW_RATIO;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
    let painter = ui.painter();
    let stroke = egui::Stroke::new(
        DIRECTORY_TREE_UI_STROKE_WIDTH,
        palette.text_normal.gamma_multiply(0.88),
    );
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
                    egui::vec2(
                        icon_rect.width() * 0.82,
                        icon_rect.height() * super::DIRECTORY_TREE_DOWNLOADS_TRAY_HEIGHT_RATIO,
                    ),
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
                    egui::vec2(
                        icon_rect.width() * DIRECTORY_TREE_NODE_ICON_DRAW_RATIO,
                        icon_rect.height() * DIRECTORY_TREE_DOWNLOADS_TRAY_HEIGHT_RATIO,
                    ),
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
    egui::Color32::from_rgba_unmultiplied(
        palette.accent2.r(),
        palette.accent2.g(),
        palette.accent2.b(),
        if palette.is_dark { 40 } else { 30 },
    )
}

fn directory_tree_row_selected_text(palette: &ThemePalette) -> egui::Color32 {
    // Fill already uses accent tint; keep body text readable on both themes.
    palette.text_normal
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

fn active_list_thumb_px(view: &DirectoryTreeView) -> f32 {
    if view.show_list_previews() {
        view.list_preview_thumb_px()
    } else {
        0.0
    }
}

pub(super) fn image_list_row_height(view: &DirectoryTreeView) -> f32 {
    if view.show_list_previews() {
        view.list_preview_thumb_px()
    } else {
        DIRECTORY_TREE_IMAGE_ROW_HEIGHT_COMPACT
    }
}

pub(super) fn image_list_interaction_enabled(list: &DirectoryTreeListState) -> bool {
    !list.scanning && !list.image_list_reordering
}

pub(super) fn image_list_interaction_enabled_view(view: &DirectoryTreeView) -> bool {
    !view.scanning() && !view.image_list_reordering()
}

pub(super) fn image_list_sorting_available_view(view: &DirectoryTreeView) -> bool {
    image_list_interaction_enabled_view(view) && !view.image_rows().is_empty()
}
pub(super) fn image_list_sorting_available(list: &DirectoryTreeListState) -> bool {
    image_list_interaction_enabled(list) && !list.image_rows.is_empty()
}

pub(super) struct DirectoryTreeDrawParams<'a> {
    pub(super) view: &'a DirectoryTreeView,
    pub(super) chrome: &'a mut DirectoryTreeUiChrome,
    pub(super) command_tx: &'a Sender<DirectoryTreeCommand>,
    pub(super) root_wake: Option<&'a crate::app::RootRedrawWake>,
    pub(super) palette: &'a ThemePalette,
    pub(super) embedded: bool,
    pub(super) allow_image_context_menu: bool,
}

struct DirectoryTreeNodeParams<'a> {
    view: &'a DirectoryTreeView,
    chrome: &'a mut DirectoryTreeUiChrome,
    command_tx: &'a Sender<DirectoryTreeCommand>,
    root_wake: Option<&'a crate::app::RootRedrawWake>,
    palette: &'a ThemePalette,
}

struct ImageDetailsRowParams<'a> {
    row: &'a DirectoryTreeFileRow,
    row_index: usize,
    selected: bool,
    columns: &'a ImageListColumnLayout,
    body_font: &'a egui::FontId,
    thumb_px: f32,
    row_height: f32,
    texture: Option<&'a egui::TextureHandle>,
    logical_size: Option<(u32, u32)>,
    command_tx: &'a Sender<DirectoryTreeCommand>,
    chrome: &'a mut DirectoryTreeUiChrome,
    list_enabled: bool,
    allow_image_context_menu: bool,
    palette: &'a ThemePalette,
}

pub(super) fn draw_directory_tree_window(
    ui: &mut egui::Ui,
    mut params: DirectoryTreeDrawParams<'_>,
) {
    ui.visuals_mut().button_frame = false;
    ui.visuals_mut().override_text_color = Some(params.palette.text_normal);
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, params.palette.panel_bg);
    draw_directory_tree_top_panels(
        ui,
        &mut params,
        egui::vec2(ui.available_width(), ui.available_height()),
    );
    if params.embedded {
        // Detached nav lives in a separate viewport; its ui.max_rect() is viewport-local
        // (0,0)-based and must not be stored in shared ctx temp data or it falsely blocks
        // main-window wheel zoom/navigation for most pointer positions.
        publish_directory_tree_nav_wheel_block_rect(ui);
    }
}

pub(super) fn publish_directory_tree_nav_wheel_block_rect(ui: &egui::Ui) {
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            egui::Id::new(super::DIRECTORY_TREE_NAV_WHEEL_BLOCK_RECT_ID),
            ui.max_rect(),
        );
    });
}

pub(super) fn pointer_in_directory_tree_nav_block_rect(
    pointer: Option<egui::Pos2>,
    block_rect: Option<egui::Rect>,
) -> bool {
    match (pointer, block_rect) {
        (Some(pos), Some(rect)) => rect.contains(pos),
        _ => false,
    }
}

pub(super) fn directory_tree_left_panel_width_limits(viewport_width: f32) -> (f32, f32) {
    let viewport_width = viewport_width.max(0.0);
    let available = (viewport_width - DIRECTORY_TREE_SPLITTER_GRAB_WIDTH).max(0.0);
    let max_left = (available - DIRECTORY_TREE_RIGHT_MIN_WIDTH).max(0.0);
    let min_left = DIRECTORY_TREE_LEFT_MIN_WIDTH.min(max_left);
    (min_left, max_left.max(min_left))
}

pub(super) fn clamp_directory_tree_left_panel_width(width: f32, viewport_width: f32) -> f32 {
    let (min_left, max_left) = directory_tree_left_panel_width_limits(viewport_width);
    width.clamp(min_left, max_left)
}

pub(super) fn directory_tree_panel_layout(
    left_panel_width: f32,
    _image_list_panel_width: f32,
    viewport_width: f32,
) -> (f32, f32) {
    let splitter_w = DIRECTORY_TREE_SPLITTER_GRAB_WIDTH;
    let min_list = DIRECTORY_TREE_RIGHT_MIN_WIDTH;
    let min_left = DIRECTORY_TREE_LEFT_MIN_WIDTH;

    if viewport_width <= splitter_w {
        return (0.0, 0.0);
    }

    let available = viewport_width - splitter_w;
    // Keep the splitter position (left width) fixed; the file list absorbs viewport resize.
    // Only shrink the folder tree when the list would drop below its minimum width.
    let max_left = (available - min_list).max(0.0);
    let mut left_w = left_panel_width.clamp(min_left, max_left);
    let mut list_w = available - left_w;

    if list_w < min_list {
        list_w = min_list;
        left_w = (available - min_list).clamp(min_left, max_left);
    }

    (left_w, list_w)
}

fn draw_directory_tree_top_panels(
    ui: &mut egui::Ui,
    params: &mut DirectoryTreeDrawParams<'_>,
    panel_size: egui::Vec2,
) {
    let viewport_height = panel_size.y;
    let viewport_width = panel_size.x;
    let (left_w, list_w) = directory_tree_panel_layout(
        params.chrome.left_panel_width,
        params.view.image_list_panel_width(),
        viewport_width,
    );
    let view = params.view;
    let command_tx = params.command_tx;
    let root_wake = params.root_wake;
    let palette = params.palette;
    let embedded = params.embedded;
    let allow_image_context_menu = params.allow_image_context_menu;
    let chrome = &mut *params.chrome;
    let splitter_w = DIRECTORY_TREE_SPLITTER_GRAB_WIDTH;
    let right_w = list_w;
    let stored_left_before = chrome.left_panel_width;

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
        draw_folder_panel(ui, view, chrome, command_tx, root_wake, palette);
    });

    ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
        ui.set_clip_rect(right_rect);
        ui.set_width(right_w);
        draw_image_file_list(
            ui,
            view,
            chrome,
            command_tx,
            palette,
            embedded,
            allow_image_context_menu,
        );
    });

    let splitter_id = ui.id().with("directory_tree_splitter");
    let splitter_response = ui.interact(splitter_rect, splitter_id, egui::Sense::drag());
    let mut splitter_drag_delta_x = 0.0;
    if splitter_response.dragged() {
        splitter_drag_delta_x = splitter_response.drag_delta().x;
        chrome.left_panel_width = clamp_directory_tree_left_panel_width(
            chrome.left_panel_width + splitter_drag_delta_x,
            viewport_width,
        );
        chrome.panel_layout_dirty = true;
        ui.ctx().request_repaint();
    }
    maybe_log_directory_tree_panel_layout(
        embedded,
        viewport_width,
        left_w,
        right_w,
        stored_left_before,
        chrome.left_panel_width,
        splitter_response.dragged(),
        splitter_drag_delta_x,
    );
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
        chrome.embedded_nav_panel_width = Some(viewport_width);
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

/// Places-loading / worker-unavailable status shared by embedded and detached panels.
pub(super) fn draw_directory_tree_places_status(ui: &mut egui::Ui, view: &DirectoryTreeView) {
    if view.places_loaded() {
        return;
    }
    if view.places_loading() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(t!("directory_tree.places_loading"));
        });
    } else if let Some(err) = view.places_load_error() {
        ui.label(egui::RichText::new(err).color(ui.visuals().error_fg_color));
    } else if !view.workers_available() {
        ui.label(t!("directory_tree.workers_unavailable"));
    }
}

fn draw_folder_panel(
    ui: &mut egui::Ui,
    view: &DirectoryTreeView,
    chrome: &mut DirectoryTreeUiChrome,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
) {
    let scroll_height = ui.available_height();
    chrome.folder_selected_row_rect = None;

    let mut scroll = egui::ScrollArea::vertical()
        .id_salt("directory_tree_folders")
        .auto_shrink([false, false])
        .max_height(scroll_height);
    if !chrome.scroll_folder_tree_to_selected {
        scroll = scroll.vertical_scroll_offset(chrome.folder_scroll_offset_y);
    }
    let scroll_output = scroll.show(ui, |ui| {
            if !view.places_loaded() {
                draw_directory_tree_places_status(ui, view);
                return;
            }
            for entry in view.known_folders() {
                draw_directory_node(
                    ui,
                    DirectoryTreeNodeParams {
                        view,
                        chrome: &mut *chrome,
                        command_tx,
                        root_wake,
                        palette,
                    },
                    &entry.namespace_path,
                    0,
                );
            }
            draw_directory_node(
                ui,
                DirectoryTreeNodeParams {
                    view,
                    chrome: &mut *chrome,
                    command_tx,
                    root_wake,
                    palette,
                },
                &this_pc_namespace_path(),
                0,
            );
            if view.network_visible() {
                draw_directory_node(
                    ui,
                    DirectoryTreeNodeParams {
                        view,
                        chrome: &mut *chrome,
                        command_tx,
                        root_wake,
                        palette,
                    },
                    &network_namespace_path(),
                    0,
                );
            }
            if chrome.scroll_folder_tree_to_selected {
                let miss_id = egui::Id::new("directory_tree_folders")
                    .with("scroll_to_selected_miss");
                if let Some(rect) = chrome.folder_selected_row_rect {
                    ui.ctx().data_mut(|d| d.remove_temp::<u32>(miss_id));
                    ui.scroll_to_rect(rect, None);
                    let clip = ui.clip_rect();
                    if rect.min.y >= clip.min.y && rect.max.y <= clip.max.y {
                        chrome.scroll_folder_tree_to_selected = false;
                    } else {
                        ui.ctx().request_repaint();
                    }
                } else if view.places_loaded() {
                    let misses = ui.ctx().data_mut(|d| {
                        let entry = d.get_temp_mut_or_insert_with(miss_id, || 0u32);
                        *entry = entry.saturating_add(1);
                        *entry
                    });
                    if misses >= super::DIRECTORY_TREE_SYNC_MAX_DEFER_FRAMES {
                        log::debug!(
                            "[DirectoryTree] Abandoning scroll-to-selected after {} frames without row rect",
                            misses
                        );
                        ui.ctx().data_mut(|d| d.remove_temp::<u32>(miss_id));
                        chrome.scroll_folder_tree_to_selected = false;
                    } else {
                        ui.ctx().request_repaint();
                    }
                } else {
                    // Places still loading; keep flag until apply_directory_tree_places re-reveals.
                    ui.ctx().request_repaint();
                }
            }
        });
    chrome.folder_scroll_offset_y = scroll_output.state.offset.y;
}

fn draw_directory_node(
    ui: &mut egui::Ui,
    params: DirectoryTreeNodeParams<'_>,
    path: &Path,
    depth: usize,
) {
    let DirectoryTreeNodeParams {
        view,
        chrome,
        command_tx,
        root_wake,
        palette,
    } = params;
    let Some(node) = view.nodes().get(path) else {
        return;
    };
    let node = node.as_ref();

    let icon = directory_tree_node_icon_fields(view.known_folders(), view.nodes(), path);
    let expandable = directory_tree_node_expandable(node, path);
    let selected = view
        .selected_namespace_path()
        .is_some_and(|selected| selected.as_os_str() == path.as_os_str());

    let row_width = ui.available_width();
    let row_response = ui.allocate_ui_with_layout(
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
                    send_directory_tree_command(
                        command_tx,
                        DirectoryTreeCommand::ToggleExpanded(path.to_path_buf()),
                    );
                }
            } else {
                ui.add_space(DIRECTORY_TREE_EXPAND_ICON_WIDTH);
            }

            let folder_rect = ui.allocate_exact_size(
                egui::vec2(DIRECTORY_TREE_FOLDER_ICON_WIDTH, DIRECTORY_TREE_ROW_HEIGHT),
                egui::Sense::hover(),
            );
            paint_tree_folder_icon(ui, folder_rect.0, icon, palette);

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
            let mut name_response = name_response;
            if name_response.hovered() {
                let hover_text = node.fs_path.to_string_lossy().into_owned();
                name_response = name_response.on_hover_ui(move |ui| {
                    ui.label(hover_text);
                });
            }
            if name_response.clicked() {
                if is_places_sentinel_namespace_path(path) {
                    send_directory_tree_command(
                        command_tx,
                        DirectoryTreeCommand::ToggleExpanded(path.to_path_buf()),
                    );
                } else {
                    let fs_path = node.fs_path.clone();
                    send_directory_tree_command(
                        command_tx,
                        DirectoryTreeCommand::SelectDirectory {
                            namespace_path: path.to_path_buf(),
                            fs_path,
                        },
                    );
                }
                if let Some(wake) = root_wake {
                    wake();
                }
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                ui.ctx().request_repaint();
            }
        },
    );
    if selected {
        chrome.folder_selected_row_rect = Some(row_response.response.rect);
    }

    if let Some(error) = node.error.as_deref() {
        ui.horizontal(|ui| {
            ui.add_space((depth + 1) as f32 * DIRECTORY_TREE_INDENT);
            ui.label(egui::RichText::new(error).color(ui.visuals().error_fg_color));
        });
    }

    if node.expanded {
        for child in &node.children {
            draw_directory_node(
                ui,
                DirectoryTreeNodeParams {
                    view,
                    chrome: &mut *chrome,
                    command_tx,
                    root_wake,
                    palette,
                },
                child,
                depth + 1,
            );
        }
    }
}

fn draw_image_file_list(
    ui: &mut egui::Ui,
    view: &DirectoryTreeView,
    chrome: &mut DirectoryTreeUiChrome,
    command_tx: &Sender<DirectoryTreeCommand>,
    palette: &ThemePalette,
    embedded: bool,
    allow_image_context_menu: bool,
) {
    chrome.begin_image_list_paint();
    let list_enabled = !view.scanning() || !view.image_rows().is_empty();

    if view.image_rows().is_empty() {
        if view.scanning() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(view.scan_status()).weak());
            });
        } else {
            ui.label(egui::RichText::new(t!("directory_tree.no_images")).weak());
            if let Some(warning) = view.sync_warning() {
                ui.label(egui::RichText::new(warning).weak());
            }
        }
        return;
    }

    let show_sync_warning = view.sync_warning().is_some();
    let status_height = if show_sync_warning {
        DIRECTORY_TREE_ROW_HEIGHT
    } else {
        0.0
    };
    let thumb_px = active_list_thumb_px(view);
    let row_height = image_list_row_height(view);
    let row_spacing = ui.spacing().item_spacing.y;
    let row_height_with_spacing = row_height + row_spacing;
    let body_font = egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
    let column_layout = image_list_column_layout(
        ui.available_width(),
        ui.spacing().item_spacing.x,
        view.image_list_col_size_w(),
        view.image_list_col_modified_w(),
        thumb_px,
    );

    draw_image_details_header(ui, view, &column_layout, palette, command_tx);

    let interaction_enabled = image_list_interaction_enabled_view(view);
    let viewport_height = (ui.available_height() - status_height).max(row_height_with_spacing);

    if interaction_enabled {
        try_handle_image_list_arrow_keys(ui, view, chrome, command_tx, embedded);
    }

    let mut pending_scroll_offset = None;
    if list_enabled && chrome.scroll_image_list_to_current && !view.image_rows().is_empty() {
        pending_scroll_offset = min_scroll_offset_to_show_row(
            chrome.current_index,
            row_height_with_spacing,
            row_height,
            viewport_height,
            chrome.image_list_scroll_offset_y,
        )
        .map(|offset| offset.max(0.0));
        chrome.scroll_image_list_to_current = false;
    }

    ui.add_enabled_ui(list_enabled && interaction_enabled, |ui| {
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("directory_tree_images")
            .auto_shrink([false, false])
            .max_height(viewport_height);

        if let Some(offset) = pending_scroll_offset {
            scroll = scroll.vertical_scroll_offset(offset);
        }

        let total_rows = view.image_rows().len();
        let current_index = chrome.current_index;
        let scroll_output = scroll.show_rows(ui, row_height, total_rows, |ui, row_range| {
            chrome.image_list_visible_row_range = Some((row_range.start, row_range.end));
            for row_index in row_range {
                let Some(row) = view.image_rows().get(row_index) else {
                    continue;
                };
                let clicked = draw_image_details_row(
                    ui,
                    ImageDetailsRowParams {
                        row,
                        row_index,
                        selected: row_index == current_index,
                        columns: &column_layout,
                        body_font: &body_font,
                        thumb_px,
                        row_height,
                        texture: view.preview_textures().get(&row_index),
                        logical_size: view.preview_logical_sizes().get(&row_index).copied(),
                        command_tx,
                        chrome: &mut *chrome,
                        list_enabled: list_enabled && interaction_enabled,
                        allow_image_context_menu,
                        palette,
                    },
                );
                if clicked {
                    chrome.image_list_keyboard_active = true;
                }
            }
        });
        chrome.image_list_scroll_offset_y = scroll_output.state.offset.y;
    });

    if show_sync_warning {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), status_height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if let Some(warning) = view.sync_warning() {
                    ui.label(egui::RichText::new(warning).weak());
                }
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
    ctx: &egui::Context,
    body_font: &egui::FontId,
    header_size: &str,
    header_modified: &str,
) -> (f32, f32) {
    let measure = |text: &str| {
        ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    body_font.clone(),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
        })
    };
    let mut size_w = measure(header_size);
    for sample in FORMAT_FILE_SIZE_WIDTH_SAMPLES {
        size_w = size_w.max(measure(sample));
    }
    let modified_w = measure(header_modified)
        .max(measure(IMAGE_LIST_MODIFIED_CELL_SAMPLE))
        .max(measure(&t!("directory_tree.modified_unknown")));
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
    thumb_px: f32,
) -> ImageListColumnLayout {
    let thumb_w = thumb_px.max(0.0);
    let gutters = spacing_x * if thumb_w > 0.0 { 4.0 } else { 3.0 };
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
    let mut modified_w =
        (available_for_right_cols * super::DIRECTORY_TREE_IMAGE_LIST_MODIFIED_COL_WEIGHT).clamp(
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

pub(super) fn image_list_thumb_column(
    row_rect: egui::Rect,
    spacing_x: f32,
    thumb_px: f32,
) -> egui::Rect {
    let left = row_rect.left() + spacing_x;
    let width = thumb_px.max(0.0);
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(left + width, row_rect.bottom()),
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
    thumb_px: f32,
) -> egui::Rect {
    let thumb = image_list_thumb_column(row_rect, spacing_x, thumb_px);
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
        let mid = (lo + hi).div_ceil(2);
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
    view: &DirectoryTreeView,
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
    let sorting_enabled = image_list_sorting_available_view(view);

    let paint_header =
        |column: ImageListSortColumn, label: String, rect: egui::Rect, halign: egui::Align| {
            let text = format!(
                "{}{}",
                label,
                super::sort::image_list_sort_indicator_fields(
                    column,
                    view.image_list_sort_active(),
                    view.image_list_sort_column(),
                    view.image_list_sort_ascending()
                )
            );
            let galley = ui.painter().layout_no_wrap(text, header_font.clone(), weak);
            paint_clipped_galley(ui.painter(), galley, rect, weak, halign);
            if sorting_enabled {
                let response = ui.interact(
                    rect,
                    ui.id().with(("image_list_sort", column)),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    send_directory_tree_command(
                        command_tx,
                        DirectoryTreeCommand::SortImageList(column),
                    );
                }
            }
        };

    paint_header(
        ImageListSortColumn::Name,
        t!("directory_tree.col_name").to_string(),
        image_list_name_column(header_rect, columns, spacing_x, active_list_thumb_px(view)),
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
    view: &DirectoryTreeView,
    chrome: &mut DirectoryTreeUiChrome,
    command_tx: &Sender<DirectoryTreeCommand>,
    embedded: bool,
) {
    if !ImageViewerApp::directory_tree_list_accepts_keyboard_input(ui.ctx(), embedded) {
        return;
    }

    if !chrome.image_list_keyboard_active
        || view.image_rows().is_empty()
        || !image_list_interaction_enabled_view(view)
    {
        return;
    }

    let current = chrome.current_index;
    let len = view.image_rows().len();
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
    chrome.image_list_keyboard_active = true;
    chrome.current_index = index;
    chrome.scroll_image_list_to_current = true;
    send_directory_tree_command(command_tx, DirectoryTreeCommand::SelectImage(index));
}

fn draw_image_details_row(ui: &mut egui::Ui, params: ImageDetailsRowParams<'_>) -> bool {
    let ImageDetailsRowParams {
        row,
        row_index,
        selected,
        columns,
        body_font,
        thumb_px,
        row_height,
        texture,
        logical_size,
        command_tx,
        chrome,
        list_enabled,
        allow_image_context_menu,
        palette,
    } = params;
    let row_width = ui.available_width();
    let (row_rect, response) =
        ui.allocate_exact_size(egui::vec2(row_width, row_height), egui::Sense::click());
    if ui.is_rect_visible(row_rect) {
        if selected {
            ui.painter()
                .rect_filled(row_rect, 0.0, directory_tree_row_selected_fill(palette));
        } else if response.hovered() {
            ui.painter()
                .rect_filled(row_rect, 0.0, palette.widget_hover);
        }

        let spacing_x = ui.spacing().item_spacing.x;
        if thumb_px > 0.0 {
            let thumb_column = image_list_thumb_column(row_rect, spacing_x, thumb_px);
            paint_image_list_thumbnail(ui.painter(), palette, thumb_column, texture, logical_size);
        }

        let text_color = if selected {
            directory_tree_row_selected_text(palette)
        } else {
            palette.text_normal
        };

        let name_column = image_list_name_column(row_rect, columns, spacing_x, thumb_px);
        let size_column = image_list_size_column(row_rect, columns, spacing_x);
        let modified_column = image_list_modified_column(row_rect, columns, spacing_x);

        let name_text =
            truncate_single_line_text(ui.painter(), &row.name, body_font, name_column.width());
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

        let size_galley =
            ui.painter()
                .layout_no_wrap(row.size_text.clone(), body_font.clone(), text_color);
        paint_clipped_galley(
            ui.painter(),
            size_galley,
            size_column,
            text_color,
            egui::Align::RIGHT,
        );

        let modified_galley =
            ui.painter()
                .layout_no_wrap(row.modified_text.clone(), body_font.clone(), text_color);
        paint_clipped_galley(
            ui.painter(),
            modified_galley,
            modified_column,
            text_color,
            egui::Align::LEFT,
        );
    }

    if list_enabled && response.double_clicked() {
        send_directory_tree_command(
            command_tx,
            DirectoryTreeCommand::SelectImageAndHideNav(row_index),
        );
        return true;
    }
    if list_enabled && response.clicked() {
        send_directory_tree_command(command_tx, DirectoryTreeCommand::SelectImage(row_index));
        return true;
    }
    if selected {
        chrome.image_list_selected_row_rect = Some(row_rect);
        if allow_image_context_menu
            && list_enabled
            && response.secondary_clicked()
            && let Some(pos) = response
                .interact_pointer_pos()
                .or_else(|| ui.input(|input| input.pointer.interact_pos()))
        {
            chrome.pending_image_context_menu = Some((pos, ui.ctx().viewport_id()));
            chrome.image_list_keyboard_active = true;
        }
    }
    if response.hovered() {
        response.on_hover_text(row.path.to_string_lossy());
    }
    false
}

pub(super) fn directory_display_name(path: &Path) -> String {
    if is_places_sentinel_namespace_path(path) {
        return String::new();
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Whether reveal should expand **Places** for a shell namespace selection (mount subtree).
pub(super) fn should_expand_this_pc_for_namespace_path(
    tree: &Path,
    known_folders: &[KnownFolderEntry],
) -> bool {
    if is_places_sentinel_namespace_path(tree)
        || super::namespace::is_network_share_namespace_path(tree)
    {
        return false;
    }
    if known_folders.iter().any(|entry| {
        tree == entry.namespace_path.as_path() || tree.starts_with(&entry.namespace_path)
    }) {
        return false;
    }
    super::namespace::is_mount_namespace_path(tree)
}

#[allow(dead_code)]
pub(super) fn filesystem_ancestor_chain(target: &Path) -> Vec<PathBuf> {
    filesystem_ancestor_chain_limited(target, usize::MAX)
}

pub(super) fn filesystem_ancestor_chain_limited(target: &Path, max_depth: usize) -> Vec<PathBuf> {
    if let Some(root) = volume_root_for_path(target) {
        if target == root.as_path() {
            return vec![root];
        }
        let mut chain = vec![root.clone()];
        if let Ok(relative) = target.strip_prefix(&root) {
            let mut current = root;
            for component in relative.components() {
                if chain.len() >= max_depth {
                    break;
                }
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
        if chain.len() >= max_depth {
            break;
        }
        chain.push(current.clone());
    }
    chain.reverse();
    chain
}

fn volume_root_for_path(path: &Path) -> Option<PathBuf> {
    if let Some(share_root) = unc_share_root(path) {
        return Some(share_root);
    }

    #[cfg(target_os = "windows")]
    {
        let text = path.to_string_lossy();
        let bytes = text.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return Some(PathBuf::from(format!("{}:\\", bytes[0] as char)));
        }
        None
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
        if let Ok(rest) = path.strip_prefix("/run/media") {
            if let Some(first) = rest.components().next() {
                return Some(PathBuf::from("/run/media").join(first.as_os_str()));
            }
        }
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

#[allow(dead_code)]
pub(super) fn directory_ancestor_chain(root: &Path, target: &Path) -> Vec<PathBuf> {
    directory_ancestor_chain_limited(root, target, usize::MAX)
}

pub(super) fn directory_ancestor_chain_limited(
    root: &Path,
    target: &Path,
    max_depth: usize,
) -> Vec<PathBuf> {
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
            if chain.len() >= max_depth {
                break;
            }
            current.push(component);
            chain.push(current.clone());
        }
    }
    chain
}
