# Simple Image Viewer (SimpleImageViewer)

A high-performance, cross-platform image viewer built with Rust. Designed for fast browsing of large photo libraries with a clean, customizable UI (Dark/Light/System themes), background music playback, and persistent settings.
 
![Screenshot](assets/screenshot.jpg)

---

## Features

- **Fast image loading** — background thread pre-loads adjacent images so navigation is instant
- **Wide format support** — JPEG, PNG, GIF, BMP, TIFF, TGA, WebP, ICO, PNM, HDR, AVIF, HEIF/HEIC, QOI, EXR, PSD, PSB and **60+ Camera RAW formats**
- **Professional RAW Support** — viewing-only support for 60+ RAW formats, including Canon (`.cr2`, `.cr3`), Nikon (`.nef`, `.nrw`), Sony (`.arw`), Fujifilm (`.raf`), Panasonic (`.rw2`), Olympus (`.orf`), Pentax (`.pef`), Hasselblad (`.3fr`), Phase One (`.iiq`), and many more.
- **Gigapixel image support** — tiled rendering engine for ultra-high-resolution images (100MP+); only visible tiles are uploaded to GPU, with efficient memory management to keep VRAM usage constant
- **PSD / PSB support** — native Photoshop Document reader; PSB (Large Document, 4 GB+) supported with automatic RAM safety check before loading
- **HEIF / HEIC Support** — native decoding of Apple iPhone high-efficiency photos with high performance and cross-platform compatibility
- **Image Printing** — print images directly from the app.
  - **Windows**: Uses the system native print wizard. Supports high-quality JPEG (95% quality) and automatic alpha flattening.
  - **macOS / Linux**: Automatically exports the image to a perfectly sized, margin-less PDF and opens it with the system default viewer for printing.
  - **Flexible Modes**: Print the entire image or just the currently zoomed-in "Visible Area" with precise cropping.
- **Theme Support** — choose between **Dark** (classic), **Light**, or **System** (follows OS preference) themes instantly via settings.
- **Windows Integration** — Register as a recommended image viewer in the Windows "Open With" menu via the settings panel (no admin required). Includes an "Associate Formats" dialog to select specific file types and a one-click "Remove Association" to cleanly uninstall all registry entries
- **Animated image playback** — animated GIF, APNG, and animated WebP play automatically with correct frame timing
- **Smooth navigation** — arrow keys or `PageUp`/`PageDown` for navigation, mouse wheel zoom, pan in 1:1 mode
- **Two scale modes** — *Fit to Window* (default) and *Original Size (1:1)*; toggle with `Z`
- **EXIF & XMP Metadata Display** — right-click an image to view detailed EXIF information or XMP properties. XMP extraction is optimized for fast, structured viewing of common tags (Creator, Copyright, Tool, etc.)
- **Modal Dialogs** — metadata and settings dialogs now behave as true modals; background interactions are blocked with a visual dimmer for a focused experience
- **Distraction-Free Mode** — hide all on-screen display (OSD) texts via settings for a pure image view
- **Resume Viewing** — optionally remember the last viewed image and automatically resume from it on next launch
- **Auto-play Slideshow** — configurable interval (0.5 s – 1 h), with optional loop / stop-at-end
- **Background music** — high-fidelity playback for MP3, FLAC, OGG, WAV, AAC, M4A, **APE**; includes specialized **audio device auto-reconnection** on Windows to handle exclusive-mode conflicts (e.g., from foobar2000)
- **CUE Sheet & Precise Navigation** — full support for `.cue` files (WAV+CUE, FLAC+CUE, etc.). Accurate track skipping based on `INDEX 01` timestamps is supported even for large single-file audio albums
- **Smart Metadata Extraction** — automatically extracts Title, Artist, and Track info from files. Supports built-in tags and external CUE descriptions
- **5-Button Control Bar** — compact UI bar (⏮ ⏪ ▶/⏸ ⏩ ⏭) for quick navigation between physical music files and logical CUE tracks
- **Dynamic Metadata Display** — two-line status display with smart truncation for long filenames (e.g., `Start...End.wav`), ensuring information remains legible in the settings panel
- **Real-time volume control** — slider in the settings panel, persisted between sessions
- **Recursive directory scan** — optionally include images in all sub-folders
- **Set as Desktop Wallpaper**: Right-click on any image to set it as your wallpaper with various layout modes (Crop, Fit, Stretch, Tile, Center).
- **Atmospheric Transitions**: Professional dual-texture transitions including **Cross-Fade**, **Zoom & Fade**, **Slide**, **Push**, **Page Flip**, **Ripple (Water)**, and **Curtain**.
- **Customizable Duration**: Fluid animations with adjustable duration (50ms - 2000ms).
- **Audio Integration**: Play background music during your viewing session.
- **CJK filename rendering** — loads standard CJK fonts so Chinese, Japanese, and Korean characters in file paths display correctly
- **Persistent settings** — all preferences are saved to `siv_settings.yaml` next to the executable and restored on next launch
- **Session restore** — last image directory and music path are remembered and auto-loaded on startup
- **Full-screen mode** — toggle with `F11` or `F`; app always starts windowed (OS title bar visible)
- **Modern UI** — sleek two-column settings panel, click the background to quickly dismiss, and fully adjustable font sizes (12-32px)
- **Image Preloading Toggle** — optionally disable neighbor preloading to save resources
- **Jump to image** — press `G` to open a *Go to image…* dialog and jump directly to any index
- **Smart Format Detection** — automatically identifies the true image format even if the file extension is mismatched (e.g., a JPEG file incorrectly named as `.png`), ensuring robust loading for mislabeled files.
- **File Deletion** — press `Delete` to move the current image to the Recycle Bin/Trash, or `Shift + Delete` to permanently remove it (no confirmation dialog for speed)
- **Context Menu** — right-click to copy the image's absolute path, copy the actual file to clipboard, view EXIF metadata, set as desktop wallpaper, **Rotate 90° CW/CCW**, or **Print** (Full or Visible Area) directly
- **Multi-Language Support (i18n)** — UI automatically adapts to system language (English, Simplified Chinese, Traditional Chinese - Taiwan & Hong Kong) with fallback support, and can be manually overridden in the settings panel.

---

## Controls

| Key / Action | Effect |
|---|---|
| `→` / `↓` / `PageDown` | Next image |
| `←` / `↑` / `PageUp` | Previous image |
| `Home` | First image |
| `End` | Last image |
| `+` / `-` | Zoom in / out |
| `*` (or `Numpad *`) | Reset zoom & pan |
| `Ctrl + Mouse wheel` | Zoom |
| `Mouse wheel` | Next / Previous image |
| `Space` | Pause / Resume slideshow |
| `Z` | Toggle Fit ↔ Original size |
| `G` | Open *Go to image…* dialog (jump to index) |
| `F` / `F11` | Toggle full-screen |
| `F1` | Open / close Settings panel |
| `Esc` | Exit full-screen / Close dialogs |
| `Left-Click (bg)` | Close Settings panel |
| `Right-click` | Open context menu (Copy Path / Copy File / View EXIF / View XMP / Set Wallpaper / Print) |
| `Delete` | Move current image to Recycle Bin / Trash |
| `Shift + Delete` | Permanently delete current image (no Recycle Bin) |
| `Ctrl + P` (or `Cmd + P`) | **Print** current image (Full Image) |
| `Ctrl + →` / `Ctrl + ←` | Rotate 90° CW / CCW |
| `Alt + Wheel Down / Up` | Rotate 90° CW / CCW |
| `Alt+F4` | Quit (Windows) |

---

## Settings Panel (`F1`)

| Setting | Description |
|---|---|
| **Directory** | Browse button to pick image folder, recursive scan toggle, preload toggle, and resume viewing toggle |
| **Display** | Full-screen toggle, scale-mode selector, and OSD info visibility toggle |
| **Slideshow** | Enable auto-advance to next image, set interval, and toggle loop playback |
| **Background Music** | Enable music, pick file or folder, navigation controls (⏮ ⏪ ▶/⏸ ⏩ ⏭), and adjust volume |
| **Font & UI** | Choose system font family, interface size, and UI **Theme** (Dark/Light/System) |
| **Language** | Manually switch between English, Simplified Chinese, and Traditional Chinese |
| **System Integration** | *(Windows only)* Register/unregister file type associations for the Windows "Open With" menu |


---

## Platform Support

| Platform | Status |
|---|---|
| Windows 10/11 (x64 / arm64) | ✅ Native support |
| Windows 7+ (x64) | ✅ Specialized Win7 x64 release package is compatible with Windows 7 and above* |
| macOS (Apple Silicon / Intel) | ✅ Native support |
| Linux (x64 / arm64) | ✅ Requires audio libraries (see below) |

*\*Note for Windows 7: Requires **Service Pack 1 (SP1)**, **KB2670838** (Platform Update for Windows 7), and a GPU driver that supports **DirectX 11**.*

---

## Building from Source

### Prerequisites

- [Rust](https://rustup.rs/) 1.85+ (edition 2024)
- On **Linux**: `libasound2-dev` (ALSA) or PipeWire
  ```bash
  sudo apt install libasound2-dev   # Debian / Ubuntu
  sudo dnf install alsa-lib-devel   # Fedora
  ```

### Build

```bash
git clone git@github.com:z16166/SimpleImageViewer.git
cd SimpleImageViewer

# Development build
cargo run

# Optimised release build
cargo build --release
# Output: target/release/SimpleImageViewer (or SimpleImageViewer.exe on Windows)
```

### Optional: Regenerate the app icon

```bash
cargo run --bin make_ico   # converts assets/icon.jpg → assets/icon.ico
```

---

---

## Settings File

`siv_settings.yaml` is written next to the executable after the first run:

```yaml
recursive: false
last_image_dir: "D:\\Photos"
auto_switch: true
auto_switch_interval: 5.0
loop_playback: true
scale_mode: fit_to_window
play_music: true
music_path: "D:\\Music"
volume: 0.8
music_paused: false
last_music_track: "D:\\Music\\Album\\CD1.flac"
font_family: "Microsoft YaHei"
font_size: 16.0
preload: true
language: "zh-CN"
theme: "system"
```

Delete the file to reset all settings to defaults.

---

## License

GNU General Public License v3 (GPL v3) — see [LICENSE](LICENSE).
