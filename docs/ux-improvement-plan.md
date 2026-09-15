# macSFTP — UX 提升分阶段完整计划

**日期：** 2026-07-13 GMT+8
**状态更新：** 2026-07-14 — **阶段 1–6 均已交付**；后续进度和改进 backlog 参见 **`docs/progress-analysis-2026-07-14.md`**。
**基准：** `docs/ui-ux-guidelines.md`（强制设计规范）以及当前实现的实测状态（`crates/app/src/workspace/*`、`crates/core/src/core.rs`、`crates/sftp/*`）。
**用户确认的优先方向：** ① 核心文件操作 ② 反馈与可观测性。**实施节奏：** 优先保证系统完整性，因此每个维度应完整实施，避免零散的 quick win。

---

## 0. 当前 UX 状态

> **2026-07-14 注：** 下列“已具备”描述计划起草时的基线；“确认缺失”表中的条目**均已在阶段 1–6 完成**。阶段后的专项（Settings Profiles、Connect profile picker、Transfer drawer 高度）也已合并。具体内容和剩余改进参见 `docs/progress-analysis-2026-07-14.md`。

**已具备（起草时基线以及后续交付）：**

- 视觉系统完整（M7）：全局 `Theme`、语义颜色、尺寸和字体均使用令牌，并且支持 dark/light/system 三种模式。
- 主布局完整：tab bar、左右 pane、transfer drawer 和 status bar；其中 drawer 支持拖拽调整高度。
- 文件列表**已虚拟化**（`uniform_list`），因此可以处理 10k entries。
- 拖拽操作支持 local↔remote 上传和下载（`DragPreview` + drop handlers）。
- 传输支持进程内分组（active/queued/completed/failed）、cancel/retry 和流式 planning，并且显示**真实速度/ETA**。但是应用退出后不恢复传输目录。
- 连接功能包含 connect form（**紧凑 profile picker**）、host-key modal、冲突 modal（含 apply-to-all）、Keychain、**Settings Profiles CRUD** 和 recents。
- 文件操作包含删除、重命名、新建文件夹、右键菜单和行内编辑。
- 导航功能包含 type-to-filter、back/forward、面包屑、Go to Path、隐藏文件和列排序。
- 效率功能包含 command palette、键盘多选/翻页和 MRU tab 切换。
- 持久化功能包含 session 布局恢复（不自动连接）、recents 和窗口标题。
- 其他界面包含 Settings/About surface、复制路径、多标签和多窗口。

**原“确认缺失”表（历史记录；自 2026-07-14 起均已完成）：**

| 起草时的缺项 | 完成阶段 |
| --- | --- |
| 缺少用户级文件操作 | 阶段 1 |
| 传输速度/ETA 使用占位符 | 阶段 2 |
| 缺少目录内搜索/过滤 | 阶段 3 |
| 缺少右键上下文菜单 | 阶段 1 |
| path bar 缺少后退功能 | 阶段 3 |
| command palette 为 stub | 阶段 4 |
| 键盘导航不完整 | 阶段 4 |
| tab 未采用 MRU 顺序 | 阶段 4 |
| 缺少 session 恢复 | 阶段 5 |
| 首次加载反馈不足 | 阶段 2 |

---

## 阶段 1 — 核心文件操作（最高优先级，完整实施）

**状态（2026-07-14）：✅ 已交付**

**目标：** 使 macSFTP 从“只读 + 传输”扩展为可供日常使用的 SFTP 客户端。远程端和本地端都应支持删除、重命名和新建文件夹，并且同时提供键盘、右键菜单和按钮三种入口（规范 §2.2）。

**优先实施原因：** 这是当前最主要的功能缺项，同时也是用户确认的第一优先级。如果缺少这些能力，macSFTP 就无法支持日常文件管理任务。该阶段涉及后端协议和 UI 两层，因此这是六个阶段中唯一超出纯 UX 范围且需要修改 `crates/sftp` 的阶段。

### 1a. 后端协议（`crates/core` + `crates/sftp`）

- 为 `AppCommand` 新增远程操作：`DeleteEntry { tab_id, path, is_dir }`、`RenameEntry { tab_id, from, to }`、`CreateDirectory { tab_id, parent, name }`。对应的本地操作由 `crates/platform` 执行，因为本地 IO 不需要 SFTP actor。
- 为 `AppEvent` 增加确认/失败事件。失败继续使用现有 `RemoteOperationFailed`，但是成功后需要请求一次目录 refresh。
- 在 `session_actor.rs` 中实现 `russh-sftp` 的 `remove_file`、`remove_dir`、`rename` 和 `create_dir`。递归删除目录需要服务端逐项删除，也就是先枚举内容再删除。由于这是危险操作，因此 UI 层必须要求用户确认。

### 1b. 交互 UI（`crates/app/src/workspace`）

- **右键上下文菜单：** 在 `crates/ui` 中新增组件。文件行菜单包含 Rename、Delete、New Folder、Download(remote)、Upload(local)、Copy Path 和 Reveal；空白区域菜单包含 New Folder 和 Refresh。
- **行内重命名：** F2 或菜单进入编辑状态，并且文件行转换为输入框（复用 `InputState`）。Enter 提交，Esc 取消；如果名称冲突或名称非法，则在当前位置显示错误。
- **删除确认 modal：** 显示待删除条目的数量和名称。目录删除需要明确标示“递归”，并且主操作使用危险操作颜色（规范 §8/§10）。多选情况下统一显示确认信息。
- **New Folder：** 菜单和快捷键均可请求该操作，并且新目录以默认名称进入行内重命名状态。

### 1c. 快捷键（`app_actions.rs`）

- `cmd-delete` 对选中项执行 Delete，`enter`/`F2` 执行 Rename，`cmd-shift-n` 执行 New Folder。所有入口均使用稳定 action（`DeleteSelection`、`RenameEntry`、`NewFolder`），因此阶段 4 的 command palette 可以引用这些操作。

### 1d. 批量操作

- 使用现有 selection 集合执行批量删除、下载和上传；确认 modal 汇总条目数量。

**验证：** 使用真实服务器创建、删除和重命名远程目录及文件，并且对本地目录及文件执行相同验证。删除目录时必须显示确认；操作成功后列表自动刷新；权限不足或文件占用导致失败时，应在当前位置显示错误并允许重试。执行 `cargo test`，并且为新增命令提供 core/sftp 单元测试。

---

## 阶段 2 — 反馈与可观测性（第二优先级，完整实施）

**状态（2026-07-14）：✅ 已交付**

**目标：** 所有“进行中/加载中/出错”状态都应在对应位置明确显示（规范 §2.3/§6.3/§7/§10），因此界面不再使用占位内容，也不再省略状态反馈。

### 2a. 真实传输速度 + ETA（替代 `— MB/s · ETA —` 占位符）

- 在 `TransferJob` 或 view 侧记录进度样本，包括上一次 `bytes_done` 和时间戳；现有模型已经包含 `Timestamp`。瞬时速度由滑动窗口计算，ETA 使用 `(bytes_total - bytes_done) / speed` 计算。
- `render_transfer_job` 使用真实值替代字面占位符。速度需要平滑处理，但是**不得使用动画隐藏真实停滞**（规范 §7）；停滞时显示“Stalled”。
- drawer 顶部显示聚合信息，包括总进度、总速度和剩余时间。

### 2b. 加载状态

- 远程目录首次加载时显示骨架屏或居中 spinner，同时保留 path bar（规范 §6.1/§6.3）。
- 重新加载时保留原列表并显示“Refreshing…”标记。当前实现已包含部分逻辑，因此该阶段需要统一其余状态。
- 连接期间，pane/tab 显示目标 host 和**可取消**操作（规范 §6.3）。

### 2c. 原位错误恢复

- 发生 `RemoteOperationFailed` 后，在 pane 内显示 `UserFacingError`，其中包含 title、message 和 **Retry** 按钮；错误信息不能只显示在 status bar（规范 §10）。当前错误已存入 `tab.remote.error`，因此需要增加原位错误状态的渲染和恢复操作。
- status bar 仅显示摘要，详细信息在对应位置显示。

### 2d. status bar 增强

- drawer 折叠时，status bar 显示 active/failed 传输的简要提示（规范 §4 的明确要求）；用户选择该提示后展开 drawer。
- 显示当前 tab 的连接状态和选中条目数量。

**验证：** 大文件传输期间应实时显示 MB/s，并且 ETA 逐渐稳定；网络中断或权限错误应在 pane 内显示，并且允许原位重试；首次加载需要显示骨架屏，重新加载不能清空列表；drawer 折叠后，status bar 仍需显示传输提示。

---

## 阶段 3 — 搜索与目录导航（完善核心文件操作的可用性）

**状态（2026-07-14）：✅ 已交付**

**目标：** 用户可以在大型目录中快速定位条目并切换目录（规范 §6.1/§6.2）。

- **目录内 type-to-filter：** pane 获得焦点后，用户直接输入字符即可增量过滤当前列表。该操作只过滤已经加载的 entries，因此不请求网络；`Esc` 清除过滤条件。
- **path bar 后退/前进：** 每个 pane 维护导航历史栈，并且提供 back/forward 按钮以及 `cmd-[` / `cmd-]` 快捷键；这些快捷键需要与 tab 切换键区分。
- **面包屑 / 路径跳转：** path bar 的各级目录可以选择，用户也可以使用 `cmd-shift-g` 输入路径并直接访问目标目录。
- **隐藏文件开关：** 控制 dotfiles 的显示状态，并且将该状态写入 config。
- **列排序：** 选择表头后，列表可以按照 name、size 或 mtime 排序。`file_table_header` 已经显示排序状态，因此还需要实现选择后的切换逻辑并覆盖三列。

**验证：** 包含万级条目的目录应在输入字符后立即显示过滤结果；back/forward 应按照历史记录切换目录；面包屑应能访问对应目录；隐藏文件开关的状态应持久化。

---

## 阶段 4 — 键盘与命令面板（效率）

**状态（2026-07-14）：✅ 已交付**

**目标：** 高频操作同时具备三种入口，并且用户可以只使用键盘完成全部操作（规范 §2.2/§9/§12）。

- **Command palette：** 实现当前为 stub 的 `OpenCommandPalette`。`cmd-shift-p` 显示面板，用户可以模糊搜索所有 app-level action，并用 Enter 执行所选操作（规范 §9 要求用户即使忘记快捷键，也能通过 palette 完成操作）。该功能复用 `InputState` 和虚拟列表。
- **完善键盘导航：** 支持 page up/down、home/end、`shift+↑/↓` 范围多选和 `cmd-a` 全选（规范 §6.2）。
- **MRU tab 切换：** 修改 `activate_tab_in_direction`，使其按照最近使用顺序选择 tab；同时提供 `ctrl-tab` 切换器（规范 §5）。
- **提高快捷键可发现性：** 菜单项和 tooltip 显示快捷键，并且 command palette 的每个条目显示对应按键。

**验证：** 用户无需鼠标即可完成“连接→浏览→多选→传输→重试→关闭 tab”的完整流程；palette 可以请求每个 action。

---

## 阶段 5 — 初次使用与持久化（减少重复操作）

**状态（2026-07-14）：✅ 已交付**（恢复布局，但是不自动 Connect）

**目标：** 应用重启或重新连接后，用户无需重新配置整个工作环境（规范目标：状态透明、路径直接）。

- **Session 恢复：** 应用关闭时持久化已存在的 tab，包括 host、path 和 profile 引用，但是不保存 secret。应用重新启动后，用户可以选择恢复上次工作区。
- **最近连接 / 连接历史：** 新 tab 的空状态列出最近成功连接，因此用户可以直接重新连接；该功能用于完善 profile 之外的“最近使用”。
- **首次启动指引：** 空状态提供“Open Connection / New Folder”后续操作（规范 §2.1 要求只提供后续操作，不显示营销页面）。
- **窗口标题显示当前 host：** 该信息在多窗口场景中具有更高价值。

**验证：** 应用重新启动后应恢复 tab 和路径；空 tab 应显示最近连接；首次启动时不显示营销页面，并且用户可以直接进入工作区。

---

## 阶段 6 — 细节完善与规范验收

**状态（2026-07-14）：✅ 已交付**（审计清单参见 `docs/plans/2026-07-14-phase6-polish-audit.md`；交互式 GUI 手工测试部分记为 accepted risk）

**目标：** 满足规范 §12/§13 和 §15 的评审清单。

- **微动效：** drawer 展开、tab 切换和 hover 的动效应短暂、轻量并允许跳过，同时不能影响输入或滚动（规范 §13）。
- **可访问性：** 审查 modal 的 focus order、开关操作后的焦点恢复，以及所有 icon-only button 的 tooltip（规范 §12）。
- **窄窗口审查：** tab、路径、按钮和 modal 文本不能发生溢出（规范 §15）。
- **性能烟雾测试：** 在 10k local entries、10k remote entries、4 active transfers 和 3 tabs 的组合场景下，界面不能出现卡顿（规范 §13）。
- **文案统一：** UI 文案统一使用一种语言，描述用户操作，并且不显示内部术语（规范 §11/§14）。

**验证：** 依次检查 §15 评审清单的 10 个项目；在真实设备上目视检查动效和窄窗口；手工执行性能烟雾测试场景。

---

## 优先级与依赖

```text
阶段 1 核心文件操作 ──┐（涉及 core+sftp+ui；对应最大缺项和用户优先方向 ①）
阶段 2 反馈可观测性 ──┤（主要涉及 app 层；对应用户优先方向 ②；可与阶段 1 并行）
阶段 3 搜索与导航   ──┘（依赖阶段 1 的菜单和键位结构）
阶段 4 键盘/palette    （依赖阶段 1、2 稳定后的 action 集合）
阶段 5 初次使用/持久化 （相对独立，因此可以提前实施）
阶段 6 细节与验收      （在其他阶段完成后统一执行）
```

- **阶段 1、2 是用户明确指定的两个优先方向，因此应优先并完整实施。** 二者的依赖较少，所以可以并行：阶段 1 主要涉及 core/sftp/ui 和交互，阶段 2 主要涉及 render/event_handling 反馈层。
- 阶段 3 使用阶段 1 提供的上下文菜单和键位结构，因此安排在阶段 1 之后。
- 每个阶段都可以独立交付和验证，并且符合规范 §15：合并前必须能够说明 loading、empty、error、focus、tooltip 和性能各项的状态。

## 每阶段通用验收（规范 §15 摘录，每个 PR 均需回答）

- 是否提供 command palette action 或快捷键路径？
- 是否覆盖 loading/empty/error/disabled/focused/hover/selected 的全部状态？
- 窄窗口中的文本是否保持在容器范围内？操作是否避免阻塞主线程？列表是否可以处理 10k entries？
- icon-only button 是否包含 tooltip？界面是否避免显示 secret 或内部术语？
- 危险操作（删除、覆盖）是否要求确认或提供可撤销语义？

---

## 阶段后专项（2026-07-14，不属于原六个阶段）

| 专项 | 状态 | 文档 |
| --- | --- | --- |
| Settings → Profiles 管理 | ✅ | `docs/plans/2026-07-14-profile-management-*` |
| Connect profile 紧凑选择器 | ✅ | `docs/plans/2026-07-14-connect-panel-profile-picker-*` |
| Transfer drawer 高度调整 / 粘性头 | ✅ | `docs/plans/2026-07-14-transfer-drawer-ux-*` |

**后续改进 backlog（摘要）：** Connect picker 的行标签和 Enter 行为、Settings 草稿丢弃、§15 交互人工测试记录、profile 保存路径去重、多窗口 session 写入策略。完整列表参见 `docs/progress-analysis-2026-07-14.md` §5–§6。
