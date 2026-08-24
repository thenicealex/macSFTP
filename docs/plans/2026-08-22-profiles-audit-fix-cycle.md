# Profiles 审计修复周期（2026-08-22 收口）

## 背景

针对 Profiles 功能的严格审计发现 6 个可复现缺陷（4 个高危）与多类一致性风险。
原始审计对话未存档，本周期按「每批一个 commit、先写失败回归测试再改实现」推进；
批次定义经代码考古重建并逐批由维护者确认。本文是该周期的持久档案。

## 批次与提交

| 批次 | 主题 | Commit |
| --- | --- | --- |
| A | 存储正确性地基：原子写分阶段错误（NotCommitted / ReplacedButNotDurable）、版本校验、schema 校验、ID 高水位、tmp 清理修正、advisory flock 写事务 | `251a9bd` |
| D | 远程编辑身份：编辑会话去重键从 `(profile_id, path)` 改为 `(ConnectionKey, path)`，消灭 ProfileId(0) 手工编辑身份 | `c476909` |
| B | passphrase 三态：`has_passphrase` + `passphrase_ref` 模型取代 `remember_passphrase` 布尔；profiles.json v2→v3 迁移；编辑器策略切换 | `236564f` |
| C1 | 删除 profile 的引用解耦：recents 条目与 live tab 不再保留死 profile_id，避免重复条目与会话快照传播 | `e5cdf08` |
| C2 | 跨实例写序列化：recents upsert/forget 与 config setter 改为锁内重载事务；`reconcile_next_id` 防 id 撞车 | `0bd731e` |
| E | residual 写事务化（add_and_save / remove_and_save）+ recents/session/residual 版本分类对齐 `UnsupportedVersion` | `a370b0a` |
| F | 本收口文档 + remote_edit.rs 过时注释修正 | 本文 |

## 验证状态

- 每批交付前：相关 crate 测试全绿、`cargo clippy --all-targets` 零警告、
  `cargo fmt --check` 干净、`scripts/check_architecture.sh` 与
  `scripts/check_sensitive_logs.sh` 通过。
- 2026-08-22：完整 `scripts/check.sh` 全绿（fmt、架构门禁、敏感日志门禁、
  workspace 全部测试、clippy `-D warnings`）。

## 有意不做（决策记录）

- **session.json 跨实例合并**：写入已是原子 rename，不会撕裂；跨实例丢更新
  需要合并语义（含 WindowSessionId 分配冲突），属产品决策而非锁能解决。
- **传输路由不按 profile**：`StartTransferCommand.profile_id` 只是元数据，
  runtime 按 `(tab_id, session_epoch)` 键控；manual 连接的 `ProfileId(0)`
  无混流风险。
- **Keychain 孤儿 secret 不做自动清扫**（2026-08-22 决定）：触发条件罕见，
  且 OS 后端 `set_generic_password` 对同 ref 是 upsert——最常见的孤儿来源
  （profiles.json 丢失后重建同 id profile）会在新 secret 写入时自然覆盖。
  反向代价不对称：误删的密码无副本可恢复；清扫设计必须处理
  RecoveryRequired 时禁止扫描等边界，为一个几乎不触发的问题引入不可逆
  删除路径不值得。若未来出现真实用户报告再重新评估。
- **`last_local_path` 字段移除而非补全**（2026-08-22 决定，`fe2fd41`）：
  plan §22-7 的「按 profile 记住上次本地目录」从未接线；跨会话持久化已由
  session 快照按 tab 覆盖。实现需先解决 revision 豁免策略（自动记录路径会
  搅动 AuthFingerprint 的池身份），边际价值不足以支撑；死字段已删除，
  serde 双向兼容无需版本升级。

## 遗留开放项

1. **交互式 GUI smoke 签字**：本周期全部 UI 改动（passphrase 策略切换等）
   仅由 gpui::test 驱动 dispatch 路径验证；需要一次人工交互式冒烟并归档到
   `docs/release-evidence/`。这是当前唯一的开放项。

### 其他已知不一致（低优先级）

- session 文件 version 0 仍按 v1 legacy 解析（recents/profiles 拒绝 0）；
  无真实写入方，暂保持宽容。
