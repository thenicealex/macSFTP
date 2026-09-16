# macSFTP 架构评审 v3

> 评审对象：`docs/gpui-russh-plan.md`（第三版）
> 对比基线：`docs/architecture-review-v2.md`
> 评审立场：深入审查一致性。前两轮指出的架构问题和实现风险均已解决，因此本轮仅审查数据模型不一致和语义不明确的问题。

---

## 0. 评审结论

**v2 指出的 4 个实现风险和 5 个新增语义问题均已明确解决，完成率为 100%。文档状态已经由“架构完整，但仍有实现风险”更新为“实现风险已解决，但仍有一项轻微的数据模型不一致”。**

**可以开始实现。** 本轮仅发现 1 个需要在 M0 类型草案阶段修正的实质问题，以及 4 个预计可在 10 分钟内完成的语义说明。**不建议进行第四轮架构评审**，因为剩余不确定性已经降低至实现阶段的 spike 可以验证的程度，因此继续修改架构文档的边际收益有限。

---

## 1. v2 问题解决情况（逐项）

| v2 编号 | 项 | 已解决 | 位置 |
|---------|-----|------|------|
| 风险 1 | flume try_send vs send | ✅ | 第 285-286 行，明确使用 `try_send`（GPUI 侧）和 `send_async`（Tokio 侧） |
| 风险 2 | Runtime shutdown_timeout | ✅ | 第 327-330 行，明确使用 `shutdown_timeout(3s)`，并统一 reject pending request |
| 风险 3 | AuthFingerprint 未定义 | ✅ | 第 687-706 行，包含完整结构和规则 |
| 风险 4 | russh-sftp 单 channel 并发 | ✅ | 第 723-727 行、spike 表和 M5 验收标准 |
| N1 | StartTransferCommand 结构 | ✅ | 第 364-383 行，包含完整字段和规则 |
| N2 | TransferSession 认证复用 | ✅ | 第 716-721 行，区分 Dedicated 和 Borrow 两条路径 |
| N3 | event drain Send 约束 | ✅ | 第 289 行，规定“只允许 await recv_async” |
| N4 | 无 Docker 测试策略 | ✅ | 第 1570-1576 行和决策第 10 条 |
| N5 | M0 类型草案边界 | ✅ | 第 1637-1638 行和决策第 11 条 |
| 附 | planning 不处理 conflict | ✅ | 第 1041-1043 行 |
| 附 | Plan 与 Job 的 conflict_policy | ✅ | 第 1044-1046 行 |
| 附 | known_hosts 子集以外的 entry | ✅ | 第 837-842 行和决策第 9 条 |
| 附 | DomainError timeout 与 cancel | ✅ | 第 1439-1440 行 |

其中两项设计尤其合理：

- `AuthFingerprint` 使用 `profile_revision`，而不是 secret hash。因此，这项设计既不会泄露 secret，又可以识别配置变化，符合安全建模要求。
- borrow 模式规定“优先创建独立 SFTP channel，而不是复用单个 channel”（第 727 行）。因此，fallback 的可用条件由“单 channel 支持并发”扩展为“同一连接支持多个 channel”，从而减小 russh-sftp 并发 spike 失败的影响。

---

## 2. 本轮新增问题

### V3-1 — `ConnectionProfile` 缺少 `revision` 字段（实质问题，需要在 M0 修正）

`AuthFingerprint` 依赖 `profile_revision: u64`（第 696 行），而且规则明确规定“用户修改 profile 时 `profile_revision` 递增”（第 704 行）。但是，`ConnectionProfile` 结构（第 866-875 行）**没有 `revision` 字段**：

```rust
pub struct ConnectionProfile {
    pub id: ProfileId,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub default_remote_path: Option<RemotePath>,
    // 缺少: pub revision: u64
}
```

因此，文档还需要说明 `profile_revision` 的来源。候选方案有三个：

- **为 `ConnectionProfile` 增加 `pub revision: u64`**（推荐）。storage 在每次写入 profile 时递增该值。
- 由 `ProfilesFile` 维护 `Map<ProfileId, u64>`。但是，这个方案增加了一层不必要的间接关系。
- 根据 profile 内容计算 hash。但是，这个方案违反“不依赖 secret hash”的原则，而且 secret 不在 profile 中。

**建议**：为 `ConnectionProfile` 增加 `pub revision: u64`，并由 storage 层在 `save_profile` 时自动递增。这个问题应在 M0 类型草案阶段修正，否则 `AuthFingerprint` 的 `profile_revision` 字段没有明确来源。

这是本轮唯一需要在开始实现之前修正的实质问题。

### V3-2 — planning 的“失败”和“取消”语义未区分（低至中等）

第 1048 行规定：“planning 失败时保留已发现 child job，但默认暂停并提示用户是否继续部分传输”。第 1185 行的取消语义规定：“Planning：取消 planning task”。

但是，这两项规定对“planning 中途停止后如何处理已发现的 child job”没有形成一致定义：

- **planning 因错误而失败**（例如远端目录 stat 权限不足）时，保留 child job 并提示用户是否执行部分传输。这项行为合理。
- **planning 被用户取消**时，文档仅规定取消 planning task，却没有说明保留还是丢弃已发现的 child job。

如果用户取消 planning 表示不再执行传输，那么保留 child job 会增加无效状态。如果用户因为 planning 时间过长而选择其他目录，那么保留这些 child job 也没有实际用途。

**建议明确规定**：用户取消 planning 时，整个 plan 进入 `Cancelled`，并丢弃全部 child job；planning 因错误而中断时，plan 进入 `Failed`，保留并暂停已发现的 child job，同时提示用户是否执行部分传输。因此，两种停止原因应对应两种不同语义。

### V3-3 — borrow 转为 dedicated 的条件表述不明确（低）

第 721 行规定：borrow tab 已关闭或 epoch 不匹配时，borrow transfer 立即失败或改用 dedicated。

但是，“立即失败或改用 dedicated”没有说明两种结果各自适用的条件。建议始终优先使用 dedicated，因为 dedicated 不依赖 tab；只有 dedicated 也不可用时，例如 credential 读取失败，transfer 才应进入 `Failed`。

**建议改为**：“borrow tab 已关闭或 epoch 不匹配时，改用 dedicated；dedicated 也不可用时才进入 `Failed`。”

### V3-4 — TransferProgress 节流位置未明确（低）

第 311 行规定：“TransferProgress 必须节流，例如每个 transfer 最多以 10 Hz 的频率进入 UI”。但是，文档没有说明执行节流的位置。

候选位置有三个：

- TransferManager 发送 event 之前，即在源头节流。**推荐此方案。**
- event drain task 接收 event 之后。
- AppModel update 之前。

如果不在源头节流，progress event 可能占用 event channel（容量为 1024）的大部分空间，并影响状态转换 event 的传递。第 310 行规定“event channel 满时 progress event 可以合并”，但是 drain 侧合并仅属于事后保护，不能替代源头节流。

**建议明确规定**：TransferManager 在发送 event 前执行源头节流，drain 侧合并仅用于异常流量下的保护。

### V3-5 — dedicated session 的引用计数未说明（低）

多个 transfer 复用同一个 dedicated session 时，第一个 transfer 完成后不能关闭 session，因为其他 transfer 可能仍在执行。因此，TransferManager 需要维护引用计数。

文档第 656 行列出了“transfer session lifecycle”职责，但是没有说明相应机制。**建议增加一项规定**：dedicated session 使用引用计数，并在最后一个 transfer 完成或取消时关闭 session。这属于实现细节，但是在 M5 设计阶段明确该要求，可以避免 session 泄漏或提前关闭。

---

## 3. 剩余待确认问题评估

第 23 节仅剩 2 个待确认事项：

1. **UI 默认语言**：该事项不影响架构。建议使用中文，因为作者位于深圳，而且目标用户可能以中文用户为主；但是，error code 体系已经支持后续切换。因此，可以在 M1 阶段确定。
2. **metadata warning 显示位置**：建议在 transfer drawer 中默认显示 warning 图标，并在 details 展开后显示完整信息。该事项同样不影响架构。

这两项均属于产品决策，因此无需延迟 M0。

---

## 4. 三轮评审的变化

| 轮次 | 状态 | 主要发现 | 解决率 |
|------|------|----------|--------|
| v1 | 架构存在问题 | C1-C7 七项关键关切和 C8-C15 八项次要关切 | — |
| v2 | 架构完整，但仍有实现风险 | 4 个实现风险和 5 个新增语义问题 | v1 问题解决率 100% |
| v3 | 实现风险已解决，但仍有轻微的数据模型不一致 | 1 个实质问题和 4 个语义说明问题 | v2 问题解决率 100% |

每轮发现的问题均比上一轮严重程度更低、数量更少。v1 关注系统能否正确实现，v2 关注实现阶段的具体风险，v3 关注类型定义是否完整。因此，文档已经达到可以指导实现的程度。

---

## 5. 最终判断

**修正 V3-1，即为 ConnectionProfile 增加 revision；其余 4 项属于预计可在 10 分钟内完成的文档修订。完成这些修改后即可进入 M0。**

不建议进行第四轮架构评审。剩余风险均可由实现阶段的 spike 验证：

- GPUI list API（M1 前）
- flume recv on GPUI executor（M2a 前）
- shutdown_timeout 行为（M2a 前）
- ssh-key known_hosts 能力（M3 前）
- check_server_key 签名（M3 前）
- russh-sftp 并发（M5 前）

这 6 个 spike 的结果会决定具体实现细节，但是不会改变架构。因此，架构已经稳定。

**最终结论**：从架构评审角度看，这份规划已经完成三轮严格评审，所有架构级风险均已识别，并且已有相应的缓解策略。继续修改架构文档的边际收益已经低于直接开始实现的边际收益，因此可以开始实现。
