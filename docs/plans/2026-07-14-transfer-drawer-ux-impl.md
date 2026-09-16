# Transfer Drawer UX 实施计划

> **Agent 执行要求：** 必须使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans，并按任务实施本计划。各步骤使用 checkbox（`- [ ]`）记录状态。

**目标：** 为 transfer drawer 增加垂直高度调节功能，具体包括拖拽手柄、双击重置、sticky header、列表独立滚动和会话内高度记忆。但是，本次修改不改变 transfer 业务语义。

**架构：** `Workspace` 保存仅在会话内有效的 `drawer_height`。纯函数 `clamp_drawer_height` 负责最小值和最大值限制。顶部手柄使用 GPUI `on_drag` 和 `on_drag_move`，payload 保存初始高度和 Y 坐标。布局使用固定 `h(drawer_height)` 的列，其中手柄和 header 使用 `flex_none`，job 列表主体使用 `flex_1` 和 `overflow_y_scroll`。因此，不使用 `AppConfig` 持久化，也不新增 `ui` crate 组件。

**Tech Stack:** Rust, GPUI 0.2.2 (`Pixels`, `on_drag` / `on_drag_move`, `MouseButton`, `cursor_row_resize`, `ClickEvent::click_count`), existing `Workspace` / `render_transfer_drawer`.

**Spec:** `docs/plans/2026-07-14-transfer-drawer-ux-design.md`

## 全局约束

- **范围：** 仅实现 resize 和 P0 级别的体验优化；不调整 Active / Queued / Completed / Failed / History 信息架构。
- **记忆：** 高度和展开状态仅在**当前窗口会话**内有效，并保存在 `Workspace` 字段中。因此，不写入 `AppConfig` 或磁盘。
- **拖拽：** 高度连续变化；但是，高度达到最小值时不自动关闭 drawer。
- **关闭方式：** 仍仅使用 `ShowTransferDrawer` 或 status bar；因此，本次不在 header 中增加关闭按钮。
- **双击手柄：** 先恢复默认高度 `240px`，然后执行 clamp。
- 可恢复错误路径不得使用 `unwrap` 或 `expect`；fallible operation 不得使用 `let _ =` 忽略结果。
- 不修改 `core` transfer 状态机、`sftp` 或 Keychain。
- 修改范围必须精确，并与现有 theme token 和 drawer chrome 样式一致。
- 在 `.worktrees/transfer-drawer-ux` 下的 `transfer-drawer-ux` worktree 分支中实施。

## 文件职责

| File | Responsibility |
| --- | --- |
| **Create** `crates/app/src/workspace/drawer_height.rs` | 常量、`clamp_drawer_height` 和纯单元测试 |
| **Modify** `crates/app/src/workspace/mod.rs` | `drawer_height` 字段、默认值、`mod drawer_height` 和可选的 resize helper |
| **Modify** `crates/app/src/workspace/render.rs` | 固定高度布局、sticky header、手柄拖拽和双击 |
| **Modify** `crates/app/src/workspace/tests.rs` | 会话高度、toggle 和 reset 的 GPUI 测试 |
| **Do not modify** | `crates/core/**`、`crates/sftp/**` 和 `crates/storage/**`；如果必须进行小范围 re-export，则应优先避免 |

## 建议的 PR 划分

| PR | Tasks |
| --- | --- |
| PR1 | Task 1–2 (height math + Workspace field) |
| PR2 | Task 3–4 (layout + drag/double-click) |

---

### Task 1：Clamp helper 和常量（纯逻辑）

**文件：**
- Create: `crates/app/src/workspace/drawer_height.rs`
- Modify: `crates/app/src/workspace/mod.rs` — 增加 `mod drawer_height;`；如果有必要，再增加 `pub(crate) use drawer_height::{...}`

**接口：**
本任务提供以下内容：

```rust
use gpui::{Pixels, px};

/// 默认展开高度，与原有 `max_h(240)` 一致。
pub(crate) const DEFAULT_DRAWER_HEIGHT: Pixels = px(240.0);
/// 允许的最小高度：手柄加一行 header，列表可以为空。
pub(crate) const MIN_DRAWER_HEIGHT: Pixels = px(40.0);
/// 与窗口尺寸无关的绝对最大值。
pub(crate) const MAX_DRAWER_HEIGHT_ABS: Pixels = px(480.0);
/// 主内容区域的比例，区域范围从 tab bar 底部到 status bar 顶部。
pub(crate) const MAX_DRAWER_HEIGHT_RATIO: f32 = 0.5;
/// resize 手柄的命中区域。
pub(crate) const RESIZE_HANDLE_HEIGHT: Pixels = px(5.0);

/// 计算最大高度时，内容区域以外 chrome 的估算值。
/// tab_bar（约 36）+ status_bar（由 theme 决定，约 22–28）。该值采用保守估计，因此最大高度不会过大。
pub(crate) const APPROX_CHROME_HEIGHT: Pixels = px(64.0);

/// 将请求的 drawer 高度限制在 [min, max(content)] 内。
///
/// `content_area_height` 是 pane 和 drawer 可使用的垂直空间，
/// 其值等于 viewport 减去估算的 chrome。如果该值未知，那么调用方可以传入
/// `viewport_height - APPROX_CHROME_HEIGHT`，并将最小值限制为 min。
pub(crate) fn clamp_drawer_height(height: Pixels, content_area_height: Pixels) -> Pixels {
    let max_from_ratio = content_area_height * MAX_DRAWER_HEIGHT_RATIO;
    let mut max_height = if max_from_ratio < MAX_DRAWER_HEIGHT_ABS {
        max_from_ratio
    } else {
        MAX_DRAWER_HEIGHT_ABS
    };
    if max_height < MIN_DRAWER_HEIGHT {
        max_height = MIN_DRAWER_HEIGHT;
    }
    if height < MIN_DRAWER_HEIGHT {
        MIN_DRAWER_HEIGHT
    } else if height > max_height {
        max_height
    } else {
        height
    }
}

/// 根据 window viewport 估算内容区域高度，以便在 render 时执行 clamp。
pub(crate) fn content_area_height_from_viewport(viewport_height: Pixels) -> Pixels {
    let raw = viewport_height - APPROX_CHROME_HEIGHT;
    if raw < MIN_DRAWER_HEIGHT {
        MIN_DRAWER_HEIGHT
    } else {
        raw
    }
}
```

- [ ] **Step 1：增加当前会失败的测试**，位于 `drawer_height.rs` 的 `#[cfg(test)] mod tests` 中。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    #[test]
    fn clamp_enforces_minimum() {
        let content = px(800.0);
        assert_eq!(clamp_drawer_height(px(10.0), content), MIN_DRAWER_HEIGHT);
    }

    #[test]
    fn clamp_enforces_absolute_maximum() {
        let content = px(2000.0); // 50% = 1000 > 480 abs
        assert_eq!(
            clamp_drawer_height(px(900.0), content),
            MAX_DRAWER_HEIGHT_ABS
        );
    }

    #[test]
    fn clamp_enforces_ratio_maximum() {
        let content = px(400.0); // 50% = 200 < 480
        assert_eq!(clamp_drawer_height(px(300.0), content), px(200.0));
    }

    #[test]
    fn clamp_passes_through_in_range() {
        let content = px(800.0);
        assert_eq!(clamp_drawer_height(px(240.0), content), px(240.0));
    }

    #[test]
    fn content_area_from_viewport_subtracts_chrome() {
        assert_eq!(
            content_area_height_from_viewport(px(864.0)),
            px(800.0)
        );
    }
}
```

- [ ] **Step 2：执行测试，并确认测试失败**，因为此时尚未定义 module。

```bash
cd /Users/alex/Projects/macSFTP/.worktrees/transfer-drawer-ux
cargo test -p macsftp-app clamp_enforces -- --nocapture
```

预期结果：出现编译错误，或者 test binary 无法查找测试。

- [ ] **Step 3：实现 `drawer_height.rs`，并在 `mod.rs` 中声明 `mod drawer_height`**

将 `mod drawer_height;` 与其他 workspace module 放置在 `mod.rs` 底部的同一区域。但是，不得向 crate 外公开 re-export。

- [ ] **Step 4：执行测试，并确认测试通过**

```bash
cargo test -p macsftp-app clamp_enforces -- --nocapture
cargo test -p macsftp-app content_area_from_viewport -- --nocapture
```

预期结果：全部 5 个测试通过。

- [ ] **Step 5：提交修改**

```bash
git add crates/app/src/workspace/drawer_height.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): add transfer drawer height clamp helper"
```

---

### Task 2：Workspace 会话高度字段和 apply/reset API

**文件：**
- Modify: `crates/app/src/workspace/mod.rs`
- Modify: `crates/app/src/workspace/tests.rs`

**接口：**
- 使用 Task 1 提供的 `DEFAULT_DRAWER_HEIGHT`、`clamp_drawer_height` 和 `content_area_height_from_viewport`。
- 在 `Workspace` 上提供以下内容：

```rust
// 字段
drawer_height: Pixels, // init DEFAULT_DRAWER_HEIGHT

// 方法（在 impl Workspace 上使用 pub(crate)）
fn set_drawer_height(&mut self, height: Pixels, viewport_height: Pixels) {
    let content = content_area_height_from_viewport(viewport_height);
    self.drawer_height = clamp_drawer_height(height, content);
}

fn reset_drawer_height(&mut self, viewport_height: Pixels) {
    self.set_drawer_height(DEFAULT_DRAWER_HEIGHT, viewport_height);
}

fn reclamp_drawer_height(&mut self, viewport_height: Pixels) {
    self.set_drawer_height(self.drawer_height, viewport_height);
}
```

Drag payload type 放在 `drawer_height.rs` 或 `mod.rs` 中，但是优先使用 `drawer_height.rs`：

```rust
/// transfer drawer resize 手柄的 GPUI drag value。
#[derive(Clone, Debug)]
pub(crate) struct TransferDrawerResize {
    pub start_height: Pixels,
    pub start_y: Pixels,
}
```

使用空 drag preview。resize 不需要可见 ghost，因此提供最小透明 view：

```rust
// 位于 drawer_height.rs 或 render.rs
pub(crate) struct ResizeDragGhost;

impl gpui::Render for ResizeDragGhost {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div().w(px(1.0)).h(px(1.0))
    }
}
```

- [ ] **Step 1：在 `tests.rs` 中增加当前会失败的 GPUI 测试**

```rust
#[gpui::test]
fn transfer_drawer_default_height(cx: &mut TestAppContext) {
    let (workspace, _cx, _channels) = init_workspace(cx);
    workspace.read_with(&_cx, |workspace, _| {
        assert_eq!(
            workspace.drawer_height,
            crate::workspace::drawer_height::DEFAULT_DRAWER_HEIGHT
        );
    });
}

#[gpui::test]
fn transfer_drawer_height_survives_toggle(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, _cx| {
        workspace.set_drawer_height(gpui::px(180.0), gpui::px(900.0));
    });
    cx.dispatch_action(ShowTransferDrawer);
    cx.dispatch_action(ShowTransferDrawer);
    workspace.read_with(&cx, |workspace, _| {
        assert!(workspace.drawer_open);
        assert_eq!(workspace.drawer_height, gpui::px(180.0));
    });
}

#[gpui::test]
fn transfer_drawer_reset_height(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, _cx| {
        workspace.set_drawer_height(gpui::px(360.0), gpui::px(900.0));
        workspace.reset_drawer_height(gpui::px(900.0));
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.drawer_height,
            crate::workspace::drawer_height::DEFAULT_DRAWER_HEIGHT
        );
    });
}
```

`ShowTransferDrawer` 已在本文件中使用。`workspace.update` / `update_in` 的用法应与相邻测试保持一致，包括 `read_with` 和 `dispatch_action`。

`drawer_height` 可以保持私有，而相关方法使用 `pub(crate)`。原因是 `tests.rs` 以 `workspace` 下的 `mod tests` 子 module 存在，因此可以访问父 module 中 `Workspace` 的私有字段。现有测试已通过 `workspace.drawer_open` 的断言确认了这一点。

- [ ] **Step 2：执行测试，并确认测试失败**

```bash
cargo test -p macsftp-app transfer_drawer_default_height -- --nocapture
```

预期结果：编译器报告字段或方法缺失。

- [ ] **Step 3：完成最小实现**

在 `Workspace` struct 的 `drawer_open` 之后增加：

```rust
drawer_height: Pixels,
```

在 `Workspace::new` 中初始化：

```rust
drawer_open: true,
drawer_height: DEFAULT_DRAWER_HEIGHT,
```

增加 `Pixels` 和 `drawer_height::*` 的 import，然后在 `impl Workspace` 中实现 `set_drawer_height`、`reset_drawer_height` 和 `reclamp_drawer_height`。

- [ ] **Step 4：执行测试，并确认测试通过**

```bash
cargo test -p macsftp-app transfer_drawer_ -- --nocapture
```

预期结果：新增测试和现有 `transfer_drawer_toggles_via_action` 测试均通过。

- [ ] **Step 5：提交修改**

```bash
git add crates/app/src/workspace/mod.rs crates/app/src/workspace/drawer_height.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): session-local transfer drawer height on Workspace"
```

---

### Task 3：固定高度、sticky header 和可滚动 body 布局

**文件：**
- Modify: `crates/app/src/workspace/render.rs` — `render_transfer_drawer`

**接口：**
- 使用 `self.drawer_height`、`clamp_drawer_height` / `content_area_height_from_viewport` 和 `RESIZE_HANDLE_HEIGHT`。
- drawer root 使用以下结构：

```text
div#transfer-drawer  h(clamped) flex_col flex_none
  handle (Task 4 wires events; Task 3 can render static bar)
  header (existing chrome)
  body flex_1 min_h_0 overflow_y_scroll
    sections / rows / empty
```

- [ ] **Step 1：替换 root 的 `max_h` 和整体 drawer 滚动设置**

删除现有实现：

```rust
let mut drawer = div()
    .id("transfer-drawer")
    .flex()
    .flex_col()
    .flex_none()
    .max_h(px(240.0))
    .overflow_y_scroll()
    ...
```

使用新实现：

```rust
let viewport_h = /* if render gains `window: &mut Window`, use window.viewport_size().height;
                   else pass window from Render::render — see Step 1b */;
let content_h = content_area_height_from_viewport(viewport_h);
let height = clamp_drawer_height(self.drawer_height, content_h);

let mut drawer = div()
    .id("transfer-drawer")
    .flex()
    .flex_col()
    .flex_none()
    .h(height)
    .min_h(MIN_DRAWER_HEIGHT)
    .bg(theme.colors.surface)
    .border_t_1()
    .border_color(theme.colors.border);
```

**Step 1b：访问 window。** 当前 `render_transfer_drawer(&self, cx: &mut Context<Self>)` 没有 `Window` 参数。按顺序选择首个可以正常编译的方案：

1. 将签名改为 `render_transfer_drawer(&self, window: &mut Window, cx: &mut Context<Self>)`，并从 `Render::render` 传入 `window`。调用点已经具有 `window`，因此优先使用此方案。
2. 如果无法访问 window，则使用较大的备用 content height 执行 clamp。但是，该方案的准确性较低。

窗口高度减少后，如果已保存高度超过新的最大值，那么 render 时还需要重新 clamp：

```rust
// 在使用 &self 的 render 中无法修改 self。因此，可以选择：
// (A) 仅在 drag/reset 路径和 window resize observer 中重新 clamp；或者
// (B) 改为 &mut self。Render::render 已经具有 &mut self，因此该方法可以使用 &mut self。
```

**优先方案：** 将方法改为使用 `&mut self` 参数，并在 render 开始时执行：

```rust
self.reclamp_drawer_height(window.viewport_size().height);
let height = self.drawer_height;
```

`mod.rs` 中 `Render::render` 的调用点已经使用 `&mut self`，因此可以调用 `self.render_transfer_drawer(window, cx)`。

- [ ] **Step 2：将布局划分为手柄、sticky header 和可滚动 body**

```rust
// 手柄：本任务仅提供静态 chrome，Task 4 增加事件
drawer = drawer.child(
    div()
        .id("transfer-drawer-resize-handle")
        .flex_none()
        .w_full()
        .h(RESIZE_HANDLE_HEIGHT)
        .cursor_row_resize()
        .bg(theme.colors.border) // thin line affordance; hover polish in Task 4
);

// 现有 header child（Transfers + agg_label）保持 flex_none，不纳入滚动区域
// ...

// 主体
let mut body = div()
    .id("transfer-drawer-body")
    .flex()
    .flex_col()
    .flex_1()
    .min_h_0()
    .overflow_y_scroll();

// 将 section loop 和 empty state 移入 `body`，然后：
drawer = drawer.child(body);
```

Empty state 在 body 内使用居中短文案。如果需要，body 的最小高度可以通过 flex 支持居中：

```rust
body = body.child(
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_size(px(12.0))
        .text_color(theme.colors.text_muted)
        .child("No transfers"),
);
```

- [ ] **Step 3：编译验证**

```bash
cargo check -p macsftp-app
```

预期结果：编译成功。视觉验证中，drawer 使用默认高度，header 保持固定，长列表仅在 body 内滚动。

- [ ] **Step 4：提交修改**

```bash
git add crates/app/src/workspace/render.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): sticky transfer drawer header and fixed session height layout"
```

---

### Task 4：拖拽调整高度和双击重置

**文件：**
- Modify: `crates/app/src/workspace/render.rs`
- Modify: `crates/app/src/workspace/drawer_height.rs`（如果 ghost/payload 尚未定义）
- Modify: `crates/app/src/workspace/tests.rs`（Task 2 已包含逻辑层测试；如果 drag 测试实现成本较高，则可以省略）

**接口：**
- 使用 `TransferDrawerResize`、`ResizeDragGhost`、`set_drawer_height` 和 `reset_drawer_height`。
- 使用以下 GPUI 0.2.2 API：

```rust
// StatefulInteractiveElement 要求 element 具有 state，因此必须设置 .id。
.on_drag(
    TransferDrawerResize {
        start_height: self.drawer_height,
        start_y: px(0.0), // overwritten at drag start via constructor position if needed
    },
    |value, _offset, _window, cx| {
        // 在构建 drag 时从 window.mouse_position() 记录 start_y：
        cx.new(|_cx| ResizeDragGhost)
    },
)
.on_drag_move(cx.listener(
    |workspace, event: &DragMoveEvent<TransferDrawerResize>, window, cx| {
        let Some(drag) = event.drag::<TransferDrawerResize>(cx) else {
            // 如果 API 不同，则使用 event.dragged_item downcast，并核对 DragMoveEvent 方法：
            // event.dragged_item 的类型是 Arc<dyn Any>；优先使用：
            // div.rs 中记录的 let drag = event.drag(cx)
            return;
        };
        // 优先在 on_drag constructor 执行时将 start_y/start_height 保存到 payload。
        let start = drag; // TransferDrawerResize
        let current_y = event.event.position.y;
        let delta = start.start_y - current_y; // drag up → taller
        let new_height = start.start_height + delta;
        workspace.set_drawer_height(new_height, window.viewport_size().height);
        cx.notify();
    },
))
```

**实现要求：正确记录初始位置。**

element 每帧构建时都会确定 `on_drag` 的 value。但是，为了获得正确的 delta，需要选择以下一种方案：

**方案 A（推荐）：** 在 drag constructor 中将初始值保存到 ghost entity，或者更新 workspace：

```rust
.on_drag((), move |_unit, _offset, window, cx| {
    // 问题：constructor 不是 Workspace listener。
})
```

**方案 B（与 GPUI file-row 模式兼容）：** 在 **mouse-down** 时使用 `on_mouse_down` 设置 `workspace.drawer_resize = Some(...)`，从而将 `start_height` 和 `start_y` 写入 drag value。随后在 move 时从 workspace 读取。但是，该方案还需要全局 move 处理。

**方案 C（使用每帧更新的 on_drag value）：** 每次 render 都重新构建：

```rust
TransferDrawerResize {
    start_height: self.drawer_height,
    start_y: /* cannot know until mouse down */,
}
```

GPUI 源码显示，drag 在超过 threshold 后开始，constructor 的参数包含 `cursor_offset`，并且可以读取 `window.mouse_position()`。但是，value `T` 在 element 构建时复制，而不是在 drag 开始时复制。因此，如果不在 constructor 中通过 side channel 设置，那么 `T` 中的 `start_height` 和 `start_y` 将是过期值。

**本代码库可使用的模式：**

```rust
// Workspace 字段（仅在会话内有效）：
drawer_resize: Option<TransferDrawerResize>, // drag 开始时设置

// 手柄：
.on_mouse_down(MouseButton::Left, cx.listener(|ws, event, window, cx| {
    if event.click_count >= 2 {
        ws.reset_drawer_height(window.viewport_size().height);
        ws.drawer_resize = None;
        cx.notify();
        return;
    }
    ws.drawer_resize = Some(TransferDrawerResize {
        start_height: ws.drawer_height,
        start_y: event.position.y,
    });
    cx.notify();
}))
// 在 workspace root 上增加 move/up，或者使用 on_drag + on_drag_move：

// 优先使用 GPUI native resize 路径：
.on_drag(
    TransferDrawerResize {
        start_height: self.drawer_height,
        start_y: px(0.0),
    },
    cx.listener(|ws, value, offset, window, cx| {
        // 说明：on_drag constructor 签名是 Fn(&T, Point, &mut Window, &mut App) -> Entity
        // 它不是 cx.listener，因此需要通过 Entity handle 访问 Workspace。
    }),
)
```

**确定的实现方案：**

1. 在构建手柄之前获取 `Entity<Workspace>`，可以使用现有常用形式 `let workspace = cx.entity()`。
2. `on_drag` 的 payload 类型为空的 `TransferDrawerResize`；constructor 实现如下：

```rust
let workspace_entity = cx.entity();
.on_drag(
    TransferDrawerResize {
        start_height: self.drawer_height,
        start_y: px(0.0),
    },
    move |value, _cursor_offset, window, cx| {
        let start_y = window.mouse_position().y;
        let start_height = value.start_height;
        workspace_entity.update(cx, |ws, _cx| {
            ws.drawer_resize = Some(TransferDrawerResize {
                start_height,
                start_y,
            });
        });
        cx.new(|_| ResizeDragGhost)
    },
)
.on_drag_move(cx.listener(|ws, event: &DragMoveEvent<TransferDrawerResize>, window, cx| {
    let Some(start) = ws.drawer_resize.clone() else {
        return;
    };
    let current_y = event.event.position.y;
    let new_height = start.start_height + (start.start_y - current_y);
    ws.set_drawer_height(new_height, window.viewport_size().height);
    window.set_window_cursor_style(CursorStyle::ResizeRow); // if available; else cursor on handle is enough
    cx.notify();
}))
```

3. mouse up 时清除 `drawer_resize`。因此，当 `drawer_resize.is_some()` 时，在 workspace root 上增加：

```rust
// 在 Render::render 的 workspace root 中，当 drawer_resize 为 Some 时：
.on_mouse_up(MouseButton::Left, cx.listener(|ws, _e, _w, cx| {
    if ws.drawer_resize.take().is_some() {
        cx.notify();
    }
}))
```

GPUI 在 drag 结束时会丢弃 active_drag，且 mouse up 可能发生在任意位置。因此，root 上的 `on_mouse_up` 已经足够。

4. **双击：** 在手柄上增加：

```rust
.on_click(cx.listener(|ws, event: &ClickEvent, window, cx| {
    if event.click_count() >= 2 {
        ws.reset_drawer_height(window.viewport_size().height);
        ws.drawer_resize = None;
        cx.notify();
    }
}))
```

也可以在开始 drag 之前使用包含 `event.click_count >= 2` 的 `on_mouse_down`，从而避免双击触发 drag。因此，优先使用 mouse_down 双击路径。

5. 增加 tooltip：

```rust
.tooltip(text_tooltip("Drag to resize · Double-click to reset"))
```

6. Hover 使用 `.hover(|s| s.bg(theme.colors.accent))` 和较低强度的颜色，或者使用稍粗的线条。

7. 如果 API 支持，那么在 move 期间通过 `cx.set_active_drag_cursor_style(CursorStyle::ResizeRow, window)` 设置 drag cursor 样式。

- [ ] **Step 1：按上述方案实现手柄交互**

在 `Workspace` 中增加 `drawer_resize: Option<TransferDrawerResize>`，并在 `new` 中将默认值设为 `None`。

在 `render.rs` 中按需 import `MouseButton`、`DragMoveEvent`、`ClickEvent` 和 `CursorStyle`。

- [ ] **Step 2：执行编译和单元测试**

```bash
cargo test -p macsftp-app transfer_drawer_ -- --nocapture
cargo check -p macsftp-app
```

预期结果：测试通过，并且编译成功。

- [ ] **Step 3：执行手动 smoke test**。如果无法提供截图，则在 commit body 中记录结果。

1. 启动应用，并通过 ⌘J 显示 Transfers。
2. 向上拖拽手柄时 drawer 高度增加；向下拖拽时高度减少；高度在最小值和最大值处停止变化。
3. 双击手柄后，高度恢复为约 240px。
4. 关闭再显示 drawer 后，高度保持不变。
5. 滚动较长的 job 列表时，header 位置保持不变。
6. 减小窗口高度时，drawer 不超过最大允许高度。

- [ ] **Step 4：提交修改**

```bash
git add crates/app/src/workspace/mod.rs crates/app/src/workspace/render.rs crates/app/src/workspace/drawer_height.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): drag-resize transfer drawer with double-click reset"
```

---

### Task 5：回归验证和设计交叉检查

**文件：**
- 除非 Task 4 仍存在体验细节问题，否则不需要修改文件。
- 可选：如果已经修改文档，则可以在 `docs/ui-ux-guidelines.md` §7 中增加一行说明，指出用户可以在会话内调整 drawer 高度。但是，如果希望 PR 仅包含代码，则省略此项。

- [ ] **Step 1：执行完整 app 测试范围**

```bash
cargo test -p macsftp-app --lib
```

预期结果：所有测试通过。如果存在既有失败，那么本次修改不得增加新失败。

- [ ] **Step 2：核对 Spec 覆盖清单**，实施者在检查时确认各项。

| Spec requirement | Task |
| --- | --- |
| 拖拽调整垂直高度 | 4 |
| 最小值/最大值 clamp | 1 + 2 + render 时重新 clamp |
| 双击重置 | 4 |
| 仅在会话内记忆 | 2 |
| Toggle 后保持高度 | 2 |
| Sticky header 和 body 滚动 | 3 |
| 最小高度时不自动关闭 | 4（高度处理代码不设置 `drawer_open=false`） |
| 不使用 AppConfig | —（不修改 storage） |
| P0 empty state | 3 |
| 手柄 cursor/tooltip | 4 |

- [ ] **Step 3：仅在文档已更新时创建最终 commit；否则任务完成**

---

## 自检（计划与 spec 对照）

| Spec section | Covered by |
| --- | --- |
| §1 目标（resize、clamp、双击、会话记忆、sticky） | Tasks 1–4 |
| §1 非目标（持久化、snap-close、虚拟化、clear completed） | 全局约束，不对应实施任务 |
| §3 状态模型 | Task 2（`drawer_height`、`drawer_resize`） |
| §4 布局 | Task 3 |
| §5 交互 | Task 4 |
| §6 P0 体验优化 | Tasks 3–4 |
| §6 P1/P2 | 明确不在本次范围内 |
| §8 测试 | Tasks 1–2 自动化；Task 4 手动 smoke test |
| GPUI drag 风险 | Task 4 记录候选方案和具体的 `on_drag`/`drawer_resize` 组合方案 |

**Placeholder 检查：** 没有有意保留的 placeholder。如果 0.2.2 中 `DragMoveEvent::drag` 的方法名不同，那么使用 `dragged_item` 和 downcast，或者仅使用 constructor 已设置的 `workspace.drawer_resize`。后者使 move handler 不需要从 event 获取 payload。

**已验证的 GPUI 0.2.2 API：** `on_drag`、`on_drag_move`、`on_mouse_down`、`on_mouse_up`、`on_click` + `click_count()`、`cursor_row_resize()`、`window.viewport_size()`、`Pixels` 的 `PartialOrd` + `Mul<f32>`。

---

## 执行说明

本计划已完成，并保存在 `docs/plans/2026-07-14-transfer-drawer-ux-impl.md`。

**执行方式：**

1. **Subagent-Driven（推荐）：** 每个任务使用一个新 subagent，并在任务之间进行 review。
2. **Inline Execution：** 当前会话按顺序实施各任务，并使用 checkpoint。

请选择执行方式。
