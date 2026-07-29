# 自定义主题滚动条设计

- 日期：2026-07-29
- 状态：已确认（设计阶段，待实现）
- 分支：`feat/custom-scrollbar`

## 1. 背景与目标

macSFTP 的可滚动区域（本地/远端文件列表、传输抽屉、命令面板、弹窗）目前使用 GPUI 内置 `overflow_y_scroll()` / `overflow_x_scroll()`。在 macOS 上该滚动条跟随系统设置**自动隐藏**、又细、且无法主题化，存在可见性与一致性问题。

**目标**：为所有可滚动区域提供**统一样式、常驻可见、适配浅色/深色主题**的自定义滚动条；替换 GPUI 默认视觉，但保留原生滚动行为（滚轮/触控板/键盘）。

**非目标（YAGNI，首版不做）**：
- 不做横向滚动条（路径栏横向滚动暂保留 GPUI 默认）。
- 不实现滚动条自动隐藏 / 淡入淡出动画（首版常驻可见）。
- 不引入新的键盘滚动交互（沿用现有 focus 逻辑）。

## 2. GPUI 0.2.2 约束（设计前已核实源码）

- `overflow_*_scroll()` 渲染内置滚动条；其自动隐藏由平台设置 `should_auto_hide_scrollbars()` 决定（macOS 默认自动隐藏的覆盖层风格）。
- 唯一公开定制点是 `scrollbar_width(impl Into<AbsoluteLength>)`（仅宽度）。**无**颜色 / 可见性 / 圆角 API，也**无**可手动实例化的 `Scrollbar` 元素。
- `ScrollHandle(Rc<RefCell<ScrollHandleState>>)`；`ScrollHandleState` 字段：`offset`、`bounds`（视口）、`max_offset`（内容 − 视口）、`child_bounds` 等。方法：`offset() -> Point<Pixels>`、`bounds() -> Bounds<Pixels>`、`set_offset(Point<Pixels>)`、`scroll_to_item(...)`。
- `UniformListScrollHandle(pub Rc<RefCell<UniformListScrollState>>)`；`UniformListScrollState { pub base_handle: ScrollHandle, ... }` → 文件列表可通过 `handle.0.borrow().base_handle` 取到底层 `ScrollHandle`，与普通 `div` 统一处理。
- 主题：`crates/ui/src/theme.rs` 的 `Theme`（GPUI `Global`）含 `appearance: Appearance::{Dark, Light}` 与 `ThemeColors`（`background / surface / elevated_surface / border / text / text_muted / element_hover / element_active / accent / ...`）。`ActiveTheme` trait 提供 `cx.theme()`。

## 3. 组件设计

新文件 `crates/ui/src/scrollbar.rs`，导出：

- **`Scrollbar`**：垂直滚动条元素。
  - 构造：`Scrollbar::vertical(scroll_handle)` 接收 `&ScrollHandle`；`Scrollbar::vertical_uniform(&UniformListScrollHandle)` 内部取 `base_handle`。
  - 渲染：绝对定位的**轨道 `div`**（贴右缘，宽度 = 滑块宽 + 内边距）+ **圆角滑块 `div`**。
  - 滑块几何见 §4。
  - 颜色取自 `cx.theme().colors` 的滚动条 token（§5）。常驻可见；hover / active 加深。

- **`ScrollArea`** 辅助：封装「内容容器 `overflow_y_scroll().scrollbar_width(px(0))` + 叠加 `Scrollbar`」，避免每个接入点重复样板。签名形如：
  ```rust
  pub fn scroll_area(
      content: impl FnOnce(&mut Window, &mut Context<...>) -> impl IntoElement,
      handle: &ScrollHandle,
      cx: &App,
  ) -> impl IntoElement
  ```

## 4. 滑块几何

设：
- `viewport_h = handle.bounds().size.height`
- `content_h = viewport_h + handle.max_offset().height`（`max_offset` = 内容超出视口的量）
- `offset_y = handle.offset().y`（GPUI 中 offset 为负值表示已向上滚动；实现时按符号约定取绝对值）

规则：
- 若 `content_h <= viewport_h`（无溢出）→ **不渲染滑块**。
- `track_h = viewport_h`（轨道占满视口高）。
- `thumb_h = max(min_thumb, viewport_h * viewport_h / content_h)`，`min_thumb = 24px`。
- 可滚动量 `scrollable = content_h - viewport_h`（≥ 0）。
- `thumb_top = (|offset_y| / scrollable) * (track_h - thumb_h)`；当 `scrollable == 0` 时 `thumb_top = 0`。
- 滑块圆角 = `thumb_h / 2`。

边界：offset 超出 `[0, scrollable]` 时钳制（GPUI 的 `clamp_scroll_position` 已保证，但显式钳制更稳）。

## 5. 主题 token（`ThemeColors` 新增）

新增 4 个字段，dark / light 两套：

| token | dark | light |
|---|---|---|
| `scrollbar_thumb` | `hsla(0, 0, 1.0, 0.22)` | `hsla(0, 0, 0, 0.30)` |
| `scrollbar_thumb_hover` | `hsla(0, 0, 1.0, 0.36)` | `hsla(0, 0, 0, 0.45)` |
| `scrollbar_thumb_active` | `hsla(0, 0, 1.0, 0.50)` | `hsla(0, 0, 0, 0.55)` |
| `scrollbar_track` | `transparent` | `transparent` |

`ThemeSizes` 新增 `scrollbar_width: Pixels`（建议 `px(10.0)`）。

更新 `Theme::one_dark()` / `one_light()` 与现有主题测试（断言两套 token 都已定义且互不相同）。

## 6. 交互策略（复用原生滚动，仅替换外观）

- 容器**保留** `overflow_y_scroll()`（GPUI 继续处理滚轮 / 触控板 / 键盘并维护 `ScrollHandle`），用 `.scrollbar_width(px(0))` **抑制 GPUI 自带滚动条的视觉**，由我们的 `Scrollbar` 覆盖绘制。
- `Scrollbar` 自身只实现两件事：
  1. **滑块拖拽**：`on_mouse_down` 记录起点 → `on_mouse_move` 计算 delta → 按 `(delta / (track_h − thumb_h)) * scrollable` 换算 → `handle.set_offset(...)`；`on_mouse_up` 结束。拖拽中用 `scrollbar_thumb_active`。
  2. **轨道点击翻页**：在滑块上方 / 下方点击 → `set_offset(当前 ± viewport_h * 0.9)`。
- 滚轮 / 触控 / 键盘沿用原生。
- **风险点**：`scrollbar_width(0)` 是否仍允许原生滚轮滚动。实现时先验证；退路是在容器上挂 `on_scroll_wheel` → `handle.set_offset(offset + delta)`（很简单）。

## 7. 接入点

用 `ScrollArea`（或等价改造）替换：

- 文件列表（本地 / 远端）：`crates/app/src/workspace/render.rs` 的 `div().overflow_y_scroll().track_scroll(handle)`（约 180 / 869 行）。handle 为 `UniformListScrollHandle`。
- 传输抽屉：`crates/app/src/workspace/transfer_render.rs:285` `.overflow_y_scroll()`。
- 命令面板：`crates/app/src/workspace/command_palette.rs:329` `.overflow_y_scroll()`。
- 弹窗：`crates/app/src/workspace/modals.rs:664` `.overflow_y_scroll()`。

## 8. 测试

- **单元**（`crates/ui`）：给定 `ScrollHandle` 的 offset / bounds / max_offset，断言 thumb 几何（比例、min 钳制、无溢出不渲染）。
- **交互**（GPUI `test-support`）：模拟滑块拖拽 → 断言 `handle.offset` 改变；轨道点击 → 断言翻页。
- **回归**：`uniform_list` 的 `scroll_to_item` 与 `Scrollbar` 共用同一 handle，断言拖拽后 `scroll_to_item` 仍同步正确（`panes.rs` 现有调用不受影响）。
- **门禁**：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`bash scripts/check_architecture.sh`、`bash scripts/check_sensitive_logs.sh`。app / GPUI 渲染测试在本机可能受 Xcode metal 限制，CI 跑。

## 9. 提交 / 任务拆分（AGENTS.md §11，一改动一提交，TDD）

- **T1**：主题 token（`ThemeColors` / `ThemeSizes` + dark / light + 测试）。
- **T2**：`Scrollbar` + `ScrollArea` 组件（ui crate）+ 单元 / 交互测试。
- **T3**：接入文件列表（`render.rs`）。
- **T4**：接入传输抽屉 / 命令面板 / 弹窗。
- **T5**：全部门禁 + 回归测试。

## 10. 开放风险

- `scrollbar_width(0)` 与原生滚轮的兼容（§6 已列退路）。
- 首帧 handle 状态为零（content / bounds 未布局完成）→ 滑块可能短暂不显示，下一帧自愈；与内置滚动条行为一致，可接受。
- 同一帧读取 handle 与布局的先后：内置滚动条同样依赖 layout 后状态，行为一致，无需特殊处理。
