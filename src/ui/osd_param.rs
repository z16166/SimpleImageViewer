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

use crossbeam_channel::Sender;

pub struct TrackedParam<T, E> {
    value: T,
    tx: Sender<E>,
    map_fn: fn(&T) -> E,
}

impl<T, E> TrackedParam<T, E>
where
    T: PartialEq,
{
    pub fn new(initial: T, tx: Sender<E>, map_fn: fn(&T) -> E) -> Self {
        let _ = tx.send(map_fn(&initial));
        Self {
            value: initial,
            tx,
            map_fn,
        }
    }

    pub fn set(&mut self, new_value: T) {
        if self.get() == &new_value {
            return;
        }
        self.value = new_value;
        let _ = self.tx.send((self.map_fn)(self.get()));
    }

    /// Read the business value.
    ///
    /// Do not add `allow(dead_code)` here. If nobody reads a `TrackedParam`, it may be an
    /// OSD-only shadow variable instead of the real business state, which can hide stale data
    /// bugs or missed updates.
    pub fn get(&self) -> &T {
        &self.value
    }
}
