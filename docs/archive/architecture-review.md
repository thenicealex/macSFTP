# macSFTP 架构与可行性评审

> 评审对象：`docs/gpui-russh-plan.md`
> 评审视角：软件架构师，聚焦可行性、架构一致性、隐藏成本与失败模式。
> 评审原则：先说问题与约束，再给方案；命名取舍而非堆叠"最佳实践"。

---

## 0. 评审结论 (TL;DR)

**整体判断：方案成熟度高于绝大多数从零起步的桌面产品规划，可行，但有几个被低估的架构张力需要在 M0/M2 之前闭合，否则会在 M3-M5 之间集中爆发返工。**

- **分层与 crate 划分**：优秀。bounded context 清晰，依赖方向单向且可验证。这是整份文档最扎实的部分。
- **SFTP 后端选型**：合理。`russh + russh-sftp` 的异步模型与 actor 思路契合，比 `ssh2` 更适合多标签。RSA 兼容性风险被正确识别。
- **GPUI 选型**：高风险但可控。pre-1.0 的代价文档已承认，缓解策略（隔离 UI 逻辑、pin 版本）方向正确，但**双运行时边界（GPUI 自带 executor 与 Tokio）的具体桥接机制未定义**，这是第一号架构缺口。
- **多标签与传输分离**：思路对，但"传输 session 与浏览 session 是否共享"这件事**文档没有显式建模**，而它直接决定连接数、取消语义、服务器 MaxSessions 兼容性。
- **里程碑**：可验证、粒度合适。M2（Runtime bridge）是整个项目的"咽喉"，建议在这里投入比文档暗示的更多时间。

下面按严重度展开。

---

## 1. 架构优点（先肯定做对的事）

1. **Domain-first 的 crate 切分**。`core` 不依赖 GPUI/russh/Tokio，是稳定核心；`sftp` 是 adapter；`app`/`ui` 是表现层。这是 Hexagonal Architecture 的正确落地，意味着"换 UI 或换 SFTP 库"在结构上是可能的——虽然实践中永远不会换，但这个边界让测试变得可行（`test_support` 能 fake command/event sink）。

2. **显式非目标**。不做目录同步、远程编辑、内置终端、多协议、App Store、自动更新、ssh-agent、jump host。这是这份文档最重要的克制。范围失控是从零项目最常见的死法，这里防住了。

3. **Correlation ID 与稳定 ID**（TabId/SessionId/TransferId/TrustRequestId/ConflictRequestId）。文档第 7 节点名了"快速切换 tab、关闭 tab、重连时旧事件污染新状态"——这是异步 UI 的经典 bug，能提前识别说明作者吃过亏。

4. **Host key 策略的安全姿态**。Mismatch 阻断、不提供 Accept Once、app-owned known_hosts 不污染用户 OpenSSH 配置。这是正确的安全默认值。

5. **版本化配置从第一天开始**。`ProfilesFile { version, ... }`。 Migration 地狱的预防针。

6. **错误三层模型**（Technical / Domain / UserFacing）。分层正确，且明确"不向 UI 泄露密码/私钥路径片段"——这条很多产品做漏。

7. **里程碑可验证**。每个 M 都有"验收"标准，且是可执行的（`cargo test` 通过、能连真实 OpenSSH、4 并发受控等）。

---

## 2. 关键架构关切（按严重度排序）

### C1 — 双运行时边界未闭合（严重）

**问题**。文档第 6 节说"GPUI 主线程负责 UI；Tokio runtime 负责网络"，第 9 节说 `RuntimeController` 创建 Tokio runtime。但 GPUI **自身带一个异步 executor**（`cx.background_executor()`、`cx.foreground_executor()`、`Task`），它不是 Tokio，底层是 smol/async-executor 系列。

这意味着实际运行时存在**两套异步世界**：

```
GPUI executor (smol 系)        Tokio runtime
  - cx.spawn()                   - tokio::spawn()
  - Task                         - JoinHandle
  - 主线程同步 notify             - IO driver / blocking pool
```

文档没有回答：

- 谁拥有 Tokio runtime 的生命周期？（通常是一个 `RuntimeController` Entity 持有 `tokio::runtime::Runtime`，drop 时 shutdown）
- Command 从 GPUI 发到 Tokio 走什么 channel？（`tokio::sync::mpsc`？还是 `flume`？还是 GPUI 的 `cx.background_executor().spawn` 里再 `runtime_handle.spawn`？）
- Event 从 Tokio 回 GPUI 怎么"回到主线程"？GPUI 的正确做法是 `cx.spawn(async move { ... cx.notify() })`，但 Tokio task 不能直接持有 `cx`——必须有显式的"投递回主线程"机制。
- russh 是 tokio-native 的，**不能在 GPUI executor 上跑**。这强化了"必须在 Tokio runtime 上跑 russh"的硬约束，但桥接代码本身要写在边界上。

**这是整份文档最大的架构缺口**。第 6 节那张 ASCII 图只画了"Command 向下、Event 向上"，但没定义 channel 类型、投递机制、背压策略。

**后果**。如果不闭合，M2（Runtime bridge）会反复返工，且 bug 表现为"偶发死锁 / event 丢失 / 主线程卡顿"，极难定位。

**建议**。在 M0 之前补一个 ADR：

```
ADR-002: GPUI <-> Tokio 桥接机制
- Context: GPUI 有自带 executor，russh 必须跑在 Tokio 上。
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

Zed 自己就是这么干的（参考 `zed/crates/gpui`），可以读源码确认。

---

### C2 — host key 回调的阻塞语义未说明（严重）

**问题**。`russh::client::Handler::check_server_key` 在握手过程中被**同步调用**，返回 `Result<bool, Self::Error>`。文档第 10 节说"NotFound: emit HostKeyUnknown and wait user decision"——这个"wait"必须发生在 `check_server_key` 内部，因为它返回 bool 决定握手是否继续。

这意味着 actor 必须在 `check_server_key` 里**阻塞等待 UI 决策**。标准做法：

```rust
fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
    // 1. 查 known_hosts
    // 2. NotFound -> 通过 oneshot::Sender 把 HostKeyPrompt 发出去
    //    + 把 Receiver 存进 self，await 它
    // 3. 返回用户决策
}
```

但 `check_server_key` 是 `&mut self` 方法，且russh 的 handler 通常是 owned——这里到底用阻塞 `recv()` 还是 `await`（如果 handler 是 async）取决于 russh 版本的 trait 签名。**文档没有说明这个机制，也没说明 UI 端如何把决策回传给一个正在阻塞的 actor**。

**后果**。如果实现者不提前想清楚，会出现：用户不点 modal，actor 永久阻塞；用户切换 tab，旧 modal 的确认误作用到新 session（文档第 15 节提到了 modal 绑 request id，但没说回传路径）。

**建议**。

- 在 `core` 里定义 `TrustRequest { id: TrustRequestId, server_key: PublicKey, ... }` 和一个 `TrustRegistry`（存 `Map<TrustRequestId, oneshot::Sender<bool>>`）。
- actor 在 `check_server_key` 里 create request，emit event，await receiver。
- UI 的 "Trust and Save" / "Cancel" 调用一个 command，command 在 RuntimeController 里找到 sender 并 send。
- 必须有超时（如 5 分钟自动 reject），否则泄漏 receiver。

---

### C3 — 传输 session 与浏览 session 的分离未显式建模（严重）

**问题**。文档第 9 节 `RemoteSessionActor` 负责"目录浏览"，第 9 节 `TransferManager` 职责里列了"传输 session 创建"——所以**传输用独立 session**，不是复用浏览 session。但这件事没有在架构图、连接数预算、或 `ConnectionState` 里显式建模。

由此引出几个被低估的子问题：

1. **连接数**。文档默认 `per host active transfers: 2`。加上 1 个浏览 session，每个 tab 对同一 host 至少 3 条 SSH 连接。OpenSSH 默认 `MaxSessions 10`（这是 channel 数，不是 TCP 连接数），但**很多 hardened 服务器配 `MaxSessions 2` 或 `MaxStartups 2:30:3`**。MVP 不做 session pool 的代价在受限服务器上会直接表现为"连不上"。

2. **传输 session 的认证与 host key**。传输 session 也要走一遍握手 + host key 校验。是否复用浏览 session 已确认的 trust？如果复用，怎么传递？如果重新校验，用户会被弹两次 host key modal。文档没说。

3. **`ConnectionState` 只描述浏览 session**。传输 session 的连接状态在哪？如果传输 session 断了，`TransferState::Failed` 的 `retryable` 怎么判定？

4. **tab 关闭后传输继续**（第 8 节"关闭 tab 时"第 4 条）。这意味着 TransferManager 持有的 session 不能依赖 tab 的 actor。结构上 TransferManager 必须自己管 session lifecycle——这点文档隐含但没显式。

**建议**。

- 显式建模：`BrowsingSession`（per tab，短生命周期，跟随 tab）vs `TransferSession`（per host，由 TransferManager 持有，引用计数）。
- 决定 host trust 是否在 profile 级别缓存（一个 host trust 过一次，同 host 的传输 session 复用）。
- 在风险清单里把"受限服务器 MaxSessions"的严重度从"连接数增加"提升到"可能直接不可用"，并给一个 fallback：传输复用浏览 session（牺牲"tab 关闭后传输继续"这个目标，或把它降级为"tab 关闭后传输 graceful 停止"）。

这是一个**目标冲突**：要么"tab 关闭后传输继续"（需要独立 session），要么"少连接数"（需要复用 session）。文档想要前者但没承认后者的代价。**架构就是取舍，这里必须明取。**

---

### C4 — 陈旧事件防护机制未具体化（中高）

**问题**。文档第 7 节说要 correlation id 防止"旧事件污染新状态"，第 8 节说"关闭 tab 后迟到 event 被忽略"，M2 验收说"stale event guard"。但**机制是什么没说**。

候选机制：

- **Generation number**：TabId 复用，但每次 reconnect 递增 generation，event 带 generation，不匹配丢弃。
- **SessionId 比对**：event 带 SessionId，当前 TabState 的 session_id 不匹配则丢弃。
- **Epoch/Token**：类似 generation 但更细粒度。

`ConnectionState::Reconnecting { previous_session_id }` 暗示用的是 SessionId 比对，但 reconnect 期间新旧 session 的 event 怎么区分？一个 tab 同时有两个 in-flight session（旧的在断、新的在连）是可能的。

**建议**。

- 每个 tab 持有 `session_epoch: u64`，每次发起 connect 递增。
- 所有 remote-originated event 必须 carry `(tab_id, session_epoch)`。
- RuntimeController 在投递 event 前校验 epoch，过期直接丢。
- 这个机制必须在 `core` 里实现并有单元测试（M2 验收已经隐含，但要写成具体测试用例）。

---

### C5 — 递归传输的冲突与规划缺失（中）

**问题**。文档第 13 节的 TransferJob 是**单文件粒度**（source/destination 都是 endpoint）。但用户拖一个目录上传，实际产生 N 个文件级 job。文档说 `TransferManager` 负责"transfer planning"，但没说：

- 递归遍历是同步还是流式？（远端大目录 stat 很慢）
- 冲突检测是 planning 阶段一次性全检，还是传输时逐个检？
- `apply_to_all` 的作用域是一次 plan 还是全局？
- planning 阶段发现 10k 文件，UI 怎么显示？（"Planning" 状态可以持续很久）

`TransferState::Planning` 存在，说明作者意识到了，但 planning 的产物（job list）没有建模。

**建议**。

- 引入 `TransferPlan { root_job_id, child_jobs: Vec<TransferJobId>, planned_count, total_bytes }`。
- planning 流式产出（每发现一批就 emit），避免 UI 长时间空白。
- `apply_to_all` 作用域绑定到 `root_job_id`。
- planning 失败（如远端 stat 权限不足）要有部分恢复语义。

这不阻塞 MVP 但影响 M5/M6 的体验。如果 M5 才想起来，`TransferManager` 的数据结构要返工。

---

### C6 — known_hosts 解析与写入（中）

**问题**。文档说"OpenSSH-compatible" + "parse OpenSSH known_hosts"。OpenSSH 的 `known_hosts` 格式**比看起来复杂**：

- 哈希主机名 `|1|<salt>|<hash>`（出于隐私）——解析需 SHA1 + base64。
- 多种 key type（ssh-rsa, ssh-ed25519, ecdsa-sha2-nistp256/384/521, sk-*）。
- 一行可含多个 host pattern（逗号分隔）。
- 通配符 `*`、`?`、否定 `!host`。
- 注释行 `#`、空行。
- cert-authority、revoked 标记。

自己写解析器是**实打实的工作量**，且容易在边缘格式上出错。文档没提用什么 crate。

**建议**。

- 优先找现成 crate（如 `known_hosts`、`ssh-key` 的解析能力）。
- MVP 可以只支持：明文 host、单 key type、无通配符、无哈希——但要在文档里**显式声明这个子集**，否则用户从 OpenSSH 导入会踩坑。
- 写入只用 app-owned known_hosts，格式与 OpenSSH 兼容（明文），这样用户能手动迁移。

---

### C7 — 取消时的远端清理可靠性（中）

**问题**。文档第 13 节"取消"说"取消 running 时默认清理 `.macsftp-part`"。但取消发生时：

- 如果是**网络断开**导致的取消，远端 SFTP session 可能已经不可用，`remove_file(.part)` 会失败——文档说"清理失败则记录 warning"，这是对的，但**残留 .part 文件会污染目标目录**，下次传输冲突检测会把它当冲突源。
- 如果是**用户主动取消**，session 还活着，清理可以成功——但取消信号传到正在 `write` 的 future 需要 drop future 或 cancel token，drop future 时远端 channel 可能处于半状态。

**建议**。

- `.macsftp-part` 在冲突检测时**显式排除**（不当作正常文件）。
- 启动时扫描 app 工作目录下的残留 `.macsftp-part` 并提示清理（或自动清理，但这是远端文件，扫描成本高——可以只在传输失败时记录 path，下次连接该 host 时尝试清理）。
- 用 `tokio_util::sync::CancellationToken` 而不是 drop future，语义更可控。

---

## 3. 次要关切

| # | 关切 | 严重度 | 说明 |
|---|------|--------|------|
| C8 | Keychain crate 未选型 | 低 | macOS 用 `security-framework` crate，需 pin。文档说"用 Keychain"但没说怎么用。 |
| C9 | ~~单窗口 vs 多窗口未定义~~ **→ 已解决（2026-07-13）** | 低 | MVP 选定单窗口；多窗口后于 2026-07-13 作为 post-MVP 交付（共享 global 化的 `AppModel`、broadcast event routing、每窗口私有 modal）。详见 `docs/progress-analysis-2026-07-13-multiwindow.md`。 |
| C10 | Accessibility 未提 | 中 | macOS 用户期望 VoiceOver。GPUI 的 a11y 支持有限，可能需要在文档里显式列为"第一版不达标"。 |
| C11 | 日志策略不完整 | 低 | 提了 log 路径，没说用 `tracing` 还是 `log`，没说分级、轮转、敏感字段过滤。建议 `tracing` + `tracing-subscriber` + `tracing-appender`。 |
| C12 | keyboard-interactive 留扩展点但无机制 | 低 | 文档说"保留扩展点"但 `AuthMethod` enum 没有这个 variant。建议加 `KeyboardInteractive { /* placeholder */ }` 或文档说明扩展点在哪。 |
| C13 | 国际化 | 低 | Zed 风格默认英文，但 macOS 中文用户多。MVP 可英文，但 `UserFacingError` 的 message 应该是可替换的（不要硬编码到业务逻辑里）。 |
| C14 | 测试用真实 OpenSSH | 中 | `test_support` 说"local temp SFTP server harness"。in-process OpenSSH 不可行（不是库），需要 Docker container 或系统 OpenSSH + 临时账号。CI 上跑需要容器化。这块工作量被低估。 |
| C15 | `SftpSession::new(channel.into_stream())` API 假设 | 低 | 需在 M3 初期验证 russh-sftp 当前版本的确切 API shape，文档画的流程图是"意图"不是"API 真实签名"。 |

---

## 4. 里程碑可行性评估

| 里程碑 | 可行性 | 风险 | 评审意见 |
|--------|--------|------|----------|
| M0 skeleton | 高 | 低 | 直接做。补 ADR-002（运行时桥接）后再开工。 |
| M1 GPUI shell + mock | 中高 | GPUI API 漂移 | `uniform_list` 是关键 API，先验证存在且能用 10k 行。 |
| **M2 Runtime bridge** | **中** | **高** | **项目咽喉。** C1/C2/C4 全在这里暴露。建议把 M2 拆成 M2a（channel + event drain）M2b（stale guard）M2c（mock actor 全链路）。 |
| M3 russh connection | 中 | RSA 兼容、host key 阻塞 | C2 必须在 M3 之前解决。RSA-sha2 与 legacy ssh-rsa 的兼容矩阵要有真实服务器测试。 |
| M4 remote browsing | 高 | 低 | SFTP read_dir 直白。注意 10k 条目的 stat 延迟。 |
| M5 transfers | 中 | 递归 planning、取消清理 | C5/C7 在这里爆发。建议 M5 之前先做 planning 数据结构的设计 spike。 |
| M6 conflict + metadata | 中 | setstat 兼容性 | metadata preservation 失败要降级为 warning，不能 fail 传输。文档已说，OK。 |
| M7 polish | 中 | 打包不签名也能跑 | macOS Gatekeeper 对未签名 app 的体验差，需文档化"右键打开"流程。 |

**总体**：7 个里程碑里 3 个（M2/M3/M5）是硬骨头，其余是执行量。这个分布对从零项目是合理的，没有"后期才发现做不了"的致命项——前提是 C1/C2/C3 在 M2 之前闭合。

---

## 5. 建议（按优先级）

1. **M0 之前补三个 ADR**：
   - ADR-002: GPUI <-> Tokio 桥接（解决 C1）
   - ADR-003: Session 分离模型——browsing vs transfer（解决 C3）
   - ADR-004: host key 回调的阻塞与回传机制（解决 C2）

2. **M2 拆分为三个子里程碑**，把"stale event guard"做成有具体测试用例的验收项。

3. **在风险清单里提升两项严重度**：
   - "受限服务器 MaxSessions"：从"连接数增加"提升到"可能不可用"，给 fallback。
   - "GPUI pre-1.0"：补充"uniform_list API 可能不存在或签名变化"作为 M1 的前置 spike。

4. **显式声明 known_hosts 支持子集**（C6），避免用户从 OpenSSH 导入踩坑。

5. **`TransferPlan` 数据结构提前到 M5 设计阶段**（C5），不要等实现时再补。

6. **决定单窗口**（C9），写进决策摘要。多窗口是后期可加项，但 AppModel 现在就要假设单窗口。

7. **测试容器化**（C14）：用 Docker 跑 OpenSSH 作为 integration test fixture，CI 上要有 Linux 容器选项（即使目标是 macOS 桌面，SFTP 协议层测试可在 Linux CI 跑）。

---

## 6. 待确认问题的架构影响

文档第 22 节列了 8 个待确认问题。从架构角度逐条点评：

1. **Profile folder/group**：影响 `storage` 的查询结构。MVP 不要 group，但 `ProfileId` 之外预留 `group_id: Option<...>` 字段，免得后期 migration。
2. **同 profile 多 tab**：直接允许。`TabState` 与 `ProfileId` 是多对一，结构上已支持。
3. **关闭 app 时运行中 transfer**：架构上必须二选一——"等待完成"会要求 app 能后台驻留（macOS 需特殊处理），"取消"简单。**建议 MVP 取消 + 下次启动展示未完成 history**。这件事会影响 M7 的 app lifecycle 代码，要早决定。
4. **远端删除 trash vs 确认删除**：SFTP 无 trash 概念。直接 confirm 删除，但删除是批量操作时要明确"不可撤销"提示。不进 MVP 的 trash flow。
5. **passphrase 默认记住**：建议默认记住到 Keychain，但 profile 里存 `remember_passphrase: bool`，用户可关。
6. **导入 `~/.ssh/config` Host alias**：不进 MVP（已在非目标）。但 `ConnectionProfile` 的字段要与 ssh config 概念对齐，方便后期导入。
7. **默认本地起始目录**：home。记住"上次路径"是 per-profile 的偏好，存 profile 里。
8. **transfer completed history 保留多久**：7 天 + 100 条取小。history 是 UI 层便利，不是审计需求。

---

## 7. 总结

这份规划在"做什么/不做什么"上极为清醒，crate 边界和里程碑设计是专业水准。它**不是一份会失败的计划**，但有三个架构张力（双运行时桥接、host key 阻塞回传、session 分离）被 ASCII 图和"command/event"措辞掩盖了，需要在写第一行代码之前用 ADR 显式闭合。

闭合之后，最大不确定性是 GPUI pre-1.0——这不是架构能解决的，只能靠版本 pin + UI 逻辑隔离来对冲。russh 一侧的风险是工程性的（RSA 兼容、setstat 差异），有真实 OpenSSH integration test 就能控住。

**一句话**：架构可行，工程量大，最大的架构风险是文档还没说清楚的那部分桥接代码。先补三个 ADR，再开 M0。
