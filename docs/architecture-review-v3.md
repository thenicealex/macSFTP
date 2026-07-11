# macSFTP 架构评审 v3

> 评审对象：`docs/gpui-russh-plan.md`（第三版）
> 对比基线：`docs/architecture-review-v2.md`
> 评审立场：深层一致性审视。前两轮的架构缺口和实现陷阱已闭合，本轮只挖数据模型不一致与语义模糊。

---

## 0. 评审结论

**v2 的 4 个实现陷阱 + 5 个新模糊全部显式闭合，落实率 100%。文档已从"架构闭合，剩实现陷阱"升级到"实现陷阱闭合，剩一个数据模型小不一致"。**

**可以开工。** 本轮只发现 1 个需要在 M0 类型草案阶段修正的真洞，和 4 个 10 分钟级的语义澄清。**不建议再来第四轮架构评审**——剩余不确定性已经降到实现期 spike 能覆盖的水平，继续在文档层迭代是 diminishing returns。

---

## 1. v2 闭合确认（逐条）

| v2 编号 | 项 | 闭合 | 位置 |
|---------|-----|------|------|
| 陷阱1 | flume try_send vs send | ✅ | 第 285-286 行，显式 `try_send`（GPUI 侧）+ `send_async`（Tokio 侧） |
| 陷阱2 | Runtime shutdown_timeout | ✅ | 第 327-330 行，显式 `shutdown_timeout(3s)` + pending request 统一 reject |
| 陷阱3 | AuthFingerprint 未定义 | ✅ | 第 687-706 行，完整结构 + 规则 |
| 陷阱4 | russh-sftp 单 channel 并发 | ✅ | 第 723-727 行 + spike 表 + M5 验收 |
| N1 | StartTransferCommand 结构 | ✅ | 第 364-383 行，完整字段 + 规则 |
| N2 | TransferSession 认证复用 | ✅ | 第 716-721 行，Dedicated/Borrow 路径分离 |
| N3 | event drain Send 约束 | ✅ | 第 289 行，"只允许 await recv_async" |
| N4 | 无 Docker 测试策略 | ✅ | 第 1570-1576 行 + 决策第 10 条 |
| N5 | M0 类型草案边界 | ✅ | 第 1637-1638 行 + 决策第 11 条 |
| 附 | planning 不做 conflict | ✅ | 第 1041-1043 行 |
| 附 | Plan vs Job conflict_policy | ✅ | 第 1044-1046 行 |
| 附 | known_hosts 子集外 entry | ✅ | 第 837-842 行 + 决策第 9 条 |
| 附 | DomainError timeout vs cancel | ✅ | 第 1439-1440 行 |

**特别肯定两点**：
- `AuthFingerprint` 用 `profile_revision` 而非 secret hash——既不泄露 secret，又能感知配置变化。这是正确的安全建模。
- borrow 模式新增"优先开独立 SFTP channel 而非复用单个 channel"（第 727 行）——这把 fallback 的可用性从"依赖单 channel 并发"拓宽到"依赖同连接多 channel"，spike 失败的后果被降低了。

---

## 2. 本轮新发现

### V3-1 — `ConnectionProfile` 缺少 `revision` 字段（真洞，需 M0 修正）

`AuthFingerprint` 依赖 `profile_revision: u64`（第 696 行），规则明确"用户修改 profile 时 `profile_revision` 递增"（第 704 行）。但 `ConnectionProfile` 结构（第 866-875 行）**没有 `revision` 字段**：

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

`profile_revision` 从哪来？三个候选：
- **`ConnectionProfile` 加 `pub revision: u64`**（推荐）—— storage 在每次 profile 写入时递增。
- `ProfilesFile` 维护 `Map<ProfileId, u64>` —— 多一层间接，不必要。
- 从 profile 内容 hash 推导 —— 违背"不依赖 secret hash"原则，且 secret 不在 profile 里。

**建议**：`ConnectionProfile` 加 `pub revision: u64`，storage 层在 `save_profile` 时自动递增。这是 M0 类型草案就要修的——否则 `AuthFingerprint` 的 `profile_revision` 字段无来源。

这是本轮唯一需要在开工前修的真洞。

### V3-2 — planning"失败"与"取消"的语义未区分（低-中）

第 1048 行："planning 失败时保留已发现 child job，但默认暂停并提示用户是否继续部分传输"。
第 1185 行取消语义："Planning：取消 planning task"。

这两条对"planning 中途停止后已发现 child job 的处理"不一致：
- **planning 因错误失败**（如远端目录 stat 权限不足）→ 保留 child job，提示部分传输。合理。
- **planning 被用户取消** → 取消 planning task。已发现 child job 保留还是丢弃？文档没说。

如果用户取消 planning 是"我不想传了"，保留 child job 反而是负担。如果用户取消是"planning 太慢我想换个目录"，保留也没意义。

**建议明确**：用户取消 planning = 整个 plan 进入 `Cancelled`，child job 全部丢弃；planning 因错误中断 = plan 进入 `Failed`，已发现 child job 保留并暂停，提示部分传输。两种停止，两种语义。

### V3-3 — borrow 回退 dedicated 的语义用"或"模糊（低）

第 721 行："如果 borrow tab 已关闭或 epoch 不匹配，borrow transfer 立即失败或回退到 dedicated"。

"立即失败**或**回退到 dedicated"——什么时候失败，什么时候回退？应该总是优先回退 dedicated（因为 dedicated 不依赖 tab），只有 dedicated 也不可行（如 credential 读取失败）才 failed。

**建议改为**："borrow tab 已关闭或 epoch 不匹配时，回退到 dedicated；dedicated 也不可行时才进入 failed。"

### V3-4 — TransferProgress 节流的实现位置未显式（低）

第 311 行："TransferProgress 必须节流，例如每个 transfer 最多 10 Hz 进入 UI"。但节流在哪做？

三个候选位置：
- TransferManager 发 event 前（源头节流）—— **推荐**
- event drain task 收到后节流
- AppModel update 前节流

如果不在源头节流，progress event 会塞满 event channel（容量 1024），挤掉状态转换 event。第 310 行"event channel 满时 progress event 可以合并"是 drain 侧兜底，但那是 reactive 不是 proactive。

**建议显式**：节流在 TransferManager 发 event 前做（源头），drain 侧合并是兜底。

### V3-5 — dedicated session 的引用计数未说明（低）

多个 transfer 复用同一 dedicated session 时，第 1 个完成后 session 不能关（还有其他 transfer 在跑）。TransferManager 需要引用计数。

文档第 656 行列了"transfer session lifecycle"职责，但没说机制。**建议一行**：dedicated session 用引用计数，最后一个 transfer 完成或取消时关闭 session。这是实现细节，但 M5 设计时明确能避免 session 泄漏或过早关闭。

---

## 3. 剩余待确认问题评估

第 23 节只剩 2 个待确认：

1. **UI 默认语言**：不阻塞架构。建议中文（作者在深圳，目标用户大概率中文），但 error code 体系已支持后续切换。M1 定即可。
2. **metadata warning 显示位置**：建议 transfer drawer 默认显示 warning 图标，details 展开看完整信息。不阻塞。

这两个都是产品决策，不是架构决策。可以不阻塞地进 M0。

---

## 4. 三轮评审演化

| 轮次 | 状态 | 主要发现 | 闭合率 |
|------|------|----------|--------|
| v1 | 架构有缺口 | C1-C7 七个关键关切 + C8-C15 八个次要 | — |
| v2 | 架构闭合，剩实现陷阱 | 4 个实现陷阱 + 5 个新模糊 | v1 闭合 100% |
| v3 | 实现陷阱闭合，剩数据模型小不一致 | 1 个真洞 + 4 个语义澄清 | v2 闭合 100% |

**演化曲线**：每轮发现的问题严重度递减，数量递减。v1 是"会不会失败"，v2 是"实现会不会踩坑"，v3 是"类型定义完不完整"。这表明文档已经收敛到可以支撑实现的水平。

---

## 5. 最终判断

**修掉 V3-1（ConnectionProfile 加 revision），其余 4 项是 10 分钟级文档修订。然后进 M0。**

不建议第四轮架构评审。剩余风险全部落在实现期 spike 能覆盖的范围内：
- GPUI list API（M1 前）
- flume recv on GPUI executor（M2a 前）
- shutdown_timeout 行为（M2a 前）
- ssh-key known_hosts 能力（M3 前）
- check_server_key 签名（M3 前）
- russh-sftp 并发（M5 前）

这 6 个 spike 的结果决定具体实现细节，但不改变架构。架构层已经稳定。

**一句话**：从架构师角度，这份规划已经过三轮严格评审，所有架构级风险已识别并有缓解策略。继续在文档层迭代的边际收益已经低于直接写代码的边际收益。开工。
