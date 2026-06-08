# Changelog

All notable changes to this project will be documented in this file.

## [2.4.3] - 2026-06-09

### Added
- **Skip paired RAW files**: New Library setting lets you hide RAW files when a matching JPG/JPEG is in the same folder—handy for cameras that save both per shot (e.g. Sony RAW+JPEG), so browsing stays fast without heavy RAW decoding.

## [2.4.2] - 2026-06-08

### Changed
- **RAW preview setting clarity**: Renamed and expanded the **High-Quality RAW preview** option in Settings (all languages) so it clearly explains performance mode (embedded previews only) versus demosaic mode (~2048/4096 on HDR-capable displays).

### Fixed
- **RAW quality mode behavior**: The RAW preview quality toggle now controls loading as intended—performance mode keeps camera embedded previews; high-quality mode demosaics when the embedded preview is too small.
- **RAW on HDR displays**: High-quality RAW files on HDR monitors now route through the HDR rendering pipeline after demosaic completes, instead of staying on the SDR path.
- **RAW refinement sharpness**: Fixed high-quality RAW previews that could look sharp at first and then turn softer after refinement finished, especially on common ~10MP cameras.
- **Small embedded RAW thumbnails**: Cameras that ship tiny embedded previews (for example some Epson and Fuji RAW files) no longer skip demosaic incorrectly in high-quality mode.
- **RAW setting refresh**: Switching RAW preview quality now clears stale preloaded neighbors so arrow-key browsing shows the correct preview type immediately.

## [2.4.1] - 2026-06-08

### Added
- **Diagnostic logging controls**: File logging and log level can be enabled with environment variables, and detailed startup/preload diagnostics are gated behind local-only features.

### Fixed
- **Large image memory pressure**: Reduced CPU/GPU memory spikes while rapidly browsing large JPEG, TIFF, PSD/PSB, RAW, and HDR images by throttling background texture uploads, evicting stale preloaded textures sooner, and tuning mimalloc to return burst allocations more promptly.
- **Large tiled image preloading**: Nearby oversized images can now preload lightweight tiled previews without being blocked by full-RGBA decode estimates, greatly reducing `loading...` flashes when browsing giant Hubble-style images.
- **Transition smoothness**: Background texture uploads are deferred during active transition animations so page flip, ripple, and curtain transitions remain smooth.
- **HDR/AVIF page flips**: Fixed a brief black flash that could appear when flipping between HDR AVIF photos and standard images, including when changing direction at the ends of a folder.
- **Large-image page flips**: Page Flip transitions between very tall and very wide tiled images now preserve the outgoing image's shape and use the correct direction when wrapping around a folder.
- **Loading indicator polish**: The `loading...` message is now shown only when there is truly nothing useful to display, so normal image-to-image navigation feels cleaner and faster.
- **Crash handling**: Windows crash dialogs are more visible, and GPU out-of-memory reports from wgpu are logged instead of immediately taking the default fatal panic path.
- **RAW scene-linear HDR**: LibRaw 16-bit output is unpacked row-by-row using each buffer's stride, so padded rows no longer corrupt HDR pixels. Tiled RAW refinement always applies the resolved orientation flip before `develop()`.

## [2.4.0] - 2026-06-06

### Added
- **Custom context menu**: A new **Context Menu** tab in Settings lets you reorder built-in actions, add separators, and define custom menu items that launch an executable or a full command line with `%1` as the current image path. Built-in items cover all current-image operations; defaults match the previous hardcoded menu.
- **OSD file size**: The bottom-left on-screen display now shows the current image file size (bytes, KB, MB, or GB).
- **Ctrl+0 zoom reset**: **Ctrl+0** resets zoom to 100%, alongside the existing `*` shortcut (Chrome-style).

### Changed
- **Log file location**: When file logging is enabled, logs are written to the OS temporary directory instead of the settings folder.

### Fixed
- **Context menu apply/load validation**: Applying or loading a configuration with no enabled menu items (separators excluded) is rejected or falls back to defaults so the runtime menu cannot become empty.
- **Context menu upgrade merge**: New built-in menu items from a newer app version are merged into existing saved configurations.
- **Hotkey config upgrade**: Upgrading from hotkeys config version 1 automatically adds **Ctrl+0** to zoom reset without overwriting your other bindings.
- **Hotkey text input**: Keyboard layouts that report shortcuts as text events (for example `+`, digits, and letters) reuse the same key parser as saved hotkey strings.
- **Custom command quoting**: Executable paths that are already wrapped in quotes but contain internal quote characters are escaped correctly when building the command line.
- **Context menu settings UX**: Apply only commits on **Apply**; drag-and-drop reordering; keyboard navigation and shortcuts; fixed table headers; non-intrusive help dialog; and other layout and interaction refinements from the settings tab.
- **Simplified Chinese UI**: Translated the context menu **Action name** label that was still shown in English.

## [2.3.6] - 2026-06-06

### Fixed
- **Fullscreen transitions and stability**: Fixed an issue where switching between windowed and fullscreen modes (via F or F11) could cause the main window to disappear or become unresponsive.
- **Visual transition quality**: Significantly reduced screen flicker and flashes when entering fullscreen mode, delivering smoother and higher-quality display transitions.

## [2.3.5] - 2026-06-06

### Fixed
- **Gallery launch and file drop**: Fixed an issue where launching the viewer by double-clicking a photo in the system gallery or dropping a file onto the window would open the last viewed image instead of the targeted one.

## [2.3.4] - 2026-06-05

### Fixed
- **Fullscreen transitions**: Entering and leaving full-screen mode no longer flashes a stretched, clipped, or top-left-aligned stale frame during the window resize handoff.
- **Context menu fullscreen toggle**: Fixed a fullscreen toggle deadlock that could occur when using the context menu.
- **Hotkeys settings layout**: Improved hotkey settings layout so controls stay readable and aligned.

## [2.3.3] - 2026-06-03

### Added
- **Smooth HDR Transitions**: High-dynamic-range (HDR) images now transition smoothly without flickering or briefly dropping back to standard SDR brightness.
- **Large HDR Photo Support**: Large (tiled) HDR photos now support smooth transition animations instead of hard-cutting between images.

### Fixed
- **Navigation Lockup**: Fixed an issue where the viewer could get stuck on a black loading screen when navigating between large photos with transitions disabled.
- **Tiled Image Reliability**: Added defensive loading checks to prevent images from getting stuck in a loading state when system caches are evicted or inconsistent.

## [2.3.2] - 2026-06-03

### Added
- **F5 List Refresh**: Press F5 to refresh the folder's image list. The scan runs asynchronously in the background, keeping the user interface completely responsive. It retains your current viewing context—including active image, zoom/pan position, rotation, and playing GIF/APNG animations—so the screen never flickers or goes blank. If the current image was deleted from the disk, the viewer smoothly falls back to the first available image in the folder. Slideshow playback automatically pauses and resumes once the scan finishes.
- **HDR Ripple Transition**: The **Ripple (Water)** transition effect now fully supports high-dynamic-range (HDR) displays. When browsing HDR photos or images with gain-map details, the expanding water ripple animation runs on the GPU's HDR float plane, preserving full highlight brightness and vivid colors during the transition.

### Fixed
- **Glitch-Free Transitions**: Fixed a masking bug in the Ripple transition where the expanding circle did not discard pixels of the old image correctly, ensuring smooth and glitch-free animations.
- **Seamless Hotkey Upgrades**: Upgrading the application no longer triggers persistent red OSD warning messages due to newly introduced shortcuts (like the F5 refresh hotkey) missing from your old configuration file. The viewer now automatically populates and saves missing shortcuts with their default bindings on exit without flashing warnings.
- **Background Resource Efficiency**: Optimized HDR fallback refinement to run on-demand only for the current active photo, significantly reducing CPU/GPU overhead during navigation. Background queues and in-flight channels clean up late-arriving tasks more reliably, preventing resource leaks.
- **Dead i18n Strings**: Cleaned up obsolete warning messages in all supported language localization files to keep translation files tidy.

## [2.3.1] - 2026-06-02

### Fixed
- **HDR and AVIF browsing**: Flipping through photos with **No transition** is less likely to show a black frame, especially when you change direction mid-folder or browse recently preloaded images.
- **AVIF brightness after navigation**: Reverse browsing no longer leaves some images stuck too dark or too bright, and exposure adjustments keep working on HDR displays.
- **HDR ↔ SDR monitor moves**: When the window crosses an HDR and SDR display, the current image reloads more reliably so brightness and exposure controls stay in sync with the active screen.
- **Corrupt EXR headers**: Opening files that look like EXR but are incomplete or malformed no longer risks crashing the viewer.

### Improved
- **Faster still-image loading**: Common JPEG, PNG, and similar formats decode with less file I/O overhead, so nearby images in a folder feel snappier to open and preload.
- **Large tiled HDR previews**: Preview quality follows whether you are viewing in HDR or SDR mode, with fewer wasted decode steps on the wrong path.
- **Display capability changes**: Clearing stale preloaded images when HDR output mode changes is smoother and less likely to stall the UI on large folders.
- **File association / IPC launch**: Opening the viewer from another app handles local socket path quirks more gracefully on some systems.

## [2.3.0] - 2026-06-02

### Added
- **Custom hotkeys**: Remap keyboard shortcuts, mouse wheel actions, and modifier-assisted mouse clicks from the new **Hotkeys** tab in Settings.

### Changed
- **Settings layout**: The settings panel uses vertical tabs and clearer sections for browsing, viewing, slideshow, music, appearance, and system options.
- **Default interface size**: New installs default to a slightly smaller on-screen text size for a less crowded layout.
- **Slideshow playback**: Auto-advance slideshows always loop through the folder.

### Fixed
- **Hotkey setup feedback**: You are notified when saved hotkey settings cannot be loaded or contain conflicts, instead of shortcuts failing silently.
- **Hotkey warnings with OSD off**: Hotkey problem messages still appear when the on-screen display is hidden.
- **Settings polish**: Library scan status is no longer shown twice; the System tab has more comfortable spacing; settings toggles use checkboxes that read better in dark themes.

## [2.2.2] - 2026-05-30

### Added
- **Windows per-monitor wallpaper target**: You can choose which monitor receives wallpaper updates instead of applying to all screens at once.
- **m3u dynamic playlist expansion**: `.m3u` entries are expanded at playback time, and `Next/Prev file` navigation now follows expanded tracks consistently.

### Changed
- **Settings and layout polish**: Settings pages were reorganized and several mixed-height control rows were aligned for cleaner, more consistent spacing.

### Fixed
- **Playback dedup across playlists**: Tracks parsed from `.m3u` are now deduplicated against the base playlist by absolute path, and fully deduplicated `.m3u` files are skipped without breaking navigation.
- **Transition/rendering resilience**: Transition state handling is more stable across HDR and decode failure paths.
- **Startup/resume behavior**: First-batch preload is deferred when resuming the last image to reduce startup contention.
- **Localization refresh on language switch**: Window title and related context-menu labels now update more reliably after changing language.
- **Remote file deletion safety**: Deleting remote files now shows an explicit confirmation before recycle-bin operations.

## [2.2.1] - 2026-05-28

### Added
- **Random slideshow order**: Turn on **Random order** in Slideshow settings to shuffle your folder during auto-advance. With loop enabled, each pass through the album uses a fresh shuffle so repeat viewings stay varied.

### Fixed
- **Maximized window on launch**: Reopening the app while maximized no longer flashes or briefly redraws the image at the wrong size—the window appears at full size right away.
- **Window position after maximize**: Closing while maximized remembers your normal (restored) window size and position correctly, instead of odd off-screen coordinates on the next launch.
- **Multi-monitor startup (windowed)**: If you closed on a secondary display at normal size, the app no longer briefly pops up on your primary monitor before settling on the display where you left it.
- **Multi-monitor startup (white client area)**: When reopening at normal size, the window background now shows the themed viewer color right away instead of flashing solid white before your last image appears.
- **Multi-monitor startup (maximized)**: If you closed while maximized on a secondary display, the next launch maximizes there again instead of jumping to the primary monitor.
- **Mislabeled iPhone / Live Photo files**: Exports or copies that use the wrong extension (for example a Live Photo motion clip saved as `.jpg`) now show a clear message pointing you to the paired still photo, instead of failing silently or wasting time on recovery attempts.
- **Mislabeled still images**: When a file’s extension does not match what is inside but it is still a viewable photo, the viewer is more likely to open it anyway—helpful for some iPhone and camera imports with incorrect filenames.

### Improved
- **Preloading and memory**: Background preloading uses memory more wisely—nearby images stay ready for quick browsing, while images far from the current one are released sooner. Compressed photos (HEIC, JPEG, and similar) count more realistically toward preload limits, so large folders feel smoother with less RAM pressure.
- **Faster jumps to preloaded images**: Switching to a nearby image you already preloaded reuses decoded data instead of loading again from scratch.
- **Cleaner return visits**: If you have **Resume viewing** enabled, the settings panel stays tucked away on launch so you land on your last folder and image with less clutter.


## [2.2.0] - 2026-05-25

### Added
- **Ultra HDR JPEG (gain map)**: Compatible phone and camera JPEGs with an ISO gain map can use **GPU-accelerated HDR compose** on HDR-capable displays, including **MPF-embedded** gain maps that were previously missed.
- **Large Ultra HDR JPEGs**: Very large tiled Ultra HDR photos can compose gain maps on the **GPU tile path**, so 8K-class images stay responsive when you adjust HDR headroom.
- **AVIF HDR gain map**: AVIF stills and animations with ISO gain maps route through the same **GPU-deferred compose** path as JPEG and HEIC, with animated AVIF sequences presented on the HDR float plane.
- **JPEG XL HDR gain map**: JPEG XL files carrying an ISO **`jhgm`** box defer gain-map compose to the GPU; precomposed Adobe **`base_hdr`** primaries display directly without a redundant forward compose pass.

### Fixed
- **Ultra HDR JPEG variants**: **ISO backward-compatible** and **BaseRenditionIsHDR** payloads decode and compose correctly instead of mis-reading the primary as SDR baseline.
- **AVIF without alpha**: Stills missing an alpha plane no longer show incorrect transparency after HDR decode.
- **AVIF colour (unspecified CICP)**: Unspecified colour metadata no longer blocks sensible HDR gain-map handling on typical phone and browser-style AVIFs.
- **HDR animations**: The first frame after opening an animated HDR asset now uses the **float HDR plane** immediately; exposure adjustments apply without waiting for a second navigation step.
- **HDR upscaling**: Bilinear sampling on the HDR plane reduces jagged edges when zoomed or upscaled.
- **JPEG XL Adobe HDR base**: Precomposed **`base_hdr`** JPEG XL masters no longer look incorrectly dim relative to their embedded HDR primary.
- **Tiled HDR tone mapping**: Large tiled HDR canvases tone-map consistently when the GPU compose path is active.

### Improved
- **Gain-map performance and memory**: Opening and scrubbing HDR headroom on gain-map JPEG, AVIF, HEIC, and JXL assets avoids redundant CPU compose and large buffer copies where the GPU deferred path applies; SDR fallback paths reuse shared buffers instead of cloning full frames.


## [2.1.5] - 2026-05-25

### Added
- **Apple HDR photos (HEIC)**: Compatible iPhone and other HEIC shots that include Apple's HDR gain map can show extra highlight detail when you view them on an HDR-capable display and raise HDR headroom in settings.

### Fixed
- **Apple HDR brightness**: The viewer now reads the camera's intended highlight headroom from file metadata instead of a generic default, so bright areas in gain-map–equipped HEIC photos look closer to what you saw on the phone.
- **Apple HDR (portrait photos)**: Fixed uneven bright patches and ghosting on many vertically shot HEIC HDR images when gain-map enhancement is active.
- **Apple HDR on SDR displays**: On a normal (non-HDR) screen, the viewer skips gain-map processing when it would not change the picture, which saves time and memory.

### Improved
- **Apple HDR performance**: Large HEIC HDR photos open faster, and HDR slider adjustments apply more responsively.


## [2.1.4] - 2026-05-23

### Added
- **HDR OSD**: The HDR status bracket line (**`[ … ]`**), drawn **above** the usual index/zoom/`[STATIC|TILED]` row, now includes **current exposure in stops** (`· +n.n EV`). It reflects `effective_hdr_tone_map_settings()`, so swapping monitors or flipping between native HDR and SDR tone‑mapped composition updates the stops you are actually applying.
- **HDR exposure settings**: Persisted **`hdr_exposure_ev_native`** and **`hdr_exposure_ev_sdr`** — separate EV for **native HDR swap chains** vs **tone‑mapping into an SDR framebuffer**. Legacy YAML keys **`hdr_exposure_ev`** still load (alias for the native slot). Settings UI tooltip text is mode‑aware (`locales/`).

### Fixed
- **HEIF / HEIC (HDR, no embedded colour descriptor)**: Stills whose container exposes **no NCLX colour box** and **no readable embedded ICC profile** previously inherited a **scene‑linear (`transfer = Linear`)** default. Normalised RGB from libheif is **display‑referred gamma**, so SDR previews looked **flat and milky** next to Chrome and the desktop photo stack. Metadata now follows the same **`sRGB`** transfer assumption as ICC‑tagged stills **without guessing primaries**.
- **Static HDR bookkeeping**: Installing a static HDR asset now records the cached SDR fallback in `hdr_sdr_fallback_indices`; the viewer paint path uses the same **`has_sdr_fallback`** signal as **`hdr_status`**, avoiding **“float‑plane HDR” OSD** while the canvas is actually blitting the **tonemapped fallback texture**.
- **`hdr_status` / OSD bookkeeping drift**: **`current_hdr_render_path`** now respects **`hdr_image_cache`** / **`hdr_tiled_source_cache`** for the active index whenever **`CurrentHdr*`** pointers are transiently cleared, mirroring **`tiled_canvas_matches_current_index`** guidance so the HDR line (including EV) is not suppressed while HDR content still applies.
- **SDR framebuffer path (HDR image‑plane shaders)**: On **`Rgba8Unorm` / `Bgra8Unorm`** targets — typical **Windows gamma** canvases — the HDR float‑plane **`encode_sdr`** path now distinguishes **manual piecewise sRGB OETF** in WGSL versus **\*UnormSrgb** surfaces where the GPU encodes linear output to gamma. Prevents **double gamma** (washed mid‑tones) when the HDR callback targets an 8‑bit **non‑`srgb`** surface.
- **`SdrToneMapped` + HDR float plane**: When the conservative output mode stays **`HdrRenderOutputMode::SdrToneMapped`** yet the swap chain exposes a **`TextureFormat`** target (`Rgba16Float`, `Bgra8Unorm`, …) and decoding produced an HDR float plane, **`select_render_backend`** now promotes **`PlaneBackendKind::Hdr`** instead of pinning to the stale CPU‑baked SDR texture path. Exposure / sliders and keyboard ±½ EV adjustments stay effective (static and tiled canvases).

### Improved
- **CICP (ITU‑T H.273)**: **`transfer_characteristics = 1` (BT.709)** and **`= 6` (SMPTE 170 BT.601‑like)** decode as **`Srgb`**, aligning phone / conformance HEIF with browser‑style unmanaged stills rather than **`Unknown`** (which skipped proper EOTF on the HDR plane).
- **HEIF transfer refine**: **`Unknown`** CICP transfer with **colour primaries = 1 (BT.709 / sRGB chromaticities)** is promoted to **`Srgb`**, restoring contrast on **10‑bit** primaries after depth‑heuristic passes.
- **HEIF NCLX overrides**: **Primaries 1 + H.273 transfers 1/6** narrows explicitly to **`Srgb`**‑like display codes inside **`heif_nclx_to_metadata`** (PQ / wider primaries retain strict CICP handling). Matching depth / unknown‑transfer helpers and the **no‑colour‑box** orphan path now converge on **`Srgb`** decoding assumptions for typical phone/desktop still parity.
- **CPU HDR→SDR tonemap fallback**: PQ and display‑referred **sRGB** masters can take the **IEC 61966‑2‑1** OETF curve instead of a generic **Reinhard + gamma** stack where that matches unmanaged browser output on physically SDR displays.
- **Diagnostics**: **`INFO`** line per decoded HEIF primary — resolution, **`transfer_function`**, profile kind (**LinearSrgb / Cicp / Icc**), **`cicp (primaries, transfer)`**, optional mastering peak guess, auxiliary gain‑map hints.

### Notes
- Behaviour changes focus on **HEIF/HEIC**, **persistent HDR exposure split by presentation path**, and **HDR SDR previews / tone‑mapping** when **native HDR swap chains differ from conservative `SdrToneMapped`** output. PQ / OpenEXR / Radiance native paths are unchanged except where tone‑map sliders or bookkeeping were tightened above.


## [2.1.3] - 2026-05-24

### Fixed
- **Linux HDR10 metadata (ST 2086)**: MaxCLL / MaxFALL sent to the Wayland HDR10 PQ swap chain are now validated against the **ST 2084 PQ reference luminance** (10,000 nits). Extreme linear EXR peaks and invalid container CLLI values no longer reach `vkSetHdrMetadataEXT`, avoiding compositor protocol errors on pathological content.

### Improved
- **Linux HDR diagnostics**: Per-frame Vulkan HDR metadata and swap-chain format mismatch messages are logged at **debug** level only; duplicate `vkSetHdrMetadataEXT` calls are skipped when the payload is unchanged.


## [2.1.2] - 2026-05-24

### Improved (Windows)
- **Crash diagnostics**: Windows now registers a lightweight **vectored exception handler** alongside the existing top-level crash filter so serious native faults can leave a clearer **early breadcrumb** (when possible) before heavier crash reporting runs. Most users never see this; it mainly helps pinpoint rare **hard exits** during support.


## [2.1.1] - 2026-05-23

### Added
- **Linux HDR10 metadata (per image)**: On Wayland HDR10 PQ swap chains, the viewer now submits `VK_EXT_hdr_metadata` (ST 2086) when you open or switch HDR images—**MaxCLL** / **MaxFALL** follow the current picture instead of using fixed defaults. Applies to **all native HDR decode paths** (AVIF, HEIF/HEIC, JPEG XL, Ultra HDR JPEG_R, OpenEXR, Radiance `.hdr`/`.pic`, float/LogLuv TIFF, and tiled large images).

### Improved
- **Linux HDR10 color metadata**: Mastering display primaries in the Vulkan HDR infoframe use **BT.2020 + D65**, matching the HDR10 PQ pipeline.
- **Ultra HDR JPEG_R**: Gain-map headroom is mapped into luminance hints so MaxCLL can be derived from container metadata before pixel scanning.

### Fixed
- **AVIF HDR PQ colour** (since **2.1.0**; note added in this release): PQ AVIF decoded through YUV→RGB—including Microsoft **Chimera** (`Chimera_10bit_…_with_HDR_metadata.avif`)—no longer look oversaturated on **Windows** and **Linux**. The viewer treats `libavif` RGB output as **display sRGB gamma**, not PQ code values (avoids a second PQ EOTF in the HDR shader), and applies BT.2020 **matrix MC=10→NCL** fallback for Chimera-class payloads where the container tag does not match the coded luma/chroma.

### Notes
- **Linux HDR scope (memo)**: Wayland native HDR on Linux targets **HDR10 PQ** via **`Rgb10a2Unorm`** (`HDR10_ST2084` + `VK_EXT_hdr_metadata`). **`Rgba16Float` scRGB / EDR** swap chains (the Windows-style linear float path) are **not planned for this release** and remain out of scope until compositor and driver support is clearer. X11 stays SDR tone-mapped.

## [2.1.0] - 2026-05-22

### Added
- **Linux Wayland HDR presentation (experimental)**: Native HDR10 swap chains on Wayland when the compositor exposes HDR via color management (`wp_color_management`; KDE Plasma 6 / GNOME 50+ with Mesa ≥ 25.1). Prefers 10-bit HDR surfaces (`Rgb10a2Unorm`). X11 sessions remain SDR tone-mapped. NVIDIA proprietary Linux drivers may not yet expose the required Vulkan HDR extensions.

### Changed
- **Linux HDR settings**: The native HDR surface toggle is available under Wayland; X11 sessions show an explanatory hint instead.


## [2.0.7] - 2026-05-22

### Fixed
- **Windows ARM64 only**: Startup no longer probes the OpenGL/WGL backend during wgpu adapter enumeration (uses DX12/Vulkan only). Fixes a first-launch crash in `strlen` inside GLES/WGL init on native ARM64 Windows and Parallels VMs. x64 Windows is unchanged.


## [2.0.6] - 2026-05-21

### Fixed
- **Windows ARM64 only**: Pinned the bundled mimalloc C library so the app no longer crashes on startup on native ARM64 Windows (including Parallels VMs). Other platforms are unchanged.


## [2.0.5] - 2026-05-20

### Improved
- **App icon**: New colorful aperture-style icon with a cleaner transparent background.
- **Windows**: The taskbar and title bar now use the same icon source, so they stay in sync when the artwork is updated.

### Fixed
- **Mouse wheel**: Scrolling to move between images and **Ctrl + scroll** to zoom work again in the main viewer area.


## [2.0.4] - 2026-05-15

### Improved
- **Startup speed**: The app should reach its window noticeably faster, especially after you have run it once on the same PC. The first launch still does a bit more setup; later launches skip repeat work where it is safe to do so.

### Fixed
- **Music HUD (virtual machines)**: The on-screen music progress bar could appear stuck or not advance in some virtual-machine setups; timing updates should now keep moving more reliably while a track is playing.


## [2.0.3] - 2026-05-13

### Linux
- **HDR presentation**: Linux builds do not request HDR / wide-color swapchains from the window system.
- **`libstdc++`**: The GNU C++ runtime is linked statically; released binaries no longer list `libstdc++.so.6` in `DT_NEEDED`.
- **glibc baseline**: Linux binaries are built against **glibc 2.28** (for example **Debian 10** and **UnionTech UOS**).
- **Audio**: ALSA is linked statically so end users are not required to match a particular `libasound.so` from the distro.


## [2.0.2] - 2026-05-07

### Added / improved
- **Radiance HDR (`.hdr` / `.pic`)**: Faster, native-style decode with correct **upright display** driven by the file’s resolution line (Greg Ward HDR layout), instead of guessing from EXIF. Large RGBE scans should decode more smoothly thanks to tighter inner-loop pixel placement.
- **JPEG XL**: **Orientation metadata** parsed from the **codestream** so rotated `.jxl` frames line up consistently with modern layout rules.
- **HEIF / AVIF HDR-style paths**: **Orientation handling** tightened on these GPU-oriented decode routes so thumbnails and previews match how the image should be viewed.

### Fixed
- **EXIF rotation**: Miscellaneous **loader orientation** fixes so common camera/Mirrorless tags are applied more reliably when opening stills (including paths that share the same metadata utilities across decoders).

### Notes
- **Under the hood**: The image decode pipeline was **split into smaller modules** (same behavior, easier maintenance and testing). This should not change day-to-day use, but it helps keep fixes and new formats consistent across platforms.


## [2.0.1] - 2026-05-06

### Fixed
- **HEIF / HEIC (HDR)**: Phone and camera shots that looked sideways or upside down now open in the correct orientation, including many **HDR / wide-color** `.heic` files where the viewer previously ignored rotation metadata.

### Improved
- **OpenEXR (.exr)**: Noticeably **faster** to open and preview very large files—scrolling and zooming huge EXRs should feel **snappier**, with work spread across your CPU instead of stalling one core.
- **HDR color labels**: Images that carry an **embedded ICC profile** are more reliably described in HDR status (for example **Display P3** vs **Rec.709**), instead of falling back to a vague “unknown” gamut when the file really did include profile data.
- **HDR preview noise**: Fewer **duplicate preview jobs** when you flip through folders quickly, and **less log spam** from harmless HDR preview updates so troubleshooting stays readable (`tile-debug` still exposes extra detail when you need it).

### Changed
- **Heavy HDR / tiled previews**: Background preview work is **capped more safely** so the app stays responsive when you stress it with huge images or rapid navigation.


## [2.0.0] - 2026-05-05

### Added
- **HDR viewing & tone mapping**: Scene-linear HDR pipeline with adjustable exposure (EV), PQ/HLG and scRGB-style paths where supported, tiled HDR for large images, and on-screen HDR status where applicable.
- **GPU backends**: HDR and modern formats use **WGPU** with **DirectX 12** on Windows and **Metal** on macOS for composition and presentation.
- **Format support** (native decode paths where noted):
    - **OpenEXR** (.exr) via OpenEXRCore, including large/tiled EXR workflows.
    - **AVIF / AVIFS** and **HEIF / HEIC** via libavif / libheif (HDR-capable where the bitstream allows).
    - **JPEG gain-map HDR** (Ultra HDR / `JPEG_R`): decode and display with capacity-aware handling.
    - **JPEG XL** (.jxl) as an optional native path when enabled in the build.
    - **TIFF**: extended coverage for float / LogLuv / high bit-depth and HDR-oriented TIFFs via libtiff integration (not every TIFF variant).

### Changed
- **TurboJPEG**: Treat non-fatal **`tjGetErrorCode` warning** as success after `tjDecompressHeader3` / `tjDecompress2` so JPEGs with reserved/unknown markers (e.g. `0x9d`) still decode instead of aborting.
- **MINISWHITE float grayscale TIFF**: File-level white reference using `SMaxSampleValue` or image-wide maximum (not per-scanline pivot); corrected **`TIFFGetField`** scalar read for `SMinSampleValue` / `SMaxSampleValue`.

### Notes
- Requires up-to-date **libjpeg-turbo** (TurboJPEG **`tjGetErrorCode`**, ≥ 1.6) when using the bundled static link.


## [1.5.8] - 2026-04-29

### Changed
- **Zero-Copy Pixel Pipeline**: Major optimization of the image decoding and rendering path to minimize memory allocations and redundant data copies.
    - **LibRaw RAII & Single-Pass Packing**: Implemented `LibRawMemory` RAII wrapper for automatic FFI memory management. RAW development now uses a SIMD-accelerated "single-pass" conversion from LibRaw's internal RGB buffers directly to Rust RGBA buffers, eliminating a redundant intermediate 400MB copy.
    - **Zero-Copy Tile Management**: Updated `TiledImageSource` trait and `TilePixelCache` to use `Arc<Vec<u8>>`. Decoded tiles are now passed by reference (Arc) from decoders to the cache, avoiding megabytes of buffer moves per frame during gigapixel image exploration.
    - **Buffer Reuse**: Replaced `to_rgba8()` with `into_rgba8()` in hot paths (refinement worker, preview generation) to move existing buffers instead of cloning them.
- **SIMD Interleaving Utility**: Centralized high-performance pixel swizzling logic in a new `simd_swizzle` module with AVX2, SSE4.1, and Neon support, ensuring consistent performance across RAW, PSB, and TIFF loaders. Added SSE4.1 paths for `interleave_rgb_with_alpha` on x86_64; completed Neon coverage for planar RGB/RGBA interleave helpers on aarch64; moved duplicated PSB SIMD out of `psb_reader` into the shared module.
- **Tiled Preview Cache Policy**: When re-opening a large (tiled) image, the synchronous stage-1 preview (EXIF thumbnail or small `generate_preview`) no longer overwrites `TextureCache` if it already holds a **larger** uploaded preview texture from stage-2 HQ generation (`TextureCache::cached_preview_max_side` compares the long side of the GPU texture). Prevents a brief “downgrade” from HQ back to LQ on navigation.
- **macOS Giant Stripped TIFFs**: `TiffStripCachingSource` is used for oversized strip-based TIFFs with **any** EXIF orientation (not only orientation `1`). Logical display coordinates are mapped to physical strip pixels via an inverse of the same EXIF transform used elsewhere; oriented tiles sample horizontal strips with a per-strip `Arc` buffer cache to avoid repeated `strip_cache` mutex traffic.
- **RGBA buffer sharing**: `DecodedImage` and `AnimationFrame` keep decoded RGBA8 in `Arc` buffers through decode, channels, and tiled memory sources where applicable, avoiding redundant full-buffer clones; tiled HQ preview work reuses `Arc::clone` on the source instead of cloning an entire `LoadResult` for the channel send.
- **Loader queue hygiene**: On navigation, stale entries are discarded from the unbounded loader receive path; a single delayed-fallback worker replaces per-request OS threads for the slow decode path. Arrow-key navigation is throttled to reduce load storms.
- **Async housekeeping**: Metadata extraction and wallpaper queries are deferred off the UI thread; a shared `FileOp` channel ensures delete/rename results are not dropped under load. Added i18n strings for async loading states.
- **HQ preview / refine resolution cap**: RAW refine, tiled HQ preview generation, and WIC/ImageIO “performance mode” RAW previews cap the longest side with `min(hardware tier, monitor cap, 4096)`. Tier limits (`HardwareTier::max_preview_size`: 1024 / 2048 / 4096) apply via `PREVIEW_LIMIT`. The monitor cap uses each visible frame’s egui viewport `monitor_size` (UI points) × `native_pixels_per_point` for physical pixels, then `ceil(max(width,height) × HQ_PREVIEW_MONITOR_HEADROOM)` (1.1), clamped to `[256, 4096]`; eframe supplies the monitor for the current window. `refresh_hq_preview_monitor_cap` runs on the UI thread while the window is not minimized.

### Fixed
- **LibRaw Memory Leak**: Fixed a critical bug where `libraw_dcraw_make_mem_image` was called twice per image, causing massive heap memory leaks and redundant buffer allocations.
- **SIMD Unsafe Warnings**: Resolved compiler warnings related to unsafe intrinsic calls in the new SIMD module.
- **SIMD RGB→RGBA Packed Bounds**: `interleave_rgb_packed_to_rgba_packed` now caps work to valid RGBA output and input length so LibRaw buffers with trailing padding cannot read past the intended RGB extent.
- **TilePixelCache Re-Insert**: Inserting a tile key that already exists now evicts the old entry first (LRU + byte accounting), avoiding overstated CPU cache usage.
- **macOS TIFF Strip Tile**: Restored a missing assignment in the CoreGraphics strip path so oriented tile assembly does not drop decoded strip data.
- **RAW Refinement Race**: Fixed a race condition where stale background refinement results (from previous navigations) could overwrite the current image or cause flickering by prematurely evicting texture caches. Re-enabled strict generation (gen_id) validation for all asynchronous RAW updates.
- **Deletion Race Safety**: Fixed a bug where deleting an image could cause the next image at the same index to briefly display data from the deleted file due to stale loader results being accepted. File removal now runs off the UI thread with optimistic delete and rollback on failure; rollback restores viewer state and re-queues the image load when appropriate.
- **Scan Consistency**: Fixed a consistency issue where preloading during a directory scan could result in displaying wrong images if the file indices shifted during the final global sort. Cancelling in-flight scans prevents background work from piling up; index-dependent live state is cleared after the final sort.
- **Loader stale decode**: Corrected an inverted guard in the image decode pool (and the coalescing delayed fallback worker): tasks whose navigation `generation` no longer matches the current global counter exit before decoding, instead of continuing when the load slot still held the old generation. Added a matching early check in `do_load`. Rapid paging no longer stacks full decodes for obsolete generations.
- **Post-scan “infinite loading”**: `ImageLoader::is_loading` is now generation-aware so a superseded load for an index does not block later `request_load` calls (e.g. after a directory scan completes).
- **Stale preview delivery**: `PreviewResult` handling and prefetched tiled-image preview upgrades validate generation so background previews cannot repoint the wrong entry in the texture cache.
- **Async metadata races**: EXIF/XMP and wallpaper queries validate the file path against the current scan generation before applying results, avoiding cross-talk when the directory list changes mid-flight.

## [1.5.6] - 2026-04-28

### Changed
- **Tiled Rendering Optimization**: Removed tile fade-in animations to eliminate redundant UI repaints, significantly reducing CPU/GPU usage during idle periods. Tiles now pop-in instantly at full opacity.
- **GPU Upload Quota**: Refined the per-frame GPU upload quota system. Background preloading is now strictly limited to prevent GPU command queue saturation, while the active image and high-quality previews bypass the quota for maximum responsiveness.
- **UI Refinement**: Streamlined the settings panel by removing the redundant "Exit Application" button and OS-specific quit hints.
- **High Quality RAW Control**: Fully implemented the "High Quality" toggle logic for the RAW pipeline. When disabled, the viewer prioritizes fast embedded thumbnails to save power; when enabled, it performs high-fidelity demosaicing for maximum visual accuracy.
- **Unified RAW Pipeline**: Standardized the RAW image loading sequence across all paths (preview, full development, and background refinement). Orientation is now determined by a centralized "source of truth" (LibRaw metadata with EXIF fallback), ensuring perfect visual parity between Windows and macOS.
- **Metadata Consistency**: Migrated EXIF orientation detection to a unified utility (`metadata_utils`), eliminating platform-specific metadata disparities between WIC, ImageIO, and native decoders.
- **RAW Compatibility Boost**: Enhanced support for high-end digital backs (e.g., Leaf MOS) by enabling hardware color matrices and optimizing auto-brightness normalization.

### Fixed
- **WGPU Stability**: Fixed a critical "Dimension X is zero" panic in the rendering pipeline by adding dimension sanitization for corrupted or malformed images.
- **Process Lifecycle**: Ensured the application terminates cleanly after a fatal crash by adding an explicit exit call to the emergency error dialog.
- **IPC Robustness**: Fixed a critical bug where oversized IPC messages were silently truncated and accepted. The system now explicitly detects and rejects payloads exceeding the 8KB safety limit, preventing malformed command execution. Improved handling with non-blocking operation on Windows to prevent application freezes.
- **IPC Consistency**: Unified Unix socket paths in `cleanup_stale_socket` to use the `IPC_SOCKET_NAME` constant.
- **Input System**: Enabled `F1` as a global toggle to both show and hide the settings panel.

## [1.5.5] - 2026-04-27

### Added
- **Input System Refactoring**: Replaced the hardcoded input logic with a prioritized, bitmask-based lookup table. This ensures consistent modifier matching (Ctrl/Cmd, Shift, Alt) across platforms and provides a foundation for future user-configurable hotkeys.
- **Unified Dialogs**: Replaced native system dialogs for Windows file association management with custom, theme-aware modal dialogs, achieving a more consistent and professional UI experience.
- **Modal Sequencing**: Improved the modal dispatching system to support sequential dialog flows, enabling "Success" or "Confirm" prompts to appear immediately after a primary operation is completed.

### Fixed
- **Hotkey Conflicts**: Resolved an issue where modified shortcuts (e.g., Ctrl+Arrow keys for rotation) were sometimes intercepted by simple navigation keys.
- **UI Focus**: Fixed a bug where the Tab key (used for OSD toggle) could cause egui to trap focus, leading to non-responsive keyboard input.
- **Accessibility**: Added the `=` key as a secondary shortcut for zooming in to improve accessibility for laptop keyboards without numeric pads.

## [1.5.4] - 2026-04-26

### Added
- **Audio Engine Refactoring**: Major structural overhaul of the audio thread. Extracted state into `AudioLoopState` and shared objects into `AudioSlots`, reducing the monolithic `run_audio_loop` from 700+ lines to a lean event loop for better maintainability.

### Fixed
- **APE+CUE Playback**: Resolved high-precision synchronization issues where the UI slider would lag behind track changes.
- **Playlist Looping**: Implemented seamless automatic looping of the music playlist (APE+CUE and standard files).
- **Audio Reliability**: Fixed potential deadlocks and UI flickering during file transitions by implementing synchronous state updates.
- **UI Settings**: Compacted music settings by grouping checkboxes horizontally to conserve vertical space.


## [1.5.3] - 2026-04-25

### Added
- **UI Architecture**: Introduced unified `MovableModal` system for all pop-up dialogs (EXIF, XMP, File Association, Go-to, etc.), featuring improved centering and modal backdrop logic.
- **Music Persistence**: Added support for resuming music playback across application restarts, including track selection and CUE sheet position.
- **File Association**: Refined the Windows file association dialog with localized format group names and a more professional, platform-agnostic terminology.

### Fixed
- **UI**: Fixed inconsistent button colors in light theme and resolved checkbox interaction issues in modal dialogs.
- **Egui 0.34.1**: Resolved all remaining deprecation warnings from the egui 0.34.1 update.

## [1.5.2] - 2026-04-24

### Added
- **UI**: Added `TAB` hotkey to quickly toggle the visibility of the on-screen display (OSD) HUD.

### Fixed
- **TIFF**: Replaced buggy manual scanline decoding with native libtiff RGBA output, fixing visual artifacts in 32-bit HDR, float TIFFs, and color inversion in CMYK/non-standard bit depths.
- **CI / Build**: Resolved MSVC `/MT` vs `/MD` CRT linkage conflicts on legacy Win7 CI pipelines.
- **CI / Build**: Updated Linux CI environment to GCC-10/Clang to fix AVX2 intrinsic bugs.
- **Cross-Compilation**: Fixed string pointer casting mismatch (`i8` vs `u8`) for `c_char` on AArch64 Linux.

## [1.5.1] - 2026-04-23

### Added
- **Monkey Audio (APE)**: Migrated to official CMake-based build system for the SDK.
- **SIMD Acceleration**: Enabled AVX2, AVX512, and Neon hardware acceleration for Monkey Audio decoding.
- **Unified JPEG Decoding**: Migrated all platforms to high-performance `libjpeg-turbo` for JPEG decoding, replacing system-native decoders (WIC/ImageIO) to ensure consistent and faster loading.
- **Zero-Copy Loading**: Implemented `memmap2` based memory-mapping for JPEG decoding to minimize memory allocations and improve performance for large images.
- **LibRaw Resilience**: Enabled JPEG support within LibRaw to improve loading for certain hybrid RAW/JPEG formats.

### Changed
- Decoupled Monkey SDK build from manual source lists, improving cross-platform maintainability.

### Fixed
- Cleaned up compiler warnings in `libraw-sys` and UI transitions logic.

## [1.5.0] - 2026-04-23

### Fixed
- **RAW Stability**: Resolved `ACCESS_VIOLATION` (0xc0000005) when loading Nikon NEF files by implementing strict FFI memory boundary checks using `data_size`.
- **RAW Color Accuracy**: Fixed the lavender/purple tint issue in RAW images by correctly enabling camera white balance and auto-brightness in the LibRaw engine.
- **Concurrency**: Fixed "data corrupted" errors when preloading multiple RAW files concurrently by enabling LibRaw's internal thread-safety mechanisms (removing `LIBRAW_NOTHREADS`).

### Added
- **High-Performance Parallelism**: Switched to a fully lock-free RAW processing pipeline. Demosaicing (the most intensive part) now runs in true parallel across all CPU cores.
- **Robustness**: Added automatic fallback to system WIC/preview rendering if native RAW development reports warnings or corruption.
- **I18n**: Added missing translations for buffer alignment and memory errors.

### Changed
- Updated LibRaw internal C API to expose necessary white balance and error-tracking controls.
