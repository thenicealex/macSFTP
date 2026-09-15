# Phase 6 完善审计

**日期：** 2026-07-14
**设计文档：** `docs/plans/2026-07-14-phase6-polish-design.md`
**窗口最小尺寸：** 720×480（`crates/app/src/main.rs`）

## §15 评审问题

| # | 问题 | 状态 | 说明 |
| --- | --- | --- | --- |
| 1 | 是否保持单窗口工作上下文？ | pass | 不显示营销 landing page，并且首先显示 Files 界面 |
| 2 | 是否提供 command palette 或快捷键入口？ | pass | Phase 4 已提供 palette 和 bindings |
| 3 | 是否包含 loading/empty/error/disabled/focused/hover/selected 状态？ | pass | pane/path-bar 边框表示 focus 状态；工具按钮具有 disabled 状态；empty/loading/error 界面均已存在 |
| 4 | 窄窗口中是否不存在内容溢出？ | pass | Task 4 已完成布局清单，并且只在必要位置使用 `min_w_0`/`truncate`/`flex_wrap`；`window_min_size` 保持 720×480。人工测试记录见下文 |
| 5 | 是否不存在装饰性卡片或渐变？ | pass | 只使用 theme token |
| 6 | 网络操作是否不阻塞主线程？ | pass | 使用 runtime bridge；剩余风险已经接受 |
| 7 | 是否支持 10k entries 和多个 transfer？ | pass | Task 5 和 Task 6 已对 10k `visible_*_indices` 执行 unit smoke，并且未设置计时断言，结果为 **pass**。交互式多 transfer GUI 的结论为：**accepted risk: interactive GUI smoke deferred; 10k unit smoke pass** |
| 8 | icon-only 控件是否具有 tooltip？ | pass | Task 2 已确认所有 `icon_button`、path bar 的 back/forward 和 status transfer chip 均具有 label；适用位置使用 `labeled_shortcut` |
| 9 | UI 中是否不存在 secret 或内部术语？ | pass | Task 3 已将 `Runtime is…` 状态字符串改为用户文案，并且增加常量和 banlist unit test；2026-07-14 使用 `rg` 检查 user-visible string literal，结果无违规内容 |
| 10 | modal 过期和 session_epoch 是否安全？ | pass | Phase 1 及后续阶段已经提供 core guard，本次审计再次确认 |

状态值为 `pass`、`fail`、`unknown` 或 `accepted risk`；使用 `accepted risk` 时必须说明原因。

## 区域检查矩阵

| 区域 | Tooltip | 打开时的 focus | 关闭时的 focus | Truncate | 说明 |
| --- | --- | --- | --- | --- | --- |
| Tab bar + close | pass | n/a | n/a | pass | Tab title 使用 `min_w_0`、`truncate` 和 `max_w(220)`；tab strip 使用 `flex_1`、`min_w_0` 和 `overflow_x_scroll` |
| Path bar（back/up/refresh/copy/…） | pass | n/a | n/a | pass | Breadcrumb trail 使用 `flex_1`、`min_w_0` 和 `overflow_x_hidden`；因此，深层路径会被裁剪，但是不会导致窗口横向溢出 |
| Filter clear | pass | n/a | pass | pass | inactive 状态下，query cell 使用 `flex_1`、`min_w_0` 和 `truncate` |
| Transfer drawer cancel/retry | pass | n/a | n/a | pass | title 会截断；detail 使用 `max_w(160)` 和 truncate；drawer header 的汇总 label 也会截断 |
| Status bar transfer chip | pass | n/a | n/a | pass | 左侧区域使用 `flex_1` 和 `min_w_0`；status/message 会截断；chip 使用 `flex_none` |
| Connect form | n/a | pass | pass | pass | field/profile 行使用 `min_w_0`；profile name/summary 会截断；footer 使用 `flex_wrap` |
| Host key modal | n/a | pass | pass | pass | value cell 会截断；footer 使用 `flex_wrap` |
| Conflict modal | n/a | pass | pass | pass | path 会截断；action row 使用 `flex_wrap` |
| Delete confirm | n/a | pass | pass | pass | name preview 会截断；footer 使用 `flex_wrap` |
| Go to Path | n/a | pass | pass | pass | footer 使用 `flex_wrap`；固定宽度为 460 的 card 符合最小窗口宽度 |
| Command palette | n/a | pass | pass | pass | 沿用现有固定宽度 card，因此不属于 Task 4 的布局修改范围 |
| Tab switcher | n/a | pass | pass | pass | title 使用 `flex_1`、`min_w_0` 和 `truncate` |
| Context menu / inline edit | n/a | n/a | pass | n/a | Esc 先关闭当前界面，然后执行 `focus_pane` |
| About | n/a | n/a | pass | n/a | Esc + Close → `close_about` → `focus_pane`，并且已经测试 |
| Settings surface | n/a | pass | pass | pass | content column 已经使用 `min_w_0` |

## 窄窗口人工测试记录（Task 4）

**基准：** `window_min_size` 保持 720×480。项目没有 pixel CI，因此本项通过代码评审和布局清单验证。

| 检查项 | 结果 |
| --- | --- |
| 最小尺寸 720×480 | 已在 `main.rs` 中确认，并且没有降低该值 |
| 较长的 tab title | tab 使用最大宽度和 truncate；tab strip 可以横向滚动 |
| 较深的 path bar | breadcrumb 在 pane 内缩小，并且使用 `overflow_x_hidden` |
| Transfer drawer 中的长路径 | `transfer_title` 会截断，并且 detail 具有最大宽度限制 |
| Connect + Delete modals | 固定 card 宽度不超过 460/420；footer 可以换行；较长名称会截断 |

剩余问题：单个超长 breadcrumb segment 会被裁剪，但是没有省略号，该结果可以接受。在 720 宽度下，双 pane path bar 的 trail 宽度约为 110px；虽然空间有限，但是不会导致窗口溢出。

## 人工性能 smoke test（Task 5）

**自动测试：** `crates/app/src/workspace/visible_entries.rs` 中的
`visible_indices_handle_ten_thousand_entries` 和
`visible_remote_indices_handle_ten_thousand_with_hidden` 只验证 10k entries 的 filter/hide 正确性。

**准备步骤**
1. 生成本地目录：`mkdir -p /tmp/macsftp-10k && seq -w 1 10000 | xargs -I{} touch /tmp/macsftp-10k/f{}`
2. 打开 macSFTP，并且在 local pane 中进入该目录或对应 symlink。
3. 如果存在较大的远端目录，则连接该远端；否则使用 mock backend。
4. 启动最多 4 个 transfer，并且保留 3 个 tab。

**观察项**
- 滚动文件列表：不得出现持续数秒的界面冻结。
- 输入 filter：filter 更新时不得错误清除 selection。
- 切换 tab 或切换 drawer 状态：界面应及时响应。
- progress 更新：继续采用 Phase 2 中的节流处理。

**结果：** **accepted risk: interactive GUI smoke deferred; 10k unit smoke pass**。agent 环境没有交互式 GUI session，但是本地 10k unit test 已经通过。

## 用户可见文案禁用词

UI label/status 中禁止出现以下 substring，并且检查不区分大小写：`runtime`、`actor`、`channel`、`session epoch`、`AppCommand`、`crate`。
允许使用：`Keychain`、host、port、profile、transfer、permission。

**Task 3（2026-07-14）：** 使用 `runtime|actor|channel|session.epoch|AppCommand` 检查 `crates/app/src`。用户可见内容中只有 `workspace/mod.rs` 的 `send_command` 状态字符串符合条件，因此已将其改为 `STATUS_BUSY_TRY_AGAIN` / `STATUS_CONNECTION_SERVICE_UNAVAILABLE`。其余结果均为标识符、注释或日志。对应 guard 为 `user_status_strings_avoid_internal_jargon`。

**Task 6 完成检查（2026-07-14）：**
```bash
rg -n -i "runtime is|actor|session epoch" crates/app/src --type rust -g '!**/tests.rs'
```
结果只包含注释或标识符，包括 `main.rs` 文档以及 `modals`/`mod`/`panes`/`event_handling`/`file_ops` 注释。因此，用户可见字符串没有回归问题。

## 完成检查（Task 6）

| 检查项 | 结果 |
| --- | --- |
| 回归测试 `cargo test -p macsftp-platform -p macsftp-storage -p macsftp-app --bin macsftp` | **pass**：platform 9，storage 34（另有 1 个 ignored），app bin 107；全部通过 |
| §15 各项 | 全部为 `pass`；第 7 项只对交互式 GUI 标记 accepted risk；不存在 `unknown` |
| 区域检查矩阵 | 已完成 |
| 人工性能 smoke test | accepted risk: interactive GUI smoke deferred; 10k unit smoke pass |
| Banlist 剩余内容 | UI 文案中不存在违规内容 |

## 抽查记录（PR0 / Task 1）

| 检查项 | 结果 |
| --- | --- |
| `window_min_size` | 已在 `crates/app/src/main.rs` 中确认值为 720×480 |
| `icon_button` API | 位于 `crates/ui/src/components.rs`，并且**要求**提供 `tooltip_label`；调用位置包括 tab bar、path bar、filter clear 和 transfer rows |
| About Esc 路径 | **已在 Task 2 修正：** `cancel_active_modal` → `close_about` → `focus_pane` |
| About Close button | **已在 Task 2 修正：** 点击 Close → `close_about` → `focus_pane` |
| About 打开行为 | `ShowAbout` 设置 `about_open = true`；Esc 使用 workspace 级别的 `CancelActiveModal`，因此不要求 modal focus |
| Tooltip 审计 | 所有 `icon_button` 调用位置均具有非空 label；back/forward/status chip 等直接点击的 icon 使用 `text_tooltip` |
