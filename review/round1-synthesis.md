# Round 1 综合审核与修复说明

**分支**: `codex/dir-tree-navigation` vs `main`  
**日期**: 2026-06-21  
**来源**: `cursor-round1.md` + `trae-round1.md` + `deepseek-round1.md`  
**状态**: 已核实并落地 P0/P1 修复（见文末变更列表）

---

## 综合结论

三份审核对架构评价一致：RCU 快照 + worker I/O + eframe fork 的设计合理，测试与文档较充分。分歧主要在严重级别与部分 race 是否真实存在。经对照源码核实后：

- **必须修复（已修）**: Detached `viewpaint_app` 生命周期、UI 阻塞 `send`、错误信息双重 i18n、GPL 头缺失
- **建议修复（已修）**: read_dir 超时边界、helper 线程命名、metadata batch `split_off(0)` 可读性、误导注释
- **核实为误报或过度严重**: Trae「Critical 裸指针数据竞争」、DeepSeek「orphan inflight 双递减」、Trae「mtime 秒/毫秒混用导致显示错误」
- **暂缓/后续**: folder picker 无法取消原生对话框、ui.rs 拆分、非 Windows 窗口 clamp、未使用 i18n 键

---

## 问题核实矩阵

### F1. Detached 模式 `viewpaint_app` 在 paint 时为 null（Cursor M1）

**三审结论**: Cursor / DeepSeek 认为功能缺陷；Trae 升格为 Critical「跨线程数据竞争」。

**核实**:

- 原实现：`prepare` 写入指针 → ROOT `ui()` 末尾清零 → 子窗口 deferred 回调 `load()` 常为 null。
- Trae 的「多线程 race」**不成立**：契约限定 UI 线程，`ImageViewerApp` 由 eframe 持有，生命周期覆盖所有 viewport。
- 真正问题是 **ROOT 过早清零**，导致 Detached 下 strip GPU 上传、图片右键菜单静默跳过。

**修复**（已落地）:

1. 删除 `eframe_app.rs` ROOT `ui()` 末尾对 `viewpaint_app` 的清零。
2. 删除 detached 回调结束时的清零；保留 `prepare` 每帧写入。
3. 指针在 Detached-only repaint 时仍有效（地址不变，App 未移动）。

**涉及文件**: `src/app/eframe_app.rs`, `src/app/directory_tree/app.rs`, `src/app/directory_tree/mod.rs`

**说明**: 曾尝试在 deferred 闭包内 capture `*mut ImageViewerApp`，因 `show_viewport_deferred` 要求 `Send` 闭包而编译失败；上述方案更简洁且符合 egui 生命周期。

---

### F2. UI 使用阻塞 `command_tx.send()`（Cursor M2）

**核实**: 通道 `bounded(64)`，UI 路径 7 处 `send()`，logic 阻塞时可能卡住 paint。

**修复**（已落地）: 新增 `send_directory_tree_command()`，统一 `try_send` + warn；CloseWindow 同样改为 `try_send`。

**涉及文件**: `src/app/directory_tree/mod.rs`, `ui.rs`, `app.rs`

---

### F3. 目录错误信息双重 i18n（Cursor M3）

**核实**: `workers.rs` / `mod.rs` 写入的 `node.error` 已是完整本地化字符串；`ui.rs` 再次 `t!("directory_tree.read_failed", err = error)` 包装。

**修复**（已落地）: 直接显示 `error` 文本。

**涉及文件**: `src/app/directory_tree/ui.rs`

---

### F4. GPL 版权头缺失（Cursor M4）

**核实**: `domains.rs`、`tests.rs` 缺标准 GPLv3 头；Trae 清单误报「全部已有」。

**修复**（已落地）: 补全两处文件头。

---

### F5. 文件夹选择器超时后对话框仍阻塞（Cursor M5）

**核实**: rfd `AsyncFileDialog` **无 cancel API**；超时后 generation 门控正确，worker 仍阻塞至用户关闭对话框。

**处理**: 在 `folder_picker.rs` 增加注释说明限制；**未改行为**（无可靠取消手段）。

---

### F6. read_dir orphan 线程 inflight 双递减（DeepSeek 2.6）

**核实**: **误报**。`InflightGuard::drop` 在 `orphan_flag == true` 时 **不再** `fetch_sub`；超时路径由主线程 `fetch_sub` 一次。设计正确。

**附加修复**（已落地）: 超时后增加 `rx.try_recv()`，若 helper 恰好在边界完成则仍返回结果，避免误报超时。

**涉及文件**: `src/app/directory_tree/workers.rs`

---

### F7. mtime 秒/毫秒混用（Trae #7）

**核实**: **当前路径一致为 UTC 秒**。

- `scanner.rs` 注释与实现：`as_secs()`
- `workers.rs::read_file_modified_unix`：`as_secs()`
- `ui.rs::modified_unix_for_display`：仅对 **历史毫秒数据** 做兼容（阈值 > 1e12）

**修复**（已落地）: 命名常量 `MODIFIED_UNIX_MILLIS_THRESHOLD`；workers 增加注释说明与 scanner 一致。

**未做**: 引入 `UnixSeconds` 新类型（收益有限，改动面大）。

---

### F8. Trae #1 viewpaint_app Critical 数据竞争

**核实**: 见 F1。降级为 **已修复的功能缺陷**，非 Critical 内存安全/数据竞争。

---

### F9. 节点 arena 8192 上限静默失败（Trae #3 / DeepSeek 7.1）

**核实**: `mod.rs` 在 `cap_reached` 时设置 `node.error = t!("directory_tree.nodes_cap_reached")`；F3 修复后 UI 会直接显示该错误。DeepSeek「无视觉提示」**部分不成立**。

**未做**: settings 可配置上限（低优先级产品决策）。

---

### F10. apply_directory_tree_image_list_sort 非事务性（Trae #8）

**核实**: 排序为内存重排 + `permute_*`；无 I/O，无跨 await，panic 会终止进程而非半状态。Rust 无异常，**风险可接受**。

**未做**: 事务包装（过度设计）。

---

### F11. send_worker_result 2ms sleep（Trae #11）

**核实**: 仅 worker 线程；有 5s 超时后 drop；UI 不阻塞。可接受。

**未做**: 指数退避（低优先级）。

---

### F12. COM 重复初始化（Trae #12）

**核实**: `ensure_strip_worker_com_initialized` 在 strip pool 线程上调用；`RPC_E_CHANGED_MODE` 已处理。`thread_local` 优化可选。

**未做**: thread_local 标记（微优化）。

---

### F13. 其他低优先级项（三审共有）

| 项 | 核实 | 决策 |
|----|------|------|
| ui.rs 超 2000 行 | 当前 ~1543 行，未超限 | 后续拆分，本次不动 |
| 未使用 i18n 键 folders/images/empty | 确认未引用 | 后续接入或删除 |
| Size/Date 列未 i18n | 复用 osd 硬编码单位 | 后续与 osd 一并 i18n |
| logic 4ms coalesce 跳过 aux logic | 设计如此，延迟 <=4ms | 观察，不改 |
| 非 Windows 窗口 restore 无 clamp | 确实存在 | 后续平台 API 补齐 |
| strip generation 争用放宽 | 有注释 + path relocate | 观察，不改 |
| share_image_rows debug_assert | 仅 debug | 保持 |
| sort tie-break descending | 故意反转 | 保持，可加注释 |
| LRU evict 无条件 bump revision | 微优化 | 后续 |

---

## 已落地代码变更摘要

```
src/app/eframe_app.rs
  - 移除 ROOT ui() 末尾 viewpaint_app 清零

src/app/directory_tree/app.rs
  - prepare 保留写入 viewpaint_app；回调不再清零
  - CloseWindow 改为 try_send

src/app/directory_tree/mod.rs
  - 新增 send_directory_tree_command()
  - 更新 viewpaint_app 安全契约注释

src/app/directory_tree/ui.rs
  - 全部 command 改 try_send 辅助函数
  - 目录 error 直接显示
  - MODIFIED_UNIX_MILLIS_THRESHOLD 命名常量

src/app/directory_tree/workers.rs
  - metadata batch: split_off(0) -> mem::take
  - helper 线程名: 原子递增 ID
  - read_dir 超时后 try_recv 边界结果
  - read_file_modified_unix 注释（UTC 秒）

src/app/directory_tree/domains.rs, tests.rs
  - 补 GPLv3 文件头

src/app/logic_update.rs
  - 修正 logic_root_only 误导注释

src/app/folder_picker.rs
  - 超时行为注释（rfd 无 cancel）
```

---

## 建议修复优先级（综合后）

**P0 -- 已完成**

- F1 viewpaint_app 生命周期
- F2 非阻塞 command 发送
- F3 错误文案双重包装

**P1 -- 已完成**

- F4 GPL 头
- F6 read_dir 超时边界 try_recv
- workers 可读性/调试性小改

**P2 -- 待后续迭代**

- F5 folder picker 超时 UX（需平台层方案或产品文案）
- 非 Windows 窗口位置 clamp
- 未使用 i18n 键处理
- Size/Date 列 i18n

**P3 -- 可选优化**

- ui.rs 模块拆分
- COM thread_local
- LRU revision 精细 bump
- index_cache_permute / strip LRU 单测（DeepSeek 8.2）

---

## 合并前 Smoke Test（综合三审）

```
[ ] Embedded：树展开、选目录、选图、列排序、预览图
[ ] Detached：同上；重点预览 GPU 上传 + 图片右键菜单（验证 F1）
[ ] Detached 聚焦时与主窗口选中双向同步
[ ] 重启后 Detached 位置/最大化恢复
[ ] Windows：This PC、盘符、UNC
[ ] 目录读失败/超时/8192 上限：错误文案正确且无双重包装（F3/F9）
[ ] 快速连点树节点：UI 不卡死（F2）
[ ] 文件夹选择器：正常、超时、再次打开
[ ] Detached 聚焦时 Settings 仍能 autosave
```

---

## 三审文档索引

- Cursor: `review/cursor-round1.md`
- Trae: `review/trae-round1.md`
- DeepSeek: `review/deepseek-round1.md`
- 本文: `review/round1-synthesis.md`
