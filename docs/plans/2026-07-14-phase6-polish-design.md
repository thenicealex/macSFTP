# 阶段 6 设计 — 完善与达标

**Date:** 2026-07-14 GMT+8
**来源：** `docs/ux-improvement-plan.md` 阶段 6；`docs/ui-ux-guidelines.md` §11–§15。
**方法：** brainstorming；用户已经确认相关决策，详情参见末尾的“决策记录”。
**前置条件：** 阶段 1–5 已经完成文件操作、反馈、导航、键盘/palette 和初次使用/持久化功能。

---

## 决策摘要（已确认）

1. **微动效：** 基本不增加动效，因此不增加 drawer、tab 或 modal 过渡动画，并保留现有 hover 等即时反馈。
2. **可访问性：** 审计现有实现并修正问题，包括 icon-only tooltip 和 modal 焦点恢复；不重新设计 focus 系统。
3. **性能冒烟测试：** 使用手动测试清单和轻量自动化，因此不设置容易产生不稳定结果的 CI 计时阈值。
4. **文案：** 检查用户可见字符串，并修正内部术语和不一致内容；不执行 i18n 提取。
5. **交付形式：** 使用审计清单和按主题划分的小型 PR，建议顺序为 A → B → C → D。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| §15 可检查 | 仓库包含 Phase 6 审计清单，而且十项评审问题均标记为 pass 或记录明确取舍 |
| Tooltip | 所有 icon-only 控件均有 tooltip/label，符合规范 §12 |
| Modal 焦点 | modal 显示后焦点位置合理；Esc 或关闭后，焦点恢复至触发操作的 pane；审计现有路径并修正问题 |
| 文案 | 用户可见字符串不包含 runtime、actor、channel、session epoch 等内部术语；使用英文单语和面向用户动作的表述 |
| 窄窗口 | 在 `window_min_size` 720×480 下，tab、path、drawer 和 modal 不产生横向溢出 |
| 性能 | 记录手动测试场景；轻量自动化验证 10k 列表的可见项过滤等操作能够完成，并且不会使界面失去响应 |

### 非目标

- 不增加传输策略、多窗口独立 session 文件、自动连接或新 palette 命令等功能。
- 不增加动效系统或 Reduce Motion 基础设施，因为本阶段没有新动画。
- 不重新设计 FocusHandle 拓扑、键盘体系或 MRU 体系。
- 不实现完整 i18n，也不将文案集中至 constants 模块。
- 不修改 `crates/sftp` 协议或 session actor 行为。
- 不增加 CI 强制性能计时检查，因为该检查容易产生不稳定结果。

---

## 2. 架构与工作包

本阶段**不增加 crate**。工作方式为：先建立审计文档，再按主题修正问题。

```text
docs/plans/…-phase6-polish-audit.md   (或同设计内嵌清单，实现时创建可勾选版)
        │
        ├─ PR-A  a11y / tooltip / modal focus
        ├─ PR-B  用户可见文案检查
        ├─ PR-C  窄窗口布局
        └─ PR-D  性能 smoke（手动测试 + 轻量自动化）
```

| 工作包 | 修改内容 | 明确不包含 |
| --- | --- | --- |
| **Audit doc** | §15 十项问题对照表；按照 tab bar、path bar、file list、drawer、modals 和 status bar 等区域记录 icon、tooltip、focus 和 truncate 状态；实现期间更新 pass/fix/N/A | 不能替代代码修复 |
| **PR-A a11y** | 检查全仓 `icon_button` 和可点击 icon；为缺少 tooltip 的控件增加 tooltip；修正 `cancel_active_modal` 和其他关闭路径中的焦点恢复问题；增加 1–2 个 focus 回归测试 | 不重新设计 focus 架构 |
| **PR-B copy** | 检查 `crates/app` 的用户可见字符串；例如将 `Runtime is unavailable` 改为用户可理解的文案；统一 Connect/Retry 等内容；注释可以保留内部术语 | 不提取 i18n 表；不修改 `ErrorCode` |
| **PR-C narrow** | 在最小窗口尺寸下修正 tab 标题、path bar、drawer 行和 modal 按钮行；增加 `min_w_0` 和 `truncate`，并保证 flex 内容不超出窗口 | 不重新设计布局网格 |
| **PR-D perf** | 记录 10k local、10k remote、4 active transfers 和 3 tabs 的手动测试步骤；增加针对 10k entries 的 `visible_*_indices` 与 filter pure/app smoke 测试 | 不增加 flamegraph 工具链；不设置严格的毫秒级 CI 阈值 |

**建议顺序：** A → B → C → D。首先验证交互正确性，然后验证界面和性能。A 与 B 没有依赖关系，因此可以并行。

**crate 边界：**

- 主要涉及 `macsftp_app`、`macsftp_ui` 和 `docs/`。
- 必要时，`macsftp_core` 仅增加测试 helper。
- **禁止：** 不得修改 `sftp` 行为或 `core` 业务协议。

**依赖方向保持不变：** `ui` 不持有 session；`app` 不直接使用 russh。

---

## 3. 审计清单内容（文档要求）

实现阶段维护一份可检查的清单，建议路径为 `docs/plans/2026-07-14-phase6-polish-audit.md`，而且至少包含以下内容。

### 3.1 规范 §15 十项问题

1. 是否保留单窗口工作上下文？
2. 是否有 command palette 或快捷键路径？
3. 是否包含 loading、empty、error、disabled、focused、hover 和 selected 状态？
4. 窄窗口中的 tab、路径、按钮和 modal 是否均不溢出？
5. 是否避免无关卡片、渐变、装饰和大面积空白？
6. 是否避免阻塞 GPUI 主线程？
7. 是否可以处理 10k entries 和多个 transfer？
8. icon-only 控件是否都有 tooltip/label？
9. 是否不存在 secret、私钥完整路径或内部 debug 字符串泄露？
10. request id 或 session epoch 相关 modal 过期后是否安全？

每项使用 `pass`、`fix`（附修复 PR）或 `N/A` / `accepted risk`（附理由）之一。

### 3.2 区域审计表（示例列）

| 区域 | Tooltip | Focus | Truncate | 备注 |
| --- | --- | --- | --- | --- |
| Tab bar close | | | | |
| Path bar back/up/refresh/… | | | | |
| Transfer drawer cancel/retry | | | | |
| Connect / host key / conflict / delete modals | | open/close focus | | |
| Status bar transfer chip | | | | |

---

## 4. 文案规则（PR-B）

| 允许 | 用户可见 UI 中禁止使用 |
| --- | --- |
| Keychain（macOS 用户概念） | runtime、actor、channel、session epoch、crate、AppCommand |
| host、port、profile、transfer、permission | 内部类型名和调试路径堆栈 |

**修改示例，实现阶段可以调整具体措辞：**

- 将 `Runtime is unavailable` 改为 `Connection service is unavailable.` 或同义短句。
- 其他包含内部术语的 status/modal 字符串也应改为用户可理解的内容。

日志和 `tracing` 消息不要求使用用户文案，因为规范 §11 要求日志与 UI 文案分离。

---

## 5. 焦点与 tooltip（PR-A）

### 5.1 Tooltip

- 所有 `icon_button(...)` 已经强制要求 tooltip 参数。因此，应检查各调用点的标签是否准确，以及是否包含快捷键；优先使用阶段 4 的 `labeled_shortcut` 模式。
- 非 `icon_button` 的可点击 icon 如果存在，则必须增加 `.tooltip(text_tooltip(...))`，或者改为 `icon_button`。
- 装饰性且不可点击的 icon 不需要 tooltip。

### 5.2 Modal focus

现有模式是在显示 modal 时调用 `window.focus(&modal_focus)` 或聚焦 form，并在 `cancel_active_modal` 后调用 `focus_pane`。

审计范围至少包括：

- Connect form
- Host key
- Transfer conflict
- Delete confirm
- Go to Path
- Command palette
- About / Settings，如果它们使用独立 focus

如果 Esc 或确认关闭 modal 后焦点不存在，或者用户无法继续使用键盘操作，则视为问题。修正方式是在关闭时调用 `focus_pane(focused_side)`，或者聚焦明确的触发 handle。

本阶段**不要求**实现完整的 focus return stack，因为此前已经否决 B 档焦点模型。

---

## 6. 窄窗口（PR-C）

- 基准为 `main.rs` 中的 `window_min_size: 720×480`。
- 手动测试使用最小窗口尺寸、长 tab 标题、长 path、已显示 drawer 和已显示 modal 的组合。
- flex 子项需要收缩时增加 `min_w_0`；文本使用 `truncate()`；按钮行允许换行或滚动，因此内容不能超出窗口。
- 已经使用 truncate 的组件，例如 tab title、file name 和 transfer row，不需要重写，只修正缺少相应处理的组件。

---

## 7. 性能 smoke（PR-D）

### 7.1 手动测试（文档）

使用规范 §13 中的以下场景：

1. 本地目录约 10k entries，可以通过脚本生成临时目录。
2. 远程目录约 10k entries，可以使用 mock 或真实主机。
3. 存在 4 个 active/queued transfer。
4. 存在 3 个 tab，其中包含已连接 tab。

观察滚动、type-to-filter、tab 切换和 drawer 显示操作是否存在明显延迟，并确认主线程不会长时间失去响应。

### 7.2 自动化（轻量）

- 扩展或增加 unit test，验证 `visible_local_indices` 和 `visible_remote_indices` 在 10k 条目及非空 filter 下结果正确，而且能够完成。CI 不以严格毫秒数作为失败条件；是否增加可选的 `#[ignore]` bench 类测试可以另行决定。
- 不增加性能 profiling 依赖。

阶段 2 已经实现 Progress 节流，因此本阶段仅确认该行为仍然有效，不重复实现。

---

## 8. 测试计划

| 包 | 测试 |
| --- | --- |
| PR-A | gpui：显示某个 modal → Esc → pane 仍可通过键盘操作；tooltip 已由类型或构造函数强制时，不重复测试 |
| PR-B | 如果修改的 status 字符串涉及行为分支，则增加单元测试；否则使用 diff review 和手动检查 |
| PR-C | 以手动测试为主，不增加像素级 CI |
| PR-D | 10k visible/filter pure tests |
| 回归 | `cargo test -p macsftp-app --bin macsftp`，以及 storage/platform 相关测试 |

---

## 9. 建议的 PR 划分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR0 / 文档** | 审计清单初稿，可以与 PR-A 一同提交 | 无 |
| **PR-A** | Tooltip 和 modal focus | 无 |
| **PR-B** | 文案 | 无，因此可以与 A 并行 |
| **PR-C** | 窄窗口 | 无 |
| **PR-D** | 手动测试文档和 10k smoke tests | 无 |
| **最终检查** | 清单全部完成，并记录所有取舍 | A–D |

---

## 10. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| 微动效 | **不增加**新动画 |
| Keychain 一词 | **允许**用于用户 UI |
| 多窗口 session 写盘竞态 | 阶段 5 已经记录该 MVP 限制，因此本阶段不修正 |
| disconnect 后 path（阶段 5 已修） | 不重新处理 |
| 完整 WCAG 自动化 | **不实现**；基础对比度依赖现有 Theme token |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 微动效范围 | A — 基本不增加动效 |
| 2 | 可访问性范围 | A — 审计并修正问题 |
| 3 | 性能验证 | A — 手动测试和轻量自动化 |
| 4 | 文案范围 | A — 检查用户字符串并修正泄露或不一致 |
| 5 | 交付划分 | A — 清单和按主题划分的小型 PR |

---

## Key Decisions

1. **阶段 6 不增加功能。** 该阶段用于证明实现符合规范，因此不增加新的功能范围。
2. **不投入新动效。** 因此，工作时间用于 a11y、文案、窄窗口和性能验证。
3. **以审计清单为依据。** 清单记录权威进度，而且代码修复可以独立合并。
4. **性能验证以正确性和可完成性为主。** 因此，不增加容易产生不稳定结果的 CI 计时检查。
5. **用户 UI 不得包含内部术语。** Keychain 属于平台用户概念，因此是唯一例外。
