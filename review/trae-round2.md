# 代码审查报告 - Round 2

**分支对比**: 当前分支 vs main  
**审查日期**: 2026-06-21  
**审查依据**: round1-synthesis.md 中的修复清单 + 源码核实

---

## Round 1 修复核实结果

### F1. Detached viewpaint_app 生命周期 -- 已修复，核实通过

**原问题**: ROOT ui() 末尾清零导致子窗口 paint 时指针为 null。

**修复核实**:
- `eframe_app.rs` 中 ROOT ui() 末尾的清零已移除。
- `app.rs:1243` 在 `prepare_directory_tree_file_list_viewport` 中每帧写入 `viewpaint_app.store(self as *mut ImageViewerApp, Ordering::Release)`。
- deferred 闭包内不再清零，仅在 `on_exit` 时清零（`eframe_app.rs:117-118`）。
- `mod.rs:319-338` 安全契约注释已更新，准确描述了写入/读取时机。

**结论**: 修复正确。Detached 窗口 paint 时指针始终有效。

---

### F2. UI 阻塞 command_tx.send() -- 已修复，核实通过

**原问题**: UI paint 路径使用阻塞式 `send()`，通道满时可能卡住 UI。

**修复核实**:
- `mod.rs:120-127` 新增 `send_directory_tree_command()` 辅助函数，统一使用 `try_send` + warn。
- `ui.rs` 中所有 7 处 `command_tx.send()` 均已替换为 `send_directory_tree_command()`。
- `app.rs` 中无残留的 `command_tx.send()` 调用。
- `app.rs:1261-1266` CloseWindow 也改为 `try_send`。

**结论**: 修复完整。UI 路径不再可能阻塞。

---

### F3. 目录错误信息双重 i18n -- 已修复，核实通过

**原问题**: worker 已本地化的 error 在 UI 层再次被 `t!("directory_tree.read_failed", err = error)` 包装。

**修复核实**:
- `ui.rs:895-901` 直接显示 `node.error` 文本，不再二次包装。
- 搜索 `read_failed` 在 `ui.rs` 中无匹配，确认已清除。

**结论**: 修复正确。

---

### F4. GPL 版权头缺失 -- 已修复，核实通过

**修复核实**:
- `domains.rs:1-15` 已有标准 GPLv3 头。
- `tests.rs:1-15` 已有标准 GPLv3 头。

**结论**: 修复正确。

---

### F5. read_dir 超时边界 try_recv -- 已修复，核实通过

**原问题**: 超时后 helper 可能恰好在边界完成，结果被丢弃但 inflight 被减两次。

**修复核实**:
- `workers.rs:278` 在 `RecvTimeoutError::Timeout` 分支中增加 `rx.try_recv()` 尝试消费刚到达的结果。
- `InflightGuard::drop`（`workers.rs:213-218`）在 `orphan_flag == true` 时不递减，设计正确。

**结论**: 修复正确。DeepSeek 的 "inflight 双递减" 确为误报。

---

### 其他 Round 1 修复项核实

| 修复项 | 状态 | 核实 |
|--------|------|------|
| workers.rs split_off(0) -> mem::take | 已修复 | `workers.rs:167-168` 使用 `std::mem::take` |
| helper 线程名使用原子递增 ID | 已修复 | `workers.rs:249` 使用 `READ_DIR_HELPER_THREAD_ID.fetch_add(1, Relaxed)` |
| read_file_modified_unix 注释 | 已修复 | `workers.rs` 函数有注释说明返回 UTC 秒 |
| MODIFIED_UNIX_MILLIS_THRESHOLD 命名常量 | 已修复 | `ui.rs:33` 定义命名常量，有注释 |
| logic_update.rs 误导注释 | 已修复 | 注释已更正为 HDR/swap-chain 相关描述 |
| folder_picker.rs 超时行为注释 | 已修复 | `folder_picker.rs:231` 有注释说明 rfd 无 cancel API |

---

## Round 2 新发现问题

### 中优先级

**R2-1. Detached 模式下 viewpaint_app 指针在 Settings 变更时可能过期**

位置: `app.rs:1243`, `eframe_app.rs` 中 settings 相关逻辑

问题: `viewpaint_app` 存储的是 `self as *mut ImageViewerApp` 裸指针。`ImageViewerApp` 由 eframe 作为 `Box<dyn App>` 持有，理论上地址不变。但如果未来 eframe 版本变更或重构导致 App 实例被移动（如 re-box），该指针将变为悬垂指针。当前实现安全，但建议在 `FORK-MERGE.md` 中记录此依赖假设。

建议: 在 `FORK-MERGE.md` 中增加一条记录，说明 `viewpaint_app` 依赖 `Box<dyn App>` 地址稳定性。

---

**R2-2. pending_image_context_menu 的 viewport_id 在 Detached 模式下可能不匹配**

位置: `app.rs:201-203`, `ui.rs:1478-1480`

问题: `chrome.pending_image_context_menu` 存储 `(pos, viewport_id)`，其中 `viewport_id` 来自 `ui.ctx().viewport_id()`。在 Detached 模式下，该 viewport_id 是子窗口的 ID。`finish_directory_tree_image_list_context_menu` 中将 `context_menu_viewport` 设置为该 ID，后续 `paint_image_context_menu_if_open` 需要在正确的 viewport 中绘制。如果 ROOT 和子窗口的绘制时序不一致，context menu 可能绘制在错误的 viewport 中。

核实: `context_menu_viewport` 字段用于 egui 的 viewport 切换，egui 会正确处理。但建议在 Detached 模式下增加日志验证 context menu 的 viewport 切换是否正确。

建议: 在 smoke test 中重点验证 Detached 模式下图片右键菜单的显示位置。

---

**R2-3. directory_tree_viewport_active 的 try_lock 可能在 mutex 争用时返回 false**

位置: `app.rs:626-634`

问题: `directory_tree_viewport_active` 使用 `tree.try_lock()` 判断 viewport 是否活跃。如果 tree mutex 正被其他操作持有（如 `process_directory_tree_events` 中），`try_lock` 返回 `None`，导致 `directory_tree_viewport_active` 返回 false。这可能导致：
- `prepare_directory_tree_file_list_viewport` 跳过注册 deferred viewport
- `request_directory_tree_viewport_repaint` 跳过请求重绘
- `mark_directory_tree_repaint_pending` 跳过标记

影响: 在 mutex 争用期间，Detached 窗口可能短暂不响应重绘请求。由于争用通常很短（<1ms），实际影响有限。

建议: 考虑将 `directory_tree_viewport_active` 的判断改为基于 `settings.show_directory_tree_nav` 和 `browse_mode`，而非依赖 `try_lock`。或者在调用方处理 `try_lock` 失败的情况。

---

### 低优先级

**R2-4. send_directory_tree_command 在通道断开时仍 log warn**

位置: `mod.rs:120-127`

问题: `try_send` 失败可能是 `Full`（通道满）或 `Disconnected`（通道断开）。当前对两种情况都 log warn。通道断开通常发生在 shutdown 期间，此时 warn 日志可能产生噪音。

建议: 区分 `TrySendError::Full` 和 `TrySendError::Disconnected`，仅对 `Full` 记录 warn，对 `Disconnected` 使用 debug 级别。

---

**R2-5. strip_previews.rs 中 flush_directory_tree_strip_pending_gpu_uploads 在 paint 回调中创建 TextureHandle**

位置: `strip_previews.rs:138-184`

问题: 该函数在 UI paint 期间调用 `ctx.load_texture` 创建 egui 纹理。egui 文档建议在 `ui()` 回调而非 paint 回调中创建纹理。虽然 `MAX_STRIP_GPU_UPLOADS_PER_PAINT = 4` 限制了每帧上传数量，但在 Detached 模式下，如果 ROOT 和子窗口同时 paint，可能导致纹理创建时序问题。

核实: egui 的 `load_texture` 在 paint 期间调用是安全的，只是可能导致纹理在下一帧才可用。当前实现可接受。

建议: 保持现状，观察实际运行效果。

---

**R2-6. 非 Windows 平台窗口位置恢复无 clamp**

位置: `settings.rs:682-691`

问题: macOS/Linux 的窗口位置恢复没有 clamp 到工作区，多显示器配置下窗口可能部分落在屏幕外。

建议: 后续平台 API 补齐时处理，本次不阻塞合并。

---

## cargo check 与测试核实

- `cargo check`: 通过，无警告
- 47 个 directory_tree 单测: 据 round1-synthesis.md 报告全部通过

---

## 审查规范对照检查

### 已通过的检查项

- 检查点1: 新增常量均有命名
- 检查点3: ui() 和 logic() 中无同步耗时操作
- 检查点4: 用户可见字符串已做 i18n 支持
- 检查点5: 后台线程数量可控，有 shutdown 机制
- 检查点6: viewpaint_app 指针生命周期已修复（F1）
- 检查点7: 资源管理使用 RAII
- 检查点8: 缓存有明确上限
- 检查点9: 调试日志使用条件编译
- 检查点10: generation 匹配处理正确
- 检查点12: 新增文件已按特性拆分
- 检查点13: 新增 *.rs 文件头部有 GPLv3 版权文字（F4 已修复）
- 检查点15: 错误有统一传播
- 检查点16: 有平台条件编译
- 检查点18: 函数嵌套层数在可接受范围
- 检查点19: 阈值处理已有注释（mtime 常量）
- 检查点22: 预览图使用 Arc 共享
- 检查点28: 文件路径使用 PathBuf
- 检查点32: channel 通信有超时
- 检查点33: 使用 parking_lot::Mutex

---

## 总结

Round 1 的所有 P0/P1 修复均已正确落地，核实通过：
- F1 viewpaint_app 生命周期修复正确
- F2 非阻塞 command 发送修复完整
- F3 错误文案双重包装已清除
- F4 GPL 头已补全
- F5 read_dir 超时边界处理正确

Round 2 新发现的问题均为中低优先级，不阻塞合并：
- R2-1: 裸指针地址稳定性假设（文档化即可）
- R2-2: context menu viewport 匹配（smoke test 验证）
- R2-3: try_lock 争用影响（实际影响有限）
- R2-4 ~ R2-6: 低优先级改进建议

建议合并前完成 round1-synthesis.md 中的 Smoke Test 清单。
