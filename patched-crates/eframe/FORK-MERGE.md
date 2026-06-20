# eframe / egui-wgpu fork merge checklist

Simple Image Viewer vendors patched copies under `patched-crates/eframe` and
`patched-crates/egui-wgpu`. Search for `SimpleImageViewer patch` or
`Simple Image Viewer fork` before merging upstream releases.

## When to use this doc

After bumping `eframe` / `egui` / `egui-wgpu` in `Cargo.toml`, diff each file
below against upstream at the new tag and re-apply the fork blocks if still
needed.

## patched-crates/egui-wgpu

| File | Topic | Re-apply if |
|------|--------|-------------|
| `src/renderer.rs` | `queue_write_with_fallback` | Upstream still panics when `write_buffer_with` returns `None` on shared multi-viewport renderers (Detached nav, Linux/X11, software backends). |

## patched-crates/eframe

| File | Topic | Re-apply if |
|------|--------|-------------|
| `src/native/run.rs` | Synchronous `RepaintNow` chain on all desktop OSes | Upstream still limits immediate repaint chaining to Windows only. |
| `src/native/wgpu_integration.rs` | `App::logic` before every viewport paint | Upstream still calls `logic` only from ROOT `update`. |
| `src/native/wgpu_integration.rs` | Autosave on child viewport paint (ROOT window) | Upstream still gates `maybe_autosave` on ROOT paint only. |
| `src/native/glow_integration.rs` | Same `logic` + autosave patches as wgpu | Same as above for glow backend. |
| `src/native/epi_integration.rs` | ROOT `update` skips duplicate `logic` | Must stay paired with wgpu/glow `logic` call sites. |
| `src/epi.rs` | `LogicPass`, `Frame::painting_viewport_id`, `App::logic` docs | Fork contract: `Frame` = ROOT integration; pass names painting viewport. Re-apply when upstream changes `App` trait or `Frame`. |
| `src/web/app_runner.rs` | `LogicPass::root()` on web `logic()` | Web has no multi-viewport paint fork; pass must stay API-compatible. |

## Merge workflow

1. Note current upstream tags in `Cargo.lock` for eframe/egui/egui-wgpu.
2. Copy upstream sources into `patched-crates/*` (or merge in a scratch branch).
3. Grep `SimpleImageViewer` / `Simple Image Viewer fork` — restore every block.
4. Build desktop targets; smoke-test **Embedded** and **Detached** directory-tree navigation on Windows and at least one non-Windows OS.
5. Verify settings autosave while the detached nav window stays focused (ISSUE-20 regression).
6. Verify detached nav paint: scan/dir-tree drains still run; HDR/placement/dialog code runs only on ROOT pass (`LogicPass::is_root()`).

## App integration (Simple Image Viewer)

- **`logic_shared`** (`src/app/logic_update.rs`): tray, scan, loaders, dir-tree — runs at most once per 4ms wall clock even if ROOT + aux both paint.
- **`logic_root_only`**: window placement cache, HDR monitor/swap-chain, drag-drop, fullscreen, folder dialog — **`pass.is_root()` only**.
- Do not use `frame.winit_window()` / HDR frame APIs from aux-triggered passes unless intentionally ROOT-scoped.

## Upstream follow-up (optional)

- RepaintNow parity on Linux/macOS — may land upstream via egui PRs (see comments in `run.rs`).
- Multi-viewport shared `Renderer` staging — track egui issues #7840, #7434.
- Per-viewport `LogicPass` / autosave timer — `LogicPass` landed in fork; optional upstream submission.
