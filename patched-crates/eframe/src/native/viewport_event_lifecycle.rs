//! Viewport events for the Simple Image Viewer eframe fork.
//!
//! Native `Close` events are copied into `RawInput` and handled in
//! `EpiIntegration::update`. This fork then applies viewport commands before
//! paint (fullscreen / swap chain) on every desktop backend. Those commands
//! must not be followed by clearing `viewport.info.events`: `ViewportCommand::Close`
//! only pushes a synthetic `ViewportEvent::Close` into that list. Clearing
//! afterwards drops in-app close on X11, Wayland, and macOS alike.

use egui::ViewportInfo;

/// Drop viewport events that this frame's `update` already applied.
///
/// Call this **before** deferred viewport commands. `ViewportCommand::Close`
/// appends a new `ViewportEvent::Close` that has to survive until the next frame.
pub(super) fn consume_applied_input_events(info: &mut ViewportInfo) {
    info.events.clear();
}
