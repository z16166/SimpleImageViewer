# OOM Fix Notes

Branch: `fix/wgpu-oom-panic-dialog`

## Problem

Fast image flipping with large JPEG/PSD files could create severe CPU/GPU memory pressure. The observed crash report showed `wgpu error: Out of Memory` from texture upload. In the same failure mode, the panic hook attempted to show a dialog, but the dialog was not reliably visible.

The likely pressure sources were:

- Multiple decoded/preloaded static images waiting for or entering GPU upload.
- Several large texture uploads happening in the same frame.
- Background preloading continuing while system memory was already low.
- Stale preloaded GPU textures remaining alive until the texture cache hit its size limit.
- `wgpu` treating uncaptured `OutOfMemory` as fatal by default.

## Changes

### WGPU OOM Handling

- Installed a `wgpu::Device::on_uncaptured_error` handler in the patched `egui-wgpu` render state.
- `wgpu::Error::OutOfMemory` is now logged instead of going through wgpu's default fatal panic path.
- Validation and internal wgpu errors still panic, so programming errors are not hidden.

### Crash Dialog Visibility

- Replaced the Windows panic hook dialog path with native `MessageBoxW`.
- The dialog uses `MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND | MB_TASKMODAL`.
- Non-Windows platforms keep the existing `rfd::MessageDialog` path.

### Per-Frame SDR Upload Budget

- Added a 32 MB per-frame SDR upload budget.
- Current image uploads bypass this budget so navigation remains responsive.
- Background static SDR images and HDR SDR fallback uploads are deferred with `loader.repush(...)` when they would exceed the frame budget.
- Tiled bootstrap previews are also counted in the upload estimate.

### Background Preload Memory Guard

- Added a 1 GB available-memory guard for background preloading.
- If available memory is below the guard, background preloads are skipped and non-current preloaded assets are cleared.
- Current image loading remains allowed.

### Conservative Preload Budgeting

- Background preload candidates now use a conservative decoded-size estimate of `file_size * 12`.
- Oversized first candidates are skipped instead of being force-preloaded.
- After at least one candidate has been accepted, exceeding the direction budget stops that preload direction.

### Stale GPU Texture Eviction

- Extended `evict_distant_prefetch_caches()` to remove distant entries from `texture_cache`.
- This covers ordinary static SDR textures that were uploaded by preload and then became stale before the texture cache capacity was reached.
- Distant `animation_cache` entries are also removed.

## Tests And Verification

Commands run:

```powershell
cargo test --bin SimpleImageViewer "preload_budget"
cargo test --bin SimpleImageViewer "preload_direction_skips"
cargo test --bin SimpleImageViewer "background_preload_memory_guard"
cargo test --bin SimpleImageViewer "sdr_upload_budget"
cargo test --bin SimpleImageViewer "evict_distant_prefetch_caches"
cargo fmt --check
cargo check --bin SimpleImageViewer
```

All listed commands passed.

## Notes And Remaining Risk

- This change reduces memory and GPU upload peaks; it does not implement true region decoding for all large JPEG/PSD files.
- Large PSD/JPEG decoders can still allocate full RGBA buffers before tiled rendering if the format path requires full decode.
- `mimalloc` may continue to retain freed memory in the process working set, so OS-reported memory can remain high after pressure is reduced.
- `wgpu::OutOfMemory` is logged rather than fatal, but a real GPU memory exhaustion event can still cause missing textures or delayed uploads until pressure drops.
