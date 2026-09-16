# 主题化自定义滚动条设计

- 日期：2026-07-29
- 状态：已实现并通过交互回归测试
- 分支：`feat/custom-scrollbar`

## 1. 背景与目标

macSFTP 的可滚动区域包括本地与远端文件列表、传输抽屉、命令面板和弹窗。这些区域目前使用 GPUI 内置的 `overflow_y_scroll()` 或 `overflow_x_scroll()`。在 macOS 上，内置滚动条根据系统设置自动隐藏，而且宽度较小，也无法应用应用内主题。因此，当前实现存在可见性和视觉一致性问题。

**目标：** 为所有可滚动区域提供样式统一、持续可见且适配浅色与深色主题的自定义滚动条。自定义组件替代 GPUI 的默认视觉，但是保留滚轮、触控板和键盘的原生滚动行为。

**非目标（YAGNI，首版不实现）：**

- 不实现自定义横向滚动条，因此路径栏横向滚动暂时保留 GPUI 默认样式。
- 不实现滚动条自动隐藏或淡入淡出动画，因此首版始终显示滚动条。
- 不增加键盘滚动交互，因此继续使用现有 focus 逻辑。

## 2. GPUI 0.2.2 约束（设计前已核实源码）

- `overflow_*_scroll()` 会渲染内置滚动条，其自动隐藏行为由平台设置 `should_auto_hide_scrollbars()` 决定；macOS 默认使用自动隐藏的覆盖层样式。
- 公开 API 只提供 `scrollbar_width(impl Into<AbsoluteLength>)`，因此只能定制宽度。GPUI 没有公开颜色、可见性或圆角 API，也没有允许调用方自行实例化的 `Scrollbar` 元素。
- `ScrollHandle(Rc<RefCell<ScrollHandleState>>)` 封装滚动状态。`ScrollHandleState` 包含 `offset`、表示视口的 `bounds`、表示“内容尺寸 − 视口尺寸”的 `max_offset`，以及 `child_bounds` 等字段。相关方法包括 `offset() -> Point<Pixels>`、`bounds() -> Bounds<Pixels>`、`set_offset(Point<Pixels>)` 和 `scroll_to_item(...)`。
- `UniformListScrollHandle(pub Rc<RefCell<UniformListScrollState>>)` 使用 `UniformListScrollState { pub base_handle: ScrollHandle, ... }`。因此，文件列表可以通过 `handle.0.borrow().base_handle` 获得底层 `ScrollHandle`，随后与普通 `div` 使用统一处理方式。
- `crates/ui/src/theme.rs` 中的 `Theme` 是 GPUI `Global`，并且包含 `appearance: Appearance::{Dark, Light}` 和 `ThemeColors`。`ThemeColors` 已有 `background`、`surface`、`elevated_surface`、`border`、`text`、`text_muted`、`element_hover`、`element_active` 和 `accent` 等字段。`ActiveTheme` trait 通过 `cx.theme()` 提供当前主题。

## 3. 组件设计

新增文件 `crates/ui/src/scrollbar.rs`，并导出以下类型和辅助函数：

- **`Scrollbar`：** 由宿主 view 的 `Context` 构造，并表示无状态的垂直滚动条元素。
  - 普通区域使用 `Scrollbar::vertical(area_id, scroll_handle, state, window, cx)` 构造；uniform list 使用 `Scrollbar::vertical_uniform(...)`，而后者在内部获得 `base_handle`。轨道和滑块 id 均由 `area_id` 派生，因此多个滚动区域的 hover、active 和 drag 状态不会互相冲突。
  - 渲染结果由两个绝对定位的 `div` 组成：轨道位于右侧边缘，其宽度等于滑块宽度与内边距之和；滑块使用圆角样式。
  - 滑块几何规则见 §4。
  - 颜色使用 `cx.theme().colors` 中 §5 定义的滚动条 token。滚动条持续可见，但是 hover 和 active 状态使用更深的颜色。
  - 事件通过宿主 view 的 `Context` 执行回调，并触发宿主重新渲染。因此，临时 Entity 不会在绘制完成后释放并使回调失效。

- **`ScrollbarState`：** 每个滚动区域保存一个轻量的后续渲染调度状态。当 handle 尚未完成布局，或者 uniform list 存在 deferred scroll 时，该状态只允许调度一次后续渲染；有效几何出现后，状态恢复为初始值，因此零高度区域不会形成重复渲染循环。

- **`ScrollArea` 辅助函数：** 封装内容容器 `overflow_y_scroll().scrollbar_width(px(0))` 和叠加的 `Scrollbar`，因此各个集成位置无需重复相同结构。函数签名如下：
  ```rust
  pub fn scroll_area(
      content: impl FnOnce(&mut Window, &mut Context<...>) -> impl IntoElement,
      handle: &ScrollHandle,
      state: &ScrollbarState,
      window: &mut Window,
      cx: &mut Context<HostView>,
  ) -> impl IntoElement
  ```

## 4. 滑块几何

定义以下变量：

- `viewport_h = handle.bounds().size.height`
- `content_h = viewport_h + handle.max_offset().height`，其中 `max_offset` 表示内容超出视口的尺寸。
- `offset_y = handle.offset().y`。GPUI 使用负值表示内容已经向上滚动，因此实现按照该符号约定使用绝对值。

规则：
- 如果 `content_h <= viewport_h`，那么内容没有溢出，因此不渲染滑块。
- `track_h = viewport_h`，因此轨道高度等于视口高度。
- `thumb_h = max(min_thumb, viewport_h * viewport_h / content_h)`，`min_thumb = 24px`。
- 可滚动量为 `scrollable = content_h - viewport_h`，并且该值不小于 0。
- `thumb_top = (|offset_y| / scrollable) * (track_h - thumb_h)`；但是当 `scrollable == 0` 时，`thumb_top = 0`。
- 滑块圆角 = `thumb_h / 2`。

如果 offset 超出 `[0, scrollable]`，那么实现需要显式钳制该值。GPUI 的 `clamp_scroll_position` 已经提供相同保证，但是本组件仍然执行钳制，从而使几何计算本身满足边界条件。

## 5. 主题 token（`ThemeColors` 新增）

`ThemeColors` 增加以下 4 个字段，并分别为 dark 和 light 主题定义数值：

| token | dark | light |
|---|---|---|
| `scrollbar_thumb` | `hsla(0, 0, 1.0, 0.22)` | `hsla(0, 0, 0, 0.30)` |
| `scrollbar_thumb_hover` | `hsla(0, 0, 1.0, 0.36)` | `hsla(0, 0, 0, 0.45)` |
| `scrollbar_thumb_active` | `hsla(0, 0, 1.0, 0.50)` | `hsla(0, 0, 0, 0.55)` |
| `scrollbar_track` | `transparent` | `transparent` |

`ThemeSizes` 增加 `scrollbar_width: Pixels`，建议值为 `px(10.0)`。

同时更新 `Theme::one_dark()`、`Theme::one_light()` 和现有主题测试。测试需要断言两套 token 均已定义，而且对应数值不同。

## 6. 交互策略（保留原生滚动行为并替换外观）

- 容器保留 `overflow_y_scroll()`，因此 GPUI 继续处理滚轮、触控板和键盘事件，并维护 `ScrollHandle`。容器通过 `.scrollbar_width(px(0))` 隐藏 GPUI 内置滚动条的视觉，然后由自定义 `Scrollbar` 在同一区域绘制。
- `Scrollbar` 本身只实现以下两项交互：
  1. **滑块拖拽：** 使用 GPUI active drag 捕获机制。拖拽 payload 保存 handle、初始位置和初始 offset；`on_drag_move` 按照 `(delta / (track_h − thumb_h)) * scrollable` 换算滚动量并更新 handle。即使指针移出 10px 轨道，拖拽仍然持续；用户释放按钮后，GPUI 结束 active drag。拖拽期间使用 `scrollbar_thumb_active`。
  2. **轨道点击翻页：** 用户点击滑块上方或下方的轨道后，调用 `set_offset(当前 ± viewport_h * 0.9)`。
- 滚轮、触控板和键盘继续使用 GPUI 原生行为。
- **风险：** 需要验证 `scrollbar_width(0)` 是否仍然允许原生滚轮滚动。如果验证失败，那么在容器上注册 `on_scroll_wheel`，并调用 `handle.set_offset(offset + delta)`。

## 7. 集成位置

使用 `ScrollArea` 或等价结构修改以下位置：

- 文件列表（本地与远端）：`crates/app/src/workspace/render.rs` 中约第 180 行和第 869 行的 `div().overflow_y_scroll().track_scroll(handle)`。这里的 handle 类型为 `UniformListScrollHandle`。
- 传输抽屉：`crates/app/src/workspace/transfer_render.rs:285` `.overflow_y_scroll()`。
- 命令面板：`crates/app/src/workspace/command_palette.rs:329` `.overflow_y_scroll()`。
- 弹窗：`crates/app/src/workspace/modals.rs:664` `.overflow_y_scroll()`。
- 标签切换器：`crates/app/src/workspace/render.rs` 中的 MRU 列表。

轨道必须作为滚动内容容器的同级覆盖层。如果轨道位于 `overflow_y_scroll()` 的内容内部，那么轨道会随内容离开视口。

## 8. 测试

- **单元测试（`crates/ui`）：** 为 `ScrollHandle` 设置 offset、bounds 和 max_offset，并断言 thumb 几何，包括比例、min 钳制和无溢出时不渲染。
- **交互测试（GPUI `test-support`）：** 点击轨道后，断言 offset 和滑块位置立即更新；模拟滑块移出轨道后继续移动并释放，随后断言拖拽过程连续，而且释放后 offset 不再变化。
- **回归测试：** `uniform_list` 的 deferred `scroll_to_item` 与 `Scrollbar` 使用同一个 base handle，因此测试需要断言下一帧的滑块状态已经同步。标签切换器需要构造 20 个以上 tab，并断言发生溢出时会渲染自定义轨道和滑块。
- **质量检查：** 执行 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`bash scripts/check_architecture.sh` 和 `bash scripts/check_sensitive_logs.sh`。本机环境中的 app/GPUI 渲染测试可能受到 Xcode Metal 限制，因此 CI 需要执行这些测试。

## 9. 提交与任务划分（AGENTS.md §11，每项改动单独提交并采用 TDD）

- **T1：** 定义 `ThemeColors` 和 `ThemeSizes` token，为 dark 与 light 主题提供数值，并增加相应测试。
- **T2：** 在 ui crate 中实现 `Scrollbar` 和 `ScrollArea` 组件，并增加单元测试与交互测试。
- **T3：** 在文件列表 `render.rs` 中集成自定义滚动条。
- **T4：** 在传输抽屉、命令面板和弹窗中集成自定义滚动条。
- **T5：** 执行全部质量检查和回归测试。

## 10. 已验证的风险结论

- `scrollbar_width(0)` 只隐藏原生滚动条的视觉，因此容器仍然保留 GPUI 的原生滚轮、触控板和键盘滚动行为。
- `ScrollbarState` 通过一次后续渲染处理首帧零几何和 uniform list deferred scroll，因此零高度区域不会产生无限重绘。
- 自定义轨道会遮挡其下方的文件行，因此用户点击窄轨道时不会误触列表项。
- 轨道是滚动容器的同级覆盖层，因此它不会随内容离开视口。
