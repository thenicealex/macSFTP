# Profiles 审计修复周期（2026-08-22 完成记录）

## 背景

针对 Profiles 功能的严格审计确认了 6 个可复现缺陷，其中 4 个属于高危缺陷；审计同时发现了多类一致性风险。
原始审计对话没有存档，因此本周期依据现有代码历史重新确定批次定义，并由维护者逐批确认。每个批次只创建一个 commit，而且先编写能够复现问题的失败回归测试，再修改实现。本文长期记录该修复周期的结果。

## 批次与提交

| 批次 | 主题 | Commit |
| --- | --- | --- |
| A | 存储正确性基础：区分原子写入各阶段的错误（NotCommitted / ReplacedButNotDurable），并实现版本校验、schema 校验、ID 高水位、tmp 文件清理修正和 advisory flock 写事务 | `251a9bd` |
| D | 远程编辑身份：编辑会话去重键由 `(profile_id, path)` 修改为 `(ConnectionKey, path)`，因此不再使用 ProfileId(0) 表示手工编辑身份 | `c476909` |
| B | passphrase 三态：使用 `has_passphrase` 与 `passphrase_ref` 模型替代 `remember_passphrase` 布尔值，并完成 profiles.json v2→v3 迁移和编辑器策略切换 | `236564f` |
| C1 | 删除 profile 时解除引用：recents 条目与 live tab 不再保留已经失效的 profile_id，因此不会产生重复条目，也不会通过会话快照传播该引用 | `e5cdf08` |
| C2 | 跨实例写入序列化：recents upsert/forget 与 config setter 使用锁内重新加载事务；同时，`reconcile_next_id` 防止 id 冲突 | `0bd731e` |
| E | residual 使用写事务（add_and_save / remove_and_save），并且 recents/session/residual 的版本分类统一为 `UnsupportedVersion` | `a370b0a` |
| F | 本周期完成记录，以及 remote_edit.rs 中过时注释的修正 | 本文 |

## 验证状态

- 每个批次交付前，相关 crate 的测试均已通过，`cargo clippy --all-targets` 没有警告，`cargo fmt --check`、`scripts/check_architecture.sh` 和 `scripts/check_sensitive_logs.sh` 也均已通过。
- 2026-08-22，完整的 `scripts/check.sh` 已通过，其中包括 fmt、架构检查、敏感日志检查、workspace 全部测试和 clippy `-D warnings`。

## 有意不做（决策记录）

- **session.json 跨实例合并：** 当前写入已经使用原子 rename，因此文件内容不会处于部分写入状态。但是，避免跨实例更新丢失需要定义合并语义，其中还包括 WindowSessionId 分配冲突。该问题属于产品决策，单独增加锁无法解决，因此本周期不处理。
- **传输路由不按照 profile 区分：** `StartTransferCommand.profile_id` 只表示元数据，而 runtime 使用 `(tab_id, session_epoch)` 作为路由键。因此，manual 连接使用 `ProfileId(0)` 不会导致不同传输使用同一路由。
- **Keychain 无引用 secret 不自动删除**（2026-08-22 决定）：该情况很少发生，而且 OS 后端的 `set_generic_password` 对同一个 ref 执行 upsert。最常见的无引用 secret 来源是 profiles.json 丢失后重新创建相同 id 的 profile，但是新 secret 写入时会覆盖原有值。因此，自动删除的收益和风险并不对称：错误删除的密码没有副本可供恢复，而且删除方案还必须处理 RecoveryRequired 状态下禁止扫描等边界。由于问题出现概率很低，所以本周期不增加不可逆删除流程；如果未来出现真实用户报告，那么再重新评估该决策。
- **删除 `last_local_path` 字段，而不实现缺失功能**（2026-08-22 决定，`fe2fd41`）：plan §22-7 描述的“按照 profile 记忆上次本地目录”从未完成实现，而跨会话持久化已经由 session 快照按照 tab 提供。实现该功能之前需要定义 revision 豁免策略，因为自动记录路径会改变 AuthFingerprint 的池身份。但是，该功能的边际价值低于相关实现复杂度，因此已经删除未使用字段。serde 仍然保持双向兼容，所以无需升级版本。

## 遗留开放项

1. **交互式 GUI smoke 确认：** 本周期的全部 UI 修改，包括 passphrase 策略切换，目前只通过 gpui::test 验证 dispatch 路径。因此，还需要执行一次人工交互式冒烟测试，并将结果归档到 `docs/release-evidence/`。这是当前唯一的开放项。

### 其他已知不一致（低优先级）

- session 文件的 version 0 仍然按照 v1 legacy 解析，但是 recents/profiles 会拒绝 version 0。当前没有实际写入方会生成该版本，因此暂时保留这一兼容行为。
