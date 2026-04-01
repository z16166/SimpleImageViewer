# Simple Image Viewer (siv)

基于 Rust 和 [egui](https://github.com/emilk/egui) 构建的高性能跨平台图片查看器。专为快速浏览大型图片库而设计，配备简洁的深色界面、后台音乐播放功能，以及持久化设置。

---

## 功能概要

Simple Image Viewer 是一款轻量、快速的桌面图片查看器。它在后台预加载相邻图片以实现即时翻页，支持自动幻灯片播放，并可在查看图片的同时播放背景音乐。所有用户设置均自动保存到磁盘，下次启动时自动恢复上次的图片目录和音乐。

---

## 特性列表

- **快速图片加载** — 后台线程预加载前后图片，翻页无延迟
- **广泛格式支持** — JPEG、PNG、GIF、BMP、TIFF、TGA、WebP、ICO、PNM、HDR
- **流畅导航** — 方向键翻页、鼠标滚轮缩放、1:1 模式下拖拽平移
- **两种缩放模式** — *适应窗口*（默认）和*原始尺寸（1:1）*；按 `Z` 切换
- **自动播放幻灯片** — 间隔时间可设置（0.5 秒 – 1 小时），支持循环/到末尾停止
- **背景音乐播放** — 通过 [rodio](https://github.com/RustAudio/rodio) 支持 MP3 和 FLAC；可选择单个文件或整个文件夹
- **实时音量调节** — 设置面板内提供滑动条，音量在会话间持久保存
- **递归目录扫描** — 可选扫描所有子文件夹中的图片
- **CJK 文件名渲染** — 自动加载系统 CJK 字体（微软雅黑 / PingFang / Noto CJK），正确显示路径中的中文、日文、韩文字符
- **持久化设置** — 所有偏好设置保存到可执行文件旁边的 `siv_settings.yaml`，下次启动时自动加载
- **会话恢复** — 记住上次的图片目录和音乐路径，启动时自动加载
- **全屏模式** — 按 `F11` 切换；程序始终以窗口模式启动（显示系统标题栏）
- **字体选择与大小调节** — 可从系统字体库中选择 UI 字体，并自由调节界面缩放（12–32 像素）
- **图片预加载开关** — 可选择禁用后台相邻图片预加载，以节省系统资源
- **右键上下文菜单** — 在图片上点击右键可快速复制文件完整路径，或直接复制文件对象到剪贴板以便于在资源管理器中粘贴

---

## 操作说明

### 键盘快捷键

| 按键 / 操作 | 功能 |
|---|---|
| `→` / `↓` | 下一张图片 |
| `←` / `↑` | 上一张图片 |
| `Home` | 第一张图片 |
| `End` | 最后一张图片 |
| `+` / `-` | 放大 / 缩小 |
| `*`（或小键盘 `*`） | 重置缩放和平移 |
| 鼠标滚轮 | 缩放 |
| `空格键` | 暂停 / 继续自动播放 |
| `Z` | 切换适应窗口 ↔ 原始尺寸 |
| `F11` | 切换全屏 |
| `F1` / `Esc` | 打开 / 关闭设置面板 |
| `鼠标右键` | 打开上下文菜单（复制路径 / 复制文件） |
| `Alt+F4` | 退出（Windows） |

### 使用流程

1. 启动程序，按 `F1` 打开设置面板
2. 点击 **📁 Pick** 选择图片目录（上次目录会自动恢复）
3. 扫描完成后设置面板自动关闭，使用方向键或鼠标滚轮浏览图片
4. 如需背景音乐：勾选"Play background music"，点击 **🎵 File** 或 **📂 Dir** 选择音乐文件/目录

---

## 设置面板说明（`F1`）

| 设置项 | 说明 |
|---|---|
| **Directory（图片目录）** | 选择图片文件夹；上次路径自动保存和恢复 |
| **Recursive scan（递归扫描）** | 扫描所有子目录中的图片 |
| **Display（显示）** | 全屏切换和缩放模式选择 |
| **Auto-Switch（自动播放）** | 启用幻灯片、设置间隔、切换循环模式 |
| **Background Music（背景音乐）** | 启用音乐、选择文件或目录、调节音量 |
| **Font & Appearance（字体与外观）** | 选择系统字体族和界面大小（松开鼠标后生效） |
| **Preloading（预加载）** | 切换是否预加载相邻图片 |

---

## 平台支持

| 平台 | 状态 |
|---|---|
| Windows 10/11（最低兼容 Win 8） | ✅ 主要目标平台 — 原生图标、Win32 音频 |
| macOS（Apple Silicon / Intel） | ✅ 少量改动即可编译运行 |
| Linux（X11 / Wayland） | ✅ 需要音频库（见下文） |

---

## 编译说明

### 前置条件

- [Rust](https://rustup.rs/) 1.85+（edition 2024）
- **Linux** 系统需安装音频库：
  ```bash
  sudo apt install libasound2-dev   # Debian / Ubuntu
  sudo dnf install alsa-lib-devel   # Fedora
  ```

### 编译步骤

```bash
git clone git@github.com:z16166/SimpleImageViewer.git
cd SimpleImageViewer

# 开发版本（含调试信息）
cargo run

# 发布版本（优化构建）
cargo build --release
# 输出：target/release/siv（Windows 下为 siv.exe）
```

### 可选：重新生成应用图标

```bash
cargo run --bin make_ico   # 将 assets/icon.jpg 转换为 assets/icon.ico
```

---

## 设置文件说明

首次运行后，`siv_settings.yaml` 会自动生成在可执行文件旁边：

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

删除该文件可将所有设置重置为默认值。

---

## 许可证

MIT — 详见 [LICENSE](LICENSE)。
