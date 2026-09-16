# macSFTP 架构与可行性评审

> 评审对象：`docs/gpui-russh-plan.md`
> 评审视角：软件架构师，重点评估可行性、架构一致性、未明确说明的成本与失败模式。
> 评审原则：先说明问题与约束，然后提出方案；明确记录取舍，不罗列“最佳实践”。

---

## 0. 评审结论（TL;DR）

**整体判断：与多数从零开始的桌面产品规划相比，该方案的成熟度较高，因此具有可行性。但是，部分架构矛盾的影响被低估，需要在 M0/M2 之前解决，否则 M3–M5 期间会出现大量返工。**

- **分层与 crate 划分**：优秀。bounded context 定义明确，依赖方向单向且可验证。这是整份文档中最完善的部分。
- **SFTP 后端选型**：合理。`russh + russh-sftp` 的异步模型适用于 actor 设计，而且比 `ssh2` 更适合多标签。RSA 兼容性风险也已得到正确识别。
- **GPUI 选型**：风险较高，但是可以控制。文档已经说明 pre-1.0 的成本，而且隔离 UI 逻辑与固定版本的缓解策略方向正确。但是，文档**尚未定义双运行时边界（GPUI 自带 executor 与 Tokio）的具体桥接机制**，因此这是最重要的架构缺口。
- **多标签与传输分离**：方向正确，但是文档**没有显式建模传输 session 与浏览 session 是否共享**，而该决策会直接决定连接数、取消语义以及服务器 MaxSessions 兼容性。
- **里程碑**：可验证，而且粒度合适。M2（Runtime bridge）决定多个后续里程碑能否实施，因此建议为该阶段分配比文档当前预估更多的时间。

以下内容按照严重度排列。

---

## 1. 架构优点

1. **Domain-first 的 crate 划分**。`core` 不依赖 GPUI、russh 或 Tokio，因此构成稳定的核心；`sftp` 是 adapter，而 `app`/`ui` 是表现层。这种结构符合 Hexagonal Architecture。虽然实践中通常不会更换 UI 或 SFTP 库，但是架构允许此类替换，而且该边界使测试成为可能，例如 `test_support` 可以提供 fake command/event sink。

2. **显式非目标**。文档排除了目录同步、远程编辑、内置终端、多协议、App Store、自动更新、ssh-agent 和 jump host。因此，项目范围得到明确限制，而范围持续扩大是新项目常见的失败原因。

3. **Correlation ID 与稳定 ID**（TabId/SessionId/TransferId/TrustRequestId/ConflictRequestId）。文档第 7 节明确指出，快速切换 tab、关闭 tab 或重新连接时，旧事件可能修改新状态。这是异步 UI 中常见的缺陷，因此提前识别该问题有助于避免状态错误。

4. **Host key 策略的安全默认值**。Mismatch 会阻止连接，不提供 Accept Once，而且 app-owned known_hosts 不会修改用户的 OpenSSH 配置。因此，这些默认值符合安全要求。

5. **从首个版本开始使用版本化配置**。`ProfilesFile { version, ... }` 可以降低后续 migration 的复杂度。

6. **错误三层模型**（Technical / Domain / UserFacing）。分层方式正确，而且明确禁止向 UI 暴露密码或私钥路径片段。许多产品容易遗漏这一约束。

7. **里程碑可验证**。每个 M 均包含可执行的验收标准，例如 `cargo test` 通过、可以连接真实 OpenSSH，以及 4 个并发任务保持受控。

---

## 2. 关键架构关切（按严重度排序）

### C1 — 双运行时边界尚未完整定义（严重）

**问题。** 文档第 6 节说明“GPUI 主线程负责 UI；Tokio runtime 负责网络”，第 9 节说明 `RuntimeController` 创建 Tokio runtime。但是 GPUI **自身包含异步 executor**（`cx.background_executor()`、`cx.foreground_executor()`、`Task`），而且该 executor 不是 Tokio，其底层属于 smol/async-executor 系列。

因此，实际运行时包含**两套异步执行环境**：

```
GPUI executor (smol 系)        Tokio runtime
  - cx.spawn()                   - tokio::spawn()
  - Task                         - JoinHandle
  - 主线程同步 notify             - IO driver / blocking pool
```

文档没有回答：

- 哪个组件负责 Tokio runtime 的生命周期？通常由一个 `RuntimeController` Entity 保存 `tokio::runtime::Runtime`，并在 drop 时执行 shutdown。
- Command 从 GPUI 发送至 Tokio 时使用哪一种 channel？候选方案包括 `tokio::sync::mpsc`、`flume`，或者先在 GPUI 的 `cx.background_executor().spawn` 中执行，再调用 `runtime_handle.spawn`。
- Event 从 Tokio 返回 GPUI 时，如何在主线程中处理？GPUI 通常使用 `cx.spawn(async move { ... cx.notify() })`，但是 Tokio task 不能保存 `cx`。因此，必须定义明确的主线程事件投递机制。
- russh 是 tokio-native 的，**因此不能在 GPUI executor 上执行**。这进一步确认 russh 必须在 Tokio runtime 上执行，而桥接代码必须位于两个运行时的边界。

**这是整份文档最重要的架构缺口。** 第 6 节的 ASCII 图仅表示 Command 与 Event 的传递方向，但是没有定义 channel 类型、投递机制或背压策略。

**后果。** 如果不完善该边界，M2（Runtime bridge）会产生多次返工，而且相关缺陷可能表现为偶发死锁、event 丢失或主线程长时间无响应，因此很难定位。

**建议。** 在 M0 之前增加一个 ADR：

```
ADR-002: GPUI <-> Tokio 桥接机制
- Context: GPUI 有自带 executor，但是 russh 必须在 Tokio runtime 上执行。
- Decision: 
  - 单独持有 tokio::runtime::Runtime（RuntimeController Entity 持有 Handle）；
  - GPUI -> Tokio: tokio::sync::mpsc::Sender<AppCommand>；
  - Tokio -> GPUI: 用 cx.spawn(async move { while let Some(ev) = rx.recv().await { ... } }) 
    在 GPUI executor 上 drain 一个 tokio::sync::mpsc::Receiver<AppEvent>，
    每个 event 更新 Entity state 并 cx.notify()；
  - 背压: command channel 有界 (容量 256)，满时 GPUI 侧记录 warning 并丢弃低优先级 command。
- Consequences: 
  - 两条 channel 是唯一边界，易测试；
  - event drain task 必须随 AppModel 生命周期存在；
  - 不能在 Tokio task 里直接 cx.notify()。
```

Zed 使用了同类实现方式，具体内容可以通过 `zed/crates/gpui` 的源码确认。

---

### C2 — host key 回调的阻塞语义尚未说明（严重）

**问题。** `russh::client::Handler::check_server_key` 在握手过程中被**同步调用**，并返回 `Result<bool, Self::Error>`。文档第 10 节要求“NotFound: emit HostKeyUnknown and wait user decision”。由于该 bool 返回值决定握手是否继续，因此等待过程必须发生在 `check_server_key` 内部。

因此，actor 必须在 `check_server_key` 中**阻塞并等待 UI 决策**。常用实现方式如下：

```rust
fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
    // 1. 查 known_hosts
    // 2. NotFound -> 通过 oneshot::Sender 把 HostKeyPrompt 发出去
    //    + 把 Receiver 存进 self，await 它
    // 3. 返回用户决策
}
```

但是，`check_server_key` 是 `&mut self` 方法，而且 russh 的 handler 通常采用 owned 形式。因此，应当使用阻塞 `recv()`，还是在 handler 为 async 时使用 `await`，取决于当前 russh 版本的 trait 签名。**文档没有说明该机制，也没有说明 UI 如何将决策返回给正在阻塞的 actor。**

**后果。** 如果实施前没有确定该机制，那么用户未确认 modal 时，actor 可能永久阻塞；用户切换 tab 后，旧 modal 的确认也可能错误地作用于新 session。文档第 15 节要求 modal 关联 request id，但是没有说明决策返回路径。

**建议**。

- 在 `core` 里定义 `TrustRequest { id: TrustRequestId, server_key: PublicKey, ... }` 和一个 `TrustRegistry`（存 `Map<TrustRequestId, oneshot::Sender<bool>>`）。
- actor 在 `check_server_key` 中创建 request、发送 event，并 await receiver。
- UI 的 "Trust and Save" 或 "Cancel" 调用一个 command，而该 command 在 RuntimeController 中查找 sender 并发送结果。
- 必须设置超时，例如 5 分钟后自动 reject，否则 receiver 会发生泄漏。

---

### C3 — 尚未显式建模传输 session 与浏览 session 的分离（严重）

**问题。** 文档第 9 节规定 `RemoteSessionActor` 负责目录浏览，同时将传输 session 创建列为 `TransferManager` 的职责。因此，**传输使用独立 session**，而不复用浏览 session。但是，架构图、连接数预算和 `ConnectionState` 均未显式表示该决策。

因此，还需要处理以下几个影响被低估的问题：

1. **连接数。** 文档默认 `per host active transfers: 2`。再计算 1 个浏览 session 后，每个 tab 对同一 host 至少需要 3 条 SSH 连接。OpenSSH 默认设置为 `MaxSessions 10`，该值表示 channel 数，而不是 TCP 连接数。但是，**许多 hardened 服务器配置为 `MaxSessions 2` 或 `MaxStartups 2:30:3`**。因此，MVP 不实现 session pool 时，受限服务器可能拒绝连接。

2. **传输 session 的认证与 host key。** 传输 session 也必须执行握手和 host key 校验。需要确定是否复用浏览 session 已确认的 trust，以及采用何种传递方式。如果重新校验，那么用户会看到两次 host key modal，但是文档没有说明该行为。

3. **`ConnectionState` 仅描述浏览 session。** 文档没有说明传输 session 的连接状态保存位置。如果传输 session 断开，那么还需要定义 `TransferState::Failed` 的 `retryable` 判定方式。

4. **tab 关闭后传输继续**（第 8 节“关闭 tab 时”第 4 条）。该要求意味着 TransferManager 保存的 session 不能依赖 tab actor。因此，TransferManager 必须自行管理 session lifecycle，但是文档只隐含了该要求。

**建议**。

- 显式建模：`BrowsingSession`（per tab，生命周期较短，并与 tab 一致）与 `TransferSession`（per host，由 TransferManager 管理，并使用引用计数）。
- 决定 host trust 是否在 profile 级别缓存（一个 host trust 过一次，同 host 的传输 session 复用）。
- 在风险清单中，将“受限服务器 MaxSessions”的严重度从“连接数增加”调整为“可能不可用”，并提供 fallback：传输复用浏览 session。采用该方案时，需要取消“tab 关闭后传输继续”目标，或者将其调整为“tab 关闭后传输 graceful 停止”。

这是一个**目标冲突**：“tab 关闭后传输继续”需要独立 session，而减少连接数需要复用 session。文档选择了前者，但是没有说明放弃后者的成本。因此，必须明确记录该架构取舍。

---

### C4 — 陈旧事件防护机制尚未具体定义（中高）

**问题。** 文档第 7 节要求使用 correlation id，防止旧事件修改新状态；第 8 节要求忽略关闭 tab 后到达的 event；M2 验收则要求 stale event guard。但是，**文档没有说明具体机制。**

可以选择以下机制：

- **Generation number**：复用 TabId，但是每次 reconnect 时递增 generation。event 包含 generation，如果数值不匹配，则忽略该 event。
- **SessionId 比对**：event 包含 SessionId。如果该值与当前 TabState 的 session_id 不匹配，则忽略该 event。
- **Epoch/Token**：与 generation 类似，但是粒度更细。

`ConnectionState::Reconnecting { previous_session_id }` 表明设计可能使用 SessionId 比对。但是，文档没有说明 reconnect 期间如何区分新旧 session 的 event。一个 tab 可能同时存在两个 in-flight session，即旧 session 正在断开，而新 session 正在连接。

**建议**。

- 每个 tab 保存 `session_epoch: u64`，并在每次发起 connect 时递增。
- 所有 remote-originated event 必须包含 `(tab_id, session_epoch)`。
- RuntimeController 在投递 event 前校验 epoch，并且忽略已经过期的 event。
- 该机制必须在 `core` 中实现并包含单元测试。M2 验收已经隐含此要求，但是仍需将其定义为具体测试用例。

---

### C5 — 缺少递归传输的冲突与规划定义（中）

**问题。** 文档第 13 节的 TransferJob 采用**单文件粒度**，即 source 和 destination 均为 endpoint。但是，用户选择一个目录进行上传时会产生 N 个文件级 job。文档规定 `TransferManager` 负责 transfer planning，但是没有说明：

- 递归遍历采用同步方式还是流式方式？远端大目录的 stat 操作可能耗时较长。
- 冲突检测是在 planning 阶段检查全部条目，还是在传输时分别检查？
- `apply_to_all` 的作用域是一次 plan 还是全局？
- planning 阶段发现 10k 文件，UI 怎么显示？（"Planning" 状态可以持续很久）

`TransferState::Planning` 已经存在，说明文档考虑了该阶段，但是没有对 planning 的结果（job list）进行建模。

**建议**。

- 引入 `TransferPlan { root_job_id, child_jobs: Vec<TransferJobId>, planned_count, total_bytes }`。
- planning 采用流式输出，每识别一批条目就 emit，因此 UI 不会长时间处于空白状态。
- `apply_to_all` 作用域绑定到 `root_job_id`。
- planning 失败（如远端 stat 权限不足）要有部分恢复语义。

该问题不会阻止 MVP 实施，但是会影响 M5/M6 的体验。如果直到 M5 才处理，那么 `TransferManager` 的数据结构需要返工。

---

### C6 — known_hosts 解析与写入（中）

**问题。** 文档要求 "OpenSSH-compatible" 和 "parse OpenSSH known_hosts"。但是，OpenSSH 的 `known_hosts` 格式包含多种复杂情况：

- 哈希主机名 `|1|<salt>|<hash>`（出于隐私）——解析需 SHA1 + base64。
- 多种 key type（ssh-rsa, ssh-ed25519, ecdsa-sha2-nistp256/384/521, sk-*）。
- 一行可含多个 host pattern（逗号分隔）。
- 通配符 `*`、`?`、否定 `!host`。
- 注释行 `#`、空行。
- cert-authority、revoked 标记。

自行实现解析器需要较多工作，而且容易在边缘格式上出现错误。文档没有指定使用哪个 crate。

**建议**。

- 优先使用现有 crate，例如 `known_hosts` 或 `ssh-key` 提供的解析能力。
- MVP 可以仅支持明文 host、单 key type、无通配符和无哈希，但是文档必须**显式声明该子集**，否则用户从 OpenSSH 导入数据时可能遇到不兼容问题。
- 仅向 app-owned known_hosts 写入数据，并使用与 OpenSSH 兼容的明文格式。因此，用户可以手动迁移数据。

---

### C7 — 取消时的远端清理可靠性（中）

**问题。** 文档第 13 节的“取消”部分规定“取消 running 时默认清理 `.macsftp-part`”。但是，取消发生时存在以下情况：

- 如果取消由**网络断开**引起，那么远端 SFTP session 可能已经不可用，并且 `remove_file(.part)` 会失败。文档要求清理失败时记录 warning，这是正确的。但是，残留的 .part 文件会影响目标目录，而下次传输的冲突检测可能将其识别为冲突源。
- 如果由**用户主动取消**，那么 session 仍然可用，因此清理可能成功。但是，将取消信号传递给正在 `write` 的 future 需要 drop future 或 cancel token，而 drop future 时远端 channel 可能处于中间状态。

**建议**。

- `.macsftp-part` 在冲突检测时**显式排除**（不当作正常文件）。
- 启动时扫描 app 工作目录中的残留 `.macsftp-part` 并提示清理。也可以自动清理，但是远端文件扫描成本较高，因此可以仅在传输失败时记录 path，并在下次连接该 host 时尝试清理。
- 用 `tokio_util::sync::CancellationToken` 而不是 drop future，语义更可控。

---

## 3. 次要关切

| # | 关切 | 严重度 | 说明 |
|---|------|--------|------|
| C8 | Keychain crate 未选型 | 低 | macOS 使用 `security-framework` crate，并且需要固定版本。文档要求使用 Keychain，但是没有说明实现方式。 |
| C9 | ~~单窗口 vs 多窗口未定义~~ **→ 已解决（2026-07-13）** | 低 | MVP 选定单窗口；多窗口后于 2026-07-13 作为 post-MVP 交付（共享 global 化的 `AppModel`、broadcast event routing、每窗口私有 modal）。详见 `docs/progress-analysis-2026-07-13-multiwindow.md`。 |
| C10 | Accessibility 未提 | 中 | macOS 用户期望 VoiceOver。GPUI 的 a11y 支持有限，因此可能需要在文档中明确说明第一版不满足该要求。 |
| C11 | 日志策略不完整 | 低 | 文档指定了 log 路径，但是没有选择 `tracing` 或 `log`，也没有定义日志级别、轮转和敏感字段过滤。建议使用 `tracing`、`tracing-subscriber` 和 `tracing-appender`。 |
| C12 | keyboard-interactive 预留扩展点但无机制 | 低 | 文档要求预留扩展点，但是 `AuthMethod` enum 没有对应 variant。建议增加 `KeyboardInteractive { /* placeholder */ }`，或者在文档中说明扩展点的位置。 |
| C13 | 国际化 | 低 | Zed 风格默认英文，但 macOS 中文用户多。MVP 可英文，但 `UserFacingError` 的 message 应该是可替换的（不要硬编码到业务逻辑里）。 |
| C14 | 测试使用真实 OpenSSH | 中 | `test_support` 提到 "local temp SFTP server harness"。in-process OpenSSH 不可行，因为 OpenSSH 不是库，因此需要 Docker container 或系统 OpenSSH 与临时账号。CI 执行需要容器化，而文档低估了这部分工作量。 |
| C15 | `SftpSession::new(channel.into_stream())` API 假设 | 低 | 需要在 M3 初期验证当前 russh-sftp 版本的准确 API shape。文档中的流程图表示设计意图，并不表示真实的 API 签名。 |

---

## 4. 里程碑可行性评估

| 里程碑 | 可行性 | 风险 | 评审意见 |
|--------|--------|------|----------|
| M0 skeleton | 高 | 低 | 增加 ADR-002（运行时桥接）后开始实施。 |
| M1 GPUI shell + mock | 中高 | GPUI API 漂移 | `uniform_list` 是关键 API，先验证存在且能用 10k 行。 |
| **M2 Runtime bridge** | **中** | **高** | **关键依赖。** C1、C2 和 C4 均会在此阶段出现。因此，建议将 M2 划分为 M2a（channel + event drain）、M2b（stale guard）和 M2c（mock actor 完整流程）。 |
| M3 russh connection | 中 | RSA 兼容、host key 阻塞 | C2 必须在 M3 之前解决。RSA-sha2 与 legacy ssh-rsa 的兼容矩阵要有真实服务器测试。 |
| M4 remote browsing | 高 | 低 | SFTP read_dir 直白。注意 10k 条目的 stat 延迟。 |
| M5 transfers | 中 | 递归 planning、取消清理 | C5 和 C7 会在该阶段产生影响。因此，建议在 M5 之前完成 planning 数据结构的设计 spike。 |
| M6 conflict + metadata | 中 | setstat 兼容性 | metadata preservation 失败应当降级为 warning，而不能使传输失败。文档已经说明该规则。 |
| M7 polish | 中 | 未签名应用的分发与执行 | macOS Gatekeeper 会降低未签名 app 的使用体验，因此需要在文档中说明“右键打开”流程。 |

**总体：** 7 个里程碑中，M2、M3 和 M5 的实现难度较高，其余里程碑主要需要完成既定实施工作。对于新项目而言，该难度分布合理，而且没有会在后期导致项目不可行的问题。但是，这一判断以 M2 之前解决 C1、C2 和 C3 为前提。

---

## 5. 建议（按优先级）

1. **M0 之前增加三个 ADR：**
   - ADR-002: GPUI <-> Tokio 桥接（解决 C1）
   - ADR-003: Session 分离模型——browsing vs transfer（解决 C3）
   - ADR-004: host key 回调的阻塞与回传机制（解决 C2）

2. **将 M2 划分为三个子里程碑，**并且将 stale event guard 定义为包含具体测试用例的验收项。

3. **在风险清单中提高两项风险的严重度：**
   - “受限服务器 MaxSessions”：从“连接数增加”调整为“可能不可用”，并提供 fallback。
   - “GPUI pre-1.0”：增加“uniform_list API 可能不存在或签名变化”，并将其作为 M1 的前置 spike。

4. **显式声明 known_hosts 支持子集**（C6），因此用户从 OpenSSH 导入数据时可以提前确认兼容范围。

5. **在 M5 设计阶段定义 `TransferPlan` 数据结构**（C5），而不要延迟至实现阶段。

6. **确定使用单窗口**（C9），并将该决定写入决策摘要。多窗口可以在后续版本中增加，但是 AppModel 当前应当按照单窗口设计。

7. **测试容器化**（C14）：使用 Docker 执行 OpenSSH，并将其作为 integration test fixture。CI 需要提供 Linux 容器选项；虽然产品目标是 macOS 桌面应用，但是 SFTP 协议层测试可以在 Linux CI 中执行。

---

## 6. 待确认问题的架构影响

文档第 22 节列出 8 个待确认问题。以下内容分别说明其架构影响：

1. **Profile folder/group**：该功能会影响 `storage` 的查询结构。MVP 不实现 group，但是可以在 `ProfileId` 之外预留 `group_id: Option<...>` 字段，从而减少后续 migration 的复杂度。
2. **同 profile 多 tab**：允许该行为。`TabState` 与 `ProfileId` 是多对一关系，因此现有结构已经支持。
3. **关闭 app 时仍有运行中的 transfer**：架构上必须选择一种行为。等待完成要求 app 能够在后台驻留，而 macOS 需要特殊处理；取消则较为简单。**建议 MVP 采用取消，并在下次启动时显示未完成 history。** 该决定会影响 M7 的 app lifecycle 代码，因此需要尽早确定。
4. **远端删除使用 trash 或确认删除**：SFTP 没有 trash 概念。因此，删除前需要确认，而且批量删除时必须明确提示该操作不可撤销；trash flow 不属于 MVP。
5. **默认记住 passphrase**：建议默认将其保存至 Keychain，但是 profile 中需要保存 `remember_passphrase: bool`，从而允许用户关闭该选项。
6. **导入 `~/.ssh/config` Host alias**：该功能不属于 MVP，并且已经列入非目标。但是，`ConnectionProfile` 字段应当与 ssh config 概念一致，从而便于后续实现导入。
7. **默认本地起始目录**：使用 home。上次路径属于 per-profile 偏好，因此保存在 profile 中。
8. **transfer completed history 保留时长**：在 7 天和 100 条两个限制中，以先达到的限制为准。history 用于改善 UI 使用体验，但是不用于审计。

---

## 7. 总结

这份规划明确规定了范围与非目标，而且 crate 边界和里程碑设计较为完善。因此，计划整体可行。但是，ASCII 图和 command/event 说明没有充分表达三个架构矛盾，即双运行时桥接、host key 阻塞返回和 session 分离。需要在开始实现之前通过 ADR 明确解决这些问题。

解决上述问题后，最大的不确定性来自 GPUI pre-1.0。架构本身无法消除该不确定性，因此只能通过固定版本和隔离 UI 逻辑降低影响。russh 侧的风险属于工程实现问题，包括 RSA 兼容性和 setstat 差异；真实 OpenSSH integration test 可以验证这些行为。

**结论：** 架构可行，但是工程量较大。最大的架构风险是文档尚未完整定义运行时桥接代码。因此，应当先增加三个 ADR，然后实施 M0。
