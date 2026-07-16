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

//! Coalescing background YAML saver (settings / hotkeys / context menu).
//!
//! One worker thread multiplexes all three YAML targets. Each target keeps an
//! independent trailing quiet period so rapid edits to one file never flush
//! another file early, and never block the others forever.

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::context_menu::model::ContextMenuConfigFile;
use crate::hotkeys::model::HotkeyConfigFile;
use crate::settings::Settings;

/// Payload for a best-effort background YAML write.
#[derive(Clone, Debug)]
pub(crate) enum YamlSaveRequest {
    Settings(Box<Settings>),
    Hotkeys(HotkeyConfigFile),
    ContextMenu(ContextMenuConfigFile),
}

/// Persistence failure reported back to the UI thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum YamlSaveError {
    Settings(String),
    Hotkeys(String),
    ContextMenu(String),
}

/// Stable slot index for multiplexed trailing saves.
///
/// `pending[kind as usize]` stores the latest payload for that YAML target.
///
/// **Layout contract:** real kinds are dense `0..KIND_COUNT`. Insert new real
/// kinds **before** [`YamlSaveKind::__Count`] so `KIND_COUNT` updates automatically.
/// Never place a real kind after `__Count`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
enum YamlSaveKind {
    Settings = 0,
    Hotkeys,
    ContextMenu,
    /// Sentinel only -- not a save target. Must remain the last variant.
    __Count,
}

/// Number of real save kinds / pending slots (`__Count` discriminant).
const KIND_COUNT: usize = YamlSaveKind::__Count as usize;

// Catch accidental reordering: first real kind is 0; sentinel alone defines KIND_COUNT.
// Add new real kinds before `__Count` -- do not list them here (avoids another manual sync).
const _: () = assert!(YamlSaveKind::Settings as usize == 0);
const _: () = assert!(YamlSaveKind::__Count as usize == KIND_COUNT);

impl YamlSaveKind {
    fn index(self) -> usize {
        let i = self as usize;
        debug_assert!(
            i < KIND_COUNT,
            "YamlSaveKind::__Count is not a pending slot"
        );
        i
    }
}

impl YamlSaveRequest {
    fn kind(&self) -> YamlSaveKind {
        match self {
            Self::Settings(_) => YamlSaveKind::Settings,
            Self::Hotkeys(_) => YamlSaveKind::Hotkeys,
            Self::ContextMenu(_) => YamlSaveKind::ContextMenu,
        }
    }

    fn kind_index(&self) -> usize {
        self.kind().index()
    }

    fn save(&self) -> Result<(), String> {
        match self {
            Self::Settings(settings) => settings.save(),
            Self::Hotkeys(config) => crate::hotkeys::io::save_hotkeys_file(config),
            Self::ContextMenu(config) => crate::context_menu::io::save_context_menu_file(config),
        }
    }

    fn to_error(&self, message: String) -> YamlSaveError {
        match self {
            Self::Settings(_) => YamlSaveError::Settings(message),
            Self::Hotkeys(_) => YamlSaveError::Hotkeys(message),
            Self::ContextMenu(_) => YamlSaveError::ContextMenu(message),
        }
    }
}

/// Trailing debounce for one request kind: persist once min_interval has passed
/// with **no new requests of that kind**.
///
/// recv_timeout may return early when a message arrives; that only refreshes the
/// pending payload and resets that kind quiet timer -- it does **not** write to
/// disk. A write happens only after the full quiet period elapses for that kind.
///
/// Uses recv_timeout (not thread::sleep) so dropping the sender during shutdown
/// wakes the thread immediately via Disconnected instead of blocking join() for
/// the remainder of the quiet window.
pub(crate) fn run_unified_yaml_saver(
    rx: Receiver<YamlSaveRequest>,
    min_interval: Duration,
    error_tx: Sender<YamlSaveError>,
) {
    run_multiplexed_coalescing_saver(
        rx,
        min_interval,
        |req| req.save(),
        |err| report_yaml_save_error(&error_tx, err),
    );
}

/// Non-blocking UI error delivery for the saver thread.
///
/// A full or disconnected channel must never stall disk I/O; drop with a warning.
fn report_yaml_save_error(error_tx: &Sender<YamlSaveError>, err: YamlSaveError) {
    match error_tx.try_send(err) {
        Ok(()) => {}
        Err(crossbeam_channel::TrySendError::Full(dropped)) => {
            log::warn!(
                "[yaml-saver] error channel full (capacity {}); dropping {:?}",
                crate::constants::BACKGROUND_YAML_SAVE_ERROR_CHANNEL_CAPACITY,
                dropped
            );
        }
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
    }
}

/// Generic multi-kind trailing saver used by production and tests.
///
/// `pending[YamlSaveKind::X as usize]` holds the latest payload for that kind and
/// the instant when its quiet period ends.
fn run_multiplexed_coalescing_saver<T, E>(
    rx: Receiver<T>,
    min_interval: Duration,
    mut save: impl FnMut(&T) -> Result<(), String>,
    mut on_error: impl FnMut(E),
) where
    T: KindedRequest<Error = E>,
{
    let mut pending: [Option<(T, Instant)>; KIND_COUNT] = [const { None }; KIND_COUNT];

    loop {
        if pending.iter().all(|slot| slot.is_none()) {
            match rx.recv() {
                Ok(req) => enqueue_pending(&mut pending, req, min_interval),
                Err(_) => return,
            }
            continue;
        }

        while let Ok(req) = rx.try_recv() {
            enqueue_pending(&mut pending, req, min_interval);
        }

        let now = Instant::now();
        let mut any_ready = false;
        let mut min_remaining: Option<Duration> = None;
        for (_, quiet_until) in pending.iter().flatten() {
            let remaining = quiet_until.saturating_duration_since(now);
            if remaining.is_zero() {
                any_ready = true;
            } else {
                min_remaining = Some(match min_remaining {
                    Some(cur) if cur <= remaining => cur,
                    _ => remaining,
                });
            }
        }

        if !any_ready {
            let wait = min_remaining.unwrap_or(Duration::ZERO);
            match rx.recv_timeout(wait) {
                Ok(req) => {
                    enqueue_pending(&mut pending, req, min_interval);
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        let now = Instant::now();
        for slot in &mut pending {
            let ready = matches!(
                slot,
                Some((_, quiet_until)) if quiet_until.saturating_duration_since(now).is_zero()
            );
            if !ready {
                continue;
            }
            if let Some((req, _)) = slot.take()
                && let Err(message) = save(&req)
            {
                on_error(req.to_error(message));
            }
        }
    }
}

/// Request types that can be multiplexed by [`run_multiplexed_coalescing_saver`].
///
/// `kind_index()` **must** be in `0..KIND_COUNT`. Out-of-range values are rejected
/// by [`enqueue_pending`] (logged and dropped) instead of panicking on `pending[kind]`.
trait KindedRequest {
    type Error;
    fn kind_index(&self) -> usize;
    fn to_error(&self, message: String) -> Self::Error;
}

/// Store `req` into its kind slot. Invalid `kind_index` values are dropped (Release-safe).
fn enqueue_pending<T: KindedRequest>(
    pending: &mut [Option<(T, Instant)>; KIND_COUNT],
    req: T,
    min_interval: Duration,
) {
    let kind = req.kind_index();
    let Some(slot) = pending.get_mut(kind) else {
        // Programmer error: KindedRequest returned an index outside the pending table.
        // Do not index `pending[kind]` (would panic). Drop the request so a saver bug
        // cannot take down the process; authoritative saves still run in on_exit.
        log::error!(
            "[yaml-saver] kind_index {kind} out of range (KIND_COUNT={KIND_COUNT}); dropping request"
        );
        return;
    };
    *slot = Some((req, Instant::now() + min_interval));
}

impl KindedRequest for YamlSaveRequest {
    type Error = YamlSaveError;

    fn kind_index(&self) -> usize {
        YamlSaveRequest::kind_index(self)
    }

    fn to_error(&self, message: String) -> Self::Error {
        YamlSaveRequest::to_error(self, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::thread;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TestReq {
        A(i32),
        B(i32),
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TestErr {
        A(String),
        B(String),
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(usize)]
    enum TestKind {
        A = 0,
        B = 1,
    }

    impl KindedRequest for TestReq {
        type Error = TestErr;

        fn kind_index(&self) -> usize {
            match self {
                Self::A(_) => TestKind::A as usize,
                Self::B(_) => TestKind::B as usize,
            }
        }

        fn to_error(&self, message: String) -> Self::Error {
            match self {
                Self::A(_) => TestErr::A(message),
                Self::B(_) => TestErr::B(message),
            }
        }
    }

    fn spawn_test_saver(
        rx: Receiver<TestReq>,
        min_interval: Duration,
        saves: Arc<Mutex<Vec<TestReq>>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            run_multiplexed_coalescing_saver(
                rx,
                min_interval,
                |req| {
                    saves.lock().push(req.clone());
                    Ok(())
                },
                |_| {},
            );
        })
    }

    #[test]
    fn trailing_quiet_period_coalesces_rapid_updates_into_one_save() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let saves = Arc::new(Mutex::new(Vec::new()));
        let handle = spawn_test_saver(rx, Duration::from_millis(200), Arc::clone(&saves));

        tx.send(TestReq::A(1)).unwrap();
        thread::sleep(Duration::from_millis(50));
        tx.send(TestReq::A(2)).unwrap();
        thread::sleep(Duration::from_millis(50));
        tx.send(TestReq::A(3)).unwrap();

        thread::sleep(Duration::from_millis(120));
        assert!(saves.lock().is_empty());

        thread::sleep(Duration::from_millis(250));
        assert_eq!(*saves.lock(), vec![TestReq::A(3)]);

        drop(tx);
        handle.join().unwrap();
    }

    #[test]
    fn independent_kinds_keep_separate_quiet_windows() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let saves = Arc::new(Mutex::new(Vec::new()));
        let handle = spawn_test_saver(rx, Duration::from_millis(200), Arc::clone(&saves));

        // B is requested first and must flush after its own quiet window even if
        // A keeps receiving updates.
        tx.send(TestReq::B(10)).unwrap();
        thread::sleep(Duration::from_millis(50));
        tx.send(TestReq::A(1)).unwrap();
        thread::sleep(Duration::from_millis(80));
        tx.send(TestReq::A(2)).unwrap();

        // B quiet window (~200ms from first send) should complete while A is
        // still being refreshed.
        thread::sleep(Duration::from_millis(100));
        {
            let locked = saves.lock();
            assert_eq!(locked.as_slice(), &[TestReq::B(10)]);
        }

        // Stop refreshing A and wait for its own quiet window.
        thread::sleep(Duration::from_millis(250));
        {
            let locked = saves.lock();
            assert_eq!(locked.as_slice(), &[TestReq::B(10), TestReq::A(2)]);
        }

        drop(tx);
        handle.join().unwrap();
    }

    #[test]
    fn disconnect_during_quiet_wait_exits_without_blocking() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let saves = Arc::new(Mutex::new(Vec::new()));
        let handle = spawn_test_saver(rx, Duration::from_secs(60), Arc::clone(&saves));

        tx.send(TestReq::A(1)).unwrap();
        drop(tx);

        let start = Instant::now();
        handle.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(saves.lock().is_empty());
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum OutOfRangeReq {
        Bad,
    }

    impl KindedRequest for OutOfRangeReq {
        type Error = ();

        fn kind_index(&self) -> usize {
            KIND_COUNT
        }

        fn to_error(&self, _message: String) -> Self::Error {}
    }

    #[test]
    fn report_yaml_save_error_drops_when_channel_full() {
        let (error_tx, error_rx) = crossbeam_channel::bounded(1);
        error_tx
            .send(YamlSaveError::Settings("prefill".to_string()))
            .expect("prefill");

        // Must not block when the UI-bound channel is saturated.
        report_yaml_save_error(&error_tx, YamlSaveError::Hotkeys("dropped".to_string()));

        assert_eq!(
            error_rx.try_recv().ok(),
            Some(YamlSaveError::Settings("prefill".to_string()))
        );
        assert!(error_rx.try_recv().is_err());
    }

    #[test]
    fn out_of_range_kind_index_is_dropped_without_panic() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let saves = Arc::new(Mutex::new(0usize));
        let sc = Arc::clone(&saves);
        let handle = thread::spawn(move || {
            run_multiplexed_coalescing_saver(
                rx,
                Duration::from_millis(50),
                |_req: &OutOfRangeReq| {
                    *sc.lock() += 1;
                    Ok(())
                },
                |_| {},
            );
        });

        tx.send(OutOfRangeReq::Bad).unwrap();
        thread::sleep(Duration::from_millis(120));
        assert_eq!(*saves.lock(), 0);

        drop(tx);
        handle.join().unwrap();
    }
}
