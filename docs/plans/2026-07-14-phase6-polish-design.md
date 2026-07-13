# 阶段 6 设计 — 打磨与达标

**Date:** 2026-07-14 GMT+8  
**来源：** `docs/ux-improvement-plan.md` 阶段 6；`docs/ui-ux-guidelines.md` §11–§15。  
**方法：** brainstorming；决策已与用户确认（见末尾 §决策记录）。  
**前置：** 阶段 1–5（文件操作、反馈、导航、键盘/palette、上手/持久化）。

---

## 决策摘要（已确认）

1. **微动效：** 几乎不动效 — 不引入 drawer/tab/modal 过渡动画；保留既有 hover 等瞬时反馈。
2. **可访问性：** 审计 + 修缺口 — icon-only tooltip、modal 焦点恢复；不重写 focus 系统。
3. **性能烟测：** 手测清单 + 轻量自动化 — 不做 flaky CI 计时门槛。
4. **文案：** 扫用户可见串，修内部术语与不一致；不做 i18n 抽取。
5. **交付：** 审计清单 + 分主题小 PR（A→B→C→D 推荐顺序）。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| §15 可勾选 | 仓库内有 Phase 6 审计清单，十项评审问题均有 pass 或明确取舍 |
| Tooltip | 所有 icon-only 控件有 tooltip/label（规范 §12） |
| Modal 焦点 | 打开后 focus 合理；Esc/关闭后恢复到触发 pane（审计现有路径，修缺口） |
| 文案 | 用户可见串无 runtime/actor/channel/session epoch 等内部词；英文单语、面向动作 |
| 窄窗 | 在 `window_min_size`（720×480）下 tab / path / drawer / modal 不横向溢出 |
| 性能 | 手测场景文档化；轻量自动化覆盖 10k 列表可见过滤等无卡死路径 |

### 非目标

- 新功能（传输策略、多窗口分 session 文件、自动连接、新 palette 命令）。
- 动效系统 / Reduce Motion 基础设施（本阶段无新动画）。
- 重写 FocusHandle 拓扑或键盘/MRU 体系。
- 完整 i18n / 文案集中到 constants 模块。
- 修改 `crates/sftp` 协议或 session actor 行为。
- CI 硬性性能计时门禁（易 flaky）。

---

## 2. 架构与工作包

本阶段**不新增 crate**。形态：**审计文档 → 按主题修缺口**。

```text
docs/plans/…-phase6-polish-audit.md   (或同设计内嵌清单，实现时落地可勾选版)
        │
        ├─ PR-A  a11y / tooltip / modal focus
        ├─ PR-B  用户可见文案扫掠
        ├─ PR-C  窄窗布局
        └─ PR-D  性能 smoke（手测 + 轻量自动化）
```

| 工作包 | 改什么 | 明确不做 |
| --- | --- | --- |
| **Audit doc** | §15 十问对照表；按区域（tab bar、path bar、file list、drawer、modals、status bar）列 icon/tooltip/focus/truncate 状态；实现中更新 pass/fix/N/A | 不替代代码修复 |
| **PR-A a11y** | 全仓 `icon_button` / 可点击 icon 审计；缺 tooltip 补齐；`cancel_active_modal` / 关闭路径焦点恢复缺口；1–2 个 focus 回归测试 | 不重写 focus 架构 |
| **PR-B copy** | 扫 `crates/app` 用户可见字符串；如 `Runtime is unavailable` → 用户语；统一 Connect/Retry 等；注释可保留内部词 | 不抽 i18n 表；不改 `ErrorCode` |
| **PR-C narrow** | 在最小窗口尺寸下修 tab 标题、path bar、drawer 行、modal 按钮行：补 `min_w_0` / `truncate` / 不撑破 flex | 不重做布局网格 |
| **PR-D perf** | 手测步骤（10k local + 10k remote + 4 active transfers + 3 tabs）；pure/app smoke：10k entries 上 `visible_*_indices` + filter | 无 flamegraph 工具链；无严格 ms CI 门禁 |

**推荐顺序：** A → B → C → D（先交互正确，再观感与性能证明）。A 与 B 可并行。

**crate 边界：**

- 主要：`macsftp_app`、`macsftp_ui`、`docs/`
- 必要时：`macsftp_core` 仅测试 helper
- **禁止：** `sftp` 行为变更；`core` 业务协议变更

**依赖方向不变：** `ui` 不持 session；`app` 不直接 russh。

---

## 3. 审计清单内容（落地文档要求）

实现期维护一份可勾选清单（建议路径：`docs/plans/2026-07-14-phase6-polish-audit.md`），至少包含：

### 3.1 规范 §15 十问

1. 是否保留单窗口工作上下文？  
2. 是否有 command palette 或快捷键路径？  
3. loading / empty / error / disabled / focused / hover / selected 是否覆盖？  
4. 窄窗口下 tab、路径、按钮、modal 是否不溢出？  
5. 是否避免无关卡片、渐变、装饰与大面积空白？  
6. 是否不阻塞 GPUI 主线程？  
7. 是否能处理 10k entries 与多 transfer？  
8. icon-only 是否有 tooltip/label？  
9. 是否无 secret / 私钥完整路径 / 内部 debug 串泄露？  
10. request id / session epoch 相关 modal 过期是否安全？

每项：`pass` | `fix`（附修复 PR）| `N/A` / `accepted risk`（附理由）。

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

| 允许 | 禁止出现在用户可见 UI |
| --- | --- |
| Keychain（macOS 用户概念） | runtime、actor、channel、session epoch、crate、AppCommand |
| host、port、profile、transfer、permission | 内部类型名、调试路径堆栈 |

**改写示例（实现时可微调措辞）：**

- `Runtime is unavailable` → `Connection service is unavailable.`（或等价短句）
- 其它命中内部词的 status/modal 串一并替换。

日志 / `tracing` 消息**不**强制用户语；与 UI 文案分离（规范 §11）。

---

## 5. 焦点与 tooltip（PR-A）

### 5.1 Tooltip

- 所有 `icon_button(...)` 已强制 tooltip 参数 — 审计**调用点**标签是否准确、是否含快捷键（阶段 4 的 `labeled_shortcut` 模式优先）。
- 非 `icon_button` 的可点击 icon（若有）必须补 `.tooltip(text_tooltip(...))` 或改为 `icon_button`。
- 装饰性、不可点击 icon 不要求 tooltip。

### 5.2 Modal focus

现有模式：打开时 `window.focus(&modal_focus)` 或 form focus；`cancel_active_modal` 后 `focus_pane`。

审计范围至少：

- Connect form  
- Host key  
- Transfer conflict  
- Delete confirm  
- Go to Path  
- Command palette  
- About / Settings（若有独立 focus）

缺口定义：Esc/确认关闭后焦点落到「无处」或不可键盘继续操作。修复方式：关闭时 `focus_pane(focused_side)` 或明确的触发 handle。

**不要求：** 完整 focus return stack（B 档加深焦点模型已否决）。

---

## 6. 窄窗（PR-C）

- 基准：`main.rs` `window_min_size: 720×480`。
- 手测：缩到最小 + 长 tab 标题 + 长 path + drawer 打开 + modal 打开。
- 代码原则：flex 子项可收缩处加 `min_w_0`；文本 `truncate()`；按钮行允许换行或滚动而非撑破窗口。
- 已有 truncate 的组件（tab title、file name、transfer row）不重写，只补缺口。

---

## 7. 性能 smoke（PR-D）

### 7.1 手测（文档）

场景（规范 §13）：

1. 本地目录约 10k entries（可用脚本生成 temp dir）。  
2. 远程目录约 10k（mock 或真实机）。  
3. 4 个 active/queued transfer。  
4. 3 个 tab（含已连接）。  

观察：滚动、type-to-filter、切 tab、开 drawer 无明显卡顿；主线程无长时间冻结。

### 7.2 自动化（轻量）

- 扩展或新增 unit test：`visible_local_indices` / `visible_remote_indices` 在 10k 条目 + 非空 filter 下正确且能完成（**不**以严格毫秒作 CI 失败条件；可选 `#[ignore]` 的 bench 式测试另议）。  
- 不新增性能 profiling 依赖。

Progress 节流：阶段 2 已做；本阶段仅确认未回退，不重复造轮子。

---

## 8. 测试计划

| 包 | 测试 |
| --- | --- |
| PR-A | gpui：打开某 modal → Esc → 仍可键盘操作 pane；tooltip 由类型系统/构造强制处不重复测 |
| PR-B | 针对改写的 status 串若有行为分支则单测；否则 diff review + 手工 |
| PR-C | 手测为主；无像素 CI |
| PR-D | 10k visible/filter pure tests |
| 回归 | `cargo test -p macsftp-app --bin macsftp` + storage/platform 相关 |

---

## 9. 建议 PR 切分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR0 / 文档** | 审计清单初稿（可与 PR-A 同提交） | 无 |
| **PR-A** | Tooltip + modal focus | 无 |
| **PR-B** | 文案 | 无（可与 A 并行） |
| **PR-C** | 窄窗 | 无 |
| **PR-D** | 手测文档 + 10k smoke tests | 无 |
| **收口** | 清单全部勾选、取舍写清 | A–D |

---

## 10. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| 微动效 | **不做** 新动画 |
| Keychain 一词 | **允许** 出现在用户 UI |
| 多窗口 session 写盘竞态 | 阶段 5 已知 MVP；本阶段不修 |
| disconnect 后 path（阶段 5 已修） | 不重开 |
| 全量 WCAG 自动化 | **不做**；基础对比依赖既有 Theme token |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 微动效深度 | A — 几乎不动效 |
| 2 | 可访问性深度 | A — 审计 + 修缺口 |
| 3 | 性能落地 | A — 手测 + 轻量自动化 |
| 4 | 文案范围 | A — 扫用户串 + 修泄露/不一致 |
| 5 | 交付切分 | A — 清单 + 分主题小 PR |

---

## Key Decisions

1. **收口不是加功能** — 阶段 6 证明规范达标，不是开新面。  
2. **无动效投资** — 工期给 a11y、文案、窄窗、性能证明。  
3. **审计驱动** — 清单是权威进度；代码修复可独立合入。  
4. **性能以正确与可完成性为主** — 避免 flaky 计时 CI。  
5. **内部术语不得上 UI** — Keychain 例外（平台用户词）。
