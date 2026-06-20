# Code Review: `codex/dir-tree-navigation` vs `main`

**Date:** 2026-06-20
**Reviewer:** AI Code Review
**Branch:** `codex/dir-tree-navigation` (49 commits ahead of `main`)
**Scope:** Full diff — 99 files changed, 13,408 insertions, 1,397 deletions

---

## Executive Summary

This branch adds a major feature: a dual-purpose navigation window (directory tree + image file list) that can operate in **Embedded** (egui SidePanel) or **Detached** (egui deferred viewport as independent OS window) mode. It also patches the `eframe` crate to support the synchronous repaint chain required for detached viewports, adds places/shell namespace support (drive letters, UNC paths, known folders on Windows), and includes a directory tree strip thumbnail cache with background worker threads.

The code quality is **generally high** — the architecture is well-thought-out, test coverage for the new modules is excellent (973 lines of tests for directory_tree alone), and cross-platform concerns are addressed. However, the review found **5 Critical**, **12 High**, **25 Medium**, and numerous Low-severity issues across the subsystems.

---

## Critical Issues (5)

### C1 — Operator Precedence Bug in `prefetch_circular_distance`
**File:** `src/app/image_management/mod.rs:1249-1251`

```rust
// BUG: % binds tighter than +/-
let dist_forward = (candidate + image_count - current_index % image_count) % image_count;
let dist_backward = (current_index + image_count - candidate % image_count) % image_count;
```

Due to Rust operator precedence (`%` > `+` > `-`), these parse as:

```
dist_forward = (candidate + image_count - (current_index % image_count)) % image_count
```

The intended formulas are:

```rust
let dist_forward = (candidate + image_count - current_index) % image_count;
let dist_backward = (current_index + image_count - candidate) % image_count;
```

**Impact:** Currently masked because `current_index` and `candidate` are always `< image_count` (making `x % image_count == x`), but any out-of-bounds index would produce silently incorrect circular distances. This affects `prefetch_window_contains` and thus `evict_distant_prefetch_caches` — incorrect cache eviction under edge conditions.

**Fix:** Remove the errant `% image_count` from the subtrahends.

---

### C2 — Synchronous `RepaintNow` Chain Re-entrancy Hazard
**File:** `patched-crates/eframe/src/native/run.rs:108-128`

```rust
if let Ok(EventResult::RepaintNow(window_id)) = event_result {
    self.windows_next_repaint_times.insert(window_id, Instant::now());
    event_result = self.winit_app.run_ui_and_paint(event_loop, window_id);
}
```

This unconditional synchronous `RepaintNow → run_ui_and_paint` chain is the central fork change. `run_ui_and_paint` calls `app.logic()`, which runs arbitrary user code. If that user code triggers a winit event (e.g., via `show_viewport_immediate`), the synchronous chain could be **re-entered** from within a running event handler.

**Impact:** Re-entrant logic calls from within an event handler are a correctness hazard. While the code is structured so the re-result only goes through the scheduling branch, there is no explicit recursion guard.

**Fix:** Add a re-entrancy guard (e.g., `bool` flag or `Cell<bool>`) to prevent nested `run_ui_and_paint` calls from within the same event dispatch cycle. At minimum, document the recursion bound explicitly.

---

### C3 — HDR Policy Contradiction: `any_active_output_supports_hdr` vs `dxgi_output_hdr_active`
**File:** `src/hdr/monitor/windows.rs:297-301`

The doc comment on line 261 says:
> "true if any DXGI output reports active HDR signaling (BitsPerColor > 8 AND ColorSpace == G2084_NONE_P2020)"

But `dxgi_output_hdr_active` (line 39-53) explicitly documents:
> "We deliberately do NOT also gate on BitsPerColor > 8 here."

Yet `any_active_output_supports_hdr` (line 299) STILL gates on `BitsPerColor > 8`:

```rust
// Contradicts the stated policy in dxgi_output_hdr_active
BitsPerColor > 8 && ColorSpace == G2084_NONE_P2020
```

**Impact:** This function will incorrectly return `false` for HDR monitors running at 8 BPC + dithering (e.g., Samsung LC49G95T). Although currently `#[allow(dead_code)]`, if it is ever called, it will produce wrong results.

**Fix:** Either remove the `BitsPerColor > 8` gate (matching `dxgi_output_hdr_active`'s stance) or update the documentation to reflect the actual policy.

---

### C4 — Orphan Thread Accumulation on Slow Network Paths
**File:** `src/app/directory_tree/workers.rs:159-221`

```rust
fn read_child_directories_with_timeout(&self, path: &Path, ...) {
    // ... spawns helper thread ...
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(children) => { /* success */ }
        Err(_) => {
            // Thread is ORPHANED — continues running on OS read_dir
            // but we decrement inflight counter and return error
        }
    }
}
```

When a timeout occurs (30s), the helper thread is **orphaned** — it continues to block on the OS `read_dir` call indefinitely. The inflight counter is decremented, allowing a NEW helper to be spawned for the **same path** on the next expansion attempt, compounding the leak. Up to `MAX_READ_DIR_HELPERS_INFLIGHT` (4) orphan threads can accumulate per slow path.

**Impact:** On UNC paths or slow network shares, repeated expansion attempts create unbounded orphan threads that consume OS thread resources and hold open file handles.

**Fix:** Implement a mechanism to cancel in-flight read_dir operations (e.g., cooperative cancellation via `AtomicBool`, or use a bounded retry counter per path to prevent re-queuing already-orphaned paths).

---

### C5 — `ComGuard::new()` Return Value Silently Ignored
**File:** `src/wic/tiled_source.rs:386`

```rust
let _com = ComGuard::new(); // Result<ComGuard> is silently dropped!
```

`ComGuard::new()` returns `windows::core::Result<Self>`. If COM initialization fails, subsequent `unsafe` WIC COM calls operate **without a valid COM apartment**.

**Impact:** Silent failure — the tile decode produces a black tile instead of a crash, with no diagnostic log. This is a pre-existing issue but should be fixed.

**Fix:** Log the error and return early:

```rust
let _com = match ComGuard::new() {
    Ok(guard) => Some(guard),
    Err(e) => {
        log::error!("COM init failed on tile worker: {e:?}");
        return std::sync::Arc::new(vec![0u8; (w * h * 4) as usize]);
    }
};
```

---

## High-Severity Issues (12)

### H1 — `Instant::duration_since` Panic on System Time Backward Jump
**File:** `src/app/logic_update.rs:17-26`

```rust
fn should_run_logic_shared(&self) -> bool {
    self.last_logic_shared_at.is_none_or(|t| {
        Instant::now().duration_since(t) >= Self::LOGIC_SHARED_COALESCE
    })
}
```

`Instant::now().duration_since(t)` **panics** if `t > now` (can happen due to system sleep/wake, VM pause, or platform-specific `Instant` quirks on 32-bit).

**Fix:** Use `Instant::checked_duration_since` or `Instant::saturating_duration_since` (stable since Rust 1.39).

---

### H2 — `queue.submit([])` Flushes ALL Pending GPU Work Mid-Frame
**File:** `patched-crates/egui-wgpu/src/renderer.rs:42`

```rust
fn queue_write_with_fallback(...) {
    // ...
    queue.submit([]); // flushes ALL pending work on this Queue
    // ...
}
```

In multi-viewport rendering, if viewport B's `update_buffers` triggers this fallback path, it submits command buffers from `CallbackTrait::prepare`/`finish_prepare` that were queued by viewport A. This could cause a **GPU timeline desync** if callbacks expect their work to submit atomically with their render pass.

**Fix:** Document that `queue.submit([])` flushes all pending work. Verify that in multi-viewport scenarios, no callbacks have pending GPU work when `update_buffers` is called for a sibling viewport.

---

### H3 — `change_gl_context` Unwrap Panic on Destroyed Surface
**File:** `patched-crates/eframe/src/native/glow_integration.rs:1135-1137`

```rust
let old_ctx = current_gl_context.take().unwrap();
let new_ctx = not_current_gl_context.take().unwrap();
```

If the window surface has been destroyed (window closed while resize is pending), both `take()` calls panic. The fork's additional synchronous resize in `run_ui_and_paint` (lines 583-612) **increases** the frequency of context switching, increasing exposure to this pre-existing upstream bug.

**Fix:** Handle `None` gracefully — log a warning and skip the context change if either context is gone.

---

### H4 — `permute_images` Silent Overwrite on Non-Bijective Mapping
**File:** `src/tile_cache.rs:223-246`

```rust
if let Some(pixels) = self.entries.remove(&key) {
    self.entries.insert((new_idx, key.1, key.2), pixels);
}
```

If `old_to_new` maps two distinct old indices to the same new index (e.g., `old_to_new = [0, 0, 0]` due to a bug), the second insertion **silently overwrites** the first in the `HashMap`. For a correct permutation this cannot happen, but there is no validation that the mapping is a bijection.

**Fix:** Add a `debug_assert!(!self.entries.contains_key(&(new_idx, ...)))` before insertion, or validate the permutation at the caller.

---

### H5 — `navigate_to` Accesses `image_files[current_index]` Without Recency Check
**File:** `src/app/image_management/navigation.rs:290` (called from mod.rs:885)

`trigger_current_hdr_fallback_refinement_if_needed` accesses `self.image_files[self.current_index]`. Although `navigate_to` checks `self.image_files.is_empty()`, there is **no check that `self.current_index < self.image_files.len()`**. If `current_index` became stale after a permutation that skipped updating it, this panics with index-out-of-bounds.

**Fix:** Add a bounds check before the access, or ensure `current_index` is always kept in sync with `image_files.len()` after all mutation paths.

---

### H6 — `AtomicPtr<ImageViewerApp>` with `Relaxed` Ordering
**File:** `src/app/directory_tree/app.rs:1203-1268`

```rust
self.viewpaint_app.store(std::ptr::null_mut(), Ordering::Relaxed);
// ...
let app_ptr = self.viewpaint_app.load(Ordering::Relaxed);
```

The safety comment says "The pointer is set only for the current UI frame on the UI thread." However, the `store` happens inside `show_viewport_deferred`'s callback, which could be invoked from egui's internal rendering threads on some backends. `Relaxed` ordering provides no formal happens-before relationship.

**Fix:** Use `Ordering::Release` on store and `Ordering::Acquire` on load, or switch to `UnsafeCell` + documented single-thread invariant.

---

### H7 — `try_send` on Bounded Channel Silently Drops User Requests
**File:** `src/app/directory_tree/app.rs:287-305`

```rust
children_request_tx.try_send(request).map_err(|e| {
    // Request silently dropped, user sees "busy" error
})
```

The channel capacity is only 64. During rapid tree expansion (deeply nested directories), the UI thread can easily outpace the worker thread (which does blocking I/O), causing spurious "busy" errors.

**Fix:** Either increase the channel capacity, use an unbounded channel, or implement a request queue that merges/coalesces redundant expansion requests.

---

### H8 — Window Placement Cached When Hidden-to-Tray
**File:** `src/app/logic_update.rs:340-394`

```rust
cached_window_placement = Some(viewport.outer_rect); // Updated unconditionally
```

When the window is hidden to tray, `viewport.outer_rect` may return stale or zeroed coordinates. Storing these could corrupt the persisted window position, causing the app to reopen off-screen.

**Fix:** Guard the placement cache update with a check that the window is visible.

---

### H9 — COM Init Failure Proceeds Without Error Handling
**File:** `src/directory_tree_places/windows.rs:46-67`

When `CoInitializeEx` returns an unexpected HRESULT (neither success, `S_FALSE`, nor `RPC_E_CHANGED_MODE`), `should_uninitialize` is set to `false` and execution continues. However, subsequent `SHGetKnownFolderPath` and `IShellItem` calls **will fail or crash** without valid COM.

**Fix:** On unexpected HRESULT, log an error and skip shell namespace enumeration for that entry.

---

### H10 — `recreate_surface` No-Op for Unsafe Surfaces Causes Error Loop
**File:** `patched-crates/egui-wgpu/src/winit.rs:381-401`

```rust
let Some(window) = old_state.window_for_surface_recreation.clone() else {
    return Ok(()); // Surface stays in Lost state permanently
};
```

For surfaces created via `set_window_unsafe`, `window_for_surface_recreation` is `None`. After a `CurrentSurfaceTexture::Lost`, the surface enters an **infinite loop**: try to render → get Lost → set needs_recreate → enter recreate_surface → return Ok(()) without doing anything → repeat. Each iteration produces a log message at L743.

**Fix:** Either store the window reference for unsafe surfaces too, or treat a non-recreatable lost surface as a fatal error that tears down the viewport.

---

### H11 — `file_modified_unix_by_index` Not Permuted During Batch Re-sort
**Cross-cutting:** `src/app/view_status.rs`, `src/app/image_management/`

The scanner populates `file_modified_unix_by_index: Vec<Option<i64>>` parallel to `image_files`. Various caches use `permute_usize_set`/`permute_usize_hashmap` during batch sort transitions, but `file_modified_unix_by_index` is a `Vec` — it is **not permuted** by these utilities. If the image list is re-sorted during a scan batch merge, indices become misaligned, causing the OSD to show wrong modified-time data.

**Fix:** Either add a `permute_vec_option` utility and apply it, or rebuild `file_modified_unix_by_index` from scratch during batch merges.

---

### H12 — Detached Delete Threads Leak Files on Rapid Shutdown
**File:** `src/app/rendering/file_ops.rs:159-174`

```rust
std::thread::spawn(move || {
    std::thread::sleep(Duration::from_millis(20));
    // ... delete file ...
});
```

The thread is **never joined**. On application shutdown, if the delete thread is mid-sleep, the OS may terminate the process before the file is deleted. The image is already removed from the list (line 177), so the user sees it as deleted but it remains on disk.

**Fix:** Use a thread handle collection that is joined on app exit, or use async file deletion that is awaited during `on_exit`.

---

## Medium-Severity Issues (25)

### M1 — ROOT Logic Split Across Two Sites
**File:** `patched-crates/eframe/src/native/epi_integration.rs:343-365`

The ROOT path in `update()` calls `app.update()` and `app.ui()` but NOT `app.logic()`. Logic runs in the integration before `update()`. If this pairing breaks during future refactoring, ROOT logic silently stops running. No compile-time check enforces the pairing.

### M2 — Child Viewport Skips ROOT Bookkeeping
**File:** `patched-crates/eframe/src/native/epi_integration.rs:343-351`

The child-viewport path skips `app.update()` / `app.ui()` entirely. The app must handle ALL shared state updates in `App::logic`. If upstream code configures something in `update()` that must run for each UI pass, it is missed for child viewports.

### M3 — Breaking API Change to `App::logic` Trait
**File:** `patched-crates/eframe/src/epi.rs:200-202`

`App::logic` now takes a `LogicPass` parameter. This is a breaking change relative to upstream eframe. The default implementation ignores `pass`, meaning an app that doesn't override `logic` operates without viewport awareness.

### M4 — Duplicate Resize Detection in Two Handlers (glow)
**File:** `patched-crates/eframe/src/native/glow_integration.rs:583-613, 999-1035`

Both `run_ui_and_paint` and `on_window_event` independently detect and apply resizes. A stale resize event arriving just before a paint causes both handlers to fire. Idempotent for GL but inefficient.

### M5 — Same `logic()` Call Pattern as Glow (wgpu)
**File:** `patched-crates/eframe/src/native/wgpu_integration.rs:770-779`

Same concern as MGDP1: logic is called for every viewport paint, requiring debouncing. Multiple calls per frame can double-drain queues or double-count timers if not designed for it.

### M6 — Node Cap Reached Is Permanent
**File:** `src/app/directory_tree/mod.rs:867-912`

When `MAX_DIRECTORY_TREE_NODES` (8192) is exceeded, remaining children are silently skipped and the node is marked `children_loaded = true`. Later re-expansion will NOT re-request children. The only recovery is app restart.

### M7 — Per-Frame Allocation in `is_places_sentinel_path`
**File:** `src/app/directory_tree/mod.rs:76-86`

`is_this_pc_tree_path` calls `this_pc_tree_path()` which constructs a `PathBuf::from(THIS_PC_TREE_PATH)` on every invocation. Called from hot UI paths (`directory_tree_node_icon_fields`, `directory_tree_node_expandable`, `toggle_expanded`, `process_directory_tree_events`).

### M8 — No Depth Limit on Ancestor Chain
**File:** `src/app/directory_tree/mod.rs:498-520`

The `for component in relative.components()` loop in `reveal_ancestor_chain` has no depth bound. On deep filesystems (NTFS/ext4 support hundreds/thousands of levels), this produces a very large `Vec<PathBuf>` and triggers a flood of children requests.

### M9 — Hardcoded Selection Colors Bypass Theme
**File:** `src/app/directory_tree/ui.rs:384-395`

Dark mode uses `Color32::from_gray(78)`, light mode uses hardcoded RGBA. Does not derive from `ThemePalette` and will clash with custom themes. The hover color correctly uses `palette.widget_hover`.

### M10 — Per-Frame Allocation in `draw_directory_tree_node` Hover
**File:** `src/app/directory_tree/ui.rs:845`

`node.browse_path.to_string_lossy()` allocates a `Cow<str>` for every tree node on every frame during painting — a per-frame allocation in the hot paint path.

### M11 — `send_scan_message` Busy-Waits on Full Channel
**File:** `src/scanner.rs:522`

The `try_send` loop sleeps 2ms on full. For the `crossbeam_channel::unbounded()` channels used in practice, `TrySendError::Full` never occurs, making this loop dead code. If a bounded channel is ever used, this adds latency.

### M12 — Settings Saver vs Hotkeys Saver Coalesce Ordering
**File:** `src/app/lifecycle.rs:96-149`

Settings-saver coalesces BEFORE sleeping; hotkeys-saver sleeps BEFORE coalescing. If the app exits right after `queue_hotkeys_save()`, the 50ms sleep might mean the thread hasn't consumed the message before channel drop. Mitigated by synchronous save in `on_exit`.

### M13 — Detached Thread Per Picker Request
**File:** `src/app/folder_picker.rs:150-176`

`std::thread::Builder::new().spawn(...)` creates a thread that is never joined. On app close, the thread may continue running inside `pollster::block_on` (native dialog). On some Linux compositors, the file dialog can outlive the application.

### M14 — Context Menu Paint from Auxiliary Viewport
**File:** `src/app/input/ui.rs` (diff ~325)

`paint_image_context_menu_if_open` calls `draw_context_menu_items` which can dispatch file operations. If painted from the directory tree viewport, file operations that modify `image_files` could race with the main window's rendering.

### M15 — `runtime_probe_completed` Set on Probe Failure
**File:** `src/hdr/monitor/state.rs:176-180`

`self.runtime_probe_completed = true` is set even when the probe returns `Err`. Callers that interpret this as "probing succeeded" without also checking `selection().is_some()` could enable HDR features without valid monitor data.

### M16 — `MONITOR_DEFAULTTOPRIMARY` Fallback for Probe Point
**File:** `src/hdr/monitor/windows.rs:152-153`

`MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY)` silently falls back to the primary monitor if the probe point lands in a gap between monitors. The primary might be SDR while the target is HDR (or vice versa), producing incorrect HDR gating.

### M17 — Alpha-Zero Draw Trick for Demosaic Bake
**File:** `src/app/rendering/standard/hdr_draw.rs:30-74`

Submits an invisible HDR plane at alpha 0.0 solely to trigger the `prepare()` callback for GPU RAW demosaic. If the callback system is refactored to skip alpha=0 draws, RAW demosaic silently breaks.

### M18 — `ComGuard::new()` Result Ignored in `extract_tile`
**File:** `src/wic/tiled_source.rs:386` (same as C5, but for the pattern itself)
See C5.

### M19 — `catch_unwind` Over `unsafe` COM Code
**File:** `src/loader/decode/directory_tree_thumb.rs:392-401`

```rust
let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    source.generate_full_image_preview(max_side, max_side)
}));
```

If the `unsafe` block inside `generate_full_image_preview` panic, COM objects could be left in an indeterminate state. `AssertUnwindSafe` is technically unsound here.

### M20 — `process_read_dir` Skipping Logic Visually Confusing
**File:** `src/scanner.rs:211-241`

The callback checks `is_non_browsable_system_directory(path)` for the parent directory at line 212 and again for each child at line 226. Both use the same function — correct but the double-check pattern is non-obvious.

### M21 — `embedded_panel` Width Estimate Fallback
**File:** `src/app/rendering/mod.rs:60-68`

On the first frame after the directory tree panel is created, `PanelState::load` may return `None` because egui hasn't persisted state yet, causing a single-frame layout glitch.

### M22 — Full Settings Clone on Every Save
**File:** `src/settings.rs:868-878`

Every `save()` clones the full `Settings` struct (hundreds of fields). On modern hardware negligible, but on Linux the clone is only needed to force `hdr_native_surface_enabled = false`, which could be done by temporary reassignment.

### M23 — Tile Silently Dropped When Larger Than Cache Capacity
**File:** `src/tile_cache.rs:152-156`

If a single tile exceeds the cache capacity (misconfiguration), it is silently dropped with no error or log.

### M24 — Startup Panic on Invalid `SIV_LOG_LEVEL`
**File:** `src/startup/logging.rs:93-94`

```rust
flexi_logger::Logger::try_with_env_or_str(...).expect("Failed to initialize logger");
```

If the env var is set to an unrecognized string, the app panics before any window is created. Users see no error message.

### M25 — `RPC_E_CHANGED_MODE` Constant Formatting
**File:** `src/wic/com.rs:22`

```rust
const RPC_E_CHANGED_MODE: HRESULT = HRESULT(0x8001_0106_u32 as i32);
```

The underscore placement (`0x8001_0106` instead of `0x80010106`) is confusing but numerically equivalent. Not a bug, but a maintenance trap.

---

## Low-Severity Issues (Selected — ~30 Total)

- **`filesystem_ancestor_chain` Infinite Loop on Root Paths (Unix)** — `src/app/directory_tree/ui.rs:1522-1528`: On Unix, `pop()` on `/` returns `/`, potentially infinite-looping the `while current.pop()` loop. Mitigated because `volume_root_for_path` should return `Some("/")` for rooted paths.
- **Null Pointer Stored Every Frame** — `src/app/eframe_app.rs:257`: `store(std::ptr::null_mut(), ...)` on every frame is an undocumented defensive pattern.
- **`unreachable!()` in draw.rs** — `src/app/rendering/standard/draw.rs:422`: Adding a new `TransitionStyle` variant would panic at runtime rather than fail to compile.
- **`transmute` for `RtlGetVersion`** — `src/startup/logging.rs:197`: Type-safe wrapper from `windows` crate would be safer.
- **`log::info!` on Every HDR Probe** — `src/hdr/monitor/windows.rs:156`: Fires every ~200ms, producing log spam at default info level.
- **`sysinfo` Refresh Every Preload Call** — `src/app/image_management/preload.rs:188-189`: Can be slow on Linux (`/proc/meminfo`).
- **Directory Tree Thumb Decoder Duplicates Main Decode Dispatch** — `src/loader/decode/directory_tree_thumb.rs:128-264`: Any new format added to main decoder must also be added here.
- **`poll_folder_picker_results` Repaints Every Frame** — `src/app/folder_picker.rs:191`: Uses `request_repaint()` instead of `request_repaint_after()`.
- **Silent Index Drops During Permutation** — Multiple files: Entries with `old_idx >= old_to_new.len()` are silently dropped — intentional (deleted files) but should be debug-logged.
- **Windows FILETIME Conversion Overflow** — `src/ui/osd.rs` (diff): `ticks = (unix_secs + 11644473600) * 10_000_000` can overflow `i64` for extreme values.
- **Double ROOT Redraw After Child Paint** — `patched-crates/egui-wgpu/src/winit.rs:784-804`: Both `window.request_redraw()` AND `ctx.request_repaint_of(ROOT)` called.
- **Temporary Buffer Allocation Per Upload** — `patched-crates/egui-wgpu/src/renderer.rs:36`: `vec![0u8; size]` for full vertex/index data each fallback.

---

## Performance Observations

1. **Hot-path allocations:** `to_string_lossy()` per tree node per frame, `PathBuf::from()` per `is_places_sentinel_path` call, and `to_lowercase()` per sort-key — these allocate in O(n) or O(n log n) per frame. Consider caching or precomputing.

2. **Busy-polling:** `send_scan_message` with 2ms busy-wait on a full channel. The channel is unbounded in practice, so this is dead code, but should be cleaned up.

3. **`sysinfo` refresh per preload call:** Could be throttled to once per second.

4. **Background thread orphans:** On network timeouts, orphan threads accumulate consuming OS thread resources. See C4.

---

## Test Coverage Assessment

**Excellent:** The `directory_tree` module has 973 lines of tests covering stale generation rejection, error recording, layout clamping, sort ordering, UNC paths, known-folder aliasing, and more.

**Adequate:** Image management tests have been updated with new fields (`directory_tree_strip_cache`, `folder_picker`).

**Missing:** No tests for:
- `prefetch_circular_distance` (the operator precedence bug would be caught by a unit test)
- Worker orphan thread behavior on timeout
- `file_modified_unix_by_index` permutation during batch re-sort
- COM init fallback paths
- Eframe patched code (no test infrastructure for patched crates)

---

## Architecture Notes

1. **Massive `ImageViewerApp` struct:** The main app struct has grown to 600+ fields. Consider further decomposition (e.g., extract directory tree into a sub-struct owned by the app, similar to how `HdrMonitor` is separated).

2. **Code duplication in directory tree thumb decoder:** The format dispatch in `directory_tree_thumb.rs` duplicates most of the main decode dispatch. Consider a shared trait or macro to reduce maintenance burden.

3. **Lock granularity:** The tree and list each have their own `Mutex`, which is good. But `try_lock` patterns throughout mean lock failures produce degraded behavior (stale data, skipped updates) rather than waiting — this is a conscious design choice documented in the code.

4. **Cross-platform robustness:** The `directory_tree_places` module has `#[cfg]`-gated Windows and Unix implementations with a `stub` fallback. This is well-structured. The `fs.rs` shared module is platform-agnostic.

---

## Recommendations Summary

| Priority | Action |
|----------|--------|
| **Immediate** | Fix operator precedence in `prefetch_circular_distance` (C1) |
| **Immediate** | Add re-entrancy guard to `RepaintNow` chain (C2) |
| **High** | Fix or remove `any_active_output_supports_hdr` policy contradiction (C3) |
| **High** | Implement orphan thread cleanup (C4) |
| **High** | Check `ComGuard::new()` return value in `extract_tile` (C5) |
| **High** | Fix `Instant::duration_since` panic potential (H1) |
| **High** | Add `file_modified_unix_by_index` permutation (H11) |
| **High** | Use `Release`/`Acquire` ordering on `AtomicPtr` (H6) |
| **Medium** | Increase/resize `children_request_tx` channel (H7) |
| **Medium** | Guard window placement cache when hidden (H8) |
| **Medium** | Add depth limit to ancestor chain traversal (M8) |
| **Medium** | Use theme palette for selection colors (M9) |
| **Medium** | Cache hot-path string allocations (M7, M10) |

---

*End of review. All file references are relative to the repository root at `H:\Rust\SimpleImageViewer`.*
