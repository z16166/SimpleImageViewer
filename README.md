# Simple Image Viewer (siv)

A high-performance, cross-platform image viewer built with Rust and [egui](https://github.com/emilk/egui). Designed for fast browsing of large photo libraries with a clean, dark UI, background music playback, and persistent settings.

---

## Features

- **Fast image loading** — background thread pre-loads adjacent images so navigation is instant
- **Wide format support** — JPEG, PNG, GIF, BMP, TIFF, TGA, WebP, ICO, PNM, HDR
- **Smooth navigation** — arrow keys, mouse wheel zoom, pan in 1:1 mode
- **Two scale modes** — *Fit to Window* (default) and *Original Size (1:1)*; toggle with `Z`
- **Auto-switch slideshow** — configurable interval (0.5 s – 1 h), with optional loop / stop-at-end
- **Background music** — MP3 and FLAC playback via [rodio](https://github.com/RustAudio/rodio); pick a single file or an entire folder
- **Real-time volume control** — slider in the settings panel, persisted between sessions
- **Recursive directory scan** — optionally include images in all sub-folders
- **CJK filename rendering** — loads the system CJK font (Microsoft YaHei / PingFang / Noto CJK) so Chinese, Japanese, and Korean characters in file paths display correctly
- **Persistent settings** — all preferences are saved to `siv_settings.yaml` next to the executable and restored on next launch
- **Session restore** — last image directory and music path are remembered and auto-loaded on startup
- **Full-screen mode** — toggle with `F11`; app always starts windowed (OS title bar visible)

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
| `Z` | Toggle Fit ↔ Original size |
| `F11` | Toggle full-screen |
| `F1` / `Esc` | Open / close Settings panel |
| `Alt+F4` | Quit (Windows) |

---

## Settings Panel (`F1`)

| Setting | Description |
|---|---|
| **Directory** | Browse button to pick image folder; last path shown and restored on restart |
| **Recursive scan** | Include images in all sub-directories |
| **Display** | Full-screen toggle and scale-mode selector |
| **Auto-Switch** | Enable slideshow, set interval, toggle loop |
| **Background Music** | Enable music, pick file or folder, adjust volume |

---

## Platform Support

| Platform | Status |
|---|---|
| Windows 10/11 (x64) | ✅ Primary target — native icon, Win32 audio |
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
```

Delete the file to reset all settings to defaults.

---

## License

MIT — see [LICENSE](LICENSE).
