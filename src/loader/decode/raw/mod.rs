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

//! LibRAW and raw tiled refinement.
//!
//! `raw_high_quality` controls whether LibRaw's expensive demosaic runs:
//! - **Off:** use embedded previews whenever present (SDR pipeline on all displays).
//!   Full develop only when the file has no embedded preview; on HDR displays that
//!   develop result uses the HDR pipeline.
//! - **On:** use embedded previews when they meet HQ size requirements; otherwise demosaic at
//!   full sensor resolution. Developed pixels always use the HDR pipeline (even on SDR displays to support exposure adjustments).

mod develop;
mod load;
mod preview;

#[cfg(test)]
mod tests;

pub(crate) use load::load_raw;

#[cfg(test)]
pub(crate) use crate::loader::preview_caps::hq_preview_max_side;
#[cfg(test)]
pub(crate) use crate::raw_processor::RawProcessor;
#[cfg(test)]
pub(crate) use preview::{
    raw_embedded_preview_covers_sensor, raw_embedded_preview_meets_hq_requirement,
};
