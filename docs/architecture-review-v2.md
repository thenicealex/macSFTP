# macSFTP 架构评审 v2

> 评审对象：`docs/gpui-russh-plan.md`（修订版，含 ADR-002/003/004）
> 对比基线：`docs/archive/architecture-review.md`（v1 评审）
> 评审立场：完整性审视与实现风险预警。文档质量已有改善，但是仍需识别实现风险。

---

## 0. 评审结论

**v1 提出的 7 个关键关切（C1-C7）和 8 个次要关切（C8-C15）均已明确处理。三个 ADR 形成了完整的架构决策，因此不属于局部修订或形式化确认。文档已从“存在架构缺口”转变为“架构完整，仍需实现验证”。**

**可以进入 M0。** 剩余问题分为两类：

- **已有决策但是存在实现风险**（4 项）：ADR 的方向正确，但是实现阶段仍有具体风险，因此需要提前标注。
- **新发现的局部模糊项**（5 项）：修订内容引入或保留了数据模型与语义上的模糊项。这些问题不阻塞 M0，但是应在 M0-M2 期间消除。

下文逐项审视仍需处理的问题，因此不再重复已解决的内容。

---

## 1. 关键关切的处理质量

### C1 双运行时桥接 — 处理质量：高，但是存在两项实现风险

ADR-002 选择 `flume` 是正确的关键决策。`flume` 不要求接收端位于 Tokio 上，因此 event drain task 可以在 GPUI executor 上执行，而这也是整个桥接方案成立的前提。`tokio::sync::mpsc` 的 `Receiver` 只能在 Tokio runtime 上执行 `recv`；如果使用该类型，那么方案将无法满足既定运行时边界。

**风险 1 — 未明确选择 `try_send` 还是 `send`。**

文档说明：“command channel 满时，用户动作类 command 返回错误并在 UI 显示轻量 warning。”因此 GPUI 侧需要使用 `flume::Sender::try_send`。但是 `try_send` 在 flume 中返回 `TrySendError::Full`，而 `send` 会阻塞。如果实现者误用 `send`，那么 GPUI 主线程会在 channel 满时阻塞，这与 ADR 的目标冲突。**建议在 ADR-002 中明确规定：GPUI 侧发送 command 必须使用 `try_send`；Tokio 侧发送 event 可以使用 `send`，因此背压会传递到 actor。**

**风险 2 — `Runtime` drop 可能阻塞。**

文档说明 `RuntimeController` 拥有 `tokio::runtime::Runtime`，并且在 drop 时执行“bounded shutdown”。但是 `tokio::runtime::Runtime::drop` 会阻塞当前线程，直至所有 task 结束。如果某个 task 无法结束，例如 host key actor 正在 `await` oneshot，而用户尚未处理 modal，那么 `Runtime::drop` 可能无限等待。文档规定 shutdown 流程先发送 `Shutdown` command，但是 Drop 发生时无法保证该 command 已经被处理。例如，AppModel 释放时，runtime task 可能已经停止。

**正确做法**：不要直接 `drop` Runtime，而应明确调用 `runtime.shutdown_timeout(Duration::from_secs(3))`，并在超时后强制释放资源。文档使用了“bounded shutdown”，但是没有明确指定 `shutdown_timeout`。**建议 ADR-002 明确规定：drop 时调用 `shutdown_timeout`，并且不得依赖 `Runtime::drop` 的默认行为。**

### C2 host key 回调 — 处理质量：高，无明显风险

ADR-004 的结构完整，包括 oneshot、TrustRegistry、5 分钟超时、epoch 绑定、tab close 自动 reject，以及 transfer session 复用 trust。因此相关逻辑完整。

仍有一个细节需要确认：timeout 后 actor 返回 false，handshake 失败，那么用户看到的结果是“连接失败”还是“超时未响应”？`UserFacingError` 应区分 `UserCancelled` 与 `TrustRequestTimeout`。这是 error taxonomy 的细节，因此不阻塞当前阶段，但是需要在 M3 实现时明确。

### C3 session 分离 — 处理质量：高，但是 fallback 存在两个未定义项

ADR-003、`TransferSessionMode`（Dedicated / BorrowBrowsingSession）和 fallback 机制形成了合理方案。文档明确说明取舍，并且提供降级路径，因此决策依据充分。

**未定义项 1 — `AuthFingerprint` 没有定义。**

`TransferSessionKey` 使用了 `auth_fingerprint: AuthFingerprint`，但是文档没有定义该类型。它是 `(AuthMethodType, Option<KeyHash>)`，还是 secret_ref 的 hash？如果用户修改密码，fingerprint 是否变化？这些问题会影响 TransferManager 的 session 复用判断。如果 host 和 user 相同，但是密码不同，那么系统需要明确是创建新 session，还是复用旧 session。**建议在 ADR-003 中增加 `AuthFingerprint` 的语义定义。**

**未定义项 2 — fallback 使用 borrow 模式时，尚未验证 russh-sftp 的并发能力。**

在 borrow 模式下，transfer 和 browsing 共用一条 SSH 连接。SFTP 协议允许 pipeline，即允许多个 in-flight request。但是，**russh-sftp 的实现是否支持单 channel 上的并发请求？** 如果其实现严格串行，即每个 request 都需要等待响应后才能发送下一个 request，那么大文件传输会使目录浏览长期等待。此时实际表现将比文档所述的“变慢”更严重，因为该表述假定底层支持并发。

**这项内容属于 M5 的实现验证，但是建议在风险清单中将“russh-sftp 单 channel 并发能力”列为 borrow 模式的前置验证项。** 如果不支持并发，那么 borrow 模式只能用于“无需目录浏览的期间”，因此 fallback 的适用范围会明显缩小。

### C4 陈旧事件防护 — 处理质量：高，无明显风险

文档将 `session_epoch` 纳入每个 `ConnectionState` variant，并且定义了 `RemoteEventScope` 校验逻辑和 4 条测试用例。因此这一部分的设计完整。`TransferEvent` 不依赖 tab 存活也符合状态边界，因为 transfer event 进入 TransferStore，而不使用 tab 的 epoch 校验。

### C5 递归传输规划 — 处理质量：中高，但是存在三项语义模糊

`TransferPlan` 数据结构、流式 planning 和绑定到 plan 的 apply_to_all 均符合预期。但是，文档仍存在三项模糊内容。

**模糊项 1 — planning 阶段是否执行 conflict 检测？**

`TransferPlanState::Planning` 和递归 planning 状态机说明“emit child jobs in batches”以及“conflict policy may pause individual child job”。这些说明更接近在 job 执行时进行 conflict 检测，而不是在 planning 阶段进行检测。但是文档没有明确规定。如果实现者认为 planning 需要执行 conflict 检测，那么系统会对 10k 个文件执行 10k 次 destination stat，因此性能无法满足使用要求。**建议明确规定：planning 阶段只遍历 source 并统计 size，不执行 destination stat；conflict 检测延迟到 child job 执行时。**

**模糊项 2 — `TransferPlan.conflict_policy` 与 `TransferJob.conflict_policy` 的关系。**

两种类型都包含 `conflict_policy`。但是，文档没有说明 plan 级字段是否为默认值、job 级字段是否允许覆盖，以及 apply_to_all 修改哪个字段。**建议明确规定：plan 级 conflict_policy 是默认值；apply_to_all 生效后更新 plan 级 policy，后续 child job 继承该值；job 级 conflict_policy 仅在单文件 plan 中与 plan 级字段相同。** 另一种更简单的方案是只保留 plan 级字段，并且不在 job 中存储该字段。

**模糊项 3 — planning 失败后“保留已发现 child job”的恢复语义。**

文档说明：“planning 失败时保留已发现 child job，但默认暂停并提示用户是否继续部分传输。”如果 planning 失败的原因是中途遇到权限不足，例如某个子目录不可读，那么保留已发现的 job 是合理行为。但是文档还需要说明“部分传输”在 UI 中的呈现方式，因此用户才能确认哪些内容已经传输、哪些内容没有传输。该问题会影响 M5 UI 设计，所以虽然不阻塞架构决策，但是应该提前定义。

### C6 known_hosts 子集 — 处理质量：高，但是有一项待确认问题的位置不当

子集边界清晰，并且“优先使用 ssh-key crate，禁止自行实现完整 parser”的决策合理。但是第 23 节 Q2“known_hosts MVP 子集之外的 entry 是忽略、warning 还是阻断？”不应继续属于“待确认”，而应在 M0 阶段形成架构决策。

原因是用户的 `~/.ssh/known_hosts` 中经常包含 hashed host。OpenSSH 默认不对已知 host 使用 hash，但是很多用户会手动使用 hash，或者启用 `HashKnownHosts yes`。如果 MVP 因 hashed host 行而阻断整个文件的解析，那么所有 host 都会成为 Unknown，因此每次连接都会显示确认窗口。如果忽略该行，那么该 host 仍会成为 Unknown，并且每次连接都会显示确认窗口，但是其他 host 仍可正常识别。

**建议明确规定：忽略无法解析的行，并且写入 WARN log，但是不得阻断整个文件的解析。** 该决策同时兼顾安全性和可用性，因此应该从“待确认”调整为“已决策”。

### C7 取消清理 — 处理质量：高，无明显风险

该方案包括 `CancellationToken`、`ResidualTempFile` 记录、下次连接时仅清理已记录的内容且不扫描整个目录、冲突检测排除 `.macsftp-part`，以及 retry 时优先清理旧 temp。因此设计内容完整。

---

## 2. 次要关切处理确认

| v1 编号 | 项 | 处理结果 | 说明 |
|---------|-----|---------|------|
| C8 | Keychain crate | ✅ | `security-framework` + `zeroize`，第 826-829 行 |
| C9 | 单窗口 | ✅ | 决策摘要、非目标和 UI 规划三处一致 |
| C10 | Accessibility | ✅ | 非目标与独立章节中的“不主动破坏基础可用性”边界清晰 |
| C11 | 日志策略 | ✅ | tracing 生态、分级和 redaction 规则完整 |
| C12 | keyboard-interactive | ✅ | `AuthFlow` enum 与明确错误提示，因此不会被错误表示为密码失败 |
| C13 | 国际化 | ✅ | error code 映射，并且 UI 层负责文案 |
| C14 | 测试容器化 | ✅ | Docker/OpenSSH harness；无 Docker 时 skip 并显示提示 |
| C15 | SftpSession API | ⚠️ | 流程图未变，因此需要在 M3 实现阶段验证。当前可以接受。 |

---

## 3. 新发现的局部问题

### N1 — 未提供 `StartTransferCommand` 结构，tab 绑定关系仅为隐含信息

文档没有定义 `AppCommand::StartUpload(StartTransferCommand)` 和 `StartDownload(StartTransferCommand)` 的 payload 结构。该结构必须包含 `tab_id`，因为 upload/download 需要绑定发起操作的 tab 所对应的远端 session，并且 fallback 使用的也是该 tab 的 browsing session。因此 ADR-003 中“使用当前 tab 的 browsing session”所指的“当前 tab”需要由 `StartTransferCommand.tab_id` 明确表示。

**建议在 ADR-003 或 Command 协议章节提供 `StartTransferCommand` 的字段草案，至少包括 `tab_id`、`source`、`destination`、`metadata_policy` 和 `conflict_policy`。**

### N2 — 未说明 `TransferSession` 的认证复用机制

ADR-003 规定 host trust 按 `(host, port, fingerprint)` 缓存，因此 transfer session 不会重复显示 host key 确认。但是文档没有说明 **transfer session 如何复用密码或私钥认证**。TransferManager 创建 dedicated session 时，需要通过 profile 的 `SecretRef` 从 Keychain 读取密码，这与浏览 session 使用相同的认证路径。但是在 borrow 模式下，transfer 直接使用 browsing session，因此不需要再次认证。

文档已经隐含上述行为，但是尚未明确规定。**该问题不阻塞 M0，但是 M5 实现需要保证 dedicated 模式执行完整认证流程，而 borrow 模式省略再次认证。**

### N3 — event drain task 与 GPUI `Context` 的 `Send` 约束

ADR-002 规定 event drain task 在 GPUI executor 上执行。task 收到 event 后进入 GPUI update closure，更新 Entity state，然后调用 `cx.notify()`。GPUI 的 `cx.spawn` 所返回的 `Task` 通常不要求 `Send`，因为 GPUI executor 是单线程 executor，所以 task 可以持有 `cx: Context<AppModel>`。同时，`flume` 的 `Receiver::recv()` future 实现了 `Send`，而实现 `Send` 的 future 可以由非 `Send` task 轮询，因此这里不存在类型约束冲突。

但是 event drain task 不应 `await` 任何 GPUI 之外的阻塞操作，否则会阻塞主线程。它只应 `await` flume recv。ADR-002 没有明确写出该限制，但是“GPUI 主线程不 await 网络”的原则已经隐含相关约束，因此当前可以接受。

### N4 — 第 23 节 Q3（无 Docker 时的测试策略）应改为已决策项

“没有 Docker 时，本地 integration tests 默认 skip，还是要求用户提供 OpenSSH server？”该问题会影响本地开发体验和 CI。**建议明确规定：无 Docker 时 skip 并显示明确提示，但是不使 build 失败；CI 环境必须提供 Docker。** 这项工程决策不需要继续保持“待确认”状态。

### N5 — M0 验收中“ADR 关键数据流已反映到 core 类型草案”的边界

M0 验收要求“ADR 中的关键数据流已反映到 `core` 类型草案”。该要求有助于尽早定义 `RemoteEventScope`、`TrustRequest`、`TransferSessionMode` 和 `TransferPlan` 等类型。

**建议明确“草案”的边界**：是只定义 struct/enum 且不实现方法，还是需要提供 `impl` 骨架？建议 M0 只定义类型以及 `Default` / `new` 骨架，不实现业务逻辑。否则 M0 的工作量会明显增加，而类型定义本身已经能够验证 ADR 的内部一致性。

---

## 4. 实现阶段 spike 清单（不阻塞 M0，但是必须在对应里程碑前完成）

| Spike | 里程碑 | 验证项 | 风险 |
|-------|--------|--------|------|
| GPUI `list`/uniform list API | M1 前 | API 存在、签名稳定、10k 行操作保持流畅 | 已在文档说明 |
| `flume` 在 GPUI executor 上 recv | M2a 前 | `cx.spawn` + `flume::recv()` 能编译并执行 | 低 |
| `tokio::runtime::shutdown_timeout` 行为 | M2a 前 | 超时后 task 是否被强制取消 | 低 |
| russh-sftp 单 channel 并发请求 | M5 前 | borrow 模式是否可用 | **中高**，因为该结果决定 fallback 的适用范围 |
| russh `check_server_key` 的 async 签名 | M3 前 | 当前版本是同步还是 async，并确认是否可以 await oneshot | 中 |
| `ssh-key` crate 的 known_hosts 能力 | M0/M3 | 是否覆盖 MVP 子集，或者仍需小型 parser | 中 |

**其中最关键的是 russh-sftp 单 channel 并发能力。** 如果该能力不存在，那么 ADR-003 的 borrow fallback 实际上不可用，整个 session 分离模型将只包含 dedicated 模式，因此 MaxSessions 风险没有缓解方案。建议将该 spike 提前至 M3 之前，或者在 M2c 执行，因为它会影响是否应实现 fallback。

---

## 5. 相较 v1 的改进确认

v1 建议的实施情况如下：

| v1 建议 | 实施结果 | 位置 |
|---------|---------|------|
| 增加 ADR-002 桥接机制 | ✅ | 第 269-327 行 |
| 增加 ADR-003 session 分离 | ✅ | 第 514-549 行 |
| 增加 ADR-004 host key 回调 | ✅ | 第 693-733 行 |
| M2 拆分为子里程碑 | ✅ | M2a/M2b/M2c |
| 提高 MaxSessions 严重度并提供 fallback | ✅ | 风险清单 + ADR-003 fallback |
| 将 GPUI list API 设为 M1 前置 spike | ✅ | 第 1311-1316 行 + M1 交付 |
| 明确声明 known_hosts 子集 | ✅ | 第 759-786 行 |
| 在 M5 设计中提前定义 TransferPlan | ✅ | 第 936-961 行 + M5 交付 |
| 确定单窗口 | ✅ | 决策摘要 + 非目标 |
| 测试容器化 | ✅ | test_support + 测试章节 |

**实施率为 100%。** 该结果说明文档修订具有明确的执行纪律。但是评审仍需识别下一层问题，因此前文继续分析了实现细节和语义边界。

---

## 6. 最终判断

**架构内容已经完整，因此可以进入 M0。** 剩余问题属于实现细节或局部语义澄清，所以不会影响 crate 边界、运行时模型或里程碑结构。

进入 M0 前建议处理以下事项，预计每项均为约 10 分钟的文档修改：

1. 在 ADR-002 中明确规定 GPUI 侧使用 `try_send`，并且在 drop 时使用 `shutdown_timeout`。
2. 在 ADR-003 中增加 `AuthFingerprint` 的语义定义。
3. 将第 23 节 Q2（known_hosts 子集之外的 entry）从“待确认”调整为“已决策：忽略并记录 WARN”。
4. 将第 23 节 Q3（无 Docker 时的测试）从“待确认”调整为“已决策：skip 并显示提示”。
5. 在 TransferPlan 章节明确规定 planning 不执行 conflict 检测。

进入 M5 前必须完成：

- 验证 russh-sftp 单 channel 并发能力，因此可以确定 borrow fallback 是否适合实现。

**最终结论**：v1 存在架构缺口，而 v2 已经形成完整架构，但是仍有实现风险。该文档可以支持 M0 到 M7 的完整开发，前提是实现者完整阅读 ADR，并且遵守其中的行为约束与决策依据。
