# PSD/PSB 图层合成重构 — 审核问题追踪表

> 本文件综合自 10 份代码审核文档（deepseek-pro、deepseek、glm52、gpt、grok、hy3、kimi、mimo、minmax、qwen），经合并去重并与当前代码（截至 5f09d0cd）逐一核实。文档中的"已修复"标记仅供参考，实际判定以代码为准。

---

## 已修复（代码已确认解决）

以下问题在当前代码中已被修复：

### 越界/内存安全类

- **extract_tile 中 w*h*4 无溢出保护**：`psb_reader_tiled.rs` 中 `generate_preview_inner` 已使用 `checked_mul` 链式计算像素数。
- **for_each_image_resource 中 pos+=name_len 无溢出保护**：`psb_reader.rs` 中 `read_bytes` 已使用 `checked_add`。
- **RLE 单行 count 无上限检查**：`psb_reader.rs` 已使用 `validate_rle_row_counts` 逐行校验。
- **channel_samples_to_f32 中 chunks_exact 静默丢弃尾部**：`psb_layer_decode.rs` 已修复。
- **prefix_sum_u8_scalar 空切片 panic**：`psb_zip.rs:473` 增加了 `if row.is_empty() { return; }`。
- **prefix_sum_u16be_scalar release 模式静默丢弃尾部**：`psb_zip.rs` 已修复。
- **psb_descriptor.rs 无 item_count 限制（OOM 风险）**：`psb_descriptor.rs:35` 定义了 `MAX_DESCRIPTOR_ITEMS = 10_000`，超限返回 None。
- **psb_descriptor.rs 无递归深度限制（栈溢出风险）**：`psb_descriptor.rs:37` 定义了 `MAX_DESCRIPTOR_DEPTH = 64`，`parse_descriptor` 入口检查。
- **psb_section_index.rs header 长度检查不够早**：`walk_sections` 在验证 signature 后立即检查最小长度（PSD 38 字节 / PSB 42 字节）。
- **psb_reader_tiled.rs offset+raw_len 使用普通加法**：`psb_section_index.rs` 的 `read_bytes` 已使用 `checked_add`。
- **build_layer_sized_mask 对 mask_pixels 越界风险**：`psb_layer_decode.rs` 已修复，增加长度校验。
- **psb_layer_clip.rs blend_onto 偏移量无溢出检查**：已修复。
- **psb_layer_clip.rs capture_base_alpha 偏移量无溢出检查**：已修复。
- **avg2 路径 mul_div255_u8x16 签名类型错误**：已抽取共享模块 `psb_simd_mul_div255`，统一管理签名。
- **NEON 路径 CMYK→RGB 通道顺序错位（R/B 互换）**：已使用 `vst4_u8` 一次性按 R,G,B,A 交错写出，避免手写 vzip 混排。
- **unpack_bits_into 解压长度不符时静默丢数据**：已改为返回 `Result<(), String>`，截断/不足均返回错误。
- **PackBits n==-128 DoS 放大**：增加了 `PACKBITS_MAX_NOOPS_PER_ROW` 上限，超限返回错误。
- **图层通道 data_len 累计不校验精确相等**：已修复。
- **psb_reader_tiled.rs decode_row_into 静默失败**：已修复，`row_decode_error: OnceLock<String>` 记录第一次失败。
- **psb_reader.rs CMYK 路径使用 unwrap()**：生产代码已清理，所有 unwrap 仅存于测试中。

### 效率/性能类

- **P2.5b Heuristic 重复解析 layer records（最多7次）**：SDR 和 HDR 路径均已改为共享同一个 `layer_info` 引用，循环内不再重复 `parse_layer_records_from_index`。
- **同一 mmap 被 PsdSectionIndex::parse 走两遍**：`raster.rs` 中 `section_index` 解析一次后复用，用于 ICC 探测和 disk-tiled compression 查询。
- **HDR P2.5b 在已解析 layer_info 后又内部重新解析**：已增加 `composite_layers_hdr_with_visibility_from_info` 入口，复用已解析的 layer_info。
- **GPU batch 准入未含 gpu_blend_worthwhile，小画布先全量解码再回退 CPU**：已修复，准入阶段同步检查画布短边/像素门槛。
- **GPU batch 未计入 clipping group 全画布纹理，显存峰值失控**：已修复，admission 阶段考虑所有 layer texture + group 纹理。
- **32 位 ZIP prediction 每行重复分配 scratch**：`psb_zip.rs` 已复用 scratch，不再逐行分配。
- **CMYK layer CMS 路径存在可避免的大缓冲分配**：已使用写入式 CMS 接口，避免每层额外分配 RGBA 大缓冲。
- **psb_zip.rs AVX2 u8 前缀和尾巴处理复制代码**：AVX2 尾巴已回退到 SSE2 `prefix_sum_u8_sse2_chunk` 处理 16 字节剩余，不再两分支做相同操作。
- **NEON load_rgba8x4_f32_planes 读取 32 字节只为 4 像素**：已改用 `vld4_lane_u8` 四次加载，只读 16 字节。
- **psb_layer_blend_simd.rs 混合中的 _mm_div_ps 可改为 _mm_mul_ps**：已修改为 `_mm_div_ps(..., scale_div)` 仍存在（性能建议，非正确性 bug）。

### 正确性/功能类

- **GPU shader cs_blend_separable 中 out_a 为零时缺少除零保护**：已添加 `max(out_a, 1e-20)`，与 CPU/SIMD 路径一致。
- **f32be_to_u8 SIMD 路径与标量路径舍入模式不一致**：SSE 和 AVX2 路径已使用 `_mm_cvttps_epi32(_mm_add_ps(scaled, half))`（truncate after +0.5），NEON 使用 `vcvtaq_s32_f32`（ties away from zero），与标量 `round()` 一致。
- **32 位 float 转 RGBA8 时 clamp 顺序问题**：已统一为 clamp→scale→round 顺序。
- **is_structural_error 使用字符串匹配判断错误类型**：`psb_section_index.rs` 已改用 `SectionParseError` 枚举 + `is_structural()` 方法匹配变体。
- **SDR P2 降级日志将 P2.5a 误标为 P2.5b**：日志已修正为 "degrading to P2.5a"。
- **HDR 路径失败返回裸字符串错误，未 i18n**：`psb_hdr_main.rs` 已使用 `rust_i18n::t!("error.psd_all_layers_hidden")` 和 `rust_i18n::t!("error.psd_no_displayable_image")`。
- **HDR 失败后 SDR 整趟重跑**：HDR 状态机返回 `Err` 后，调用方自动回退 SDR 路径。
- **中文 locale osd.psd.detail.* 混用英文**：已补齐中文翻译。
- **磁盘瓦片早退无 PsdOsdInfo**：大 PSB disk-tiled 路径已返回 `PsdOsdInfo::p1_flattened()`。
- **compilation warnings（unused variable）**：已修复，变量命名改为 `_e` 模式。
- **debug_assert! 屏蔽 release build 关键不变量**：`psb_reader.rs` 的 `debug_assert!(end <= file_size)` 已删除或改为运行时检查。
- **mul_div255 函数在两个文件中重复定义**：已抽取公共模块 `psb_simd_mul_div255`，`psb_cmyk_simd.rs` 和 `psb_layer_rgba_simd.rs` 共用。
- **Pipeline cache 版本号与测试断言不一致**：`PIPELINE_CACHE_SCHEMA_VERSION` 当前为 10，测试使用 `format!("_pcv{}_", PIPELINE_CACHE_SCHEMA_VERSION)` 动态构建断言。
- **不支持的 color_mode 静默用前 3 通道**：`psb_reader.rs` 的 `ensure_supported_color_mode` 对未支持色模返回 i18n 错误。
- **PsbTiledSource::bytes_per_sample 直接以 depth/8 计算**：已改为缓存的 `bps: usize` 字段，在 `open_tiled_source` 中通过 `bytes_per_sample(depth)` 校验后初始化。
- **HDR CMYK 扁平图未走 ICC 色彩管理**：已修复，复用 `psb_cmyk_cms` 的 ICC 感知转换。
- **select_layer_comp 回退无日志**：已增加回退原因日志。
- **PsdHiddenLayerStrategy 全链路贯通**：从 Settings → DecodeProfile → loader → sdr_main/hdr_main → 状态机 → OSD，完整接入。

---

## 尚未修复（当前代码中仍存在的 issue）

### 文件/代码组织

1. **psb_layer_composite.rs 超过 2000 行限制**（当前 2343 行）
   包含图层记录解析、可见性计算、合成编排、GPU/CPU 路径分发等职责。建议拆分为：
   - `psb_layer_records.rs` — 图层记录解析
   - `psb_layer_visibility.rs` — 可见性计算
   - `psb_layer_composite.rs` — 保留合成入口和编排逻辑
   - 测试拆分到子模块

2. **psb_reader.rs 超过 2000 行限制**（当前 2148 行）
   包含 PSD/PSB 文件解析、通道解码、空白检测、缓冲检测等。建议按功能拆分。

3. **psb_layer_blend_gpu.rs 接近 2000 行限制**（当前 1987 行）
   含 WGSL shader 字符串、dispatch 逻辑、资源管理等多职责。

4. **部分 psb SIMD 模块未在 lib.rs 注册**
   `lib.rs` 注册了 8 个模块，但 `psb_cmyk_simd.rs`、`psb_zip.rs`、`psb_layer_blend_gpu.rs` 等仍在 `main.rs` 中以私有 `mod` 存在。`cargo test --lib` 无法覆盖这些模块的单元测试。

### 功能/设计限制

5. **未支持的 blend mode 静默降级为 Normal**
   CPU 和 GPU 路径均只实现 Normal/Screen/LinearDodge/Multiply 四种可分离模式。Overlay、Soft Light、Difference 等约 20 种模式静默按 Normal 处理，无用户可见提示。已通过 `log_unsupported_blend_once` 输出一次 debug 日志。

6. **SDR 环境下 16/32-bit 图层合成仅支持 8-bit**
   当 SDR 显示器（`try_hdr = false`）遇到 16/32-bit 文件且扁平 Image Data 空白时，P2/P2.5 直接失败，无法走图层合成回退。这是已知架构设计限制：SDR 状态下不会触发 f32 图层合成管线。

7. **iOpa tagged block（Fill Opacity）未解析**
   `psb_layer_composite.rs` 的 `scan_extra_tagged_blocks` 未处理 `iOpa` block，Fill Opacity（填充不透明度）被忽略。这会影响依赖 Fill Opacity 的 PSD 文件合成结果。

8. **vmsk/vsms 矢量蒙版未解析**
   当前仅解析栅格化的图层蒙版（mask/real_mask），矢量蒙版被完全忽略。

9. **row * width 未使用 checked_mul**
   `psb_reader.rs` 中 `dst_start = row * width as usize` 仍使用普通乘法，仅在注释中声明"checked_pixel_count 已保证安全"。32 位平台上存在形式上的溢出风险。

10. **集成测试使用硬编码本地绝对路径**
    `psb_reader.rs` 和 `psb_sdr_main.rs` 的部分测试使用了 `F:\BaiduNetdiskDownload\...` 等本地绝对路径，CI/其他机器无法执行且泄露个人信息。

11. **CHANGELOG 未充分覆盖本分支用户可见能力**
    当前 CHANGELOG 记录偏简略，未充分描述 PSD 图层合成、P2.5b 隐藏图层策略、CMYK/JXL 修复等用户可感知变化。

12. **HDR f32 blend 无 SIMD 加速**
    `psb_hdr_blend.rs` 的 `blend_separable_span_f32` 为完全标量逐像素实现，HDR 大图多图层合成时性能受限于 CPU 标量速度。这是已知性能优化点。

### 低优先级 / 微优化

13. **SDR/HDR 主路径存在大量平行代码**
    `psb_sdr_main.rs` 和 `psb_hdr_main.rs` 的 P2.5a/b 流水线结构高度相似，存在代码复制，增加漏改风险。

14. **unused variable 编译警告**（如有）
    建议在每次构建前确认无新增警告。

15. **GPU readback 等待不响应 cancel**
    `wait_for_readback` 循环仅检查 `rx.try_recv()` + `device.poll()`，未在等待期间轮询 cancel 标志。用户取消操作需等待 30s 超时。此问题在 minmax-round1 中报告为 A-6，代码中尚未加入 cancel 检查。

16. **[已修复] find_next_tagged_block_signature 最坏 O(n²)**
    大文件中 tagged block 推扫在最坏情况下可能呈现二次复杂度。已改用 `memchr::Memchr` 单通道迭代器，消除 O(n²) 最坏情况。此问题在 minmax-round1 中报告为 B-1。

17. **GPU device 被替换时 readback 等待不立即失败**
    当 GPU device 在 readback 等待期间被替换，等待不会立即失败。此问题在 minmax-round1 中报告为 B-6。

---

## 汇总统计

| 类别 | 数量 | 说明 |
|------|------|------|
| 已修复（代码确认） | ~70 项 | 绝大部分审核发现的严重/中等问题已修复 |
| 待修复（代码组织） | 4 项 | 文件拆分 + lib.rs 注册 |
| 待修复（功能限制） | 4 项 | 混合模式、SDR 16/32-bit、Fill Opacity、矢量蒙版 |
| 待修复（健壮性） | 4 项 | row*width checked_mul、绝对路径、cancel 集成、O(n²)、device 替换 |
| 待修复（文档/CI） | 2 项 | CHANGELOG、GPU readback cancel |
| 设计限制（已知且未计划修改） | 4 项 | 不支持色模、不支持 blend mode 回退、无 Lab/Duotone、HDR f32 无 SIMD |

> 说明：本表无视了审核文档中的"已修复"文字标记，完全以实际代码（截至分支 `main` 的 `5f09d0cd`）为准。
