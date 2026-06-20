# 导航窗口功能代码审核 — Cursor Round 1

**分支：** `codex/dir-tree-navigation`  
**基线：** `main...HEAD`（49 commits，99 files，+13408 / −1397 lines）  
**审核日期：** 2026-06-20  
**审核者：** Cursor  
**对照规范：** `docs/review-checklist.md`

---

## 1. 审核范围与功能概述

本分支实现导航窗口（目录树 + 图片列表），支持 Embedded（SidePanel）与 Detached（deferred viewport 独立 OS 窗口）两种模式，并与主窗口同步当前图片选中状态。主要变更包括：

| 区域 | 关键路径 |
|------|----------|
| 导航 UI / 状态机 | `src/app/directory_tree/`（`app.rs`、`ui.rs`、`domains.rs`、`view.rs`、`workers.rs`、`strip_previews.rs` 等） |
| 主窗口集成 | `src/app/eframe_app.rs`、`src/app/logic_update.rs` |
| 扫描 / 缩略图 | `src/scanner.rs`、`src/loader/decode/directory_tree_thumb.rs` |
| Places / 平台 | `src/directory_tree_places/` |
| 窗口持久化 | `src/settings.rs` |
| 异步对话框 | `src/app/folder_picker.rs`、`src/app/rfd_parent.rs` |
| 多 viewport fork | `patched-crates/eframe/`、`patched-crates/egui-wgpu/` |

---

## 2. 总体结论

**架构质量：良好，可合并。** 模块已完成从单体到多文件拆分，RCU 快照 + generation 防陈旧数据 + 后台 worker 隔离 I/O 等设计符合 checklist 核心要求。eframe fork 与 `LogicPass` 拆分逻辑清晰，Detached 模式下的 logic/ui 分工有据可查。

**仍建议跟进 3 项 High 级问题**（GPU 上传路径、metadata 阻塞 send、节点上限不完整），其余多为 Medium/Low 的 I/O 复用、背压一致性与可维护性项。无 Critical 级阻断合并项。

| 严重级别 | 数量 | 说明 |
|----------|------|------|
| Critical | 0 | — |
| High | 3 | 见 §3.1 |
| Medium | 14 | 见 §3.2 |
| Low / Info | 10 | 见 §3.3 |

---

## 3. 发现项

### 3.1 High

#### H1 — logic 路径执行 GPU 纹理上传（checklist #3）

| 项 | 内容 |
|----|------|
| **位置** | `strip_previews.rs:436-515` → `cache_directory_tree_strip_thumbnail()` → `directory_tree_strip_cache.rs:140-184` |
| **描述** | `run_directory_tree_logic_updates()` 在 logic 阶段轮询 strip 预览结果并调用 `ctx.load_texture()` 上传 GPU。checklist #3 禁止在 `logic()` 中执行单帧大量 GPU 上传；当前实现将解码后的 `ColorImage` 在 logic 路径直接 `load_texture`。 |
| **建议** | 将 GPU 上传推迟到 paint 阶段：logic 仅缓存 CPU 侧 `ColorImage` 或 pending 队列，在 `ui()` / viewport paint 中批量上传；或限制每帧上传数量并移入 ROOT viewport 的 paint pass。 |

#### H2 — metadata 请求使用阻塞 `send`（checklist #3、#32）

| 项 | 内容 |
|----|------|
| **位置** | `app.rs:60-66`、`1160-1164` |
| **描述** | `send_directory_tree_metadata_request()` 对 bounded(64) 的 channel 使用阻塞 `metadata_tx.send()`。children 请求已改用 `try_send` 并展示 i18n 繁忙提示；metadata 在 worker 落后（200 路径批 × 文件系统 `metadata()`）时可能阻塞 logic 线程。 |
| **建议** | 与 children 路径对齐：`try_send` + 重试/backoff + 用户可见 `sync_warning` 或 tree 节点错误提示。 |

#### H3 — 节点上限 `MAX_DIRECTORY_TREE_NODES`  enforcement 不完整（checklist #8、#15）

| 项 | 内容 |
|----|------|
| **位置** | `mod.rs:878-896`（children 结果应用处） vs `438-647`（`initialize_places`、`reveal_selected_dir`、`ensure_network_share_mounted`、`set_selected_tree_node`） |
| **描述** | 8192 节点上限仅在 `apply_children_result` 插入子节点时检查。Places 初始化、UNC 挂载、选中路径展开等路径通过 `or_insert_with` 直接插入，无 cap。极端深度浏览可突破上限，触发 `node_store.rs:60,73` 的 `expect("directory tree node arena overflow")` panic。 |
| **建议** | 在 `DirectoryTreeNodeStore::or_insert_with` 统一检查 cap 并返回 `Result`/错误枚举；或在所有插入点共享 `try_insert_node()` helper。 |

---

### 3.2 Medium

#### M1 — worker 循环无超时 / 优雅退出（checklist #32）

| 项 | 内容 |
|----|------|
| **位置** | `workers.rs:83,104` |
| **描述** | children/metadata worker 在 `request_rx.recv()` 上永久阻塞，无 shutdown handshake。进程退出依赖 OS 回收线程，不符合 checklist #32「channel 通信需合理超时」的严格解读。 |
| **建议** | 应用退出时发送 shutdown 哨兵或使用 `recv_timeout` + 检查 `Arc<AtomicBool>` 运行标志。 |

#### M2 — read_dir 超时后 orphan 线程（checklist #5、#32）

| 项 | 内容 |
|----|------|
| **位置** | `workers.rs:182-219` |
| **描述** | 30s 超时后 helper 线程不可平台级取消，最多 4 个 orphan 线程在网络路径卡住时残留。已在注释中说明设计取舍。 |
| **建议** | 维持现状可接受；若 profiling 显示资源压力，考虑进程级 watchdog 或缩短 inflight 上限。 |

#### M3 — Detached viewport 通过 `AtomicPtr` 读取主 app（checklist #6）

| 项 | 内容 |
|----|------|
| **位置** | `app.rs:1204-1267` |
| **描述** | deferred viewport paint 存储 `*mut ImageViewerApp`，通过 `unsafe { (*ptr)... }` 读取 `active_modal`、`image_files` 等。依赖「仅 UI 线程访问」假设，无显式同步；线程模型变更时 fragile。 |
| **建议** | 长期可改为 `Arc<AppSnapshot>` 或 channel 传递 paint 所需的最小只读快照。 |

#### M4 — `sync_images` 调用块重复（checklist #14）

| 项 | 内容 |
|----|------|
| **位置** | `app.rs:1093-1157` |
| **描述** | 正常 sync 与 scan 完成后 resort 分支各写一遍相同的 6 参数 `list.sync_images(...)` 调用。 |
| **建议** | 提取 `fn sync_list_from_app(&mut self, list: &mut ..., extra: SyncFlags) -> Option<...>` 减少漂移风险。 |

#### M5 — Places 加载 / 错误 UI 重复（checklist #14）

| 项 | 内容 |
|----|------|
| **位置** | `app.rs:1300-1317`、`ui.rs:709-719` |
| **描述** | embedded loading panel 与 folder panel 绘制路径重复展示 places-loading / workers-unavailable 状态。 |
| **建议** | 提取 `draw_places_status(ui, state)` 公共函数。 |

#### M6 — 大文件可维护性（checklist #12）

| 项 | 内容 |
|----|------|
| **位置** | `ui.rs`（~1504 行）、`app.rs`（~1356 行） |
| **描述** | 均未超过 2000 行硬性上限；`ui.rs` 仍混合布局数学、tree paint、image list、UNC 路径等，后续改动 review 成本较高。 |
| **建议** | 可选进一步拆分 `ui_image_list.rs`、`ui_tree_panel.rs`（非阻断）。 |

#### M7 — 局部 magic number（checklist #1）

| 项 | 内容 |
|----|------|
| **位置** | `app.rs:1032`（`MAX_DEFER_FRAMES: u32 = 120`）、`ui.rs:1081`（`0.62` 列宽权重） |
| **描述** | 模块内多数阈值已命名常量，上述两处仍为局部字面量。 |
| **建议** | 提升至 `mod.rs` 常量区与 `DIRECTORY_TREE_*` 命名风格一致。 |

#### M8 — strip downsample 多余 clone（checklist #22）

| 项 | 内容 |
|----|------|
| **位置** | `directory_tree_strip_cache.rs:316` |
| **描述** | `downsample_decoded_for_strip()` 在无缩放路径仍 `decoded.clone().into_rgba8_image()`，大图 strip worker 上额外堆分配。 |
| **建议** | 无缩放时借用现有 buffer 或 `Arc` 共享。 |

#### M9 — 缩略图解码路径重复打开文件（checklist #29）

| 项 | 内容 |
|----|------|
| **位置** | `directory_tree_thumb.rs:56-66,132-140,193-194` |
| **描述** | 入口已 `map_file`，但 EXIF fallback 仍 `extract_exif_thumbnail(path)`（二次 `File::open`）；尺寸探测失败时 `probe_still_image_logical_size(path)` 再次映射；TIFF 路径可能经 `tiff_may_be_camera_raw` / `probe_libraw_can_open` 第三次打开。 |
| **建议** | 将 `Mmap`/`&[u8]` 贯穿 EXIF、probe、TIFF/RAW sniff 全链路。 |

#### M10 — strip worker 结果 channel 阻塞 send（checklist #32）

| 项 | 内容 |
|----|------|
| **位置** | `strip_previews.rs:351,601`；channel `bounded(16)`（`lifecycle.rs`） |
| **描述** | Rayon pool 线程对 bounded channel 使用阻塞 `tx.send(job)`。UI 停止 drain 时 worker 线程无限阻塞。 |
| **建议** | 改为 `try_send` + drop/warn（与 inflight release 侧信道一致），或增大 buffer 并文档化背压策略。 |

#### M11 — Places 加载线程无超时（checklist #32）

| 项 | 内容 |
|----|------|
| **位置** | `app.rs:330-387` |
| **描述** | Windows Shell/COM Places 枚举在专用线程执行，主线程仅 `try_recv`；挂起时 UI 永久显示「Loading places…」。 |
| **建议** | 线程内 watchdog 超时 + i18n `places_load_failed`；或 cancel + 重试。 |

#### M12 — 扫描 batch send 潜在无限重试（checklist #32）

| 项 | 内容 |
|----|------|
| **位置** | `scanner.rs:502-526` |
| **描述** | `send_scan_message` 在 `try_send` 失败时 2ms sleep 循环直至成功或 cancel。若 UI 永不 drain 且 cancel 未设置，扫描线程可空转。 |
| **建议** | 增加最大重试次数或 wall-clock 超时后 abort scan 并发送 Error/Done。 |

#### M13 — worker 结果 channel 阻塞 send（checklist #32）

| 项 | 内容 |
|----|------|
| **位置** | `workers.rs:86-91,115-121` |
| **描述** | children/metadata worker 对 bounded(64) 结果 channel 使用阻塞 `send()`。 |
| **建议** | 与请求侧一致采用 `try_send` + 丢弃计数/日志。 |

#### M14 — eframe fork：immediate viewport 不调用 logic（架构）

| 项 | 内容 |
|----|------|
| **位置** | `wgpu_integration.rs:1250-1415`、`glow_integration.rs:1780-1980` |
| **描述** | 仅 deferred viewport paint 调用 `app.logic()`；`render_immediate_viewport` 不调用。当前 Detached 导航使用 `show_viewport_deferred`，无即时问题；未来若新增 immediate 子窗口会缺少 scan/loader 逻辑。 |
| **建议** | 在 `FORK-MERGE.md` 或 `epi.rs` 文档中明确「子窗口须使用 deferred viewport」约束。 |

---

### 3.3 Low / Info

| ID | 位置 | 描述 | Checklist |
|----|------|------|-----------|
| L1 | `node_store.rs:60,73` | 生产路径 `expect` panic（见 H3） | #15 |
| L2 | `folder_picker.rs:150-169` | worker 上 `pollster::block_on(AsyncFileDialog)` 无超时；主线程 `try_recv` 安全 | #32 |
| L3 | `app.rs:337` | 日志字符串 `"places load panicked"` 为硬编码英文（UI 已 i18n） | #4 |
| L4 | `app.rs:1205` | `viewpaint_app` 每帧写入、进程存活期不置 null | #6 |
| L5 | `domains.rs:349-354` | `publish_preview_snapshot()` 在 GPU revision bump 时 clone ≤128 个 `TextureHandle` | #11 |
| L6 | `directory_tree_thumb.rs:132-140` | `file_name().to_str()` 路由；Windows 非 UTF-8 路径可能误判扩展名 | #28 |
| L7 | `app.rs:676-680` | 持久化 panel 宽度未 clamp 到 min width 常量 | #17 |
| L8 | `directory_tree_places/fs.rs:22-24` | `is_dir()` + `read_dir()` 双重目录访问 | #29 |
| L9 | `scanner.rs:187` | 每次 `scan_directory` spawn 新 OS 线程（非池化）；快速切换文件夹可能短暂多线程 | #5 |
| L10 | `directory_tree_thumb.rs` 模块头 | Linux RAW strip 无 WIC/ImageIO fast-path；已在 README 说明 | #16 |

**eframe fork 附加 Info：**

- `logic_shared` 4ms coalesce + aux early return（`eframe_app.rs:158-166`）：deferred child paint 末尾 `RepaintNow(ROOT)` 保证 ROOT `logic_root_only` 同链执行；若 ROOT 链被跳过而 aux 仍 repaint，存在一帧逻辑滞后风险（Low）。
- `queue_write_with_fallback`（`egui-wgpu/renderer.rs:29-64`）每 upload 分配 CPU `Vec`：正确性修复到位，高负载双 viewport 下有 alloc 开销（Low）。
- `Frame::painting_viewport_id` 与 `LogicPass::painting_viewport_id` 信息重复（`epi.rs`）：调用方须同步更新（Info）。

---

## 4. Checklist 合规摘要

| # | 主题 | 结论 |
|---|------|------|
| 1 | 禁止 magic number | 大部分已命名；M7 残余 |
| 2 | 字符串常量去重 | locale YAML + 模块 sentinel 路径常量 |
| 3 | ui/logic 无同步耗时 I/O | **H1 GPU 上传、H2 阻塞 send 待修**；read_dir/metadata/scan 已 worker 化 |
| 4 | i18n | 用户可见字符串均 `t!("directory_tree.*")` 等；L3 仅日志英文 |
| 5 | 无限创建线程 | scan 每任务一线程（L9）；worker 固定 2+2；strip pool max 2 |
| 6 | 共享数据 race | RCU + generation；M3 AtomicPtr 为已知 fragile 点 |
| 7 | unsafe 泄漏 | COM RAII（`windows.rs`）；viewport ptr 无 paired null（L4） |
| 8 | 缓存上限 | strip 128、channel 64/16；**H3 节点 cap 不完整** |
| 9 | 频繁 info 日志 | 导航模块 hot path 无 `log::info!` |
| 10 | generation 匹配 | tree/list/strip 均有校验 + 单测 |
| 11 | 耗时操作移出循环 | strip poll 在 logic 每帧；GPU upload 应移出（H1） |
| 12 | 单文件 ≤2000 行 | 已拆分；最大 `ui.rs` ~1504 行 |
| 13 | GPLv3 头 | 新增 `.rs` 均已添加 |
| 14 | 重复代码 | M4、M5 |
| 15 | 错误传播 / panic | 多数错误 i18n 展示；H3 expect panic |
| 16 | 跨平台 | Windows/macOS/Linux 分支完整；L10 Linux RAW strip 已知差距 |
| 17 | 外部输入防御 | scanner 跳过系统目录、junction 边界；L7 宽度 clamp |
| 18 | 减少嵌套 | 广泛使用 `let else`、early return |
| 19 | 复杂策略注释 | orphan 线程、O(n) eviction、locale-free sort 均有注释 |
| 20 | README/CHANGELOG | **已同步**（2.7.0 用户向描述，无实现细节泄露） |
| 21 | 栈上大变量 | 未发现 |
| 22 | 大像素零拷贝 | M8、M9 有改进空间 |
| 24 | mmap 优先 | 入口 mmap；M9 未贯穿全格式 |
| 26 | HDR/SDR 管线交叉 | strip 强制 SDR（`DIRECTORY_TREE_THUMB_HDR_CAPACITY = 1.0`） |
| 28 | 中文路径 | `PathBuf` 全链路；L6 `to_str()` 边缘 |
| 29 | 避免重复打开 | M9、L8 |
| 30 | 枚举匹配 | sort column、browse mode、preview stage 均枚举 |
| 32 | channel 超时 | read_dir 30s、scan stall 60s 良好；M1/M10-M13 待统一 |
| 33 | parking_lot | hot mutex 已用 `parking_lot::Mutex` |

---

## 5. 做得好的方面

1. **RCU 绘制模型** — writer mutex + `ArcSwap` 不可变快照（`domains.rs`、`view.rs`）；paint 用 `try_lock()` 避免与 logic 死锁（`app.rs:127-156`）。
2. **generation 防陈旧** — tree `generation`、`file_metadata_generation`、strip `image_list_generation` 三层校验；`tests.rs` 有专项单测。
3. **请求合并** — children 按 path dedup；metadata 按 generation 合并并 `METADATA_BATCH_SIZE` 分片（`workers.rs:36-77`）。
4. **有界资源** — strip cache 128、strip inflight 2、channel 64/16、read_dir inflight cap 4。
5. **错误用户可见** — read 失败、超时、channel 繁忙、sync defer 丢弃、节点 cap 均 i18n 展示。
6. **eframe fork 设计** — `LogicPass` + `painting_viewport_id` 拆分 shared/root-only logic；deferred child paint 触发 ROOT autosave（ISSUE-20）；`FORK-MERGE.md` 维护清单完整。
7. **输入分工清晰** — 键盘、主题、drag-drop 仅在 ROOT `ui()`；logic 在 child repaint 时不处理 hotkey（`eframe_app.rs:174-183`）。
8. **测试覆盖** — `tests.rs` ~872 行：generation、sort、UNC、coalesce、scroll、alias tree 等。
9. **模块拆分** — 自 ~4408 行单体拆为 10 个文件，均在 2000 行限额内。

---

## 6. 产品决策 / 已知限制（非 backlog）

| 项 | 说明 |
|----|------|
| locale-aware 文件名排序 | 保持 Unicode 码点序（`sort.rs` 注释）；中文目录可能与资源管理器不一致 |
| Network lazy UNC | 启动不枚举 `FOLDERID_NetworkFolder`；UNC 动态挂载 |
| Linux strip RAW fast-path | 无 WIC/ImageIO；主窗口加载路径不受影响 |
| read_dir orphan 线程 | 30s 超时后线程不可取消；inflight 回收已文档化 |

---

## 7. 建议优先级

| 优先级 | ID | 行动 |
|--------|-----|------|
| P0（合并前建议） | H1 | strip GPU 上传移至 paint 或限帧 |
| P0 | H2 | metadata `try_send` + 用户提示 |
| P0 | H3 | 节点 cap 全局 enforcement |
| P1 | M9、M10 | mmap 贯穿 + strip worker 非阻塞 send |
| P1 | M4、M5 | 重复代码小 refactor |
| P2 | M1、M11-M13 | channel 背压 / 超时策略统一 |
| P2 | M6、M7 | 可维护性 polish |
| P3 | L1-L10 | 边缘 case / 文档 |

---

## 8. 合并建议

**建议合并**，条件：P0 三项（H1–H3）在合并前修复或明确接受风险并登记 issue。当前无 Critical 级数据损坏 / 安全漏洞；eframe fork 已在 Linux/macOS Detached 手工验证（见 `review/summary.md` §3 ISSUE-16）。

后续 Round 2 应重点验证 H1–H3 修复补丁，并对 M9/M10 做 profiling 驱动的 I/O 优化。

---

## 9. 三审合并修复记录（2026-06-20）

对照 `review/deepseek-round1.md`、`review/trae-round1.md` 与本报告，已落地以下修复：

| 来源 | ID | 修复 |
|------|-----|------|
| deepseek | **C1** | `prefetch_circular_distance` 运算符优先级 + 单测 |
| deepseek | **C2** | `patched-crates/eframe/run.rs` 同步 RepaintNow 重入守卫 |
| deepseek | **C3** | `any_active_output_supports_hdr` 与 `dxgi_output_hdr_active` 策略一致 |
| deepseek | **C4** | `READ_DIR_INFLIGHT_PATHS` 阻止同路径重复 orphan read |
| deepseek | **C5** / trae #1 | `extract_tile` 检查 `ComGuard` + `CopyPixels` 失败日志 |
| deepseek | **H1** | `logic_update` 使用 `saturating_duration_since` |
| deepseek | **H5** | `trigger_current_hdr_fallback_refinement_if_needed` 边界检查 |
| deepseek | **H6** / trae #4 | `viewpaint_app` Release/Acquire + 安全文档 |
| deepseek | **H8** | 隐藏/最小化时不更新窗口 placement 缓存 |
| deepseek | **H11** | **核实为误报**：`permute_image_file_arrays` 已 permute `file_modified_unix_by_index` |
| cursor | **H1–H3** | strip GPU 上传推迟至 paint；metadata `try_send`；节点 cap 全局 enforcement |
| trae | **#2** | `ensure_strip_worker_com_initialized` 重命名 |
| trae | **#5** | `#[cfg(target_os = "windows")]` 统一 |
| trae | **#8** | read_dir inflight 使用 AcqRel/Acquire |
| deepseek | **H4** | `tile_cache::permute_images` 增加 `debug_assert!` |

**低优先级 backlog（第二轮已处理）：**

| 项 | 状态 |
|----|------|
| strip 解码 mmap 贯穿（M9） | 已修复：`directory_tree_thumb` EXIF/TIFF sniff 复用 mmap |
| `sync_images` 重复块 refactor | 已修复：`sync_directory_tree_list_images()` |
| Places UI 重复 | 已修复：`draw_directory_tree_places_status()` |
| 删除/复制/剪切/文件夹 picker 线程 join | 已修复：`BackgroundThreadJoiner` + `on_exit` |
| strip reorder 增量 invalidation | 已修复：`permute_directory_tree_strip_after_image_list_reorder()` |
| eframe immediate viewport logic | **文档化**：应用仅用 `show_viewport_deferred`；见 `FORK-MERGE.md` |
| Places/COM 部分失败降级 | 第一轮已修复（空 Places） |
| Linux RAW strip fast-path | 未在本轮 scope（仍待单独迭代） |

---
