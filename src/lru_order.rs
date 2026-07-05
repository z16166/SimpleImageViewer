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

use std::collections::HashMap;
use std::hash::Hash;

/// O(1) LRU order (touch, remove, pop-oldest) via HashMap + doubly-linked list.
///
/// Internal `expect` calls are invariant assertions on HashMap <-> linked-list
/// consistency; public API callers must preserve that invariant.
pub(crate) struct LruOrder<K> {
    nodes: HashMap<K, LruLinks<K>>,
    head: Option<K>,
    tail: Option<K>,
}

impl<K> Default for LruOrder<K> {
    fn default() -> Self {
        Self {
            nodes: HashMap::new(),
            head: None,
            tail: None,
        }
    }
}

impl<K> std::fmt::Debug for LruOrder<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LruOrder")
            .field("len", &self.nodes.len())
            .finish()
    }
}

#[derive(Clone, Copy)]
struct LruLinks<K> {
    prev: Option<K>,
    next: Option<K>,
}

impl<K> LruOrder<K>
where
    K: Copy + Eq + Hash,
{
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
        self.head = None;
        self.tail = None;
    }

    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub(crate) fn contains(&self, key: K) -> bool {
        self.nodes.contains_key(&key)
    }

    pub(crate) fn touch(&mut self, key: K) {
        self.unlink(key);
        self.link_at_tail(key);
    }

    /// Insert or move `key` to the LRU (oldest-evict) side.
    pub(crate) fn push_oldest(&mut self, key: K) {
        self.unlink(key);
        self.link_at_head(key);
    }

    pub(crate) fn remove(&mut self, key: K) {
        self.unlink(key);
    }

    pub(crate) fn pop_oldest(&mut self) -> Option<K> {
        let oldest = self.head?;
        self.unlink(oldest);
        Some(oldest)
    }

    pub(crate) fn rename(&mut self, from: K, to: K) {
        if from == to || !self.contains(from) {
            return;
        }
        self.remove(to);
        let Some(links) = self.nodes.remove(&from) else {
            return;
        };
        if let Some(prev) = links.prev {
            self.nodes.get_mut(&prev).expect("LRU prev").next = Some(to);
        } else {
            self.head = Some(to);
        }
        if let Some(next) = links.next {
            self.nodes.get_mut(&next).expect("LRU next").prev = Some(to);
        } else {
            self.tail = Some(to);
        }
        self.nodes.insert(to, links);
    }

    /// Rebuild keys in current LRU order via `f`, dropping keys mapped to `None`.
    ///
    /// **Complexity:** O(n) time and O(n) auxiliary allocation via [`Self::ordered_keys`].
    /// Suitable for small caches (e.g. tile caches ≤512 entries); avoid on large hot paths
    /// without profiling.
    pub(crate) fn remap_ordered<F>(&mut self, mut f: F)
    where
        F: FnMut(K) -> Option<K>,
    {
        let ordered = self.ordered_keys();
        self.clear();
        for key in ordered {
            if let Some(new_key) = f(key) {
                self.touch(new_key);
            }
        }
    }

    fn ordered_keys(&self) -> Vec<K> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut cur = self.head;
        while let Some(key) = cur {
            out.push(key);
            cur = self.nodes.get(&key).and_then(|links| links.next);
        }
        out
    }

    fn unlink(&mut self, key: K) {
        let Some(links) = self.nodes.remove(&key) else {
            return;
        };
        match (links.prev, links.next) {
            (None, None) => {
                self.head = None;
                self.tail = None;
            }
            (None, Some(next)) => {
                self.head = Some(next);
                self.nodes.get_mut(&next).expect("LRU head next").prev = None;
            }
            (Some(prev), None) => {
                self.tail = Some(prev);
                self.nodes.get_mut(&prev).expect("LRU tail prev").next = None;
            }
            (Some(prev), Some(next)) => {
                self.nodes.get_mut(&prev).expect("LRU prev").next = Some(next);
                self.nodes.get_mut(&next).expect("LRU next").prev = Some(prev);
            }
        }
    }

    fn link_at_tail(&mut self, key: K) {
        let links = LruLinks {
            prev: self.tail,
            next: None,
        };
        if let Some(tail) = self.tail {
            self.nodes.get_mut(&tail).expect("LRU tail").next = Some(key);
        } else {
            self.head = Some(key);
        }
        self.tail = Some(key);
        self.nodes.insert(key, links);
    }

    fn link_at_head(&mut self, key: K) {
        let links = LruLinks {
            prev: None,
            next: self.head,
        };
        if let Some(head) = self.head {
            self.nodes.get_mut(&head).expect("LRU head").prev = Some(key);
        } else {
            self.tail = Some(key);
        }
        self.head = Some(key);
        self.nodes.insert(key, links);
    }
}

impl LruOrder<usize> {
    /// Drop indices for which `keep` returns false, preserving relative order of survivors.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(usize) -> bool) {
        let mut cur = self.head;
        while let Some(key) = cur {
            let next = self.nodes.get(&key).and_then(|links| links.next);
            if !keep(key) {
                self.unlink(key);
            }
            cur = next;
        }
    }

    /// Rebuild keys via `old_to_new`, dropping entries mapped to [`usize::MAX`].
    pub(crate) fn partial_remap(&mut self, old_to_new: &[usize]) {
        self.remap_ordered(|index| {
            if index >= old_to_new.len() {
                return None;
            }
            let new_idx = old_to_new[index];
            (new_idx != usize::MAX).then_some(new_idx)
        });
    }

    /// Rebuild keys via `old_to_new`, dropping indices outside the remap table.
    pub(crate) fn permute(&mut self, old_to_new: &[usize]) {
        self.remap_ordered(|index| {
            (index < old_to_new.len()).then_some(old_to_new[index])
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_order_touch_and_pop_oldest() {
        let mut lru = LruOrder::default();
        lru.touch(1);
        lru.touch(2);
        lru.touch(3);
        lru.touch(1);
        assert_eq!(lru.pop_oldest(), Some(2));
        assert_eq!(lru.pop_oldest(), Some(3));
        assert_eq!(lru.pop_oldest(), Some(1));
        assert_eq!(lru.pop_oldest(), None);
    }

    #[test]
    fn lru_order_rename_preserves_position() {
        let mut lru = LruOrder::default();
        lru.touch(1);
        lru.touch(2);
        lru.rename(1, 10);
        assert!(!lru.contains(1));
        assert!(lru.contains(10));
        assert_eq!(lru.pop_oldest(), Some(10));
        assert_eq!(lru.pop_oldest(), Some(2));
    }

    #[test]
    fn lru_order_push_oldest_evicts_first() {
        let mut lru = LruOrder::default();
        lru.touch(1);
        lru.touch(2);
        lru.push_oldest(3);
        assert_eq!(lru.pop_oldest(), Some(3));
        assert_eq!(lru.pop_oldest(), Some(1));
        assert_eq!(lru.pop_oldest(), Some(2));
    }

    #[test]
    fn lru_order_retain_drops_without_rebuild() {
        let mut lru = LruOrder::default();
        lru.touch(1);
        lru.touch(2);
        lru.touch(3);
        lru.retain(|index| index != 2);
        assert!(!lru.contains(2));
        assert_eq!(lru.pop_oldest(), Some(1));
        assert_eq!(lru.pop_oldest(), Some(3));
    }

    #[test]
    fn lru_order_permute_remaps_keys() {
        let mut lru = LruOrder::default();
        lru.touch(0);
        lru.touch(1);
        lru.touch(2);
        lru.permute(&[10, 11, 12]);
        assert!(!lru.contains(0));
        assert!(lru.contains(10));
        assert_eq!(lru.pop_oldest(), Some(10));
        assert_eq!(lru.pop_oldest(), Some(11));
        assert_eq!(lru.pop_oldest(), Some(12));
    }

    #[test]
    fn lru_order_partial_remap_skips_max_sentinel() {
        let mut lru = LruOrder::default();
        lru.touch(0);
        lru.touch(1);
        lru.partial_remap(&[5, usize::MAX]);
        assert!(lru.contains(5));
        assert!(!lru.contains(1));
        assert_eq!(lru.pop_oldest(), Some(5));
    }
}
