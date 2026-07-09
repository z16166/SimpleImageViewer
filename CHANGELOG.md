# Changelog

All notable changes to this project will be documented in this file.

## [2.9.1] - 2026-07-08

### Improved
- **Faster decoding on modern CPUs**: Pixel conversion, downscaling, and HEIF color conversion use accelerated SIMD paths on supported processors for quicker opening and smoother browsing.
- **Faster TIFF navigation strip previews**: Large TIFF files build file-list thumbnails with less redundant work per strip row.
- **Smoother HDR browsing**: HDR uploads skip unnecessary CPU copying, texture cache lookups stay fast during pan and zoom, and idle GPU memory is reclaimed more predictably.
- **Faster animated JPEG XL startup**: Later animation frames reuse mapped file data instead of reopening the file for each decode pass.
- **Faster HDR gain-map thumbnails**: ISO gain-map compositing for navigation-strip previews uses wider vector processing.
- **Faster HDR AVIF and JPEG XL animations**: Animations with gain maps load quicker when consecutive frames are nearly unchanged.
- **Animation strip thumbnails**: File-list previews for animated images no longer stay blank when the main view already shows a larger frame than the strip size.

### Fixed
- **RAW navigation strip previews**: Strip thumbnails for RAW files load more reliably, including when the viewer opens files from memory-mapped data.
- **Slideshow interval setting**: Changing the slideshow interval with the slider saves once when you release the control, not on every drag step.
- **Faster quit**: The tray icon disappears immediately on exit, and background shutdown finishes sooner.
- **Custom context menu actions on Windows**: External programs such as Paint launch correctly from both executable and full command-line custom actions, including Windows Store app aliases.

## [2.9.0] - 2026-07-08

### Improved
- **Faster HDR previews on SDR displays**: Large HDR photos now use GPU tone mapping for quicker SDR previews while keeping full HDR output on compatible monitors.
- **Faster AVIF, HEIF, and HDR loading**: These formats decode in fewer passes, reuse mapped file data across sniffing and decoding, and share one full decode between the main view and navigation strip where possible.
- **Accelerated SIMD decode paths**: AVIF, HEIF, TIFF, Radiance, and HDR YCbCr conversion use wider SIMD processing for faster opening and smoother gradients on large photos.
- **Smoother navigation strip thumbnails**: File-list previews build with less redundant decoding, reuse full-image work for small thumbnails, and move GPU downsample work off the paint thread so browsing stays responsive.
- **Smoother browsing in large folders**: Smarter background preloading, O(1) cache eviction, and staged HDR GPU uploads keep nearby images ready longer with lower memory overhead.
- **Faster TIFF and tiled image display**: Tiled TIFFs gain tile-level caching; high bit-depth and IEEE-float TIFFs decode in a single pass; strip and main-view paths share mmap and scratch buffers more aggressively.
- **Faster EXR and Radiance display**: Subsampled EXR previews use optimized scanline paths; Radiance tile and preview decode run in parallel for large HDR files.
- **Faster RAW opening and strip previews**: RAW files defer expensive sensor unpack until demosaic is needed, and developed previews rank and refresh more reliably in the navigation strip.
- **Faster PSD and PSB browsing**: Large layered files decode in the background without blocking the interface, and strip thumbnails refresh once decoding completes.
- **Faster animated format startup**: GIF, APNG, WebP, AVIF, and JPEG XL animations reuse bootstrap file mappings and show the first frame sooner while later frames load in the background.
- **Apple Silicon HDR previews**: HDR preview tone mapping on Apple Silicon uses accelerated NEON paths for smoother browsing.
- **Directory tree responsiveness**: Expanding the folder tree at launch lists subfolders correctly; changing file-list sort order keeps the canvas, preloads, and strip thumbnails aligned.

### Fixed
- **EXIF rotation on Windows and macOS**: Images decoded through the system codecs no longer appear rotated twice when orientation metadata is present.
- **Navigation strip preview orientation**: Thumbnails match the main viewer orientation when a full decode is reused, including AVIF container rotation metadata.
- **HDR tiled viewing stability**: Tile uploads, GPU write queues, image bindings, and fast pan/zoom no longer drop HDR image data or rebuild textures unnecessarily.
- **HDR tiled previews on HDR displays**: Cached HDR tiles tone-map correctly for SDR preview tiles in HDR mode, and tiled high-quality navigation stays in sync with partial cache states.
- **SDR preview paths for HDR photos**: SDR previews avoid unnecessary HDR buffers and processing, reducing memory use and startup delay on standard displays.
- **HDR status indicator accuracy**: SDR preview tiles no longer show an HDR indicator when only a standard-dynamic-range preview is displayed.
- **HEIC gain-map SDR display toggle**: Switching embedded-SDR vs tone-mapped presentation for HEIC gain-map photos refreshes correctly, including the full HDR preload window after display-mode changes.
- **HEIF studio-range color**: HEIF photos encoded with studio (limited) color range display more accurate SDR colors.
- **AVIF color on 8-bit sources**: 8-bit AVIF images display with smoother gradients instead of banding from an incorrect color conversion path.
- **Animated AVIF timing**: Frame pacing for animated AVIF files is correct again after HDR GPU uploads complete.
- **Animated HDR startup**: First frames of animated AVIF and JPEG XL HDR files appear without a visible flash while loading completes.
- **Animated strip thumbnails**: Navigation-strip previews for animated files no longer block the interface while frames decode.
- **Home and End in the file list**: Keyboard Home and End jump to the first and last image in the navigation list, including detached navigation windows.
- **File list selection after canvas wheel**: Scrolling through images with the mouse wheel on the canvas keeps the highlighted row in the file list in sync.
- **Reload after folder scan resort**: When a background scan re-sorts the file list, the currently open image reloads reliably instead of staying on stale data.
- **File list size drift**: The navigation list and internal image index stay aligned when files are added or removed during scanning.
- **PSD navigation strip previews**: Large PSD files no longer flash a blank placeholder in the file list while decoding finishes in the background.
- **PSD strip decode reliability**: Restored dependable strip previews for PSD files after an async-decode regression left gray placeholders without refreshing the UI.
- **Strip thumbnails after scrolling**: Thumbnails reappear correctly after scrolling long file lists instead of staying blank after cache eviction.
- **TIFF strip preview stability**: Fixed crashes and missing previews when opening large TIFF files in the navigation strip, including unusual JPEG-compressed TIFF strips.
- **macOS strip thumbnail timing**: Navigation-strip previews on macOS publish decoded results before clearing in-flight state so thumbnails do not disappear briefly.
- **RAW demosaic timing**: RAW development preserves intended preview timing so strip and main views do not show stale intermediate results.
- **Tiled HDR exposure fallback**: HDR tiled viewing handles missing exposure metadata more gracefully instead of showing incorrect brightness.
- **Page transitions stay on canvas**: Cross-fade, slide, and push transitions no longer spill outside the main viewing area.
- **Zoom and rotate anchor**: Mouse-wheel zoom and rotation stay centered on the main canvas instead of drifting with window chrome or embedded navigation panels.
- **Damaged or unusual files**: Improved handling of empty, tiny, malformed PSB, TIFF, and Radiance files so the viewer fails gracefully instead of hanging or crashing.
- **Cleaner shutdown**: Background loading, directory-tree, and IPC workers stop more reliably when you close the viewer.

## [2.8.4] - 2026-07-02

### Added
- **HDR gain-map viewing on SDR displays**: In Settings → Viewing, choose how HDR photos with a built-in SDR master (Ultra HDR, AVIF, HEIF/HEIC, JPEG XL) appear on SDR monitors — show the embedded SDR image directly (default) or full HDR decode with tone mapping. Native HDR monitor output is unchanged.

### Improved
- **Faster navigation strip thumbnails for HDR gain-map photos**: File-list previews skip slow gain-map compositing and use accelerated tone mapping tuned for small thumbnails, while the main canvas on HDR displays still uses full gain-map rendering.
- **Faster HDR and animated image loading**: The viewer reuses one full decode for both the main image and strip preview where possible, moves full-image tone mapping to the GPU, and shows the first frame of animated GIF, APNG, WebP, AVIF, and JXL files on the canvas immediately while later frames load in the background.
- **Smarter background preloading**: Preload queue sizing keeps more nearby images ready so decoded previews are less often discarded before you open them.
- **Mislabeled file names**: Images whose extension does not match the actual format (for example a JPEG saved as `.png`) open correctly again via automatic format detection.

### Fixed
- **Empty folder and load-error messages**: “No images” and decode-error hints are centered in the canvas and file list for clearer feedback.
- **Navigation strip previews for some AVIF and HEIF files**: Thumbnails fall back to the system decoder when the fast path cannot produce a preview, matching what the main viewer shows for difficult files.
- **Some HEIC gain-map photos**: Fixed incorrect rotation and a black main canvas when the embedded SDR path should display the built-in preview.

## [2.8.3] - 2026-07-01

### Fixed
- **Navigation panel image list selection**: The highlighted row in the file list now stays on the image you chose when using arrow keys, including while the next image is still loading, instead of jumping back to the previous row.
- **Navigation panel mouse selection**: Clicking a row in the file list now moves the selection highlight to that image reliably.
- **Navigation list double-click**: Double-clicking an image in the navigation panel’s file list hides the panel for the current session only; your saved “show navigation panel” preference is no longer turned off.

### Improved
- **Faster navigation panel startup**: Embedded and detached navigation panels show folder tree and file list progress sooner while folders are still scanning, with less layout flicker on launch.
- **Smoother scanning in the file list**: The navigation panel repaints less often when new files are discovered below the visible list area.

## [2.8.2] - 2026-06-30

### Fixed
- **Linux HDR display selection**: KDE Wayland systems now keep HDR photos on the correct HDR presentation path for HDR TVs while avoiding false HDR activation on SDR displays.
- **Navigation panel layout**: Embedded navigation panels keep their intended width and visibility state more reliably while browsing.
- **GPU startup compatibility**: The viewer avoids reusing incompatible GPU pipeline caches after graphics backend changes, improving launch reliability after upgrades.
- **Double-click and navigation panel setting**: Double-clicking an image no longer turns off “show navigation panel” in your saved settings; the panel can stay hidden for that session while your preference stays on.
- **Session-only navigation panel hide**: Opening a single image from the file list can hide the navigation panel for the current session without changing your saved “show navigation panel” preference; folder scanning stays limited to the current folder while the panel is hidden.
- **Linux HDR on non-KDE desktops**: HDR output admission on GNOME, Sway, and other non-KDE Wayland compositors remains conservative (fail-closed) until explicit desktop HDR state integration is available; KDE KScreen remains the supported path for explicit HDR toggles.

### Improved
- **Current image first when changing folders**: Switching folders now loads and shows the image you are on before preloading nearby files in the background.
- **Faster opening from the file list**: Thumbnail generation and full-size viewing share work more often, so opening an image you already saw in the list is typically quicker.
- **Faster HDR and RAW image display**: Optimized several HDR, RAW, HEIF, JPEG XL, and gain-map processing paths so large photos load and render more smoothly.
- **Smoother navigation thumbnails**: File-list thumbnails are built with less redundant decoding and processing, improving browsing responsiveness in large image folders.
- **Lower memory overhead for modern HDR formats**: JPEG XL and HEIF decoding now avoid some unnecessary full-buffer copies during preview and image preparation.

## [2.8.1] - 2026-06-28

### Fixed
- **Music playback for M4A/AAC files**: Some audiobooks and speech-heavy M4A files now play at normal voice speed instead of sounding unnaturally fast.

## [2.8.0] - 2026-06-28

### Added
- **Library folder preference**: Added an option to keep your saved gallery folder unchanged when opening an image by double-click, while still browsing that image's folder for the current session.

### Fixed
- **Opening images with navigation hidden**: Double-clicking an image from another drive now opens the selected image instead of returning to the previous hidden navigation folder.

## [2.7.9] - 2026-06-27

### Added
- **Library folder preference**: Added an option to keep the saved gallery folder unchanged when opening an image by double-click, while still browsing that image’s folder for the current session.

### Fixed
- **Opening images from the system tray**: When the app is minimized to the tray, double-clicking an image in Explorer now restores the window and opens the image reliably.

## [2.7.8] - 2026-06-26

### Fixed
- **macOS HDR startup loading**: On EDR displays, opening a folder no longer gets stuck on a black “loading…” screen while the app waits for display capability detection — startup preloading and HDR decoding proceed sooner on supported Macs.
- **RAW decode blocked by preloading**: The photo you are viewing decodes with higher priority, so heavy neighbor preloading no longer leaves the current RAW file waiting in line behind background work.

### Improved
- **Navigation strip preview generation**: File list thumbnails appear faster after opening a folder, especially for JPEG and HDR photos — the viewer now uses SIMD-accelerated downsampling and can extract previews directly from compressed JPEG data without fully decoding the image first.
- **Memory efficiency during thumbnail generation**: Generating strip previews uses less memory by sharing image buffers instead of copying them, keeping large folders smoother to browse.
- **macOS HDR tone mapping**: Brightness headroom updates when macOS reports screen or display changes, keeping HDR output aligned with your monitor without constant background probing.
- **Fast browsing through RAW and HDR folders**: Rapid arrow-key navigation no longer piles up unbounded background decode threads; preloading retries are paced while HDR detection is still settling, so long sessions stay responsive.
- **HDR brightness during display transitions**: On macOS, tone mapping no longer dips below normal SDR brightness when the system briefly reports transitional display headroom values.

## [2.7.7] - 2026-06-25

### Fixed
- **GIF animation playback**: Animated GIF images play again after a regression in the previous release.
- **Navigation strip previews for HDR photos**: HDR gain-map AVIF and JXL thumbnails appear in the file list instead of staying on a placeholder, with fewer CPU-side decode errors.
- **Navigation strip previews for RAW photos**: GPU RAW thumbnails in the file list no longer get replaced by a black placeholder while the full demosaic is still running.
- **Folder change loading**: Fixed a case where switching folders could briefly stall background image loading.

### Improved
- **Navigation panel responsiveness**: File list thumbnails fill in faster after opening a large HDR folder, and the app uses less CPU while the list is still populating.
- **Background image loading**: Switching folders, changing RAW quality, or toggling HDR mode mid-load no longer causes a brief flash of a wrong or outdated image — the loader now checks whether a finished decode still matches the current viewer settings before showing it, and cancels stale work earlier. This is especially noticeable when browsing quickly through RAW or HDR folders.

## [2.7.6] - 2026-06-24

### Fixed
- **Navigation strip previews for RAW photos**: Some camera RAW files (including Sigma X3F) no longer stay stuck on a placeholder thumbnail in the file list while the full image loads.
- **Navigation strip previews for HDR RAW photos**: HDR scene-linear RAW files keep their strip thumbnail when a temporary bootstrap preview arrives before full resolution is ready.
- **Main canvas after RAW GPU processing**: The main viewer updates promptly when GPU demosaic finishes, without needing to move the mouse to trigger a redraw.
- **RAW GPU demosaic display**: Fixed a case where the viewer could repaint repeatedly without drawing after RAW processing completed.

### Improved
- **Background image loading**: The canvas refreshes more reliably when decode workers finish while the window is idle.
- **Detached navigation panel**: Thumbnail previews in a separate navigation window update more consistently after background generation completes.

## [2.7.5] - 2026-06-23

### Added
- **Windows 7 x64**: Restored support for the legacy Win7 x64 build using ANGLE/OpenGL ES with automatic GPU backend fallback.

### Fixed
- **Linux HDR on SDR displays**: HDR mode no longer turns on incorrectly on standard SDR monitors when the compositor advertises HDR swap-chain support without matching display metadata.
- **RAW photo dimensions (Fuji RAF and similar)**: On-screen develop size and status labels stay accurate while full-resolution RAW files load, instead of briefly showing embedded preview dimensions.
- **Navigation strip previews**: Thumbnails in the file list appear as soon as background generation finishes, without needing to move the mouse over the list (including when the navigation panel is in a separate window).
- **Navigation strip previews for HDR images**: HDR photos no longer stay on a black placeholder in the file list while the full image is still loading in the background.
- **Navigation strip previews for animated HDR images**: Animated HDR sequences (such as AVIF) use the first frame for the strip preview instead of a black or temporary fallback copied from the main viewer.
- **Linux Places folder tree**: Nested removable drives (for example a CD-ROM under a media mount) no longer show up twice in **Places**, and opening one folder no longer highlights two tree entries at once.

### Improved
- **Linux in virtual machines**: The app starts reliably on Linux VMs and adapters that do not support GPU pipeline caching.
- **Folder browsing performance**: Preloading nearby images uses memory more efficiently during long sessions, keeping large folders responsive.
- **Settings and preferences**: Settings are still saved immediately when you quit; routine saves while browsing are grouped to reduce disk activity.
- **Windows audio playback**: Audio embedded in images handles audio output device changes more reliably on Windows.

## [2.7.4] - 2026-06-22

### Fixed
- **Windows directory tree places**: Loading common folders and the **This PC** drive list in the navigation panel is more reliable on Windows, with fewer edge cases when a drive or folder name cannot be read.
- **Image context menu**: Right-click actions such as **Copy file path** no longer crash the app when the menu has more than one item.

## [2.7.3] - 2026-06-21

### Fixed
- **Navigation empty folders**: Opening a folder with no images in the navigation panel no longer briefly flashes the file list column headers before showing the empty-folder message.
- **Hide and restore navigation**: Hiding the navigation panel with `Ctrl + T` or **Settings** no longer discards your place in the folder tree; showing it again expands and scrolls to the folder you are viewing.
- **Pick while navigation is hidden**: Choosing a new folder with **Pick** while the navigation panel is hidden, then turning navigation back on, now opens the correct folder in the tree and image list instead of the previous one.
- **Detached navigation window position**: In separate-window mode, hiding navigation from **Settings** and showing it again reopens the panel on the monitor and position where you left it.
- **Settings directory path**: After browsing folders in the navigation panel and then hiding navigation, the directory shown in **Settings** stays on the folder you last opened.
- **Startup folder location**: With navigation enabled at launch, the tree now expands to the last folder you were viewing.

### Improved
- **Music playback overlay**: The bottom music HUD updates more smoothly while a track is playing, with less per-frame work when showing the title and elapsed time.
- **Image viewing**: Lower overhead for the on-screen status display, HDR output indicators, and right-click context menu while browsing images.
- **Navigation panel**: Smoother rendering when the directory tree is embedded in the main window or shown in a separate window.
- **Navigation folder tree**: Clicking a folder in the tree no longer auto-scrolls the tree to center the selected node; re-opening navigation or switching folders scrolls the selected folder into view when it was off-screen.
- **Language switching**: The separate navigation window title now updates correctly when you change the app language.
- **Recursive scan with hidden navigation**: **Recursive scan** in **Settings** is available again while the navigation panel is hidden, so you can refresh a deep folder tree without keeping the panel open.
- **Refresh in Settings**: The **Refresh** button in **Settings** now matches `F5` when reloading the current folder, keeping navigation strip previews stable when the file list has not changed.

## [2.7.2] - 2026-06-21

### Fixed
- **Minimize to tray on close**: With **Close window minimizes to tray** enabled, closing the main window reliably hides the app to the system tray again after directory tree navigation was added.
- **Detached navigation with tray**: When the navigation panel is in a separate window, closing the main window to the tray now hides that navigation window too; restoring from the tray brings both windows back.
- **Navigation strip previews (detached)**: Thumbnail previews in the separate navigation window no longer reuse main-window GPU textures (which could leave a row stuck on the placeholder after a preview-size change). After the last thumbnail installs, the navigation panel now refreshes once so the final row appears without needing to hover the list.

### Improved
- **Navigation file list**: Scrolling and browsing large folders in the navigation panel is smoother, with less work each frame when many files are visible.
- **Image viewing**: Lower overhead while the on-screen display, pixel inspector, context menu, and drag-and-drop are active; switching language keeps navigation labels in sync.
- **Navigation strip previews at startup**: Cold thumbnail generation uses higher parallel limits during the initial folder scan so visible rows (including the first and last) fill in sooner on large HEIC folders.

## [2.7.1] - 2026-06-21

### Added
- **Toggle navigation panel**: Press `Ctrl + T` (customizable in **Settings > Hotkeys**) to show or hide the directory tree navigation panel from the main window or the detached navigation window.
- **Double-click to open and dismiss**: Double-click an image in the navigation file list to jump to that picture and hide the navigation panel.

### Fixed
- **Zoom in detached navigation mode**: `Ctrl + mouse wheel` on the main image canvas works again when the navigation panel is in a separate window.
- **Navigation toggle from nav window**: `Ctrl + T` to hide the navigation panel now works even when the separate navigation window or its file list has keyboard focus.

## [2.7.0] - 2026-06-20

### Added
- **Directory tree navigation**: Browse folders in an embedded side panel or a separate window, with thumbnail previews in the image list beside the tree.
- **Places shortcuts**: Jump to Desktop, Documents, Pictures, Downloads, and other common folders, plus **This PC** drives and **Network** shares when you open a UNC path.
- **Sortable folder image list**: Click **Name**, **Size**, or **Date modified** column headers to sort the current folder's images ascending or descending.

### Improved
- **Directory tree layout**: Drag the splitter between the folder tree and image list; panel widths are remembered for embedded and detached layouts.
- **Directory tree icons**: This PC, drives, known folders, and ordinary folders use distinct icons; expand arrows are lighter and easier to scan.

## [2.6.4] - 2026-06-18

### Improved
- **Linux HDR setup guide**: The README now explains Wayland HDR requirements, when **vk-hdr-layer** and `ENABLE_HDR_WSI=1` help on older NVIDIA GPUs, and which driver versions are verified—so Linux users can tell whether native HDR should work and how to enable it.

### Fixed
- **Minimize to tray on close**: With **Close window minimizes to tray** enabled, the title-bar close button now reliably hides the window to the tray instead of sometimes leaving it visible. Restoring from the tray and closing again, or picking a new image folder after restore, no longer gets stuck or skips loading the new pictures.

## [2.6.3] - 2026-06-17

### Added
- **Open folder from the main window**: Press `Ctrl + O` (customizable in **Settings > Hotkeys**) to choose an image folder without opening Settings first.
- **Settings from the system tray**: The tray right-click menu now includes **Settings**, which restores the main window and opens the options panel.
- **HDR/preload diagnostics (`preload-debug`)**: Build with `--features preload-debug` to emit `[PreloadDebug][HDR-Gate]` logs that trace swap-chain target decisions and startup preload deferral gates.

### Fixed
- **Switching folders during a refresh**: Choosing a new folder while an F5 refresh is still running no longer keeps the previous zoom, rotation, or slideshow pause state.
- **Linux tone-mapped SDR startup**: With **Native HDR surface** disabled, the viewer no longer stays on a black `loading…` screen. Startup preloads no longer wait for an HDR swap-chain hot-swap that is intentionally never requested when tone-mapped SDR output is selected (including on Wayland sessions where Vulkan WSI still reports HDR10 support).
- **Linux Wayland HDR startup logs**: Startup diagnostics now explain when native HDR may activate after Vulkan WSI probing, instead of implying the swap chain is permanently SDR-only.

## [2.6.2] - 2026-06-15

### Improved
- **GPU RAW on HDR displays**: Browsing GPU-demosaiced RAW photos is smoother; the embedded preview stays stable while the full HDR image finishes, and neighbor preloading keeps pace during long folder sessions.
- **SDR display efficiency**: On standard (SDR) monitors, high-quality RAW preloading no longer performs unnecessary HDR GPU work, saving memory and background processing.

### Fixed
- **GPU RAW fallback**: If GPU demosaicing fails, the viewer switches to the CPU path immediately instead of briefly appearing stuck on the current image.
- **Windows HDR across monitors**: Native HDR output is limited to compatible graphics paths, preventing crashes or blank rendering when dragging the window between HDR and SDR displays.
- **Graphics driver updates**: After a GPU driver update, the first launch no longer reuses an incompatible shader cache that could cause rendering glitches.
- **Crash reporting on Windows**: Error dialogs after a crash now appear more reliably, including when system resources are under heavy pressure.

## [2.6.1] - 2026-06-15

### Improved
- **HQ RAW browsing on HDR displays**: Neighbor images preload more reliably after your monitor reports its HDR brightness range, and the embedded bootstrap preview stays visible while the full HDR plane finishes loading.
- **RAW status line accuracy**: The bottom-left RAW overlay now keeps demosaic timing and processing details when preview and full-quality data arrive in different order.

### Fixed
- **macOS startup stability**: Fixed a crash that could occur on launch when GPU-accelerated rendering initializes on Apple Silicon and Intel Macs.
- **Quit while decoding RAW**: Closing the app during an active high-quality RAW load is safer on Linux and macOS, avoiding rare shutdown crashes.
- **Arch Linux builds**: Restored compatibility for community packaging on Arch Linux.

## [2.6.0] - 2026-06-14

### Added
- **RAW demosaicing mode (CPU / GPU)**: When High-Quality RAW preview is enabled, choose CPU or GPU demosaicing in **Settings > Display**. GPU mode accelerates compatible Bayer RAW on supported graphics hardware, with automatic CPU fallback for Fuji X-Trans, Super CCD, non-square pixels, oversized images, or processing errors; the OSD shows which path is actually running.
- **RAW processing details in OSD**: View demosaic timing and whether the current RAW image was processed on the CPU or GPU.

### Improved
- **GPU RAW preview speed**: Full-resolution Bayer RAW can demosaic on the GPU for faster high-quality viewing on supported hardware.
- **Sigma X-Trans RAW navigation**: High-quality CPU develop now shows the embedded preview immediately while the full image finishes, so browsing no longer holds the previous picture for about a second.
- **RAW exposure consistency**: GPU and CPU demosaic paths now match at the default exposure; adjust EV with `Ctrl + ↑` / `Ctrl + ↓` when a RAW file needs brighter or darker rendering.

### Fixed
- **Apple HDR HEIC browsing**: Fixed crashes when flipping through folders with many iPhone-style HDR HEIC photos. The viewer now releases large GPU staging buffers right after HDR blending and falls back to CPU processing if video memory is tight, so browsing long collections stays stable on laptops and integrated graphics.
- **JPEG XL HDR gain-map brightness**: HDR JPEG XL (.jxl) photos with ISO gain maps no longer look too dark after GPU HDR blending; the viewer now treats the blended result as scene-linear instead of applying sRGB gamma again on top.

## [2.5.1] - 2026-06-12

### Added
- **System Tray Support**: Added an option in System settings to minimize the application to the system tray when the window is closed. Features left-click restore, a right-click context menu (Show Window / Exit), automatic window restoration when opening new images from the file explorer, and a process-local in-app shortcut (`Ctrl + Shift + T`, not a global OS hotkey) that only minimizes the currently visible main window to tray; restore from tray uses the tray icon or its context menu.
- **Copy and Cut to Folder**: Easily copy or move the active image to a target directory via the context menu or keyboard shortcuts (`Ctrl + Shift + C` for copy, `Ctrl + Shift + X` for cut). Includes a folder picker dialog that remembers the previously used directory, an optional session-only "Overwrite if exists" checkbox, and automatically updates the viewer's image list when a file is moved.

## [2.5.0] - 2026-06-11

### Added
- **Pixel Inspector**: Real-time pixel coordinates and RGBA colors viewer on hover, with support for custom region selection (Shift + Click) to inspect a detailed grid of pixel values in a dedicated dialog.

### Improved
- **Escape Key Selection Cancellation**: Instantly cancel active pixel region selection using the Escape key.
- **Pixel Grid Memory Performance**: Optimized the pixel inspection grid layout to utilize contiguous memory, reducing heap allocation overhead for faster loading and smoother scrolling.

## [2.4.7] - 2026-06-11

### Added
- **Pixel Inspector**: View real-time pixel coordinates and colors on hover, or select a custom region to inspect a detailed grid of pixel values.

### Improved
- **High-quality RAW previews on SDR displays**: Enabled full exposure adjustments (EV settings) and tone mapping consistently on SDR monitors when High-Quality RAW preview is active.
- **Smoother RAW loading transitions**: Initial loading of RAW files now boots instantly using the embedded preview image, smoothly transitioning to the high-quality refined version in the background without causing the screen to flash black.

## [2.4.6] - 2026-06-10

### Improved
- **Window startup responsiveness**: Upgraded the core user interface framework to improve window maximization responsiveness and ensure clean, flash-free rendering on first launch.

### Fixed
- **HEVC/HEIF startup stability**: Resolved a critical startup crash on Windows that occurred when initializing the HEVC image decoding engine, ensuring stable loading of HEVC/HEIF photos.

## [2.4.5] - 2026-06-10

### Improved
- **High-quality RAW demosaic speed**: High-quality RAW development now uses multi-core parallel processing, so full-resolution refine finishes much faster on modern CPUs.

### Changed
- **RAW status line readability**: The bottom-left RAW overlay now separates embedded preview, sensor size, and active render source with clearer spacing.

### Fixed
- **Windows high-quality RAW**: Windows release packages now include the OpenMP runtime needed for faster RAW demosaic, so high-quality mode works without installing extra components.

## [2.4.4] - 2026-06-10

### Fixed
- **HDR gain-map reliability**: Improved AVIF, JPEG XL, and Ultra HDR image handling so gain-map photos render more consistently across HDR and SDR displays.
- **RAW tone-mapped viewing**: High-quality RAW loading on SDR displays now keeps the HDR tone-map path available, including OSD HDR status and exposure adjustment.
- **Windows graphics backend selection**: Windows 10/11 compatibility builds now prefer the modern DirectX path instead of falling back to OpenGL, while Windows 7 keeps the ANGLE compatibility path.
- **JPEG XL gain-map loading**: Fixed loading failures for JPEG XL files that store an SDR base image with deferred HDR gain-map data.
- **Animated image preloading**: Fixed stale preloaded animation frames after HDR/SDR display changes, so animated images reload with the correct brightness and playback state.
- **HDR browsing stability**: Reduced repeated fallback work and stale HDR state after display capability changes, improving responsiveness when switching displays or browsing HDR images.

## [2.4.3] - 2026-06-09

### Added
- **Camera RAW+JPEG pair handling**: New Library setting lets you show both files, hide RAW files, or hide JPG/JPEG files when a camera saves both formats for the same shot.

### Changed
- **OSD state updates**: Refactored the bottom-left OSD to use tracked viewer state and event-driven updates, reducing manual refresh calls and avoiding unnecessary per-frame string allocation.
- **Folder scanning efficiency**: Reduced temporary allocations while batching scan results and matching RAW/JPEG sidecars.

### Fixed
- **RAW/JPEG sidecar matching**: Paired RAW and JPG/JPEG files are matched even when their file stems use different ASCII casing, such as `IMG001.ARW` with `img001.JPG`.

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
