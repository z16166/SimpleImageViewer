# Simple Image Viewer (SimpleImageViewer)

A high-performance, cross-platform image viewer built with Rust and [egui](https://github.com/emilk/egui). Designed for fast browsing of large photo libraries with a clean, dark UI, background music playback, and persistent settings.
 
![Screenshot](assets/screenshot.jpg)

---

## Features

- **Fast image loading** — background thread pre-loads adjacent images so navigation is instant
- **Wide format support** — JPEG, PNG, GIF, BMP, TIFF, TGA, WebP, ICO, PNM, HDR, AVIF, HEIF/HEIC, QOI, EXR, PSD, PSB
- **Gigapixel image support** — tiled rendering engine for ultra-high-resolution images (100MP+); only visible tiles are uploaded to GPU, with LRU cache management to keep VRAM usage constant
- **PSD / PSB support** — native Photoshop Document reader; PSB (Large Document, 4 GB+) decoded via a custom streaming parser with automatic RAM safety check before loading
- **HEIF / HEIC Support** — native decoding of Apple iPhone high-efficiency photos via a pure-Rust parser (cross-platform, zero C++ dependencies)
- **Windows Integration** — Register as a recommended image viewer in the Windows "Open With" menu via the settings panel (no admin required). Includes an "Associate Formats" dialog to select specific file types and a one-click "Remove Association" to cleanly uninstall all registry entries
- **Animated image playback** — animated GIF, APNG, and animated WebP play automatically with correct frame timing
- **Smooth navigation** — arrow keys, mouse wheel zoom, pan in 1:1 mode
- **Two scale modes** — *Fit to Window* (default) and *Original Size (1:1)*; toggle with `Z`
- **EXIF & XMP Metadata Display** — right-click an image to view detailed EXIF information or XMP properties. XMP extraction is optimized for fast, structured viewing of common tags (Creator, Copyright, Tool, etc.)
- **Modal Dialogs** — metadata and settings dialogs now behave as true modals; background interactions are blocked with a visual dimmer for a focused experience
- **Distraction-Free Mode** — hide all on-screen display (OSD) texts via settings for a pure image view
- **Resume Viewing** — optionally remember the last viewed image and automatically resume from it on next launch
- **Auto-play Slideshow** — configurable interval (0.5 s – 1 h), with optional loop / stop-at-end
- **Background music** — high-fidelity playback for MP3, FLAC, OGG, WAV, AAC, M4A via [rodio](https://github.com/RustAudio/rodio); pick a single file or a folder (scanned recursively)
- **CUE Sheet & Precise Navigation** — full support for `.cue` files (WAV+CUE, FLAC+CUE, etc.). Accurate track skipping based on `INDEX 01` timestamps is supported even for giant album images
- **Smart Metadata Extraction** — automatically extracts Title, Artist, and Track info from files via [lofty](https://github.com/lofty-rb/lofty). Supports built-in tags and external CUE descriptions
- **5-Button Control Bar** — compact UI bar (⏮ ⏪ ▶/⏸ ⏩ ⏭) for quick navigation between physical music files and logical CUE tracks
- **Dynamic Metadata Display** — two-line status display with **Middle Truncation** algorithm for long filenames (e.g., `Start...End.wav`), ensuring information remains legible in the settings panel
- **Real-time volume control** — slider in the settings panel, persisted between sessions
- **Recursive directory scan** — optionally include images in all sub-folders
- **Set as Desktop Wallpaper**: Right-click on any image to set it as your wallpaper with various layout modes (Crop, Fit, Stretch, Tile, Center).
- **Atmospheric Transitions**: Professional dual-texture transitions including **Cross-Fade**, **Zoom & Fade**, **Slide**, **Push**, **Page Flip**, **Ripple (Water)**, and **Curtain**.
- **Customizable Duration**: Fluid animations with adjustable duration (50ms - 2000ms).
- **Audio Integration**: Play background music during your viewing session.
- **CJK filename rendering** — loads the system CJK font (Microsoft YaHei / PingFang / Noto CJK) so Chinese, Japanese, and Korean characters in file paths display correctly
- **Persistent settings** — all preferences are saved to `siv_settings.yaml` next to the executable and restored on next launch
- **Session restore** — last image directory and music path are remembered and auto-loaded on startup
- **Full-screen mode** — toggle with `F11`; app always starts windowed (OS title bar visible)
- **Modern UI** — sleek two-column settings panel, click the background to quickly dismiss, and fully adjustable font sizes (12-32px)
- **Image Preloading Toggle** — optionally disable neighbor preloading to save resources
- **Jump to image** — press `G` to open a *Go to image…* dialog and jump directly to any index
- **File Deletion** — press `Delete` to move the current image to the Recycle Bin/Trash, or `Shift + Delete` to permanently remove it (no confirmation dialog for speed)
- **Context Menu** — right-click to copy the image's absolute path, copy the actual file to clipboard, view EXIF metadata, or set as desktop wallpaper
- **Multi-Language Support (i18n)** — UI automatically adapts to system language (English, Simplified Chinese, Traditional Chinese - Taiwan & Hong Kong) with fallback support, and can be manually overridden in the settings panel.

---

## Controls

| Key / Action | Effect |
|---|---|
| `→` / `↓` | Next image |
| `←` / `↑` | Previous image |
| `Home` | First image |
| `End` | Last image |
| `+` / `-` | Zoom in / out |
| `*` (or `Numpad *`) | Reset zoom & pan |
| Mouse wheel | Zoom |
| `Space` | Pause / Resume slideshow |
| `Z` | Toggle Fit ↔ Original size |
| `G` | Open *Go to image…* dialog (jump to index) |
| `F11` | Toggle full-screen |
| `F1` / `Esc` / `Left-Click (bg)` | Open / close Settings panel |
| `Right-click` | Open context menu (Copy Path / Copy File / View EXIF / View XMP / Set Wallpaper) |
| `Delete` | Move current image to Recycle Bin / Trash |
| `Shift + Delete` | Permanently delete current image (no Recycle Bin) |
| `Alt+F4` | Quit (Windows) |

---

## Settings Panel (`F1`)

| Setting | Description |
|---|---|
| **Directory** | Browse button to pick image folder, recursive scan toggle, preload toggle, and resume viewing toggle |
| **Display** | Full-screen toggle, scale-mode selector, and OSD info visibility toggle |
| **Slideshow** | Enable auto-advance to next image, set interval, and toggle loop playback |
| **Background Music** | Enable music, pick file or folder, navigation controls (⏮ ⏪ ▶/⏸ ⏩ ⏭), and adjust volume |
| **Font & Appearance** | Choose system font family and interface size (applied instantly) |
| **System Integration** | *(Windows only)* Register/unregister file type associations for the Windows "Open With" menu |


---

## Platform Support

| Platform | Status |
|---|---|
| Windows 10/11 (Win 8+ required) | ✅ Primary target — native icon, Win32 audio |
| macOS (Apple Silicon / Intel) | ✅ Builds with minimal changes |
| Linux (X11 / Wayland) | ✅ Requires audio libraries (see below) |

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

### Windows 7 x64 Support (Experimental)

To build a version of the executable that runs on Windows 7, follow these steps:

1.  Download [VC-LTL-Binary.7z](https://github.com/Chuyu-Team/VC-LTL5/releases/download/v5.3.1/VC-LTL-Binary.7z) and extract it to `f:\win7\VC-LTL5`.
2.  Download [YY-Thunks-Lib.zip](https://github.com/Chuyu-Team/YY-Thunks/releases/download/v1.2.1-Beta.2/YY-Thunks-Lib.zip) and [YY-Thunks-Objs.zip](https://github.com/Chuyu-Team/YY-Thunks/releases/download/v1.2.1-Beta.2/YY-Thunks-Objs.zip), and extract both to `f:\win7\YY-Thunks`.
3.  Install the thunk CLI:
    ```powershell
    cargo install thunk-cli
    ```
4.  Run the build command:
    ```powershell
    set VC_LTL=f:\win7\VC-LTL5
    set YY_THUNKS=f:\win7\YY-Thunks
    thunk --os win7 --arch x64 -- --release
    ```
    *Note: The generated EXE will be a console application. Use [CFF Explorer](http://www.ntcore.com/exsuite.php) to change the subsystem from "Windows Console" to "Windows GUI".*

5.  Create a file named `combase.c` with the following content:
    ```c
    #pragma comment(linker, "/export:CoTaskMemFree=ole32.CoTaskMemFree")
    ```
6.  Open the **Visual Studio x64 Native Tools Command Prompt** and compile `combase.dll`:
    ```cmd
    cl.exe /LD combase.c /link /NODEFAULTLIB /NOENTRY /out:combase.dll
    ```
7.  Place `combase.dll` in the same directory as `SimpleImageViewer.exe`.

*Note: If flagged by antivirus software, please add an exception or ignore the warning.*

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
```

Delete the file to reset all settings to defaults.

---

## License

MIT — see [LICENSE](LICENSE).
