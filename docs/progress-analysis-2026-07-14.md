# macSFTP — 项目进展分析（2026-07-14）

**Date:** 2026-07-14 GMT+8

**基准：** `docs/ux-improvement-plan.md`、`docs/ui-ux-guidelines.md`、`docs/plans/*`、当前 `master` 提交历史与 `cargo test -p macsftp-app --bin macsftp`（**132** 通过）。

**范围：** 产品能力、UX 阶段交付、近期专项、已知缺口与改进建议。

> 早期快照：`docs/progress-analysis-2026-07-12.md`、`docs/progress-analysis-2026-07-13.md`、`docs/progress-analysis-2026-07-13-multiwindow.md`。以**本文**为 UX 收口后的最新总览。

---

## 0. 一句话结论

macSFTP 已从「可连接的文件浏览器原型」演进为 **可当主力使用的本地 macOS SFTP 客户端**：核心文件操作、传输反馈、导航、键盘/palette、session/recents、规范打磨、Profile 管理与 Connect 面板压缩、Transfer drawer 可调高度均已落地。  
**UX 提升六阶段（计划 §1–§6）整体可视为完成**；后续工作应转向 **质量硬化、体验锐化与 post-MVP 能力**，而不是再开大面积「从 0 补功能」的阶段。

---

## 1. 产品定位与架构（已稳）

| 层 | Crate | 职责（现状） |
| --- | --- | --- |
| 模型 / 状态机 | `macsftp-core` | Tab/Session、命令与事件、传输计划、错误模型 |
| 网络 / SFTP | `macsftp-sftp` | russh + actor、Connection Pool、传输与 FS 操作 |
| 存储 | `macsftp-storage` | profiles、config、Keychain、session、recents、residual temp records |
| 平台 | `macsftp-platform` | AppPaths、本地 FS |
| UI 组件 | `macsftp-ui` | Theme、列表、按钮、tooltip、transfer row |
| 应用 | `macsftp-app` | GPUI 窗口、Workspace 模块化、桥接 Runtime |

**硬约束（AGENTS / 计划）仍成立：** core 不依赖 GPUI/russh；主线程不 await 网络；secret 只进 Keychain；host key mismatch 阻断。

**基础设施已具备：** 多窗口 + 进程级 `AppResources` / `SharedTransfers`；虚拟化文件列表；拖拽传文件；host-key / 冲突 / 删除确认 modal。

---

## 2. UX 提升计划 — 阶段完成度

对照 `docs/ux-improvement-plan.md` 原「缺失」表与六阶段目标：

| 阶段 | 主题 | 状态 | 代表交付 |
| --- | --- | --- | --- |
| **1** | 核心文件操作 | ✅ 完成 | 本地/远程删改建、右键菜单、行内重命名、删除确认、快捷键 |
| **2** | 反馈与可观测 | ✅ 完成 | 真实速度/ETA、加载态、就地错误+Retry、status bar 传输摘要 |
| **3** | 搜索与导航 | ✅ 完成 | type-to-filter、back/forward、面包屑/Go to Path、隐藏文件、列排序 |
| **4** | 键盘与 palette | ✅ 完成 | Command palette、page/home/end、shift 多选、MRU/ctrl-tab、快捷键可发现性 |
| **5** | 上手与持久化 | ✅ 完成 | `session.json` 布局恢复（不自动连）、`recents.json`、空态下一步、窗口标题 |
| **6** | 打磨与达标 | ✅ 完成（审计） | tooltip/焦点、文案禁内部词、窄窗 truncate、10k visible smoke；**交互式 GUI 手测为 accepted risk** |

**原 §0「确认缺失」对照（已关闭）：**

| 原缺口 | 现状 |
| --- | --- |
| 无删除/重命名/新建文件夹 | 已有 |
| 速度/ETA 占位符 | 真实采样 |
| 无目录内过滤 | type-to-filter |
| 无右键菜单 | 已有 |
| path 无后退 | back/forward + 历史 |
| palette stub | 完整 registry |
| 键盘不全 | page/shift/cmd-a 等 |
| tab 非 MRU | MRU 切换器 |
| 无 session 恢复 | SessionStore |
| 首载反馈 | loading / empty 态 |

设计/实现文档集中在 `docs/plans/2026-07-1{3,4}-phase*.md` 与 phase6 audit。

---

## 3. 阶段 6 之后的专项（已合入 master）

在六阶段之外，又完成了三轮**产品向**改进：

### 3.1 Profile 管理（Settings）

- Settings 侧栏 **General | Profiles**。  
- 列表 / 过滤 / 新建 / 编辑 / Save；空密码更新 **不擦 Keychain**。  
- 删除 **确认**（Settings 与旧 Connect 路径对齐后，Connect 侧删除已弱化/移除）。  
- Palette：**Manage Profiles**。

文档：`docs/plans/2026-07-14-profile-management-{design,impl}.md`。

### 3.2 Connect 面板 Profile 选择重构

- 去掉线性「Use/Delete」档案墙。  
- **一行** Profile 触发器 + 内嵌 popover（过滤、选档案、Manual entry）。  
- **Save as** 默认折叠；**Manage…** 进 Settings Profiles。  
- Esc 先关 picker 再关表单。

文档：`docs/plans/2026-07-14-connect-panel-profile-picker-{design,impl}.md`。

### 3.3 Transfer Drawer UX

- Drawer 高度可拖拽调整、双击复位。  
- Sticky header、会话内高度状态。  
- 相关稳定性修复（resize 起止、session 结束清理拖拽状态等）。

文档：`docs/plans/2026-07-14-transfer-drawer-ux-{design,impl}.md`。

### 3.4 更早的 post-MVP

- **多窗口**（进程共享 stores / transfers）。  
- **Connection Pool**（SSH 多路复用）。  
见 `docs/progress-analysis-2026-07-13-multiwindow.md`。

---

## 4. 测试与工程健康度（快照）

| 项 | 现状 |
| --- | --- |
| `macsftp-app` 二进制测试 | **132 passed**（2026-07-14 实测；移除 3 个无产品入口的 History-only 测试后） |
| 覆盖面 | core/sftp/storage/app 分层均有；gpui 集成测在 app；`real_session` 集成测 21 |
| Phase 6 性能 | **10k visible indices 单元 smoke**；完整 10k GUI + 4 transfer 手测 **未系统签字** |
| **Verify gate** | **`bash scripts/check.sh` 全绿**（fmt + workspace test + clippy `-D warnings`，2026-07-14 P0 修通） |
| 文档 | 计划与 design/impl 较完整；`ux-improvement-plan` 已标阶段完成 |

---

## 5. 还差什么 / 可改进方向

按 **价值 × 成本** 分组，供排期参考。

### 5.1 体验锐化（高价值、中小成本）

| 项 | 说明 |
| --- | --- |
| Connect picker 行标签 | 用 `profile_list_label`（name · user@host:port），同名档案可区分 |
| Picker 打开时 Enter | 避免 Enter 直接 Connect；改为选中首条匹配或 no-op |
| 未保存 Profile 草稿 | 关 Settings 时丢弃 New 草稿（design 已写、实现有 residual） |
| 删除确认后焦点 | 从 Settings/Connect 删除后 focus 回到合理表面，而非一律 pane |
| Save as / 过滤清空 | Manual 或关 picker 时清空 filter（小 polish） |
| Transfer drawer | 继续压实空态、多选操作、与 status bar 的一致性 |

### 5.2 质量与安全硬化（高价值、中成本）

| 项 | 说明 |
| --- | --- |
| **交互式 §15 手测签字** | 720×480、10k 目录、多 transfer、多 tab 真实跑一遍并更新 audit |
| **Verify gate 常绿** | 保持 `fmt` + `clippy -D warnings` + test 在 CI 必过 |
| Keychain / 双保存路径 | **已收口：** Connect 与 Settings 均通过 storage `ProfileStore` 事务保存；app 不直接访问 Keychain |
| Secret 内存 / 诊断脱敏 | **已收口：** UI secret state 不可克隆并清零旧缓冲；第三方连接错误与私钥完整路径不进入 UI/log |
| 依赖审计 / RSA | cargo-deny 阻断漏洞与未知来源；RUSTSEC-2023-0071 未忽略，暂时禁用 RSA 私钥与 RSA-only host key |
| 集成测试 | 真实 sshd 路径在 CI 可跑时默认开；无 Docker skip 信息已有则保持清晰 |
| 陈旧事件 / multi-window session | 多窗口写 `session.json` last-writer 已知限制；可选 tabs-most 或仅主窗口写 |

### 5.3 功能扩展（按需）

| 项 | 说明 |
| --- | --- |
| Profile 分组 / 排序 / 导入 OpenSSH config | design 明确非 MVP |
| Settings 一键 Connect | 曾否决；若产品要「库即连接」可重开 |
| 传输队列恢复 | 不恢复；运行中任务随进程结束，重启后由用户重新发起 |
| 同步 / compare / 双向 diff | 非当前架构主路径 |
| 微动效系统 | Phase 6 明确几乎不动效；仅在有证据再上 |
| i18n | 业务层已 error code 友好；完整多语言未做 |
| 同步书签 / 云配置 | 非目标 |

### 5.4 工程与可维护性

| 项 | 说明 |
| --- | --- |
| workspace 渲染职责 | 已按主工作区、Settings/Profile、Transfer Drawer/状态栏拆成三个实现文件；不新增状态或抽象 |
| profile 持久化职责 | 已拆为 JSON 文件 I/O、ProfileStore/Keychain 事务、公共 API 组装三层；调用方 API 不变 |
| 视觉回归 | UI 改动依赖人工；可选截图基线或脚本化 smoke |
| 文档导航 | 增加本文件为「现状入口」；ux-improvement-plan 标注阶段完成 |

---

## 6. 建议的下一步优先级

1. ~~**P0 — 工程守门**~~ **已完成（2026-07-14）：** `scripts/check.sh` 全绿（fmt / test / clippy `-D warnings`）。  
2. ~~**P1 — Connect/Profile 小 polish**~~ **已完成（2026-07-14）：** picker 行标签、Enter 选首条、Settings 草稿丢弃、删除后焦点、filter 清空。  
3. **P1 — 手测签字：** 按 phase6 audit 手测清单跑一轮，把 accepted risk 收敛为 pass 或具体 bug 单。  
4. ~~**P2 — 共享 profile 保存路径**~~ **已完成（2026-07-14）：** storage 统一事务、失败回滚、orphan 清理 warning。
5. **P2 — 按用户真实痛点** 开下一专项（例如传输体验、批量操作、书签），**不要**再平行开大而全的「阶段 7 杂烩」。

---

## 7. 进度量化（粗估）

| 维度 | 粗估 | 说明 |
| --- | --- | --- |
| MVP（M0–M7） | ~100% | 原计划 delivered |
| UX 提升计划 1–6 | ~95–100% | 功能完成；GUI 手测/个别 residual 未清零 |
| 连接/档案体验 | ~90% | 管理面 + Connect 压缩已上；细节见 §5.1 |
| 传输/drawer | ~85% | 核心可用 + 可调高度；体验仍可挖 |
| 可发布打磨 | ~80% | 依赖手测、CI 纪律、少量 a11y 边角 |

**Overall：** 作为个人/小团队主力 SFTP 客户端，**功能面已可日常使用**；与「商店级抛光」之间主要差在 **手测覆盖、边角焦点/键盘、传输与多窗口极限场景**。

---

## 8. 关键文档索引

| 文档 | 用途 |
| --- | --- |
| `docs/ui-ux-guidelines.md` | 强制 UX 规范 |
| `docs/ux-improvement-plan.md` | 六阶段路线图（§0 已过时，见更新注） |
| `docs/plans/2026-07-14-phase6-polish-audit.md` | §15 勾选与 residual |
| `docs/gpui-russh-plan.md` | 架构主文档 |
| `docs/progress-analysis-2026-07-13-multiwindow.md` | 多窗口专项 |
| `docs/plans/*-design.md` / `*-impl.md` | 各阶段/专项设计与实现计划 |

---

## 9. 变更记录

| 日期 | 说明 |
| --- | --- |
| 2026-07-14 | 初版：六阶段完成态 + Profile/Connect/Drawer 专项 + 改进 backlog |
| 2026-07-14 | P0：`scripts/check.sh` 全绿；core 重复 `#[test]`、sftp clippy、app lint 清理 |
| 2026-07-14 | P1：Connect/Profile polish（行标签、Enter、草稿、焦点、filter） |
| 2026-07-14 | P0：移除无产品入口的 Transfer History catalog/retry 路径；drawer 仅保留进程内分组 |
| 2026-07-14 | 安全边界：ProfileStore 统一 profiles.json + Keychain 事务，app 移除 Keychain 直连 |
| 2026-07-14 | 安全边界：UI secret 输入清零且不可克隆；SSH 连接与私钥诊断按分类脱敏 |
| 2026-07-14 | 供应链：cargo-deny + Dependabot；因 RUSTSEC-2023-0071 暂时关闭 russh RSA feature |
| 2026-07-14 | 可维护性：拆分 profile 文件 I/O 与 secret 事务，并按产品 surface 拆分 workspace 渲染 |
