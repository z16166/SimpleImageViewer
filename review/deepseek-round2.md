# 代码审核 Round 2: `codex/dir-tree-navigation` Round 1 修复验证

**审核日期**: 2026-06-21
**审核模型**: DeepSeek-V4-pro
**审核范围**: Round 1 修复的 9 个文件 (+95 / -36 行)
**参考**: `review/round1-synthesis.md`, `review/cursor-round1.md`, `review/deepseek-round1.md`, `review/trae-round1.md`

---

## 一、修复验证总结

| Round 1 问题 | 来源 | 原始严重级别 | 修复状态 | 评价 |
|---|---|---|---|---|
| F1: `viewpaint_app` 过早清零 | Cursor M1 | MAJOR | **已修复** | 正确 |
| F2: UI 阻塞 `command_tx.send()` | Cursor M2 | MAJOR | **已修复** | 正确 |
| F3: 目录错误双重 i18n | Cursor M3 | MAJOR | **已修复** | 正确 |
| F4: GPL 版权头缺失 | Cursor M4 | MAJOR | **已修复** | 正确 |
| F6: read_dir 超时边界 try_recv | DeepSeek 2.6 | 中等 | **已修复** | 正确 |
| workers 可读性/调试性 | DeepSeek 2.3 / Trae #2 | 低 | **已修复** | 正确 |
| Trae #7: mtime 常量命名 | Trae #7 | MAJOR | **已修复** | 正确 |
| `logic_root_only` 误导注释 | Cursor m6 | MINOR | **已修复** | 正确 |
| folder picker 超时注释 | Cursor M5 | MAJOR | **注释已加** | 可接受 |

**结论**: Round 1 所有 P0/P1 问题已正确修复,未引入新缺陷。9 个文件的改动精准、最小化,没有 scope creep。

---

## 二、逐项验证

### 2.1 F1: `viewpaint_app` 生命周期修复 (Cursor M1)

**变更文件**: `src/app/eframe_app.rs`, `src/app/directory_tree/app.rs`, `src/app/directory_tree/mod.rs`

**原始问题**: ROOT `ui()` 末尾将 `viewpaint_app` 清零,导致 Detached-only repaint 时指针为 null,strip GPU 上传和右键菜单静默跳过。

**修复方案**:
1. 删除 ROOT `ui()` 末尾的清零 (`eframe_app.rs:262-264`)
2. 删除 deferred 回调结束时的清零 (`app.rs:1315`)
3. 保留 `prepare` 中每帧 `store(self, Release)` (`app.rs:1243`)
4. 仅在 `on_exit` 中清零 (`eframe_app.rs:117-118`)
5. 更新安全契约注释 (`mod.rs:329-336`)

**验证结果**:

- **指针写入**: `prepare_directory_tree_file_list_viewport` 在 ROOT `ui()` 中每帧写入 (`app.rs:1243`),Detached 模式下 ROOT 持续 repaint,指针保持有效。
- **指针读取**: deferred 回调中 `load(Acquire)` (`app.rs:1281`),正确配对。
- **生命周期**: `ImageViewerApp` 由 eframe 作为 `Box<dyn App>` 持有,覆盖所有 viewport 回调的生命周期,指针不会悬空。
- **退出清理**: `on_exit` 中在 join workers 之后清零指针 (`eframe_app.rs:116-118`),顺序安全。

**注意事项** (非 bug,仅记录): 指针在 Detached 窗口逻辑关闭后 (`CloseWindow` 处理,`show_directory_tree_nav = false`) 到 OS 窗口实际销毁前的过渡帧中仍然有效。这些帧中 `flush_directory_tree_strip_pending_gpu_uploads` 的调用是幂等的,无副作用。

**评价**: 修复简洁精准,安全契约注释与实现一致,**通过**。

---

### 2.2 F2: UI 非阻塞命令发送 (Cursor M2)

**变更文件**: `src/app/directory_tree/mod.rs`, `ui.rs`, `app.rs`

**原始问题**: UI 绘制路径中 7 处 `command_tx.send()` 阻塞调用,channel bounded(64) 满时会卡住 paint。

**修复方案**:
1. 新增 `send_directory_tree_command()` 辅助函数 (`mod.rs:119-127`),内部 `try_send` + warn log
2. `ui.rs` 中全部 7 处 `command_tx.send()` → `send_directory_tree_command()`
3. Detached 回调中的 `CloseWindow` 也改为 `try_send` (`app.rs:1261-1263`)

**验证结果**:

- `grep "command_tx\.send(" src/app/directory_tree/` 返回空,**全部已替换**。
- 辅助函数签名为 `pub(super)`,可见性范围正确(crate 级 `pub(crate)` 对 `app.rs` 可见)。
- UI 线程不会因子窗口 command 通道满而阻塞绘制。

**注意事项**: 低优先级命令(如 `ToggleExpanded`、`SortImageList`)在 channel 满时可能被丢弃,但用户重试点击即可恢复。这是 UX 降级而非功能缺陷,**可接受**。

**评价**: 修复干净利落,**通过**。

---

### 2.3 F3: 目录错误双重 i18n (Cursor M3)

**变更文件**: `src/app/directory_tree/ui.rs:898-899`

**原始问题**: Worker 写入的 `node.error` 已是完整本地化字符串,UI 再次 `t!("directory_tree.read_failed", err = error)` 包装。

**修复方案**: 直接显示 `error` 文本。

**修复前**:
```rust
ui.label(
    egui::RichText::new(t!("directory_tree.read_failed", err = error).to_string())
        .color(ui.visuals().error_fg_color),
);
```

**修复后**:
```rust
ui.label(
    egui::RichText::new(error).color(ui.visuals().error_fg_color),
);
```

**验证结果**: 代码与 Round 1 synthesis 中描述的方案一致。`node.error` 已在 `workers.rs` 和 `mod.rs` 的多个设置点使用 `t!(...)` 完整本地化,UI 层不再二次包装。

**评价**: **通过**。

---

### 2.4 F4: GPL 版权头补全 (Cursor M4)

**变更文件**: `src/app/directory_tree/domains.rs`, `src/app/directory_tree/tests.rs`

**验证结果**: 两个文件均已补全标准 GPLv3 版权头,内容与同目录其他文件一致。

**评价**: **通过**。

---

### 2.5 F6: read_dir 超时边界 try_recv (DeepSeek 2.6)

**变更文件**: `src/app/directory_tree/workers.rs:275-294`

**原始问题**: DeepSeek Round 1 报告 inflight 双递减 race。经 synthesis 核实为**误报** (`InflightGuard` 在 `orphan_flag == true` 时不递减),但建议超时后 `try_recv()` 消费边界结果。

**修复方案**:
1. `recv_timeout` 超时后增加 `rx.try_recv()` — 若 helper 恰好在边界完成,直接返回结果 (`workers.rs:277-280`)
2. `RecvTimeoutError::Timeout` 和 `Disconnected` 分 case 处理 (`workers.rs:277-294`)

**验证结果**:

- `try_recv()` 在 `orphan_flag` 和 `fetch_sub` 之前执行:如果 `try_recv()` 成功,`InflightGuard` 正常 drop 并 `fetch_sub(-1)`,计数正确。
- 如果 `try_recv()` 返回 Empty: `orphan_flag.store(true)` → `fetch_sub(-1)`,helper 线程后续 `InflightGuard` drop 时不递减。总递减次数 = 1,计数正确。
- `Disconnected` 分支: helper 线程的 `tx.send()` 失败,但 `InflightGuard` 在 helper 退出时仍会递减。主线程不额外递减,计数正确。

**评价**: 修复正确处理了边界 case,逻辑自洽,**通过**。

---

### 2.6 代码可读性改进

**workers.rs 多项修复**:

1. `split_off(0)` → `std::mem::take` (`workers.rs:167-168`):语义更清晰。
2. `READ_DIR_HELPERS_INFLIGHT` 索引 → 独立 `AtomicU64` ID (`workers.rs:62,249`):helper 线程名现在唯一,解决了 Trae #2 指出的调试困难问题。
3. `read_file_modified_unix` 增加注释 `// UTC seconds, matching scanner.rs` (`workers.rs:327`):解决了文档歧义。

**ui.rs**:
4. `MODIFIED_UNIX_MILLIS_THRESHOLD` 命名常量替代硬编码 `1_000_000_000_000` (`ui.rs:33-34`):解决了 Trae #7 / DeepSeek 指出的可维护性问题。

**评价**: 全部**通过**。

---

### 2.7 注释修正

**`logic_update.rs:401`**: 删除了 `// Global mouse activity detection to wake up Music HUD` 和 `// Drag-and-Drop handling` 两个误导注释,替换为 `// HDR / swap-chain target format (ROOT pass only)`。

**`folder_picker.rs:231`**: 增加了 `// rfd has no cancel API; the worker stays blocked until the user dismisses the dialog.` 注释。

**评价**: **通过**。

---

## 三、新发现或遗留问题

### 3.1 (低) `send_directory_tree_command` 参数传递方式

**文件**: `src/app/directory_tree/mod.rs:120-127`

```rust
pub(super) fn send_directory_tree_command(
    command_tx: &crossbeam_channel::Sender<DirectoryTreeCommand>,
    command: DirectoryTreeCommand,
) {
```

该函数在每个调用点都需要传递 `command_tx` 引用。考虑到 `command_tx` 在所有调用点都来自 `self.directory_tree.command_tx`,可以考虑将其作为 `DirectoryTreeRuntime` 的方法或在 `ui.rs` 中用一个闭包捕获。当前方式可行但不简洁。

**建议**: 后续迭代时考虑封装,本次不改。

---

### 3.2 (低) `viewpaint_app` 在非 Detached 模式下的冗余写入

**文件**: `src/app/directory_tree/app.rs:1242-1243`

```rust
let viewpaint_app = Arc::clone(&self.directory_tree.viewpaint_app);
viewpaint_app.store(self as *mut ImageViewerApp, Ordering::Release);
```

`prepare_directory_tree_file_list_viewport` 在函数开头检查了 `!self.directory_tree_nav_is_detached()` 并 early return (`app.rs:1227-1228`)。因此 `store` 只在 Detached 模式下执行,无冗余。

**评价**: 逻辑正确,无问题。

---

### 3.3 (信息) Round 1 综合结论准确性

对照源码验证后,`round1-synthesis.md` 中对三份审核的核实结论准确:

- Trae「Critical 数据竞争」修正为「已修复的功能缺陷」→ **正确** (UI 线程单线程契约,mutable alias 自然安全)
- DeepSeek「orphan inflight 双递减」标记为误报 → **正确** (`InflightGuard` 有条件递减)
- Trae「mtime 秒/毫秒混用导致显示错误」标记为过度严重 → **正确** (当前路径一致为 UTC 秒)

---

## 四、暂缓项回顾 (Round 1 P2/P3)

以下为 Round 1 三审共识的暂缓项,本次 Round 2 未改动,仅做记录:

| 项目 | 优先级 | 当前状态 |
|---|---|---|
| F5: folder picker 超时 UX | P2 | 已加注释; rfd 无 cancel API,非本分支能解决 |
| 非 Windows 窗口位置 clamp | P2 | 暂缓,需平台 API |
| 未使用 i18n 键 (folders/images/empty) | P2 | 未处理 |
| Size/Date 列 i18n | P2 | 未处理 |
| ui.rs 模块拆分 (>1500 行) | P3 | 未处理 |
| COM thread_local 优化 | P3 | 未处理 |
| LRU revision 精细 bump | P3 | 未处理 |
| index_cache_permute / strip LRU 单测 | P3 | 未处理 |
| `DirectoryTreePublishContext` 三个 Option 封装 | P3 | 未处理 |
| sort tie-break descending 注释 | P3 | 未处理 |

---

## 五、Smoke Test 清单更新

基于 Round 1 修复,建议合并前执行以下测试:

```
[ ] Embedded: 树展开、选目录、选图、列排序、预览图 (基线验证)
[ ] Detached: 同上; 重点验证预览 GPU 上传 + 图片右键菜单 (F1 修复验证)
[ ] Detached 聚焦时与主窗口选中双向同步
[ ] 重启后 Detached 位置/最大化恢复
[ ] 目录读失败/超时: 错误文案正确且无双重包装 (F3 验证)
[ ] 快速连点树节点: UI 不卡死 (F2 验证)
[ ] Detached 模式下关闭导航窗口后主窗口正常
[ ] Detached 聚焦时 Settings 仍能 autosave
```

---

## 六、总结

### Round 2 审核结论: **修复验证通过,建议合并**

所有 P0/P1 问题(共 4 项: F1/F2/F3/F4)已正确修复且未引入新缺陷。代码增量为 +95/-36 行,改动精准、最小化。

Round 1 三份审核之间的分歧(Trae Critical 数据竞争、DeepSeek orphan 双递减、mtime 混用)经 synthesis 统一核实并落地,结论准确。

P2/P3 暂缓项不影响合并决策,可在后续迭代中逐步处理。
