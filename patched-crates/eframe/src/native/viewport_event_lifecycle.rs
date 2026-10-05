//! Viewport event ordering for the Simple Image Viewer eframe fork.
//!
//! Native `Close` events are snapshotted into `RawInput` and handled in
//! `EpiIntegration::update`. They must be dropped **before** applying
//! `ViewportCommand`s, because `ViewportCommand::Close` synthesizes a new
//! `ViewportEvent::Close` that has to survive until the next frame.
//!
//! The fork applies viewport commands before paint (fullscreen / swap-chain).
//! Clearing events *after* that step discards in-app quit.

use egui::{ViewportEvent, ViewportInfo};

/// Drop Close events that were already applied this frame.
pub fn consume_applied_input_events(info: &mut ViewportInfo) {
    info.events.clear();
}

/// Paint-path protocol after `update`: consume applied input, then keep any
/// Close synthesized by `ViewportCommand::Close`.
pub fn finish_paint_path_viewport_events(info: &mut ViewportInfo, output_emitted_close: bool) {
    consume_applied_input_events(info);
    if output_emitted_close {
        info.events.push(ViewportEvent::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_app_close_survives_paint_path() {
        let mut info = ViewportInfo::default();
        finish_paint_path_viewport_events(&mut info, true);
        assert!(
            info.close_requested(),
            "ViewportCommand::Close must remain for the next frame"
        );
    }

    #[test]
    fn applied_native_close_is_consumed_before_output() {
        let mut info = ViewportInfo::default();
        info.events.push(ViewportEvent::Close);
        consume_applied_input_events(&mut info);
        assert!(
            !info.close_requested(),
            "this-frame native Close was already handled in update"
        );
    }
}
