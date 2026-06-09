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
use super::*;

    use super::should_spawn_load_task;
    use std::collections::HashMap;

    #[test]
    fn should_spawn_load_task_only_for_newer_generation() {
        let mut loading = HashMap::new();

        assert!(should_spawn_load_task(&mut loading, 7, 1));
        assert_eq!(loading.get(&7), Some(&1));

        // Same generation should not schedule duplicate load task.
        assert!(!should_spawn_load_task(&mut loading, 7, 1));
        assert_eq!(loading.get(&7), Some(&1));

        // Newer generation must schedule a fresh task, otherwise UI can stall on loading.
        assert!(should_spawn_load_task(&mut loading, 7, 2));
        assert_eq!(loading.get(&7), Some(&2));

        // Older generation should be ignored.
        assert!(!should_spawn_load_task(&mut loading, 7, 1));
        assert_eq!(loading.get(&7), Some(&2));
    }

    #[test]
    fn test_discard_pending_stale_outputs_preserves_hdr_fallback() {
        use super::{HdrSdrFallbackResult, ImageLoader, LoaderOutput};
        let mut loader = ImageLoader::new();

        let fallback_result = HdrSdrFallbackResult {
            index: 0,
            generation: 1,
            source_key: 12345,
            fallback: None,
        };
        loader.test_send_loader_output(LoaderOutput::HdrSdrFallback(fallback_result));

        // Call discard with a newer generation (2)
        loader.discard_pending_stale_outputs(2, None);

        // The fallback result should still be retrievable via poll()
        let result = loader.poll();
        assert!(result.is_some());
        if let Some(LoaderOutput::HdrSdrFallback(r)) = result {
            assert_eq!(r.generation, 1);
            assert_eq!(r.source_key, 12345);
            assert!(r.fallback.is_none());
        } else {
            panic!("Expected LoaderOutput::HdrSdrFallback");
        }
    }

    #[test]
    fn test_fallback_refinement_failure_clears_inflight() {
        use super::{ImageLoader, LoaderOutput};
        use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let mut loader = ImageLoader::new();

        // Construct a malformed HDR buffer to force a failure (only 3 floats instead of 4)
        let malformed_hdr = Arc::new(HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0]),
        });

        loader.trigger_hdr_sdr_fallback_refinement(0, 1, malformed_hdr, 12345);

        // Poll loader with a timeout until we get the fallback result
        let start = Instant::now();
        let mut fallback_received = None;
        while start.elapsed() < Duration::from_secs(3) {
            if let Some(output) = loader.poll() {
                if let LoaderOutput::HdrSdrFallback(r) = output {
                    fallback_received = Some(r);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let r = fallback_received.expect("Should have received HdrSdrFallback on failure path");
        assert_eq!(r.index, 0);
        assert_eq!(r.generation, 1);
        assert_eq!(r.source_key, 12345);
        assert!(r.fallback.is_none());
    