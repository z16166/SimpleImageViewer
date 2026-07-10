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

//! Cooperative cancel flag for long-running decode work.
//!
//! Shared across loader orchestration and format decoders (PSD composite today;
//! other slow paths can poll the same flag later). Not a generation counter.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Display text for [`DecodeError::Cancelled`] (logging / UI).
pub const DECODE_CANCELLED: &str = "decode cancelled";

/// Typed decode failure. Cancel is a distinct variant so callers match by enum
/// (checklist #30) instead of comparing error strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Cancelled,
    Message(String),
}

impl DecodeError {
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Cancelled => DECODE_CANCELLED,
            Self::Message(msg) => msg.as_str(),
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for DecodeError {}

impl From<String> for DecodeError {
    fn from(msg: String) -> Self {
        Self::Message(msg)
    }
}

impl From<&str> for DecodeError {
    fn from(msg: &str) -> Self {
        Self::Message(msg.to_string())
    }
}

/// Shared one-shot cancel flag for an in-flight load / decode request.
#[derive(Debug, Clone, Default)]
pub struct DecodeCancelFlag {
    flag: Arc<AtomicBool>,
}

impl DecodeCancelFlag {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal cooperative cancel. Idempotent; once set, stays set.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Borrow the underlying atomic for decoders that take `Option<&AtomicBool>`.
    #[inline]
    pub fn as_atomic(&self) -> &AtomicBool {
        &self.flag
    }
}

#[cfg(test)]
mod tests {
    use super::{DECODE_CANCELLED, DecodeCancelFlag, DecodeError};

    #[test]
    fn cancel_is_visible_to_clones() {
        let flag = DecodeCancelFlag::new();
        let clone = flag.clone();
        assert!(!clone.is_cancelled());
        flag.cancel();
        assert!(clone.is_cancelled());
        assert!(flag.as_atomic().load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn cancelled_is_typed_variant() {
        let err = DecodeError::Cancelled;
        assert!(err.is_cancelled());
        assert_eq!(err.as_str(), DECODE_CANCELLED);
        assert_eq!(err.to_string(), DECODE_CANCELLED);
        assert!(!DecodeError::Message("unsupported compression".into()).is_cancelled());
    }
}
