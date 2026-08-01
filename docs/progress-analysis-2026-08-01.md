# macSFTP — 项目进展分析（2026-08-01）

**Date:** 2026-08-01

**基准：** `docs/progress-analysis-2026-07-14.md`（上次总览）、`docs/plans/2026-07-29-detailed-code-implementation-plan.md`、`outputs/macsftp-bug-audit-2026-07-28.md`、当前 `master` 提交历史与今日实测验证。

**范围：** 07-14 之后的专项交付、三个高严重度缺陷修复、工程验证现状（本机首次全绿）、审计遗留台账、已知缺口与建议。

---

## 0. 一句话结论

macSFTP 在「可日常使用」的基础上又收口了三件大事：**远程编辑**（下载→外部编辑→自动回传）、**07-28 审计的三项高严重度缺陷全部修复**（TDD，独立 PR 流程），以及 **自定义主题滚动条**（替代 GPUI 原生自动隐藏滚动条）。更关键的是：**本机首次跑通完整 `scripts/check.sh` 门禁**——之前因缺少 Xcode Metal 编译器被阻断的 app/GPUI 测试现在全部在本机运行通过（467 passed，clippy/fmt/架构/敏感日志全绿）。

## 1. 上次分析（07-14）之后的提交主线

| 时间段 | 主题 | 代表提交 |
| --- | --- | --- |
| 07-15 ~ 07-16 | **远程编辑** 设计 + 实现：下载→外部编辑→mtime 轮询回传、冲突快照、100MB 大文件确认、自定义编辑器、会话复用 | `127854a`、`5b140d6`、`47658bc` … `0d04394` |
| 07-16 ~ 07-28 | 远程编辑 **post-merge 审计 11 个 bug + Phase 2 修复**：run_id 命名空间、自愈清理、编辑基线重同步、编辑中断恢复、OverwriteAll 去重弹窗 | `f5db0af`、`69f847e`、`4fd9ceb`、`e4d579d` |
| 07-28 | **全项目 Bug 审计** → `outputs/macsftp-bug-audit-2026-07-28.md`（确认 7 项缺陷：3 高、1 中、3 低） | `8637e2b` |
| 07-28 | **Transfer drawer：Cancel All / Clear Records** | `f1fcdca` … `737a11b`，`7fe57ca` 合入 master |
| 07-29 | **PR 1 传输终态保证**（SFTP-TRANSFER-001） | `6914960` |
| 07-29 | **PR 2 host-key mismatch 会话作用域**（CORE-SFTP-001） | `c242c93` … `c6680f9` |
| 07-29 | **PR 3 编辑回传前实时远端校验**（APP-EDIT-001） | `048a746` … `c4a455f` |
| 07-29 ~ 07-30 | **自定义主题滚动条**：theme tokens → 组件 → 文件 pane → drawer/palette/picker → 行为收口 | `6d1a19c` … `75ba7e1` |

## 2. 三项高严重度缺陷修复（TDD，验收通过）

对照 `docs/plans/2026-07-29-detailed-code-implementation-plan.md`：

| 缺陷 | 状态 | 关键交付 |
| --- | --- | --- |
| **SFTP-TRANSFER-001** 上传任务可能永久停留在 Queued | ✅ 已修 | `set_plan_terminal_state` 终态化 root + 全部 child；晚到 `TransferPlanProgress` 不能复活终态 plan；runtime 三处 handoff 失败点逐个 `fail_planned_jobs` 发终态事件；Retry 动作按 `retryable` 标记显隐 |
| **CORE-SFTP-001** 旧连接 host-key mismatch 可击穿新会话 | ✅ 已修 | `HostKeyMismatch` 携带权威 `RemoteEventScope`；物理握手返回无 scope 的 `HostKeyMismatchDetails`；runtime 为每个逻辑等待者各发一个 scoped 事件；core 陈旧事件守卫拒绝旧 epoch |
| **APP-EDIT-001** 远端并发修改可能被本地保存静默覆盖 | ✅ 已修 | 新增 `EditPhase::CheckingRemote` + `EditCheckId`；上传前实时 `symlink_metadata` 比对而非 UI 缓存快照；check 结果由 `AppEventCoordinator` 单一持有 |

已知文档化限制（有意保留，非缺陷）：`(size, mtime)` 快照比对无法覆盖「同秒同大小」的并发修改；byte-freeze 已评估并否决（会推送过期的本地字节，属数据回退）。

## 3. 工程验证现状（2026-08-01 本机实测）

**关键变化：Metal 编译器已可用**，完整 `scripts/check.sh` 首次在本机全绿（07-28 审计时 app/ui 测试仍被缺少 Xcode metal 阻断）。

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | ✅ |
| `scripts/check_architecture.sh` | ✅ |
| `scripts/check_sensitive_logs.sh` | ✅ |
| `cargo test --workspace` | ✅ **467 passed / 0 failed / 1 ignored** |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ |

测试分布（对比 07-14 / 07-28 快照）：

| 目标 | 数量 | 说明 |
| --- | --- | --- |
| macsftp-app（二进制） | **199** | 07-14 为 132；含远程编辑、传输终态、host-key scope、滚动条、cancel-all/clear-records 的回归 |
| macsftp-core | **64** | 含 3 个 `transfer_plan_*` 终态测试 |
| macsftp-sftp（unit） | **83** | 含 `handoff_*`、`local_upload_failure/cancellation_after_progress_*` |
| macsftp-sftp real_session | **26** | 真实 sshd 集成（07-28 为 21）；含 pooled mismatch 双等待者、remote_download 失败-after-progress |
| macsftp-sftp password_auth | 1 | |
| macsftp-storage | 45（1 ignored） | ignored 为 Keychain 门禁测试，显式运行通过 |
| macsftp-platform | 17 | |
| macsftp-ui | 30 | 含滚动条 token 与行为测试 |
| macsftp-test-support | 2 | |

CI（`.github/workflows/ci.yml`）在 `macos-15` 上以 `MACSFTP_REQUIRE_SSHD=1` 跑同一门禁 + 构建 bundle + plist 校验 + 本地网络隐私声明校验。

## 4. 07-28 审计遗留台账

| 编号 | 严重度 | 状态 |
| --- | --- | --- |
| SFTP-TRANSFER-001 / CORE-SFTP-001 / APP-EDIT-001 | 高 | ✅ 已修（见 §2） |
| **TEST-001** 固定共享临时路径并发冲突 | 中 | ❌ 未修：`crates/platform/src/platform.rs:376`（`macsftp_read_local_test`）、`crates/app/src/workspace/tests.rs:307`（`macsftp_sel_test`）仍是固定路径；`transfer_planner.rs` 等仅用 PID 的路径也未按「test label + 唯一标识」加固 |
| **APP-EDIT-002** 编辑临时目录创建错误被忽略且在 UI 同步路径 | 低 | ❌ 未修：`remote_edit.rs` 的 `create_dir_all` 仍 `let _ =` 丢弃 |
| **APP-EDIT-003** 编辑临时目录清理失败静默 | 低 | ❌ 未修：多个 `let _ = remove_dir_all` 清理点无日志/无 residual 记录 |
| **SFTP-MOCK-001** mock actor 静默丢弃终态事件 | 低 | ❌ 未修：`mock_actor.rs` 仍有 6 处 `let _ =` send |

> 审计还列出的「交互式 GUI 手测未签字」「RemoteSnapshot 同秒同大小限制」属已接受的验证/设计限制，见 §5。

## 5. 已知缺口与风险

1. **交互式 GUI 手测仍未签字**：`docs/release-evidence/v0.1.0.md` 状态仍为 IN PROGRESS，多窗口关闭顺序、10k 条目、四路并发 transfer、720×480/Retina/双主题/键盘/VoiceOver 等 `Interactive` 项未勾选；M7 矩阵 `NEEDS-INTERACTIVE` 项未关闭。
2. **版本/发布未定**：无 git tag，CHANGELOG 全部内容都在 `[Unreleased]`；未签名 bundle 按发布流程不允许作为公开 release。
3. **远端历史重写，本地未推送**：`origin/master` 是重新发布的快照历史（与本地无共同祖先），本地 master 比 origin/master 多出 07-28 之后的全部修复与功能。差异存在真实产品源码（非仅文档）。推送前需要确认策略（rebase/force-push/重新开 PR），否则后续协作会持续对不上。
4. **审计中低优先级 4 项未修**（§4），其中 TEST-001 违反 AGENTS.md §9 的并发临时路径纪律，属于已知 flaky 来源。
5. 长期已知限制（有意保留）：RSA 私钥认证禁用（RUSTSEC-2023-0071）；传输队列不跨进程恢复；i18n 未做；远程编辑同秒同大小并发修改漏检。

## 6. 建议的下一步（价值 × 成本）

1. **P1 — 交互式 GUI 验收**：按 0.1.0 release evidence 清单实机跑一遍，把 IN PROGRESS 收口为 pass 或具体 bug 单；同时关闭 M7 的 `NEEDS-INTERACTIVE` 项。这是当前唯一没被自动验证覆盖的产品风险。
2. **P1 — 推送/远端同步**：确认本地 master 与 origin/master 的关系（历史重写），选定合并策略并推送，避免分支继续分叉。
3. **P2 — TEST-001**：把两处固定临时路径改为唯一标识（label + pid + 原子序号），顺带审查仅 PID 的路径；这是唯一可确定性复现的 flaky 源。
4. **P2 — 发布准备**：跑 `scripts/test_password_auth.sh`（release evidence 未勾选）、`scripts/check_release.sh`，决定 0.1.0 是否发版及签名策略。
5. **P3 — APP-EDIT-002/003 + SFTP-MOCK-001**：统一编辑临时目录的错误传播与清理可观测性；mock actor 终态事件 send 失败即退出。

## 7. 进度量化（粗估）

| 维度 | 粗估 | 说明 |
| --- | --- | --- |
| UX 提升计划 1–6 + 专项（profile/connect/drawer） | ~100% | 07-14 已收口 |
| 远程编辑 | ~90% | 功能完整 + 两轮 bug 收敛；同秒同大小限制为文档化取舍 |
| 审计高严重度缺陷 | 100% | 3/3 修复并带回归测试 |
| 审计中低优先级 | ~0% | 4/7 未修（§4） |
| 工程门禁 | 100%（本机） | 首次全绿：467 tests + clippy/fmt/arch/sensitive |
| 可发布性 | ~70% | 缺交互式手测签字、password-auth 门禁、版本/tag 决策 |

**Overall：** 产品功能与正确性继续领先于「可发布」：核心风险已被 TDD 修复并持续回归，工程门禁从「本地跑不全」变成「本地全绿」。剩余工作量集中在**验证与发布动作**（GUI 手测、远端同步、发版决策），而非功能开发。

## 8. 关键文档索引

| 文档 | 用途 |
| --- | --- |
| `docs/progress-analysis-2026-07-14.md` | 上次总览（UX 六阶段完成） |
| `docs/plans/2026-07-29-detailed-code-implementation-plan.md` | 三高严重度缺陷修复合同 |
| `docs/plans/2026-07-28-transfer-terminal-events.md` | PR 1 分解 |
| `docs/plans/2026-07-28-host-key-mismatch-scope.md` | PR 2 分解 |
| `docs/plans/2026-07-28-authoritative-remote-edit-check.md` | PR 3 分解 |
| `docs/plans/2026-07-29-scrollbar.md` + `scrollbar-design.md` | 自定义滚动条 |
| `outputs/macsftp-bug-audit-2026-07-28.md` | 审计报告（含遗留 4 项） |
| `docs/release-evidence/v0.1.0.md` | 发布证据（IN PROGRESS） |
| `docs/gpui-russh-plan.md` | 架构主文档 |
