# macSFTP 架构评审 v2

> 评审对象：`docs/gpui-russh-plan.md`（修订版，含 ADR-002/003/004）
> 对比基线：`docs/archive/architecture-review.md`（v1 评审）
> 评审立场：闭合质量审视 + 实现期风险预警。文档改得好不等于没有洞。

---

## 0. 评审结论

**v1 提出的 7 个关键关切（C1-C7）和 8 个次要关切（C8-C15）全部显式闭合。三个 ADR 质量高于"补丁式回应"，是真正的架构决策，不是橡皮图章。文档已从"有架构缺口"升级到"架构闭合，待实现验证"。**

**可以进入 M0。** 剩余问题分两类：
- **闭合但有实现陷阱**（4 项）——ADR 方向对，但实现时会踩具体坑，需提前标注。
- **新发现的小模糊**（5 项）——修订引入或仍遗留的数据模型/语义模糊，不阻塞 M0 但应在 M0-M2 间消除。

下面逐条审视。**只写有洞的，不重复已闭合的。**

---

## 1. 闭合质量审视（关键关切）

### C1 双运行时桥接 — 闭合质量：高，但有两处实现陷阱

ADR-002 选 `flume` 是关键正确决策。`flume` 不要求接收端在 Tokio 上，所以 event drain task 能在 GPUI executor 上跑——这是整条桥接能成立的前提。`tokio::sync::mpsc` 的 `Receiver` 只能在 Tokio runtime 上 recv，用它就死路一条。作者踩到了这个点。

**陷阱 1 — `try_send` vs `send` 的选择未显式。**
文档说"command channel 满时，用户动作类 command 返回错误并在 UI 显示轻量 warning"。这要求 GPUI 侧用 `flume::Sender::try_send`。但 `try_send` 在 flume 里返回 `TrySendError::Full`，而 `send` 是阻塞 async。如果实现者误用 `send`，GPUI 主线程会在 channel 满时挂住——这正是 ADR 想避免的。**建议在 ADR-002 里显式写明：GPUI 侧发 command 必须用 `try_send`，Tokio 侧发 event 可以用 `send`（让背压传导到 actor）。**

**陷阱 2 — `Runtime` drop 的阻塞风险。**
文档说 `RuntimeController` 拥有 `tokio::runtime::Runtime`，drop 时"bounded shutdown"。但 `tokio::runtime::Runtime::drop` 会**阻塞**当前线程直到所有 task 结束。如果某个 task 卡住（比如 host key actor 正 await oneshot，用户没点 modal），`Runtime::drop` 会无限等。文档的 shutdown 流程是先发 `Shutdown` command——但 Drop 时不保证 command 已被处理（Drop 可能发生在 AppModel 释放时，此时 runtime task 可能已停）。

**正确做法**：不直接 `drop` Runtime，而是显式调用 `runtime.shutdown_timeout(Duration::from_secs(3))`，超时后强制释放。文档说"bounded shutdown"但没点名 `shutdown_timeout`。**建议 ADR-002 显式写：drop 时调用 `shutdown_timeout`，不依赖 `Runtime::drop` 的默认行为。**

### C2 host key 回调 — 闭合质量：高，无陷阱

ADR-004 结构完整：oneshot + TrustRegistry + 5 分钟超时 + epoch 绑定 + tab close 自动 reject + transfer session 复用 trust。逻辑闭环。

唯一可以挑的：timeout 后 actor 返回 false，handshake 失败，用户看到的是"连接失败"还是"超时未响应"？`UserFacingError` 应区分 `UserCancelled` vs `TrustRequestTimeout`。这是 error taxonomy 细节，不阻塞，但 M3 实现时要补。

### C3 session 分离 — 闭合质量：高，但 fallback 有两个未定义点

ADR-003 + `TransferSessionMode`（Dedicated / BorrowBrowsingSession）+ fallback 机制是对的。显式承认取舍 + 给降级路径，这是架构师该做的。

**未定义点 1 — `AuthFingerprint` 没给定义。**
`TransferSessionKey` 用了 `auth_fingerprint: AuthFingerprint`，但这个类型没定义。它应该是什么？`(AuthMethodType, Option<KeyHash>)`？还是 secret_ref 的 hash？如果用户改了密码，fingerprint 变不变？这影响 TransferManager 的 session 复用判断——同一个 host+user 但不同密码，应该建新 session 还是复用旧 session？**建议在 ADR-003 补 `AuthFingerprint` 的语义定义。**

**未定义点 2 — fallback 到 borrow 模式时 russh-sftp 的并发能力未验证。**
borrow 模式下，transfer 和 browsing 共享一条 SSH 连接。SFTP 协议允许 pipeline（多个 in-flight request），但 **russh-sftp 的实现是否支持单 channel 上的并发请求？** 如果它是严格串行（一个 request 等响应才发下一个），borrow 模式下大文件传输会完全卡住目录浏览——不是"变慢"而是"卡死"。文档说"大文件传输期间目录浏览可能变慢"，这假设了并发。**这是 M5 的实现期 spike，但建议在风险清单里把"russh-sftp 单 channel 并发能力"列为 borrow 模式的前置验证项。** 如果不支持并发，borrow 模式只能用于"无浏览需求时"——那 fallback 价值大减。

### C4 陈旧事件防护 — 闭合质量：高，无陷阱

`session_epoch` 放进每个 `ConnectionState` variant、`RemoteEventScope` 校验逻辑、4 条测试用例。这是教科书级闭合。`TransferEvent` 不依赖 tab 存活这点也对——transfer event 走 TransferStore，不走 tab 的 epoch 校验。无保留意见。

### C5 递归传输规划 — 闭合质量：中高，三处语义模糊

`TransferPlan` 数据结构对了，流式 planning 对了，apply_to_all 绑 plan 对了。但有三处模糊：

**模糊 1 — planning 阶段是否做 conflict 检测？**
`TransferPlanState::Planning` 和递归 planning 状态机说"emit child jobs in batches" + "conflict policy may pause individual child job"。这暗示 conflict 检测在 job 执行时，不是 planning 时。但文档没显式说。如果实现者误以为 planning 要做 conflict 检测，会对 10k 文件做 10k 次 stat destination——慢到不可用。**建议显式写：planning 阶段只遍历 source + 统计 size，不做 destination stat；conflict 检测延迟到 child job 执行时。**

**模糊 2 — `TransferPlan.conflict_policy` 和 `TransferJob.conflict_policy` 的关系。**
两者都有 `conflict_policy`。plan 级是默认值？job 级可覆盖？apply_to_all 改的是哪个？文档没说。**建议明确：plan 级 conflict_policy 是默认；apply_to_all 后更新 plan 级 policy，后续 child job 继承；job 级 conflict_policy 只在单文件 plan 时等于 plan 级。** 或者干脆只保留 plan 级，job 不存——更简单。

**模糊 3 — planning 失败后"保留已发现 child job"的恢复语义。**
文档说"planning 失败时保留已发现 child job，但默认暂停并提示用户是否继续部分传输"。但 planning 失败可能是中途权限不足（读不到某个子目录）。保留已发现的 job 是对的，但"部分传输"的 UI 呈现——用户怎么知道哪些传了哪些没传？这影响 M5 UI 设计，不阻塞架构，但要早想。

### C6 known_hosts 子集 — 闭合质量：高，但有一个待确认问题被错位

子集边界清晰，"优先用 ssh-key crate，禁止自己写完整 parser"是对的。但第 23 节 Q2"known_hosts MVP 子集之外的 entry 是忽略、warning 还是阻断？"——**这不是"待确认"，这是必须在 M0 决定的架构项。**

原因：用户的 `~/.ssh/known_hosts` 里有 hashed host 是常态（OpenSSH 默认对已知 host 不 hash，但很多用户手动 hash 或用 `HashKnownHosts yes`）。如果 MVP 遇到 hashed host 行就阻断解析，整个 known_hosts 文件读不进来，所有 host 都变 Unknown——每次连接都弹窗。如果忽略该行，该 host 永远 Unknown——每次连接弹窗，但其他 host 正常。

**建议明确决策：忽略无法解析的行 + 写 WARN log，不阻断整个文件解析。** 这是唯一兼顾安全和可用性的选择。应该从"待确认"移到"已决策"。

### C7 取消清理 — 闭合质量：高，无陷阱

`CancellationToken` + `ResidualTempFile` 记录 + 下次连接只清理记录过的（不扫描全目录）+ 冲突检测排除 `.macsftp-part` + retry 优先清理旧 temp。完整。无保留意见。

---

## 2. 次要关切闭合确认

| v1 编号 | 项 | 闭合 | 说明 |
|---------|-----|------|------|
| C8 | Keychain crate | ✅ | `security-framework` + `zeroize`，第 826-829 行 |
| C9 | 单窗口 | ✅ | 决策摘要 + 非目标 + UI 规划三处一致 |
| C10 | Accessibility | ✅ | 非目标 + 独立节，"不主动破坏基础可用性"边界清晰 |
| C11 | 日志策略 | ✅ | tracing 生态 + 分级 + redaction 规则完整 |
| C12 | keyboard-interactive | ✅ | `AuthFlow` enum + 明确错误提示，不伪装成密码失败 |
| C13 | 国际化 | ✅ | error code 映射，UI 层负责文案 |
| C14 | 测试容器化 | ✅ | Docker/OpenSSH harness + 无 Docker 时 skip + 提示 |
| C15 | SftpSession API | ⚠️ | 流程图未变，M3 实现期验证。可接受 |

---

## 3. 新发现的小问题

### N1 — `StartTransferCommand` 结构未给出，tab 绑定关系隐含

`AppCommand::StartUpload(StartTransferCommand)` / `StartDownload(StartTransferCommand)` 的 payload 结构没定义。它必须含 `tab_id`（因为 upload/download 绑定发起 tab 的远端 session，fallback 时 borrow 的也是这个 tab）。这是 ADR-003 fallback 机制能成立的前提——"使用当前 tab 的 browsing session"里的"当前 tab"就是 `StartTransferCommand.tab_id`。

**建议在 ADR-003 或 Command 协议节显式给出 `StartTransferCommand` 的字段草案，至少包含 `tab_id`、`source`、`destination`、`metadata_policy`、`conflict_policy`。**

### N2 — `TransferSession` 的认证复用机制未说明

ADR-003 说 host trust 按 `(host, port, fingerprint)` 缓存，transfer session 不重复弹 host key。但 **transfer session 的密码/私钥认证呢？** TransferManager 创建 dedicated session 时，要从哪拿密码？从 profile 的 `SecretRef` 去 Keychain 取。这和浏览 session 是同一条路径。但如果是 borrow 模式，transfer 直接用 browsing session，不需要重新认证。这两条路径的代码分支要在 `TransferSessionMode` 的实现里清晰分开。

文档隐含了这点但没显式。**不阻塞 M0，但 M5 实现时要确保 dedicated 模式走完整认证流程，borrow 模式跳过认证。**

### N3 — event drain task 与 GPUI `Context` 的 `Send` 约束

ADR-002 说 event drain task 在 GPUI executor 上运行，收到 event 后"进入 GPUI update closure，更新 Entity state，再 `cx.notify()`"。GPUI 的 `cx.spawn` 返回的 `Task` 通常不要求 `Send`（因为 GPUI executor 是单线程的），所以可以持有 `cx: Context<AppModel>`。但 `flume` 的 `Receiver::recv()` future 是 `Send` 的——这没问题，`Send` 的 future 可以在非 `Send` 的 task 里 poll。

**唯一要注意**：event drain task 不能 `await` 任何 GPUI 之外的阻塞操作，否则会卡主线程。它只应该 `await` flume recv。这条 ADR-002 没显式写，但"GPUI 主线程不 await 网络"原则隐含了。可接受。

### N4 — 第 23 节 Q3（无 Docker 时测试策略）应升级为决策

"没有 Docker 时本地 integration tests 默认 skip 还是要求用户提供 OpenSSH server"——这影响开发者本地开发体验和 CI。**建议明确：无 Docker 时 skip + 打印一条明确提示，不 fail build。** CI 环境必须提供 Docker。这是工程惯例，不需要"待确认"。

### N5 — M0 验收"ADR 关键数据流已反映到 core 类型草案"的边界

M0 验收说"ADR 中的关键数据流已反映到 `core` 类型草案"。这是好实践——M0 就把 `RemoteEventScope`、`TrustRequest`、`TransferSessionMode`、`TransferPlan` 等类型在 core 里定义出来。

**建议明确"草案"的边界**：是只定义 struct/enum（不实现方法），还是要实现 `impl` 的骨架？建议 M0 只定义类型 + `Default` / `new` 骨架，不实现业务逻辑——否则 M0 工作量膨胀。类型层面的闭合已经能验证 ADR 的内部一致性。

---

## 4. 实现期 spike 清单（不阻塞 M0，但必须在对应里程碑前完成）

| Spike | 里程碑 | 验证项 | 风险 |
|-------|--------|--------|------|
| GPUI `list`/uniform list API | M1 前 | API 存在、签名稳定、10k 行不卡 | 已在文档 |
| `flume` 在 GPUI executor 上 recv | M2a 前 | `cx.spawn` + `flume::recv()` 能编译运行 | 低 |
| `tokio::runtime::shutdown_timeout` 行为 | M2a 前 | 超时后 task 是否被强制取消 | 低 |
| russh-sftp 单 channel 并发请求 | M5 前 | borrow 模式是否可用 | **中高**——决定 fallback 价值 |
| russh `check_server_key` 的 async 签名 | M3 前 | 当前版本是同步还是 async，oneshot await 是否可行 | 中 |
| `ssh-key` crate 的 known_hosts 能力 | M0/M3 | 是否覆盖 MVP 子集，还是仍需小型 parser | 中 |

**最关键的是 russh-sftp 单 channel 并发**——如果它不支持，ADR-003 的 borrow fallback 实质失效，整个 session 分离模型就只剩 dedicated 模式，MaxSessions 风险没有任何缓解。建议把这个 spike 提到 M3 之前，甚至 M2c 就做，因为它影响 fallback 是否值得实现。

---

## 5. 对比 v1 的改进确认

v1 提出的建议落实情况：

| v1 建议 | 落实 | 位置 |
|---------|------|------|
| 补 ADR-002 桥接机制 | ✅ | 第 269-327 行 |
| 补 ADR-003 session 分离 | ✅ | 第 514-549 行 |
| 补 ADR-004 host key 回调 | ✅ | 第 693-733 行 |
| M2 拆为子里程碑 | ✅ | M2a/M2b/M2c |
| 提升 MaxSessions 严重度 + fallback | ✅ | 风险清单 + ADR-003 fallback |
| GPUI list API 作为 M1 前置 spike | ✅ | 第 1311-1316 行 + M1 交付 |
| 显式声明 known_hosts 子集 | ✅ | 第 759-786 行 |
| TransferPlan 提前到 M5 设计 | ✅ | 第 936-961 行 + M5 交付 |
| 决定单窗口 | ✅ | 决策摘要 + 非目标 |
| 测试容器化 | ✅ | test_support + 测试节 |

**落实率 100%。** 这在架构评审里罕见。作者的迭代纪律值得肯定——但作为评审者，我的工作是挑下一层问题，不是发奖状。

---

## 6. 最终判断

**架构层面已闭合，可进入 M0。** 剩余问题都是实现期细节或小范围语义澄清，不影响 crate 边界、运行时模型或里程碑结构。

进入 M0 前建议处理（10 分钟级）：
1. ADR-002 显式写 `try_send`（GPUI 侧）+ `shutdown_timeout`（drop 时）。
2. ADR-003 补 `AuthFingerprint` 语义定义。
3. 把第 23 节 Q2（known_hosts 子集外 entry）从"待确认"移到"已决策：忽略 + WARN"。
4. 把第 23 节 Q3（无 Docker 测试）从"待确认"移到"已决策：skip + 提示"。
5. TransferPlan 节显式写"planning 不做 conflict 检测"。

进入 M5 前必须完成：
- russh-sftp 单 channel 并发能力 spike（决定 borrow fallback 是否值得实现）。

**一句话**：v1 是"架构有缺口"，v2 是"架构闭合，剩实现陷阱"。从架构师角度，这份文档已经可以支撑从 M0 到 M7 的完整开发——前提是实现者读 ADR 时读到位，不是只看类型定义。
