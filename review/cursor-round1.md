# 代码审查报告 - Cursor Round 1

**分支对比**: `codex/dir-tree-navigation` vs `main`  
**审查日期**: 2026-06-21  
**审查工具**: Cursor Agent（对照 `docs/review-checklist.md`、`docs/coding-rules.md`）  
**变更规模**: 110 个文件，约 +15254 / -1872 行

---

## 总体结论

导航窗口功能整体架构清晰：后台 worker 负责 I/O、UI 读 ArcSwap 快照、generation 防 stale、缓存有上限、eframe fork 有合并文档。Embedded 模式路径完整。

Detached 模式存在一个较严重的生命周期问题：`viewpaint_app` 原子指针在子窗口 paint 回调执行前已被 ROOT `ui()` 清零，导致独立窗口下的预览图 GPU 上传、图片右键菜单等依赖 `ImageViewerApp` 实例的路径可能静默失效。建议合并前优先修复。

---

## 变更概述（对照需求）

1. 树形目录窗口 + 图片文件列表窗口，合称导航窗口；支持 Embedded（SidePanel）与 Detached（Deferred Viewport）两种模式。
2. 导航窗口位置、最大化状态持久化，下次启动恢复。
3. 树形目录支持常见 Shell 对象、盘符、UNC 路径。
4. 图片列表支持预览图加载、列头排序。
5. 导航窗口与主窗口状态同步（当前选中/显示图片）。
6. patch eframe crate，支持 Detached 模式下双 OS 窗口的 `ui()` / `logic()` 正确处理。
7. rfd 目录/文件选择对话框改用 `AsyncFileDialog`（macOS 原生对话框由 rfd 内部处理 UI 线程约束）。
8. 目录 tree、图片 list、预览图列表的后台线程与 UI 通过高效方式共享、发布（channel + ArcSwap + coalesce）。

---

## CRITICAL（严重）

无。

未发现生产代码中的裸 `unwrap()` / `expect()` / `panic!()`（`directory_tree` 模块测试除外）。未发现 UI / logic 路径上的同步磁盘 I/O。未发现明显的数据竞争或 unsafe 资源泄漏。

---

## MAJOR（重要）

### M1. Detached 模式下 `viewpaint_app` 指针在子窗口 paint 时可能为 null

**位置**

- `src/app/directory_tree/app.rs` 约 1243–1316 行
- `src/app/eframe_app.rs` 约 192、262–264 行
- `src/app/directory_tree/mod.rs` 约 319–326 行（安全契约注释）

**问题**

- `prepare_directory_tree_file_list_viewport()` 在 ROOT `ui()` 中写入 `viewpaint_app`（1244 行）。
- ROOT `ui()` 末尾再次清零（eframe_app.rs 262–264 行）。
- 子窗口 deferred 回调在 1279 行 `load()` 读取指针；若子窗口 paint 发生在 ROOT `ui()` 结束之后，指针已为 null。

**影响（Detached 模式）**

- `flush_directory_tree_strip_pending_gpu_uploads` 被跳过 → 预览图可能不显示或延迟。
- `finish_directory_tree_image_list_context_menu` 被跳过 → 图片列表右键菜单可能无效。
- `allow_image_context_menu` 判断失效。

**对比**

Embedded 模式在 `draw_embedded_directory_tree_panel`（1331、1385 行）直接调用 `self`，不受此问题影响。

**修复建议**

- 将 `app_ptr` 移入 deferred 闭包，在回调入口 `store(app_ptr, Release)`，回调结束再清零。
- 不要依赖 ROOT `prepare` 与子窗口 paint 的时序。
- 同步修正 mod.rs 中「在 detached viewport paint 回调开始时写入」的注释，使其与实现一致。

---

### M2. UI paint 路径使用阻塞式 `command_tx.send()`

**位置**

- `src/app/directory_tree/ui.rs` 826、868、871、1266、1373、1465 行
- `src/app/directory_tree/app.rs` 1262 行

**问题**

- 命令通道为 `bounded(64)`（mod.rs 335–336 行）。
- UI 中使用阻塞 `send()`，而 worker 结果路径使用 `try_send()`。
- 若 `logic()` 阻塞导致 64 条命令积压，下一次 paint 时 `send()` 可能阻塞 UI 线程。

**修复建议**

改为 `try_send()`，失败时 log 或合并/丢弃低优先级命令。

---

### M3. 目录错误信息双重 i18n 包装

**位置**

- `src/app/directory_tree/ui.rs` 885–891 行
- `src/app/directory_tree/workers.rs` 284、291–292 行
- `src/app/directory_tree/mod.rs` 1038 行

**问题**

- Worker 已将 `node.error` 设为完整本地化字符串（如 `directory_tree.read_timeout`）。
- UI 显示时再次用 `t!("directory_tree.read_failed", err = error)` 包装。
- 用户可能看到：「无法读取文件夹: 读取文件夹超时」这类嵌套文案。

**修复建议**

直接显示 `error`；或存 error code，仅在 UI 层本地化一次。

---

### M4. GPL 版权头缺失

**位置**

- `src/app/directory_tree/domains.rs`（第 1 行仅单行注释）
- `src/app/directory_tree/tests.rs`（无 GPL 头）

**说明**

对照 checklist 第 13 条，新增 `.rs` 文件应含 GPLv3 版权头。同目录其他文件（如 mod.rs、app.rs、ui.rs）已有标准头。

---

### M5. 文件夹选择器超时后未取消 OS 对话框

**位置**

- `src/app/folder_picker.rs` 218–233 行
- `src/app/background_threads.rs` 68–84 行

**问题**

- 600 秒超时后仅重置 `in_flight`，worker 仍阻塞在 `pollster::block_on` 等待用户关闭原生对话框。
- 退出时 join 超时 5 秒后 detach，对话框可能仍打开。
- generation 门控可防止错误应用结果，但资源清理不完整。

**修复建议**

评估能否取消 rfd 对话框，或明确文档化「超时后需用户手动关闭」的行为；至少避免 detach 后遗留模态框。

---

## MINOR（次要）

### m1. Logic 4ms 合并可能在仅辅助窗口 repaint 时跳过工作

**位置**: `src/app/logic_update.rs` 15–26、165–173 行；`src/app/eframe_app.rs` 165–173 行

当 `should_run_logic_shared()` 为 false 且 `pass.is_root()` 为 false 时，`logic()` 直接 return，不执行 shared drains。快速滚动 Detached 窗口时，scan / loader / dir-tree 同步可能延迟最多约 4ms。通常可接受，建议在 Detached 高频交互场景下观察。

---

### m2. 非 Windows 平台窗口位置恢复未 clamp 到工作区

**位置**: `src/settings.rs` 682–691 行（macOS / Linux）vs 643–679 行（Windows 有 clamp）

Detached / 主窗口在多显示器下可能部分落在屏幕外。

---

### m3. 未使用的 i18n 键

四个 locale 文件中定义但未引用：

- `directory_tree.folders`
- `directory_tree.images`
- `directory_tree.empty`

建议接入 UI（分区标题 / 空文件夹提示）或从 locale 删除。

---

### m4. 文件列表 Size / Date 列格式化未 i18n

**位置**: `src/app/directory_tree/ui.rs` 约 1416–1422 行，调用 `src/ui/osd.rs` 的 `format_file_size()` / `format_file_modified()`（硬编码 "B" / "KB" / "MB" 等）。

---

### m5. Strip 预览 generation 在 mutex 争用时放宽

**位置**: `src/app/directory_tree/strip_previews.rs` 589–605 行

已有注释说明设计意图；快速排序 / 重扫后若出现错误缩略图，可在此收紧或加强 path / index relocate 校验。

---

### m6. `logic_root_only` 注释与代码不符

**位置**: `src/app/logic_update.rs` 401–402 行

注释提到「Global mouse activity」「Drag-and-Drop」，后续实为 HDR / swap-chain 逻辑，易误导维护者。

---

### m7. 单文件体量接近上限

- `ui.rs`: 1543 行
- `app.rs`: 1390 行
- `strip_previews.rs`: 1117 行

均未超 2000 行 checklist 上限，但 ui.rs 已较大，后续功能建议继续拆分。

---

### m8. eframe RepaintNow 单跳链可能让辅助窗口晚一帧

**位置**: `patched-crates/eframe/src/native/run.rs` 127–141 行

`sync_repaint_in_progress` 为 true 时不链式触发 aux repaint，Detached UI 偶发晚一帧，通常下一帧自愈。

---

## POSITIVE（优点）

**架构与性能**

- 磁盘 I/O 仅在 worker 线程（workers.rs）；UI / logic 用 `try_recv` 非阻塞轮询。
- 全模块使用 `parking_lot::Mutex`，无 `std::sync::Mutex`。
- Generation 匹配：children / metadata / strip 结果均有 stale 丢弃（mod.rs 963–965、1002–1004 等）。
- 命名常量：`MAX_DIRECTORY_TREE_NODES=8192`、strip cache LRU 128、GPU upload 每帧上限、channel bound 64 等。
- RCU 快照：`ArcSwap` + 100ms list publish coalesce（domains.rs）。
- Mutex 争用：`try_lock` + defer 最多 120 帧（app.rs 1068–1087）。

**eframe fork（需求 6）**

- `LogicPass` 区分 ROOT / 辅助 viewport；每 viewport paint 前调用 `logic()`。
- 全桌面 RepaintNow 同步链 + 重入保护。
- 子 viewport paint 时 autosave（ISSUE-20）。
- `FORK-MERGE.md` 合并清单完整。

**logic / ui 拆分**

- `logic_shared`：tray、IPC、scan、loader、dir-tree、folder picker。
- `logic_root_only`：HDR、placement、fullscreen、folder dialog。
- 键盘处理移至 ROOT `ui()`（eframe_app.rs 188–190），避免错误 pass 的 input。

**窗口位置恢复（需求 2）**

- settings 并行字段 + YAML 单测（settings.rs 1050–1117）。
- Detached 启动最大化（app.rs 1268–1276）。

**Places / 盘符 / UNC（需求 3）**

- Windows SHGetKnownFolderPath + This PC（directory_tree_places/windows.rs）。
- Unix / macOS 对应实现。
- UNC 与 network 标签（mod.rs 728–736、ui.rs UNC helpers）。

**预览与排序（需求 4）**

- Strip preview 线程池 + cold / hot 路径 + LibRaw fallback。
- 平台 locale 排序：Windows CompareStringEx、macOS CFStringCompare（sort.rs）。

**AsyncFileDialog（需求 5）**

- Worker 线程 + generation token + stale 拒绝 + 重复请求防护。
- macOS AppKit 主线程约束有注释说明。

**文档**

- CHANGELOG 2.7.0 已更新，面向用户价值描述，未泄露实现细节。
- README 有同步更新。

---

## 功能点对照

**1. 导航窗口 Embedded / Detached**  
已实现。Embedded 完整；Detached 受 M1 影响，预览 GPU 上传与右键菜单可能失效。

**2. 位置与最大化恢复**  
已实现。非 Windows clamp 见 m2。

**3. 树形目录：Shell 对象、盘符、UNC**  
已实现，平台分支清晰。

**4. 图片列表：预览、列头排序**  
已实现。Embedded 预览正常；Detached 预览受 M1 影响。

**5. 主窗口与导航窗口状态同步**  
通过 command 通道 + snapshot + generation 实现，设计合理。Detached 下 `viewpaint_app` 路径需修复。

**6. eframe patch 支持双窗口 logic / ui**  
fork 质量高，有文档与 smoke-test 指引。

**7. AsyncFileDialog**  
已实现。超时清理见 M5。

**8. 后台线程与 UI 高效共享**  
channel + ArcSwap + coalesce + 有界缓存，符合 checklist 要求。

---

## 建议修复优先级

1. **P0** — M1：Detached 模式 `viewpaint_app` 生命周期（回调入口 store `app_ptr`）
2. **P1** — M2：UI `command_tx.send()` → `try_send()`
3. **P1** — M3：目录错误信息停止双重包装
4. **P2** — M4：补 GPL 头（domains.rs、tests.rs）
5. **P2** — M5：folder picker 超时后的对话框 / 线程清理
6. **P3** — m3：未使用 i18n 键接入或删除
7. **P3** — m2：macOS / Linux 窗口位置 clamp
8. **P3** — m4：Size / Date 列 i18n

---

## 合并前 Smoke Test 清单

```
[ ] Embedded 模式：展开树、选文件夹、选图片、列头排序、预览图显示
[ ] Detached 模式：同上，重点验证预览图 GPU 上传与图片右键菜单
[ ] Detached 聚焦时主窗口选中图片双向同步
[ ] 关闭 Detached 窗口 → 重启 exe → 位置/最大化恢复
[ ] Windows：This PC、盘符、UNC 路径
[ ] macOS/Linux：Places、根目录、网络路径
[ ] 快速排序/切换文件夹时预览图无错乱
[ ] logic 阻塞场景下快速点击树节点 UI 不卡死（M2 修复后复测）
[ ] 文件夹选择器：正常选择、超时、重复打开
[ ] Settings autosave：Detached 窗口聚焦时仍能保存
```
