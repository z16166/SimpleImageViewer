# eframe / egui-wgpu / wgpu fork merge checklist

Simple Image Viewer vendors patched copies under `patched-crates/eframe`,
`patched-crates/egui-wgpu`, `patched-crates/wgpu-hal`, and (minimally)
`patched-crates/wgpu-core`. Search for `SimpleImageViewer patch` or
`Simple Image Viewer fork` before merging upstream releases.

## When to use this doc

After bumping `eframe` / `egui` / `egui-wgpu` / `wgpu` / `wgpu-hal` in
`Cargo.toml`, diff each file below against upstream at the new tag and re-apply
the fork blocks if still needed.

## Platform scope

| Area | Win10/11, macOS, Linux | Win7 x64 (`legacy_win7` only) |
|------|--------------------------|-------------------------------|
| eframe multi-viewport / RepaintNow | Yes | Same binary feature set is off on Win7 build |
| egui-wgpu UBO 32-byte padding | Yes (harmless alignment) | Required for ANGLE GLES |
| wgpu-hal `legacy-win7-gles` | **Not compiled** | ANGLE/EGL cascade + WGL fallback |
| App `launch.rs` GLES setup | **Not compiled** | Forces `Backends::GL` |

Win7 work is gated by the app feature `legacy_win7` and wgpu-hal feature
`legacy-win7-gles`. Standard release builds do not enable those flags.

## patched-crates/egui-wgpu

| File | Topic | Re-apply if |
|------|--------|-------------|
| `src/renderer.rs` | `queue_write_with_fallback` | Upstream still panics when `write_buffer_with` returns `None` on shared multi-viewport renderers (Detached nav, Linux/X11, software backends). |
| `src/renderer.rs` | `UniformBuffer` padded to 32 bytes | ANGLE GLES rejects 24-byte egui UBO (`BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED`). Safe on all targets; keep `_pad0` / `_ubo_align_pad` in sync with WGSL. |
| `src/egui.wgsl` | `_pad0`, `_ubo_align_pad` in `Locals` | Must match 32-byte Rust struct above. |

## patched-crates/wgpu-hal (Win7 only: `legacy-win7-gles`)

Enabled from root `Cargo.toml` via `legacy_win7` -> `wgpu-hal/legacy-win7-gles`.

| File | Topic | Re-apply if |
|------|--------|-------------|
| `Cargo.toml` | Features `windows-angle`, `legacy-win7-gles` | Feature wiring lost on upstream bump. |
| `src/gles/mod.rs` | Route Windows + `legacy-win7-gles` to `win7_gles` composite | Upstream changes GLES instance selection on Windows. |
| `src/gles/win7_gles.rs` | Runtime cascade: angle-d3d11 -> angle-opengl -> wgl -> angle-warp | Core Win7 backend selection; env `WGPU_GL_BACKEND` for forced tier. |
| `src/gles/egl.rs` | `init_angle_instance`, ANGLE platform types, `choose_config` | ANGLE/EGL init for D3D11, OpenGL ES/desktop GL, WARP; fix OPENGL platform constants (`0x320D` / `0x320E`); query `WINDOW_BIT` for presentable surfaces. |
| `src/gles/wgl.rs` | `init_wgl` exposed for cascade | WGL tier must remain callable from `win7_gles`. |
| `src/gles/adapter.rs` | `surface.is_presentable()` | Composite `Surface` wrapper hides `presentable` field. |

Redistributables for the Win7 CI zip (not patched crates):

- `libEGL.dll`, `libGLESv2.dll` -- bundled from Chrome on the runner (latest tested; imports only core OS DLLs).
- `d3dcompiler_47.dll` -- copied from `redist/d3dcompiler_47.dll` (Chrome 109-era build; avoids UCRT `api-ms-win-crt-*` imports from System32).

## patched-crates/eframe

| File | Topic | Re-apply if |
|------|--------|-------------|
| `src/native/run.rs` | Synchronous `RepaintNow` chain on all desktop OSes | Upstream still limits immediate repaint chaining to Windows only. |
| `src/native/run.rs` | `sync_repaint_in_progress` reentrancy guard | Prevents nested `RepaintNow` → `run_ui_and_paint` during one event dispatch. |
| `src/native/wgpu_integration.rs` | `App::logic` before every viewport paint | Upstream still calls `logic` only from ROOT `update`. |
| `src/native/wgpu_integration.rs` | Autosave on child viewport paint (ROOT window) | Upstream still gates `maybe_autosave` on ROOT paint only. |
| `src/native/glow_integration.rs` | Same `logic` + autosave patches as wgpu | Same as above for glow backend. |
| `src/native/epi_integration.rs` | ROOT `update` skips duplicate `logic` | Must stay paired with wgpu/glow `logic` call sites. |
| `src/epi.rs` | `LogicPass`, `Frame::painting_viewport_id`, `App::logic` docs | Fork contract: `Frame` = ROOT integration; pass names painting viewport. Re-apply when upstream changes `App` trait or `Frame`. |
| `src/web/app_runner.rs` | `LogicPass::root()` on web `logic()` | Web has no multi-viewport paint fork; pass must stay API-compatible. |

## Merge workflow

1. Note current upstream tags in `Cargo.lock` for eframe/egui/egui-wgpu/wgpu/wgpu-hal.
2. Copy upstream sources into `patched-crates/*` (or merge in a scratch branch).
3. Grep `SimpleImageViewer` / `Simple Image Viewer fork` -- restore every block.
4. Re-apply Win7 blocks in `wgpu-hal` if `legacy-win7-gles` feature still exists.
5. Build desktop targets; smoke-test **Embedded** and **Detached** directory-tree navigation on Windows and at least one non-Windows OS.
6. Build `--features legacy_win7` on Windows; smoke-test Win7 VM or forced `WGPU_GL_BACKEND` tiers.
7. Verify settings autosave while the detached nav window stays focused (ISSUE-20 regression).
8. Verify detached nav paint: scan/dir-tree drains still run; HDR/placement/dialog code runs only on ROOT pass (`LogicPass::is_root()`).

## App integration (Simple Image Viewer)

- **`logic_shared`** (`src/app/logic_update.rs`): tray, scan, loaders, dir-tree — runs at most once per 4ms wall clock even if ROOT + aux both paint.
- **`logic_root_only`**: window placement cache, HDR monitor/swap-chain, drag-drop, fullscreen, folder dialog — **`pass.is_root()` only**.
- Do not use `frame.winit_window()` / HDR frame APIs from aux-triggered passes unless intentionally ROOT-scoped.
- **Immediate viewports** (`show_viewport_immediate`) do not receive `App::logic` during `render_immediate_viewport`; Simple Image Viewer uses **`show_viewport_deferred` only** for the detached directory-tree window.
- **`viewpaint_app` raw pointer** (`src/app/directory_tree/mod.rs`, `app.rs`): Detached strip GPU upload and image-list context menu read `ImageViewerApp` via `AtomicPtr` on the UI thread only. This assumes eframe keeps the app as a stable `Box<dyn App>` for the process lifetime (no re-box / move of the instance). Re-verify after any upstream change to `App` ownership or viewport paint scheduling; if upstream ever moves the app object, replace the pointer with an explicit cross-viewport snapshot or channel.

## Upstream follow-up (optional)

- RepaintNow parity on Linux/macOS — may land upstream via egui PRs (see comments in `run.rs`).
- Multi-viewport shared `Renderer` staging — track egui issues #7840, #7434.
- Per-viewport `LogicPass` / autosave timer — `LogicPass` landed in fork; optional upstream submission.
