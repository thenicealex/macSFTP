# macSFTP 项目 Bug 审计报告

- 审计日期：2026-07-28
- 审计范围：`core`、`app/ui`、`sftp`、`storage`、`platform`、测试与发布门禁
- 审计方式：架构约束核对、当前源码数据流追踪、历史问题去重、定向测试、真实 sshd 集成测试、Keychain 隔离测试、严格静态检查
- 代码状态：审计期间未修改产品源码；最终仅新增本审计报告 `outputs/macsftp-bug-audit-2026-07-28.md`

## 1. 结论摘要

当前代码中确认 7 项需要处理的问题或工程缺陷：

| 编号                | 模块                    | 严重程度 | 证据状态            | 主要影响                                |
| ----------------- | --------------------- | ---: | --------------- | ----------------------------------- |
| CORE-SFTP-001     | core / sftp / app     |    高 | 静态确认，可确定性状态机复现  | 旧连接的 host-key mismatch 可错误击穿新会话     |
| SFTP-TRANSFER-001 | sftp / core / app     |    高 | 静态确认，现有测试事件序列佐证 | 上传任务可能永久停留在 Queued，远程编辑可能搁浅         |
| APP-EDIT-001      | app / sftp            |    高 | 静态确认，可确定性场景复现   | 远程文件被外部修改后可能被本地保存静默覆盖               |
| APP-EDIT-002      | app / platform        |    低 | 静态确认            | 编辑临时目录创建失败被延迟处理，且同步文件系统调用位于 UI 路径   |
| APP-EDIT-003      | app / platform        |    低 | 静态确认            | 临时目录清理失败无日志、无残留记录，可能长期积累文件          |
| TEST-001          | app / platform / sftp |    中 | 静态确认            | 固定临时路径导致并发测试互相覆盖或随机失败               |
| SFTP-MOCK-001     | sftp test support     |    低 | 静态确认            | mock actor 静默丢失终态事件，测试可能挂起且与生产语义不一致 |

另有 3 类风险/限制需要明确，但不应与已确认 bug 混为一谈：

1. `RemoteSnapshot` 只比较大小与整秒 mtime；即使改成实时远程 `stat`，同秒同大小的并发修改仍可能漏检。这是当前设计已接受的限制。
2. 交互式 720×480、真实 10k 条目、多 transfer、多 tab、窗口 resize 的人工 GUI smoke 尚无本次审计签字。
3. 本机缺少 Xcode Metal 编译器，GPUI 0.2.2 无法构建，导致 app/ui 测试和全仓 `scripts/check.sh` 在构建阶段被阻断；这不是测试断言失败。

修复优先级建议：

1. **P0：APP-EDIT-001**，防止静默覆盖远端数据。
2. **P0：SFTP-TRANSFER-001**，保证每个已启动传输计划最终都有终态。
3. **P1：CORE-SFTP-001**，补齐 host-key mismatch 的会话作用域。
4. **P2：TEST-001**，消除已知并发 flaky 测试源。
5. **P3：APP-EDIT-002、APP-EDIT-003、SFTP-MOCK-001**，统一错误传播和清理可观测性。

---

## 2. core / sftp / app：会话与安全事件

### CORE-SFTP-001：陈旧 HostKeyMismatch 可错误终止新会话

- **描述**：`HostKeyMismatch` 从 SFTP 连接层发出时只携带 `tab_id`，丢失了生产端已经持有的 `session_id` 与 `session_epoch`。因此 core 将该事件视为全局事件放行，app 再按 `tab_id` 直接把当前标签置为失败。旧连接尝试的延迟 mismatch 可覆盖同一标签中已经建立或正在建立的新连接状态。
- **严重程度**：高
- **影响范围**：同一 Tab 快速重连、旧 SSH 握手延迟返回、网络抖动或 actor 取消与事件发送竞态。主要后果是错误断开/阻断当前合法会话，属于安全事件作用域错误和可用性问题；不会自动信任错误密钥。
- **证据状态**：当前源码静态闭环，可通过纯状态机步骤确定性复现；尚缺专门的失败回归测试。

#### 复现步骤

1. 在 `TabId(1)` 发起连接 A，scope 为 `(tab=1, session=10, epoch=1)`。
2. A 的 host-key 检查进入 mismatch 分支，但事件尚未被 app 消费。
3. 用户重连，标签进入连接 B，scope 为 `(tab=1, session=11, epoch=2)`，或 B 已经成功连接。
4. 如何投递 A 的 `HostKeyMismatch`。
5. `AppState::should_accept_event()` 因 `AppEvent::remote_scope()` 对该事件返回 `None` 而接受事件。
6. `Workspace::handle_app_event()` 按 `mismatch.tab_id` 找到当前标签并调用 `tab.fail(error)`。
7. 观察 B 被旧事件错误置为失败。

#### 原因分析

- `crates/sftp/src/physical_connection.rs:119`：`ClientHandler` 持有 `self.scope`，构造事件时却只复制 `tab_id`。
- `crates/core/src/core.rs:1821`：`HostKeyMismatch` 结构没有 `RemoteEventScope` 或 `session_id/session_epoch`。
- `crates/core/src/core.rs:1693`：`AppEvent::remote_scope()` 包含 `HostKeyUnknown`，但不包含 `HostKeyMismatch`。
- `crates/app/src/workspace/event_handling.rs:59`：事件通过通用 guard 后，仅按 `tab_id` 失败当前标签。
- 根因是来自远程 browsing session 的安全事件未遵守项目统一的 `RemoteEventScope` 约束。

#### 修复方案

1. 修改 `crates/core/src/core.rs`：
   - 首选将事件改为 `AppEvent::HostKeyMismatch(RemoteScoped<HostKeyMismatch>)`；
   - 或给 `HostKeyMismatch` 增加完整 `scope: RemoteEventScope`，不要只补 epoch 而缺 session ID。
2. 修改 `AppEvent::remote_scope()`，将 mismatch 纳入统一 stale-event guard。
3. 修改 `crates/sftp/src/physical_connection.rs`，发送时复制 `self.scope.clone()`。
4. 修改 `crates/app/src/workspace/event_handling.rs`，从 scope 取得 `tab_id`；不要新增 view 层的独立 epoch 判断。
5. 更新构造该事件的 app、core 和真实 sshd 测试夹具。

#### 修复后验证

- core 单元测试：当前连接为 epoch 2 时，epoch 1 的 mismatch 必须被拒绝；epoch 2 的 mismatch 必须接受。
- app 行为测试：旧 mismatch 到达后，新连接仍为 `Connecting/Connected`；当前 mismatch 仍显示阻断错误。
- SFTP 集成测试：真实 sshd mismatch 事件回显原始 `session_id/session_epoch`。
- 回归运行：`cargo test -p macsftp-core -p macsftp-sftp -p macsftp-app`，再运行 `bash scripts/check.sh`。

---

## 3. sftp / core / app：传输生命周期

### SFTP-TRANSFER-001：上传规划完成后无法交接时没有终态事件

- **描述**：上传会先在阻塞线程完成本地 planning，并发出 `TransferPlanCompleted`。若开始时没有匹配 epoch 的 browsing session、session actor mailbox 的 `AcquireTransferConnection` 发送失败，或后续无法把 jobs 发送到 `TransferManager`，代码会直接返回或只写 WARN，不为已创建的 queued jobs 发 `TransferFailed/TransferPlanFailed`。UI 中计划及子任务可永久停在 `Queued`。
- **严重程度**：高
- **影响范围**：断线/重连竞态、陈旧 epoch、未连接标签、session actor channel 满/关闭、TransferManager 意外退出。普通上传受影响；远程编辑 upload-back 若遇到该路径，会使编辑会话停在 `UploadingBack`，等待永远不会来的终态事件。
- **证据状态**：当前源码静态确认；现有 `start_transfer_streams_a_local_upload_plan` 在未连接 session 的情况下只断言收到 `TransferPlanCompleted`，正好覆盖了缺陷的前半段，但没有断言随后必须收到失败终态。

#### 复现步骤

1. 准备一个存在的本地文件。
2. 启动 mock runtime，但不要为 `TabId(1)` 建立连接。
3. 发送一个 Upload `StartTransferCommand`，其 tab/epoch 在 `sessions` 中无匹配项。
4. 观察事件依次出现：`TransferPlanStarted`、`TransferPlanProgress`、`TransferPlanCompleted`。
5. `runtime.rs:892` 中 `transfer_connection_rx == None`，任务直接返回。
6. 继续等待，不会收到这些 planned jobs 的 `TransferFailed`，TransferStore 中任务保持 `Queued`。
7. 另一复现分支：关闭 `TransferManager` receiver 后发送有效上传；`manager_tx.send_async()` 失败时只输出 WARN，同样无 job 终态。

#### 原因分析

- `crates/sftp/src/runtime.rs:749-759` 使用 `Option` 折叠了三种不同失败：无 session、epoch 不匹配、`try_send` 失败。
- `runtime.rs:821-831` 的上传 planning 与远端连接是否存在无关，因此仍正常创建计划和 jobs。
- `runtime.rs:892-894` 对 `None` 直接 `return`。
- `runtime.rs:895-914` 只有 oneshot sender 已成功创建、但 responder 后续丢失时才逐 job 发 `TransferFailed`。
- `runtime.rs:915-924` Enqueue 失败只记录 WARN，jobs 已从局部变量移走且没有终态补偿。
- 状态机缺少不变量：每个 `TransferPlanStarted`/planned job 必须最终收到 Completed、Failed 或 Cancelled。

#### 修复方案

1. 在 `crates/sftp/src/runtime.rs` 引入一个小型、现有文件内的辅助函数，例如 `fail_planned_jobs(event_tx, jobs, error)`，逐 job 发送同一可重试错误。
2. 在 planning task 完成后：
   - `transfer_connection_rx == None`：发送 `ErrorCode::ChannelClosed` 的 job 失败终态；
   - `connection_rx.await` 失败：复用同一辅助函数；
   - `TransferManagerRequest::Enqueue` 发送失败：从 `SendError` 中取回 request/jobs（若 flume API 允许）并逐 job 失败；或改为先保留 job ID 列表，失败时按 ID 发终态。
3. 更简单且更早的防线：在分配 plan ID 之前显式验证 session/epoch；无匹配 session 时直接发一个可关联到 root job 的失败计划。需要确保 UI 能终结 root job，不能只发一个没有 `TransferPlanStarted` 上下文的孤立 plan ID。
4. 保持下载当前的显式 `TransferPlanFailed` 行为，并统一上传/下载错误语义。
5. 对 retry map 和 cancellation map 在所有失败分支做对称清理。

#### 修复后验证

- runtime 单元测试：无 session 的上传必须在 planning 后收到每个 child job 的 `TransferFailed`，且 TransferStore 无 `Queued` 残留。
- stale epoch 测试：连接 epoch 2 时发送 epoch 1 上传，必须快速进入可重试失败终态。
- actor channel 满/关闭测试：不能永久 queued。
- TransferManager receiver 关闭测试：所有 jobs 必须失败，且 runtime task 不 panic。
- app 远程编辑测试：upload-back 遇到 handoff 失败后回到 `Editing`，`active_transfer` 清空，可以再次保存重试。

---

## 4. app：远程编辑

### APP-EDIT-001：保存前使用缓存目录快照，可能静默覆盖远端并发修改

- **描述**：编辑器保存本地临时文件后，`EditWatcher` 不是向服务器实时查询目标文件，而是从当前窗口的 `tab.remote.entries` 目录缓存读取 `(size, mtime)`。只要用户没有刷新远程目录，另一个客户端对远程文件的修改不会反映在缓存中，比较仍会认为“远端未变化”，随后以 `OverwriteAll` 上传并静默覆盖外部修改。
- **严重程度**：高
- **影响范围**：所有远程编辑并发场景，尤其是多人编辑、服务器生成文件、命令行同时修改。影响是远端数据丢失/覆盖。
- **证据状态**：当前源码静态确认，可确定性场景复现；现有测试只替换 UI 列表中的 snapshot，没有覆盖“服务器已变但 UI 缓存未刷新”的真实路径。

#### 复现步骤

1. 在 macSFTP 中远程编辑 `/srv/a.txt`，记录初始远端 snapshot `(size=20, mtime=100)`。
2. 下载完成后，保持远程目录列表不刷新。
3. 在另一 SSH/SFTP 客户端修改 `/srv/a.txt`，例如变为 `(size=40, mtime=200)`。
4. macSFTP 的 `tab.remote.entries` 仍是 `(20,100)`。
5. 在本地编辑器保存临时文件。
6. `edit_watcher.rs:156` 调用 `current_remote_snapshot()`，得到缓存值 `(20,100)`，与 session baseline 相等。
7. watcher 构造 `OverwriteAll` upload-back，外部修改被覆盖且不出现 `RemoteConflict`。

#### 原因分析

- `crates/app/src/edit_watcher.rs:181-193` 明确从“拥有 tab 的窗口 listing”读取 snapshot。
- `tab_remote_is_ready` 只验证 connected、已有 path、未刷新；“未刷新”不代表目录数据是服务器当前值。
- UI listing 适合展示和导航，不是并发写入前的权威 compare-and-set 数据源。
- 当前 SFTP 命令协议缺少“对单个远程路径做实时 stat，并把结果与 edit session/request ID 关联”的流程。

#### 修复方案

1. 在 `core` 增加稳定请求 ID 和明确命令/事件：例如 `CheckRemoteEditSnapshot { edit_session_id, scope, path }` 与 `RemoteEditSnapshotChecked`。事件必须携带 `RemoteEventScope` 和 edit session ID，防止迟到结果污染新会话。
2. 在 `sftp/session_actor.rs` 用实时 `symlink_metadata/stat` 查询目标文件，不通过目录缓存。
3. watcher 检测到本地变化后先进入显式 `CheckingRemote` 相位，或保留 `Editing` 但记录 pending check；收到实时结果后：
   - snapshot 相等：才发送 upload-back；
   - 不相等/文件消失：进入 `RemoteConflict`；
   - stat 网络失败：保留本地文件并可重试，不得默认覆盖。
4. 上传前再次验证 session epoch；检查结果与上传命令之间仍有 TOCTOU 窗口。若服务器支持，可考虑在 adapter 内尽量缩小 check→upload 间隔；SFTP 本身无通用原子 CAS 时，UI 应诚实说明剩余窗口。
5. 不建议仅在每次保存时刷新整个目录：开销更高、关联更弱，也仍可能被无关目录事件覆盖。

#### 修复后验证

- 真实 sshd 集成测试：下载编辑副本后，用第二连接修改远端；本地保存必须进入 conflict，远端第二连接内容保持不变。
- 文件被删除/重命名测试：必须进入 conflict，不得重新创建并覆盖。
- 网络断开/stat 失败测试：保留 `Editing` 和本地内容，恢复后重试。
- stale check 测试：reconnect 后旧 epoch 的 stat 结果必须丢弃。
- 相同 snapshot 正常路径：只触发一次 upload-back。

### APP-EDIT-002：编辑临时目录创建错误被忽略且发生在 UI 同步路径

- **描述**：`start_edit_download()` 在 GPUI 事件路径同步调用 `std::fs::create_dir_all`，并通过 `let _ =` 丢弃错误。失败后仍注册 `Downloading` 会话并发送传输；错误只能在后续下载创建父目录时异步暴露，用户收到延迟且不精确的失败。
- **严重程度**：低
- **影响范围**：应用支持目录权限错误、磁盘只读/满、路径异常。通常后续 transfer 会失败并清理 session，因此当前证据不支持“永久搁浅”；主要是即时反馈、错误精度和 UI 线程纪律问题。
- **证据状态**：当前源码静态确认。

#### 复现步骤

1. 让 `<Application Support>/macSFTP/edits` 的父路径不可写，或在预期目录位置创建冲突文件。
2. 触发“远程编辑”。
3. `remote_edit.rs:164` 的 `create_dir_all` 失败但被忽略。
4. UI 仍显示“Opening for edit…”，并注册 `Downloading` session。
5. 后续下载在创建父目录/临时文件时失败，用户延迟收到泛化传输错误。

#### 原因分析

- `crates/app/src/workspace/remote_edit.rs:162-165` 明确静默丢弃 fallible operation。
- 该调用位于 UI action 处理路径，违反“GPUI 主线程不执行可能阻塞的文件系统操作”的工程边界。
- 会话状态在前置资源准备未成功时仍被推进。

#### 修复方案

1. 将目录准备放到 `platform` 的明确 API，并在 GPUI background executor 上执行；或删除 app 的预创建，由 SFTP download worker 作为唯一权威创建方。
2. 如果保留前置创建，只有成功后才能注册 edit session 和发送 `StartTransfer`。
3. 失败时映射为 `UserFacingError`/状态栏消息，包含 PermissionDenied/磁盘错误的可操作提示，但不要泄露不必要完整路径。
4. 为目录权限失败加入 app 行为测试；不要用硬编码全局路径。

#### 修复后验证

- 不可写 edits 目录：不注册 `Downloading` session、不发送传输命令、立即显示可理解错误。
- 正常目录：只创建一次并成功进入下载。
- UI 响应性 smoke：模拟慢文件系统操作时主线程不阻塞。

### APP-EDIT-003：编辑临时目录清理失败被静默忽略

- **描述**：远程编辑的多个退出路径使用 `let _ = std::fs::remove_dir_all(parent)`。权限、文件占用或 I/O 错误完全不可见，也没有记录残留目录。用户可能在 Application Support 中长期积累远端文件副本。
- **严重程度**：低
- **影响范围**：下载失败、关闭标签/窗口、冲突丢弃、本地临时文件消失等清理路径。涉及磁盘占用与远端内容副本的隐私留存。
- **证据状态**：当前源码静态确认。

#### 复现步骤

1. 建立远程编辑 session 并创建临时文件。
2. 让 session 目录不可删除，例如改变权限或保持特殊文件占用。
3. 触发下载失败、关闭 tab、DiscardLocal 或 watcher 回收。
4. session 从内存删除，但目录仍存在。
5. 日志和 UI 中没有清理失败记录；当前运行也没有 residual 记录可重试。

#### 原因分析

- 静默清理分布于：
  - `crates/app/src/workspace/remote_edit.rs:210,310,503`
  - `crates/app/src/event_coordinator.rs:226`
  - `crates/app/src/edit_watcher.rs:114`
- 项目已有 transfer residual temp 机制，但 edit session temp 未复用等价的可观测清理策略。
- 各调用点重复实现 best-effort 清理，错误策略不统一。

#### 修复方案

1. 在 `platform` 增加统一 `cleanup_edit_session_dir`，NotFound 视为成功，其他错误返回。
2. app 调用方至少写结构化 WARN（只记录 session ID/脱敏目录标识，不记录远端敏感路径）。
3. 若隐私要求更高，新增 edit-temp residual 记录，启动和正常退出时仅重试清理 macSFTP 自己登记的目录。
4. 保持清理失败不把已成功的传输反转为失败，但必须可诊断。

#### 修复后验证

- NotFound 清理无告警。
- PermissionDenied 产生 WARN/残留记录，session 状态仍按原流程结束。
- 下次启动可清理已登记的残留，并只作用于 app 自己的 edits 根目录。
- 敏感日志检查确保不输出远端文件名或完整私钥/用户路径。

---

## 5. 测试基础设施

### TEST-001：固定共享临时路径导致并发测试冲突

- **描述**：至少两个当前测试使用固定的系统临时目录名，并在开始/结束时递归删除。测试并发、多进程重复执行或上次进程残留时会互相覆盖，造成随机失败或让一个测试删除另一个测试正在使用的数据。
- **严重程度**：中
- **影响范围**：本地并行测试、CI 分片/重试、同时运行多个 cargo 命令。影响工程验证可信度，并违反仓库明确要求的“并发测试临时路径必须带唯一标识”。
- **证据状态**：当前源码静态确认。

#### 已确认位置

- `crates/platform/src/platform.rs:376`：`temp_dir().join("macsftp_read_local_test")`
- `crates/app/src/workspace/tests.rs:307`：`temp_dir().join("macsftp_sel_test")`

其他不少测试只使用 PID；同一测试二次并发调用仍可能冲突，应在修复时一并审查，但本报告不在无复现证据时逐个计 bug。

#### 复现步骤

1. 并发启动两个运行同一测试过滤器的 cargo test 进程。
2. 两进程使用同一固定路径。
3. 一个进程在另一个创建/读取 fixture 时执行 `remove_dir_all`。
4. 观察随机 NotFound、缺少条目、断言失败，或测试偶发通过。

#### 原因分析

- 临时路径没有 test label、进程内原子序号或随机唯一值。
- 通过“开头先删目录”掩盖残留，反而扩大了跨进程破坏窗口。
- 当前测试未使用 RAII 临时目录。

#### 修复方案

1. 优先使用 `tempfile::TempDir`；若不增加依赖，复用项目已有 `label + pid + AtomicU64/SystemTime nanos` 模式。
2. 每个测试 fixture 独立目录，禁止跨测试共享清理根。
3. 让 RAII 负责退出清理；显式清理失败时至少在测试中断言或输出上下文。
4. 审查 `transfer_planner.rs` 等仅 PID 路径，确保同一测试并行调用也唯一。

#### 修复后验证

- 同一测试过滤器启动 10 个并发进程，连续多轮无失败。
- `cargo test --workspace` 多次并发运行无临时目录互删。
- 测试结束后不留 fixture。

### SFTP-MOCK-001：mock actor 静默丢弃关键事件发送失败

- **描述**：`MockRemoteSessionActor` 和 `MockTransferJob` 的多个事件发送使用 `let _ = send_async(...).await`。当有界事件通道关闭时，mock 可能继续运行或静默结束而不暴露缺失的 `TabConnected`、`TabDisconnected`、`TransferQueued/Progress/Completed`。测试等待方可能超时，根因却被隐藏。
- **严重程度**：低
- **影响范围**：SFTP runtime/mock 测试可靠性和生产/测试语义一致性；不直接影响发布二进制的真实 SFTP 数据路径。
- **证据状态**：当前源码静态确认。

#### 复现步骤

1. 创建 mock actor/job 后提前 drop event receiver。
2. 让 actor 接受/reject host key，或运行 mock transfer。
3. send 返回错误但被丢弃。
4. actor 继续执行剩余逻辑或无诊断退出；上层无法区分“业务未发事件”和“通道已关闭”。

#### 原因分析

- `crates/sftp/src/mock_actor.rs:128-149,197-248` 多处静默丢弃 fallible send。
- mock 没有采用生产代码常见的“event channel closed 即 shutdown”约定。

#### 修复方案

1. 对结构事件和终态事件：send 失败立即 return，并在需要时让 `run()` 返回 `Result` 供测试断言。
2. progress 可在 channel closed 时立即停止任务，不应继续模拟。
3. 测试增加 receiver drop 场景，断言 actor 快速退出且不死锁。
4. 不把正常 cancellation 误报为错误；保持现有 cancellation 语义。

#### 修复后验证

- receiver 关闭测试：actor/job 在短超时内退出。
- 正常流程仍保持当前事件序列和 10Hz 节流。
- `cargo test -p macsftp-sftp` 全部通过。

---

## 6. 已知限制与待验证风险

### RISK-EDIT-001：同秒同大小远端修改仍可能漏检

- **分类**：已接受设计限制，不计入当前 bug 数量。
- **严重程度**：潜在高影响、低概率窗口。
- **原因**：SFTP 列表/属性 mtime 为整秒精度，`RemoteSnapshot` 只含 `size + modified_at`。同一秒内修改为相同字节数，snapshot 不变。
- **修复方向**：在解决 APP-EDIT-001 的实时 stat 后，如产品要进一步闭合窗口，需要引入内容 hash、版本文件/锁或服务端能力；这属于功能与性能取舍，不能仅靠比较逻辑修好。
- **验证**：真实 sshd 上在同一秒写入同长度不同内容，确认当前 snapshot 相等；引入 hash 方案后应进入 conflict。

### RISK-UI-001：缺少本次交互式 GUI smoke 签字

- **分类**：验证缺口，不是已证实代码 bug。
- **范围**：720×480、短窗口、Retina、亮/暗主题、键盘焦点、真实 10k 条目、多 tab、多 transfer、resize。
- **建议**：按 `docs/plans/2026-07-14-phase6-polish-audit.md` 建立可重复人工 checklist，发布证据中记录系统、分辨率、截图和结论。

### RISK-ENV-001：本机缺少 Metal 编译器，app/ui 门禁无法执行

- **分类**：验证环境阻断，不是项目 bug。
- **现象**：GPUI 0.2.2 build script 失败：`xcrun: error: unable to find utility "metal", not a developer tool or in PATH`。
- **影响**：本次无法运行 `cargo test -p macsftp-app`、UI 测试和完整 `scripts/check.sh`。
- **建议**：安装/切换到包含 Metal toolchain 的完整 Xcode，确认 `xcrun -f metal` 成功后重跑完整门禁。

---

## 7. 已排除的历史问题与误报

以下问题在历史文档中出现，但当前代码或测试表明已修复，不应作为当前 open bug 重复排期：

1. Connection Pool 的重复 `TabConnecting`、私钥加载失败未发 `AuthFailed`、缺少 `TabDisconnected` 等历史问题。
2. 远程编辑通过裸 `open` 引发命令执行风险；当前 `platform` 默认使用 `open -t`。
3. 远程编辑重连 epoch 搁浅、command channel 发送失败搁浅、关闭 tab/window 泄漏、路径单字段误关联、瞬时 stat 误回收、文件离开 listing 后盲覆盖、large edit modal 陈旧、RemoteConflict 临时文件回收等历史问题。
4. 旧文档所写多窗口 `session.json` last-writer-wins：当前已由进程级 `SessionCoordinator` 单写者修复，`collect_session()` 会一次收集全部存活窗口。
5. known_hosts 比较 comment 导致合法 key mismatch、revoked host 未优先阻断、hashed host 不匹配等历史安全问题；当前 unit 与真实 sshd 测试均覆盖通过。

---

## 8. 本次验证结果

### 已通过

- `cargo test -p macsftp-core -p macsftp-sftp`
  - core：52 passed
  - sftp unit：71 passed
  - password auth integration：1 passed
  - real sshd integration：21 passed（包括 10k remote entries、真实上传/下载、host-key、冲突、symlink）
- `cargo test -p macsftp-storage`
  - 45 passed，1 ignored（OS Keychain 专用测试）
- `bash scripts/test_keychain.sh`
  - 1 passed
- `cargo test -p macsftp-platform`
  - 17 passed
- `cargo clippy -p macsftp-core -p macsftp-sftp -p macsftp-storage -p macsftp-platform --all-targets -- -D warnings`
  - 通过，无 warning
- `cargo fmt --all --check`
  - 通过
- `scripts/check_architecture.sh`
  - 通过
- `scripts/check_sensitive_logs.sh`
  - 通过
- `git status --short`
  - 仅显示新增的 `outputs/` 报告目录；产品源码无改动

### 被环境阻断

- `bash scripts/check.sh`
  - 在 GPUI 0.2.2 Metal shader 构建阶段失败；错误为本机找不到 `metal` 工具。
  - 未进入 app/ui 的编译、clippy 和测试执行，因此不能声称全仓门禁通过。

---

## 9. 建议修复批次

### 批次 A：数据与状态终态（优先）

1. APP-EDIT-001：实时远端 snapshot 检查。
2. SFTP-TRANSFER-001：所有 transfer handoff 失败路径补终态。
3. 为两者先写失败回归测试，再改实现。

### 批次 B：会话安全边界

1. CORE-SFTP-001：`HostKeyMismatch` 改为完整 remote scope。
2. 补 core、app、真实 sshd 三层测试。

### 批次 C：工程可靠性

1. TEST-001：统一唯一临时目录。
2. APP-EDIT-002/003：统一编辑目录准备与清理 API。
3. SFTP-MOCK-001：mock 通道失败显式退出。

每个批次应保持单一主题，避免同时做无关重构。修复完成后，在具备完整 Xcode Metal 工具链的环境中执行 `bash scripts/check.sh`，并补交交互式 GUI smoke 证据。
