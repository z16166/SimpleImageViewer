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

//! Coalescing background YAML saver threads (settings, hotkeys, context menu).

use crossbeam_channel::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Trailing debounce: persist once `min_interval` has passed with **no new requests**.
///
/// `recv_timeout` may return early when a message arrives; that only refreshes `latest`
/// and resets the quiet timer -- it does **not** write to disk. A write happens only after
/// the full quiet period elapses.
///
/// Uses `recv_timeout` (not `thread::sleep`) so dropping the sender during shutdown wakes
/// the thread immediately via `Disconnected` instead of blocking `join()` for the remainder
/// of the quiet window.
pub(crate) fn run_coalescing_periodic_saver<T>(
    rx: Receiver<T>,
    min_interval: Duration,
    mut save: impl FnMut(&T) -> Result<(), String>,
    mut on_error: impl FnMut(String),
) {
    while let Ok(first) = rx.recv() {
        let mut latest = first;
        let mut quiet_until = Instant::now() + min_interval;

        loop {
            while let Ok(newer) = rx.try_recv() {
                latest = newer;
                quiet_until = Instant::now() + min_interval;
            }

            let remaining = quiet_until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            match rx.recv_timeout(remaining) {
                Ok(newer) => {
                    latest = newer;
                    quiet_until = Instant::now() + min_interval;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        if let Err(e) = save(&latest) {
            on_error(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn trailing_quiet_period_coalesces_rapid_updates_into_one_save() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let save_count = Arc::new(Mutex::new(0));
        let saved_values = Arc::new(Mutex::new(Vec::new()));
        let sc = Arc::clone(&save_count);
        let sv = Arc::clone(&saved_values);

        let handle = thread::spawn(move || {
            run_coalescing_periodic_saver(
                rx,
                Duration::from_millis(200),
                |v: &i32| {
                    *sc.lock().unwrap() += 1;
                    sv.lock().unwrap().push(*v);
                    Ok(())
                },
                |_| {},
            );
        });

        tx.send(1).unwrap();
        thread::sleep(Duration::from_millis(50));
        tx.send(2).unwrap();
        thread::sleep(Duration::from_millis(50));
        tx.send(3).unwrap();

        thread::sleep(Duration::from_millis(100));
        assert_eq!(*save_count.lock().unwrap(), 0);

        thread::sleep(Duration::from_millis(150));
        assert_eq!(*save_count.lock().unwrap(), 1);
        assert_eq!(*saved_values.lock().unwrap(), vec![3]);

        drop(tx);
        handle.join().unwrap();
    }

    #[test]
    fn disconnect_during_quiet_wait_exits_without_blocking() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let save_count = Arc::new(Mutex::new(0));
        let sc = Arc::clone(&save_count);

        let handle = thread::spawn(move || {
            run_coalescing_periodic_saver(
                rx,
                Duration::from_secs(60),
                |_: &()| {
                    *sc.lock().unwrap() += 1;
                    Ok(())
                },
                |_| {},
            );
        });

        tx.send(()).unwrap();
        drop(tx);

        let start = Instant::now();
        handle.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(*save_count.lock().unwrap(), 0);
    }
}
