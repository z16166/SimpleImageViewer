# 导航窗口代码审核汇总

**分支：** `codex/dir-tree-navigation`  
**汇总日期：** 2026-06-20  
**来源：** `review/` 下 3 位审核者 × 6 轮，共 18 份 `*.md`  
**对照代码：** 当前工作区 `src/` 与 `patched-crates/`（非 git 历史快照）

---

## 1. 审核历程概览

| 审核者 | 轮次 | 基线 | 结论演进 |
|--------|------|------|----------|
| **cursor** | R1–R3 | 未提交修复补丁 | R1 发现 Major 9 项；R2–R3 大部分 actionable 项已关闭，保留结构性债务 |
| **cursor** | R4 | `main...HEAD` 全量 diff | 模块已拆分；发现 C1/C2 等阻断项与 H1–H6 正确性问题 |
| **cursor** | R5–R6 | Round 4/5 修复补丁 | R4 阻断项已修；R5–R6 残余 8/9 项已落地；R6 指出 `sync_defer_dropped` UI 不可见（**当前代码已通过 `sync_warning` 修复**） |
| **deepseek** | R1–R3 | 同 cursor R1–R3 | 强调 eframe fork 维护成本与锁竞争；R3 判定可合并 |
| **deepseek** | R4 | 全量 diff | 无 Critical；Medium 5 项多为架构/边缘 case |
| **deepseek** | R5–R6 | 修复补丁 | 43/43 actionable 项验证通过；N2 `all_files_for_done` 内存仍为 Low |
| **trae** | R1–R3 | 同 cursor R1–R3 | 与 cursor 高度重叠；R2 发现 worker coalesce 丢请求（R3 已修） |
| **trae** | R4 | 全量 diff | 10 项；tr#1 read_dir inflight 等在 R5 已修 |
| **trae** | R5–R6 | 修复补丁 | R5-H1/H2/M3 等在 R6 已修；R6 判定可合并 |

**跨轮已关闭且当前代码可确认修复的代表项（不再列入下文）：** Places 异步化、EXIF-first 缩略图、mmap 单次映射、generation 体系、扫描/ read_dir 超时、channel 有界与 loading 卡死、Linux folder_picker 编译、键盘双派发、scan Done 排序与 strip invalidate、`sync_warning` 用户可见提示、inflight_release 侧信道、nodes cap 与 `loaded_children` 等。2026-06-20 补丁关闭 ISSUE-04/05/07/09/11/12/16/25；同日后补丁关闭 ISSUE-08/10/22/23/24/26/27（见 §3）。Detached/Embedded 平台问题（egui-wgpu staging panic、Embedded 导航 splitter 拖拽）已修并手工验证 Linux/macOS。

---

## 2. 仍存在于当前代码中的 Issue 列表

以下条目经对照 **当前源码** 核实仍为未修改或仅部分缓解的问题；按优先级排序。

---

### 2.1 结构性 / 可维护性（Major）

_（ISSUE-01 已通过 ArcSwap RCU 重构，见 §3。ISSUE-02 已修复。）_

### 2.2 性能与 I/O（Medium）

_（ISSUE-04、05、07、08、09、11、12 已于 2026-06-20 修复或文档化，见 §3。）_

_（ISSUE-10 已于 2026-06-20 标注为已知设计取舍，见 §3。）_

### 2.3 功能 / 体验（Medium）

#### ISSUE-13 · 文件名排序非 locale-aware

| 项 | 内容 |
|----|------|
| **来源** | cursor R1 m2、R2 m-R2-5；trae R2 #9 |
| **位置** | `src/app/directory_tree/sort.rs:86–88` — `to_string_lossy().to_lowercase()` |
| **现状** | 中文等文件名按 Unicode 码点序，非系统 locale  collation。 |
| **影响** | 中文目录下列头「名称」排序与用户预期（拼音/笔画）可能不一致。 |
| **建议修复** | 引入 `icu_collator` 或平台 collation API；文档标注为已知限制亦可。 |

#### ISSUE-15 · Linux 非 RAW 格式 strip 无 WIC/ImageIO 级 fast-path

| 项 | 内容 |
|----|------|
| **来源** | cursor R4 M12、R5 R9；trae R5 |
| **位置** | `directory_tree_thumb.rs` — `#[cfg(windows)]` WIC / `#[cfg(macos)]` ImageIO；Linux 走 image crate / LibRaw(RAW) |
| **现状** | RAW 有 LibRaw；HEIF/部分格式在 Linux 上 strip 解码路径慢于 Win/mac。 |
| **影响** | Linux 用户 strip 加载慢或部分格式无预览；功能限制非 crash。 |
| **建议修复** | Linux 增加 libheif / embedded preview 路径；或 README/CHANGELOG 说明平台差异。 |

---

_（ISSUE-16 已于 2026-06-20 手工验证关闭，见 §3。）_

### 2.4 架构 / Fork（Medium–Low，多为已知取舍）

#### ISSUE-17 · eframe fork：同帧可能执行两次 `logic()`

| 项 | 内容 |
|----|------|
| **来源** | cursor R4 M2、R5 R5；deepseek R4 #2 |
| **位置** | `patched-crates/eframe` 子 viewport paint → ROOT `RepaintNow` |
| **现状** | Round 5 条件 repaint 已降低频率；fork 架构下仍可能发生双 logic pass。 |
| **影响** | 同帧重复 drain pipeline，CPU 浪费；现有逻辑基本幂等。 |
| **建议修复** | per-pass generation 守卫；或 profiling 确认热点后再优化。 |

---

#### ISSUE-18 · patched eframe / egui-wgpu 需随上游手动合并

| 项 | 内容 |
|----|------|
| **来源** | deepseek R1 HIGH |
| **位置** | `patched-crates/eframe/`、`patched-crates/egui-wgpu/` |
| **现状** | `logic()` 每 viewport 调用等为核心行为变更，无 upstream PR。 |
| **影响** | 每次升级 eframe/egui-wgpu 需人工 re-patch，冲突面大。 |
| **建议修复** | 向上游提交 issue/PR；维护 `docs/dev/eframe-fork.md` merge 清单。 |

---

#### ISSUE-19 · `App::logic()` 在子 viewport paint 时仍收到 ROOT `Frame`（无 runtime guard）

| 项 | 内容 |
|----|------|
| **来源** | deepseek R4 #1 |
| **位置** | `patched-crates/eframe/src/epi.rs` 文档说明 |
| **现状** | 仅注释警告，无 `debug_assert` 或 viewport id 参数。 |
| **影响** | 未来在 `logic()` 中误用 ROOT 窗口 API 可能导致 subtle bug。 |
| **建议修复** | 为 `logic()` 增加 `LogicContext { viewport_id, is_root }`；或 debug 构建断言。 |

---

#### ISSUE-20 · Autosave 可能因子窗口长期聚焦而延迟

| 项 | 内容 |
|----|------|
| **来源** | deepseek R4 #3 |
| **位置** | `patched-crates/eframe` — `maybe_autosave` 仅 ROOT viewport |
| **现状** | Detached 导航窗口聚焦时 ROOT 可能长时间不 paint。 |
| **影响** | 主窗口几何 autosave 间隔拉长；低概率。 |
| **建议修复** | 定时器驱动 autosave（wall-clock），不依赖 ROOT paint 频率。 |

---

#### ISSUE-21 · `DirectoryTreeState` 单一大锁 + 多处阻塞 `lock()`

| 项 | 内容 |
|----|------|
| **来源** | deepseek R1 §1.3、R4 #4 |
| **位置** | `app.rs` 多处 `state.lock()`（命令处理、places、sort 等） |
| **现状** | 已广泛使用 `try_lock` + defer，但关键路径仍阻塞 lock。 |
| **影响** | 与 snapshot 绘制（ISSUE-01 已修）叠加减轻；高负载下 defer 频率仍可能上升。 |
| **建议修复** | 拆分为 tree / image_list / preview_textures 细粒度锁；或 snapshot 模式。 |

---

### 2.5 测试与代码质量（Low）

_（ISSUE-22、23、24、25、26、27 已于 2026-06-20 修复或文档化，见 §3。）_

---

## 3. 已修复 / 产品决策保留 / 误报 — 不再列入待办

| 原 Issue | 说明 |
|----------|------|
| **ISSUE-01** 绘制路径长时间持锁 | **已重构（RCU + action queue）**：`logic()` 在 channel 排空/scan sync 后 `publish_directory_tree_view()`（`ArcSwap`，仅数据变更时 O(n) 构建）；paint 用 `view.load()` O(1) + `&DirectoryTreeView` 不可变绘制；交互仍走 `DirectoryTreeCommand`；滚动/splitter 等写入 `DirectoryTreeUiChrome`，帧末短锁合并回 state。 |
| **ISSUE-02** 列宽 O(n) paint | **已修复**：`DirectoryTreeState::update_image_list_column_widths()` 在 `run_directory_tree_logic_updates()` 中计算；paint 只读缓存宽度。 |
| **ISSUE-06** metadata 阻塞 send 持锁 | **已修复**：`sync_images()` 返回 `Option<FileMetadataRequest>`；`send_directory_tree_metadata_request()` 在释放 state 锁后发送。 |
| **ISSUE-03** `ui.rs` 1547 行 | **非 checklist 违规**。`docs/review-checklist.md` #12 要求单文件 **不超过 2000 行**；Round 5 已拆出 `strip_previews.rs` 等，`ui.rs`/`app.rs`/`mod.rs` 均在限额内。早期审核针对的是 **4408 行** 的 `directory_tree.rs` 单体，该问题已通过模块拆分解决；1547 行本身无需跟进。 |
| **ISSUE-14** Network lazy UNC | **产品决策：保持现状**。`windows.rs` 启动不枚举 `FOLDERID_NetworkFolder`，Network 空节点 + UNC 动态挂载；避免 Shell 枚举阻塞 Places。不列入 backlog。 |
| cursor R6 **R6-M1** `sync_defer_dropped` UI 不可见 | **当前已修复**：`DirectoryTreeState.sync_warning` + `pending_directory_tree_sync_warning`；`ui.rs` 在 `show_sync_warning` 时于列表底部展示（含非空列表场景）。 |
| cursor R6 **R6-L1** `release_tx.send` 静默 | **当前已修复**：`strip_previews.rs:348,598` 失败时 `log::warn!`。 |
| cursor R4 **C1/C2**、**H1–H6** 等 | Round 5–6 已验证；当前代码结构一致。 |
| **ISSUE-04** 冷路径 JPEG 重复 mmap | **已修复**：`load_jpeg_from_mapped()`；mmap 路径传入 `open_image_data_for_directory_tree_thumb` 的 JPEG fallback，避免二次 `map`。 |
| **ISSUE-05** 扫描 Done 双倍内存 | **已修复**：移除 `all_files_for_done`；单 `files` 向量 batch 发送后就地排序供 Done。 |
| **ISSUE-07** 扫描 batch 阻塞 send | **已修复**：`send_scan_message()` 使用 `try_send` + 2ms 重试 + cancel 检查。 |
| **ISSUE-08** strip 缓存 O(n) 淘汰 | **已文档化**：`DIRECTORY_TREE_STRIP_CACHE_MAX` 与 `evict_if_needed` 注释明确 cap=128 下 O(n) 可接受；提高上限需 LRU。 |
| **ISSUE-09** sort-active `existing_paths` clone | **已修复**：`HashSet<&PathBuf>` 引用现有行；新行先收集再 `extend`，避免 reallocate 期间借用冲突。 |
| **ISSUE-10** read_dir orphan 线程 | **已知设计取舍**：`workers.rs` 注释说明 orphan 仅回收 inflight 计数、线程不可平台级取消。 |
| **ISSUE-11** read_dir inflight TOCTOU | **已修复**：`READ_DIR_HELPERS_INFLIGHT` 使用 `compare_exchange` CAS 循环。 |
| **ISSUE-12** metadata 超大 coalesce 批 | **已修复**：coalesce 后 `split_metadata_request()` 按 `METADATA_BATCH_SIZE` 分片。 |
| **ISSUE-16** macOS/Linux Detached 未测 | **已关闭（手工验证）**：Linux/macOS Embedded + Detached 正常；另修 egui-wgpu multi-viewport staging panic、Embedded 导航 splitter 拖拽。 |
| **ISSUE-22** Strip pool panic | **已修复**：`preview_caps.rs` 多级 rayon pool fallback。 |
| **ISSUE-23** 关键逻辑缺单测 | **已修复**：coalesce/split/mark_failed/normalize、`sync_images` sort-active、`DirectoryTreeView.sync_warning`、strip inflight `try_send` 单测。 |
| **ISSUE-24** UI magic number | **已修复**：`DIRECTORY_TREE_UI_STROKE_WIDTH` 等命名常量收敛 chevron/图标比例。 |
| **ISSUE-25** strip inflight 无界 channel | **已修复**：`lifecycle.rs` `bounded(64)`；`strip_previews.rs` `try_send` + `warn!`。 |
| **ISSUE-26** 跨 FS mtime 排序 | **已文档化**：`scanner.rs` `validated_metadata` 注释说明 UTC 秒级与 FAT/NTFS 混排限制。 |
| **ISSUE-27** directory_tree_window YAML 测试 | **已修复**：`directory_tree_window_settings_yaml_roundtrip` 覆盖 maximize/restore 等字段往返。 |

---

## 4. 建议后续优先级

| 优先级 | Issue ID | 理由 |
|--------|----------|------|
| P1 | ISSUE-13 | 中文目录排序体验 |
| P2 | ISSUE-15 | Linux strip 格式 fast-path / 文档 |
| P3 | ISSUE-17–21 | 架构 / fork 长期项（待核实方案） |

---

## 5. 统计摘要

| 类别 | 数量 |
|------|------|
| 18 份审核文档合计提出（去重前） | ~120+ 条发现 |
| 当前仍存在于代码 backlog | **7 条**（ISSUE-13、15、17–21） |
| 其中 Medium（功能/体验） | 2 |
| 架构 / Fork | 5 |

---

*本汇总由对照 18 份审核文档与当前工作区源码生成；若某 issue 已在未纳入本次 diff 的提交中修复，请以 `main` 合并后代码为准重新核对。*
