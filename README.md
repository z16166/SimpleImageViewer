# Simple Image Viewer (siv)

A high-performance, cross-platform image viewer built with Rust and [egui](https://github.com/emilk/egui). Designed for fast browsing of large photo libraries with a clean, dark UI, background music playback, and persistent settings.
 
![Screenshot](assets/screenshot.jpg)

---

## Features

- **Fast image loading** — background thread pre-loads adjacent images so navigation is instant
- **Wide format support** — JPEG, PNG, GIF, BMP, TIFF, TGA, WebP, ICO, PNM, HDR
- **Animated image playback** — animated GIF, APNG, and animated WebP play automatically with correct frame timing
- **Smooth navigation** — arrow keys, mouse wheel zoom, pan in 1:1 mode
- **Two scale modes** — *Fit to Window* (default) and *Original Size (1:1)*; toggle with `Z`
- **EXIF Metadata Display** — right-click an image to view detailed EXIF information in a resizable window
- **Distraction-Free Mode** — hide all on-screen display (OSD) texts via settings for a pure image view
- **Resume Viewing** — optionally remember the last viewed image and automatically resume from it on next launch
- **Auto-play Slideshow** — configurable interval (0.5 s – 1 h), with optional loop / stop-at-end
- **Background music** — MP3, FLAC, OGG, WAV, AAC, M4A playback via [rodio](https://github.com/RustAudio/rodio); pick a single file or a folder (scanned recursively)
- **Now Playing display** — the filename of the current track is displayed in the settings panel
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
- **Context Menu** — right-click to copy the image's absolute path, copy the actual file to clipboard, view EXIF metadata, or set as desktop wallpaper

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
| `Right-click` | Open context menu (Copy Path / Copy File / View EXIF / Set Wallpaper) |
| `Alt+F4` | Quit (Windows) |

---

## Settings Panel (`F1`)

| Setting | Description |
|---|---|
| **Directory** | Browse button to pick image folder, recursive scan toggle, preload toggle, and resume viewing toggle |
| **Display** | Full-screen toggle, scale-mode selector, and OSD info visibility toggle |
| **Slideshow** | Enable auto-advance to next image, set interval, and toggle loop playback |
| **Background Music** | Enable music, pick file or folder, and adjust volume |
| **Font & Appearance** | Choose system font family and interface size (applied instantly) |


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
# Output: target/release/siv  (or siv.exe on Windows)
```

### Optional: Regenerate the app icon

```bash
cargo run --bin make_ico   # converts assets/icon.jpg → assets/icon.ico
```

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
font_family: "Microsoft YaHei"
font_size: 16.0
preload: true
```

Delete the file to reset all settings to defaults.

---

## License

MIT — see [LICENSE](LICENSE).
