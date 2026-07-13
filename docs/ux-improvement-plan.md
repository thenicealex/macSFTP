# macSFTP — UX 提升分阶段完整计划

**Date:** 2026-07-13 GMT+8  
**Status update:** 2026-07-14 — **阶段 1–6 均已交付**；后续进度与改进 backlog 见 **`docs/progress-analysis-2026-07-14.md`**。  
**基准:** `docs/ui-ux-guidelines.md`（强制设计规范）+ 实测当前实现状态（`crates/app/src/workspace/*`、`crates/core/src/core.rs`、`crates/sftp/*`）。  
**用户优先方向（已确认）:** ① 核心文件操作 ② 反馈与可观测性。**节奏:** 系统完整优先（每个维度一次做透，而非零散 quick win）。

---

## 0. 当前 UX 现状（地面真相）

> **2026-07-14 注：** 下列「已具备」为计划起草时基线；「确认缺失」表中条目**均已在阶段 1–6 关闭**。阶段后专项（Settings Profiles、Connect profile picker、Transfer drawer 高度）亦已合入。细节与剩余改进见 `docs/progress-analysis-2026-07-14.md`。

**已具备（起草时基线 + 后续交付）：**
- 视觉系统完整（M7）：`Theme` 全局 + 语义色/尺寸/字体全令牌化，dark/light/system 三态。
- 主布局齐全：tab bar + 左右 pane + transfer drawer + status bar（drawer 可拖拽调高）。
- 文件列表**已虚拟化**（`uniform_list`），支持 10k entries。
- 拖拽：local↔remote 上传/下载（`DragPreview` + drop handlers）。
- 传输：分组（active/queued/completed/failed）、cancel/retry、planning 流式、history 持久化 + 重试；**真实速度/ETA**。
- 连接：connect form（**紧凑 profile picker**）、host-key modal、冲突 modal（含 apply-to-all）、Keychain + **Settings Profiles CRUD**、recents。
- 文件操作：删除/重命名/新建文件夹、右键菜单、行内编辑。
- 导航：type-to-filter、back/forward、面包屑、Go to Path、隐藏文件、列排序。
- 效率：command palette、键盘多选/翻页、MRU tab 切换。
- 持久化：session 布局恢复（不自动连）、recents、窗口标题。
- Settings/About surface、复制路径、多标签、多窗口。

**原「确认缺失」表（历史记录，2026-07-14 起全部已关闭）：**

| 缺口（起草时） | 关闭于 |
| --- | --- |
| 无用户级文件操作 | 阶段 1 |
| 传输速度/ETA 占位符 | 阶段 2 |
| 无目录内搜索/过滤 | 阶段 3 |
| 无右键上下文菜单 | 阶段 1 |
| path bar 无后退 | 阶段 3 |
| command palette stub | 阶段 4 |
| 键盘导航不完整 | 阶段 4 |
| tab 非 MRU | 阶段 4 |
| 无 session 恢复 | 阶段 5 |
| 首载反馈不足 | 阶段 2 |

---

## 阶段 1 — 核心文件操作（最高优先，系统做透）

**状态（2026-07-14）：✅ 已交付**

**目标：** 让 macSFTP 从"只读 + 传输"变成可当主力用的 SFTP 客户端 —— 远程与本地都能删除、重命名、新建文件夹，且键盘 + 右键 + 按钮三条入口齐全（规范 §2.2）。

**为什么先做这个：** 这是当前最大的功能性缺口，也是用户第一优先级。没有它，"当主力工具用"无从谈起。它需要后端协议 + UI 两层，是纯 UX 之外唯一需要动 `crates/sftp` 的阶段。

### 1a. 后端协议（`crates/core` + `crates/sftp`）
- `AppCommand` 新增：`DeleteEntry { tab_id, path, is_dir }`、`RenameEntry { tab_id, from, to }`、`CreateDirectory { tab_id, parent, name }`（远程）；本地对应操作走 `crates/platform`（本地 IO，无需 SFTP actor）。
- `AppEvent` 新增确认/失败事件：复用现有 `RemoteOperationFailed` 表达失败；成功后触发一次目录 refresh。
- `session_actor.rs` 实现：`russh-sftp` 的 `remove_file`/`remove_dir`/`rename`/`create_dir`。递归删除目录需服务端逐项删（先列举再删）——**危险操作，必须在 UI 层确认**。

### 1b. 交互 UI（`crates/app/src/workspace`）
- **右键上下文菜单**（新组件，`crates/ui`）：文件行右键 → Rename / Delete / New Folder / Download(remote) / Upload(local) / Copy Path / Reveal。空白区右键 → New Folder / Refresh。
- **行内重命名**：F2 或菜单触发，行内变输入框（复用 `InputState`），Enter 提交 / Esc 取消，冲突/非法名就地报错。
- **删除确认 modal**：显示将删除的条目数与名称；目录删除明确标注"递归"；危险色主操作（规范 §8/§10）。多选时批量确认。
- **New Folder**：菜单 + 快捷键触发，默认名进入行内重命名态。

### 1c. 快捷键（`app_actions.rs`）
- `cmd-delete` → Delete（选中项）；`enter`/`F2` → Rename；`cmd-shift-n` → New Folder。全部走稳定 action（`DeleteSelection`/`RenameEntry`/`NewFolder`），保证 command palette 可达（阶段 4）。

### 1d. 批量
- 多选（已存在 selection 集合）驱动批量删除/下载/上传；确认 modal 汇总数量。

**验证：** 连真实服务器建/删/重命名远程目录与文件；本地同样；删除目录走确认；操作后列表自动刷新；失败（权限/占用）就地报错可重试。跑 `cargo test`（为新命令加 core/sftp 单测）。

---

## 阶段 2 — 反馈与可观测性（第二优先，系统做透）

**状态（2026-07-14）：✅ 已交付**

**目标：** 让所有"进行中/加载中/出错"状态在原位透明表达（规范 §2.3/§6.3/§7/§10），消灭占位符与静默。

### 2a. 真实传输速度 + ETA（消灭 `— MB/s · ETA —` 占位符）
- 在 `TransferJob`/view 侧记录进度采样（上次 `bytes_done` + 时间戳；`Timestamp` 已有）。用滑动窗口算瞬时速度，`(bytes_total - bytes_done) / speed` 算 ETA。
- `render_transfer_job` 用真实值替换字面量；速度平滑但**不得用动画掩盖真实停滞**（规范 §7）；停滞时显示"Stalled"。
- drawer 顶部加聚合：总进度、总速度、剩余时间。

### 2b. 加载状态
- 远程首次加载：骨架屏或居中 spinner（保留 path bar，规范 §6.1/§6.3）。
- 重载：保留旧列表 + "Refreshing…" 标记（已部分存在，补齐一致性）。
- 连接中：pane/tab 显示目标 host + **可取消**动作（规范 §6.3）。

### 2c. 就地错误恢复
- `RemoteOperationFailed` 时在 pane 内展示 `UserFacingError`（title + message + **Retry** 按钮），而非只塞 status bar（规范 §10）。当前 error 已存进 `tab.remote.error`——补一个就地错误态渲染 + 恢复动作。
- status bar 只留摘要，详情就地展开。

### 2d. status bar 增强
- drawer 收起时，status bar 显示 active/failed 传输轻量提示（规范 §4 明确要求）；点击展开 drawer。
- 显示当前 tab 连接状态 + 选中计数。

**验证：** 大文件传输实时看到 MB/s 与 ETA 收敛；断网/权限错在 pane 内就地可重试；首载有骨架、重载不清空；收起 drawer 后 status bar 仍提示传输。

---

## 阶段 3 — 搜索与目录导航（补齐"核心文件操作"的可用性）

**状态（2026-07-14）：✅ 已交付**

**目标：** 大目录里能快速定位与穿梭（规范 §6.1/§6.2）。

- **目录内 type-to-filter**：pane 聚焦时直接敲字 → 增量过滤当前列表（不发网络，纯本地过滤已加载 entries）；`Esc` 清除。
- **path bar 后退/前进**：每 pane 维护导航历史栈，back/forward 按钮 + `cmd-[` / `cmd-]`（注意与 tab 切换键区分）。
- **面包屑 / 跳转路径**：path bar 可点击各级目录跳转；或 `cmd-shift-g` 输入路径直达。
- **隐藏文件开关**：显示/隐藏 dotfiles，状态记入 config。
- **列排序**：表头点击按 name/size/mtime 排序（`file_table_header` 已展示排序态，补齐点击切换与三列）。

**验证：** 万级目录敲字秒过滤；back/forward 正确回溯；面包屑跳转；隐藏文件开关持久化。

---

## 阶段 4 — 键盘与命令面板（效率）

**状态（2026-07-14）：✅ 已交付**

**目标：** 高频动作三入口齐全、纯键盘可完成一切（规范 §2.2/§9/§12）。

- **Command palette**：实现 `OpenCommandPalette`（当前 stub）——`cmd-shift-p` 打开，模糊搜索所有 app-level action，Enter 执行（规范 §9：忘快捷键也能用 palette 完成）。复用 `InputState` + 虚拟列表。
- **补齐键盘导航**：page up/down、home/end、`shift+↑/↓` 范围多选、`cmd-a` 全选（规范 §6.2）。
- **MRU tab 切换**：`activate_tab_in_direction` 改按最近使用排序；`ctrl-tab` 切换器（规范 §5）。
- **快捷键可发现性**：菜单项与 tooltip 显示快捷键；command palette 每项显示绑定键。

**验证：** 不碰鼠标完成 连接→浏览→多选→传输→重试→关 tab 全流程；palette 能触发每个 action。

---

## 阶段 5 — 上手与持久化（降低重复成本）

**状态（2026-07-14）：✅ 已交付**（布局恢复，不自动 Connect）

**目标：** 重启/重连不必从零（规范目标：状态透明、快速路径）。

- **Session 恢复**：关闭时持久化打开的 tab（host/path/profile 引用，不存 secret），重启后可选恢复上次工作区。
- **最近连接 / 连接历史**：新 tab 空状态列出最近成功连接，一键重连（补齐 profile 之外的"最近使用"）。
- **首次启动引导**：空状态给"Open Connection / New Folder"下一步动作（规范 §2.1：只给下一步，不做营销页）。
- **每窗口标题反映当前 host**（多窗口后更有用）。

**验证：** 重启后恢复 tab 与路径；空 tab 展示最近连接；首启动无营销页、直达工作区。

---

## 阶段 6 — 打磨与达标（收尾）

**状态（2026-07-14）：✅ 已交付**（审计清单见 `docs/plans/2026-07-14-phase6-polish-audit.md`；交互式 GUI 手测部分为 accepted risk）

**目标：** 过规范 §12/§13 与 §15 评审清单。

- **微动效**：drawer 展开、tab 切换、hover ——短、轻、可跳过，不影响输入/滚动（规范 §13）。
- **可访问性**：modal focus order 与开关焦点恢复审计；所有 icon-only button tooltip 审计（规范 §12）。
- **窄窗口审计**：tab/路径/按钮/modal 文本截断不溢出（规范 §15）。
- **性能烟测**：10k local + 10k remote + 4 active transfers + 3 tabs 无卡顿（规范 §13）。
- **文案统一**：全 UI 单语一致、面向动作、不泄露内部术语（规范 §11/§14）。

**验证：** 逐条过 §15 评审清单 10 项；实机目检动效与窄窗口；性能烟测场景手测。

---

## 优先级与依赖小结

```
阶段1 核心文件操作 ──┐ (需动 core+sftp+ui，最大缺口，用户①)
阶段2 反馈可观测   ──┤ (纯 app 层，用户②，可与①并行)
阶段3 搜索导航     ──┘ (依赖①的菜单/键位框架)
阶段4 键盘/palette   (依赖①②稳定后的 action 集)
阶段5 上手/持久化    (相对独立，可提前插入)
阶段6 打磨达标       (最后统一收口)
```

- **阶段 1、2 是用户明确的两个优先方向，应最先做且做透。** 二者耦合低，可并行（1 主要在 core/sftp/ui + 交互，2 主要在 render/event_handling 反馈层）。
- 阶段 3 复用阶段 1 的上下文菜单/键位骨架，紧随其后。
- 每个阶段都能独立交付、独立验证（符合规范 §15：合并前必须能回答 loading/empty/error/focus/tooltip/性能各项）。

## 每阶段通用验收（规范 §15 摘录，逐 PR 必答）
- 是否有 command palette action 或快捷键路径？
- 是否覆盖 loading/empty/error/disabled/focused/hover/selected 全状态？
- 窄窗口文本是否不溢出？是否不阻塞主线程？是否能处理 10k entries？
- icon-only button 是否有 tooltip？是否未泄露 secret / 内部术语？
- 危险操作（删除、覆盖）是否有确认或可撤销语义？

---

## 阶段后补充（2026-07-14，不在原六阶段内）

| 专项 | 状态 | 文档 |
| --- | --- | --- |
| Settings → Profiles 管理 | ✅ | `docs/plans/2026-07-14-profile-management-*` |
| Connect profile 紧凑选择器 | ✅ | `docs/plans/2026-07-14-connect-panel-profile-picker-*` |
| Transfer drawer 可调高度 / 粘性头 | ✅ | `docs/plans/2026-07-14-transfer-drawer-ux-*` |

**后续改进 backlog（摘要）：** Connect picker 行标签与 Enter 行为、Settings 草稿丢弃、§15 交互手测签字、profile 保存路径去重、多窗口 session 写盘策略。完整列表见 `docs/progress-analysis-2026-07-14.md` §5–§6。
