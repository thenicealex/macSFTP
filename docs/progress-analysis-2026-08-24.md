# macSFTP — 项目进展分析（2026-08-24）

**Date:** 2026-08-24

**基准：** `docs/progress-analysis-2026-08-01.md`（上次总览）、`docs/plans/2026-08-21-workspace-decentralization-roadmap.md`、`docs/plans/2026-08-22-profiles-audit-fix-cycle.md`、`docs/release-evidence/v0.2.0.md`、当前 `master` 提交历史与今日实测验证。

**范围：** 08-01 之后的发布动作（0.1.0 / 0.1.1 / 0.2.0 三个版本）、07-28 审计遗留 4 项全部关闭、workspace 去中心化 Stage 1–4、断线原因分类、profiles 审计修复周期、工程验证现状、已知缺口与建议。

---

## 0. 一句话结论

macSFTP 从「功能收口、待验证」走到了**连续三个版本发布**：0.1.0（08-01）、0.1.1（08-05）、0.2.0（08-24，含交互式 GUI 冒烟签字）。07-28 审计台账 **7/7 全部关闭**（08-01 快照发出当天，遗留 4 项即已修复）。Workspace 从 64 字段 god object 收敛为 25 字段 + 职责内聚状态组，profiles 存储经一轮审计修复周期（6 个缺陷、4 个高危）加固到可发布水平。

## 1. 提交主线（08-01 → 08-24）

| 时间段 | 主题 | 代表提交 |
| --- | --- | --- |
| 08-01 | **v0.1.0 发布**：交互式 GUI 手测签字、password-auth Docker 门禁通过、CHANGELOG 收口、docs 索引与 archive 建立 | `d61efe7`、`d8deb3d`、`bb2abd4`、`d9d8e56` |
| 08-01 | **审计遗留 4 项当天关闭**：唯一临时路径、编辑临时目录错误传播与清理日志、mock actor 终态事件处理 | `1e3f4df`、`bb6c452`、`03c62a4`、`f08733a` |
| 08-05 | **v0.1.1 发布**：删除确认对话框重设计（更清晰的可逆性层级与文件/文件夹预览） | `f6711d9`、`2212c22`、`701330d` |
| 08-21 ~ 08-22 | **Workspace 去中心化 Stage 1–4**：per-tab 边表回归 core `TabState`、死代码 M3 连接流删除、`workspace/mod.rs` → `workspace.rs`、8 个 UI 状态组（PaneUi / CommandPaletteUi / TabSwitcherUi / GoToPathUi / TransferDrawerUi / ConnectFormUi / ModalInputsUi / SettingsUi） | `962c230`、`dcb5611`、`500dcc2`、S3-1…S3-8 系列 |
| 08-22 | **断线原因分类**：mid-session 断线按原因分类（"Server closed the connection" / "Connection lost" + 网络超时提示）替换笼统的 "Disconnected"，日志与 UI 双落 | `0a9f372`、`0b16055`、`b62a236` |
| 08-22 | **依赖安全**：h2 升级 0.4.18，关闭 RUSTSEC-2026-0258 | `4b2a9e0` |
| 08-22 | **UI 打磨**：tab bar 移入透明自定义 titlebar、连接空状态、icon button 标准化 + 返回导航、文件夹大小列空置、设置页头平衡 | `b7ad7b7`、`63c05b3`、`6025443`、`3fabee7`、`0fcbaf8` |
| 08-22 ~ 08-24 | **Profiles 审计修复周期（批次 A–F）**：存储原子写分阶段错误、编辑会话按连接身份键控、passphrase 三态、删除引用解耦、跨实例写序列化、residual 清理与版本错误对齐 | `251a9bd` … `a370b0a`、`046d574` |
| 08-24 | **v0.2.0 发布**：release evidence 签字（含原生 GUI 冒烟）、tag `v0.2.0` | `c8ee8f3`、`ec1993e`、`da993bf` |

## 2. 07-28 审计遗留台账 — 全部关闭

08-01 总览发出当天，遗留 4 项即已修复并带回归测试：

| 编号 | 严重度 | 状态 | 修复 |
| --- | --- | --- | --- |
| SFTP-TRANSFER-001 / CORE-SFTP-001 / APP-EDIT-001 | 高 | ✅ 07-29 已修 | — |
| **TEST-001** 固定共享临时路径并发冲突 | 中 | ✅ 08-01 已修 | `1e3f4df`：测试临时目录按 test label + pid 唯一化；`platform.rs`、`workspace/tests.rs` 均引入 `unique_temp_dir` helper |
| **APP-EDIT-002/003** 编辑临时目录创建/清理错误静默 | 低 | ✅ 08-01 已修 | `bb6c452`：创建错误传播到 UI、清理失败写日志；`remote_edit.rs` 现存 `let _ =` 为 0 |
| **SFTP-MOCK-001** mock actor 静默丢弃终态事件 | 低 | ✅ 08-01 已修 | `03c62a4`：事件通道关闭即退出，不再静默丢 send |

**审计台账 7/7 关闭，0 遗留。** 完成的问题审计报告按项目惯例删除（`3f4692f`），不留档。

## 3. 工程验证现状（2026-08-24 本机实测）

`cargo test --workspace` 全绿：**508 passed / 0 failed / 1 ignored**（08-01 为 467）。ignored 为 Keychain 门禁测试，由 CI 显式运行。

| 目标 | 数量 | 对比 08-01 |
| --- | --- | --- |
| macsftp-app（二进制） | 199 | =（内部换血：去中心化 + 断线分类 + profiles 修复回归） |
| macsftp-core | 67 | +3（per-tab 守卫迁移进 core 后的状态机测试） |
| macsftp-sftp（unit） | 92 | +9（断线分类、EOF 路径端到端） |
| macsftp-sftp real_session | 27 | +1 |
| macsftp-sftp password_auth | 1 | = |
| macsftp-storage | 71（1 ignored） | +26（审计修复周期批次 A–E 回归） |
| macsftp-platform | 19 | +2 |
| macsftp-ui | 30 | = |
| macsftp-test-support | 2 | = |

发布门禁（`docs/release-evidence/v0.2.0.md`，全 PASS）：完整 `scripts/check.sh`（fmt / 架构 / 敏感日志 / 15 套测试 / clippy `-D warnings`）、Docker password-auth 门禁、隔离 Keychain 门禁、`build_app.sh` + plutil 校验（0.2.0）、原生 GUI 冒烟（两窗口独立状态、passphrase 三态切换、pre-0.2.0 profiles.json 原位迁移、10k entries、720x480 / Retina / 明暗主题 / 键盘 / 基础 VoiceOver、日志无凭据指纹）。

## 4. 已知缺口与风险

1. **分发签名未决**：app bundle 仍未签名/公证，公开 release 只附源码归档（`docs/release-process.md` 已声明）。Developer ID 签名 + 公证是当前唯一挡住「公开可分发的二进制」的门。
2. **长期已知限制（有意保留）**：RSA 私钥认证禁用（RUSTSEC-2023-0071）；传输队列不跨进程恢复；i18n 未做；远程编辑同秒同大小并发修改漏检（文档化取舍）。
3. **有意不做的决策**（`plans/2026-08-22-profiles-audit-fix-cycle.md`）：session.json 跨实例合并、传输按 profile 路由、Keychain 孤儿 secret 自动清扫、`last_local_path` 字段（`fe2fd41` 已删除死字段）。各有成本/收益论证，未来出现真实需求再评估。
4. **低优先级不一致**：session 文件 version 0 仍按 v1 legacy 解析（无真实写入方，暂保持宽容）。
5. **文档卫生**：本次总览伴随一轮文档清理（删除已完成的 bug 审计、停用的 SDD 工作流产物、旧构建产物；修正死链），文档索引以后随发布周期同步刷新。

## 5. 建议的下一步（价值 × 成本）

1. **P1 — 签名与公证决策**：决定是否启用 Developer ID 签名/公证并产出第一个公开二进制 release。技术路径已清（Info.plist 注入、plutil 校验就绪），剩下的是证书/公证策略与 `release-process.md` 更新。
2. **P2 — session.json 合并语义（若产品需要）**：当前多实例并发写已由 advisory flock 序列化，但跨实例丢更新需要产品层面的合并决策（含 WindowSessionId 分配冲突），先观察是否有真实报告。
3. **P2 — 下个功能方向选择**：功能面已稳定，候选方向（i18n、传输恢复、更多协议）应各写一份价值/成本评估再定，不做"为做而做"。
4. **P3 — 文档索引维护**：发布动作后同步更新 `docs/README.md` 的「当前状态入口」，避免再次出现"最新总览"过期三周的情况。

## 6. 进度量化（粗估）

| 维度 | 粗估 | 说明 |
| --- | --- | --- |
| 发布动作 | 100% | 0.1.0 / 0.1.1 / 0.2.0 三个 tag，证据全部签字 |
| 审计台账（07-28） | 100% | 7/7，含 08-01 当天关闭的遗留 4 项 |
| Workspace 去中心化 | 100% | Stage 1–4 交付；顶层字段 60 → 25；mod.rs 违规清零 |
| Profiles 审计修复周期 | 100% | 批次 A–F 收口，唯一开放项（GUI 冒烟）由 0.2.0 证据签字关闭 |
| 断线可观测性 | 100% | mid-session 断线按原因分类，UI + 日志双落 |
| 工程门禁 | 100% | 508 tests 全绿 + 发布门禁全 PASS |
| 可分发性 | ~60% | 源码归档可发布；签名/公证未做，无公开二进制 |

**Overall：** 产品从「可日常使用」走到「已连续发布」：验证体系（自动门禁 + Docker sshd + 隔离 Keychain + 原生 GUI 冒烟）覆盖了此前唯一未被自动化覆盖的产品风险。剩余工作集中在分发签名决策，而非功能开发。

## 7. 关键文档索引

| 文档 | 用途 |
| --- | --- |
| `docs/progress-analysis-2026-08-01.md` | 上次总览（三高缺陷修复 + 首次全绿） |
| `docs/plans/2026-08-21-workspace-decentralization-roadmap.md` | Workspace 去中心化路线图（Stage 1–4 已交付） |
| `docs/plans/2026-08-21-stage3-stage4-plan.md` | Stage 3/4 执行计划 |
| `docs/plans/2026-08-22-profiles-audit-fix-cycle.md` | Profiles 审计修复周期持久档案（批次 A–F + 决策记录） |
| `docs/release-evidence/v0.2.0.md` | 0.2.0 发布证据（PASS，含原生 GUI 冒烟） |
| `docs/release-evidence/v0.1.1.md`、`v0.1.0.md` | 0.1.x 发布证据 |
| `CHANGELOG.md` | 用户可见变更 |
| `docs/gpui-russh-plan.md` | 架构主文档 |
