# macSFTP 当前架构

本文是 macSFTP 当前实现的架构约束，不是历史实施计划。已经完成的里程碑、评审过程和逐步实施清单保留在 Git 历史中，不在工作树内重复维护。

代码是行为事实来源；本文记录代码本身不容易表达、但修改时必须保持的边界和取舍。

## 1. 产品边界

macSFTP 是使用 GPUI 和 `russh + russh-sftp` 构建的 macOS 原生 SFTP 客户端。

当前能力：

- 多窗口、每窗口多标签；
- 本地与远端目录浏览；
- 文件和目录上传、下载、取消、重试及冲突处理；
- 密码、私钥、keyboard-interactive、SSH agent；
- 单跳 saved-profile jump host 和显式 ProxyCommand；
- OpenSSH-compatible `known_hosts` 子集；
- 外部编辑器远程编辑与显式保存回传；
- Profile、最近连接、窗口会话和残留临时文件持久化。

当前非目标：

- FTP、WebDAV、S3 等其他协议；
- 内置终端；
- 目录同步；
- 多跳 jump-host 链；
- 自动解析完整 `~/.ssh/config`；
- 完整 OpenSSH `known_hosts` grammar；
- 跨启动恢复传输任务；
- App Store、自动更新和后台常驻传输。

公开构建目前可以是 unsigned ZIP。Developer ID 签名、公证和正式二进制发布属于独立发布门禁，不能由普通构建成功替代。

## 2. Workspace 边界

```text
app -> ui
app -> core
app -> storage
app -> sftp

ui -> core
sftp -> core
sftp -> storage
storage -> core
platform -> core
test_support -> core
```

### `crates/core`

纯业务模型和状态机：tab、连接状态、command/event、传输、冲突、远程编辑、排序和错误码。禁止依赖 GPUI、russh、Tokio runtime 或 macOS API。

### `crates/app`

GPUI 入口、窗口、Entity、Action、事件协调和 UI 状态。它可以发送 runtime command 和消费 event，但不能直接调用 russh 或 Keychain。

### `crates/ui`

主题和可复用 GPUI 组件。它只接收展示模型，不拥有远端 session 或持久化状态。

### `crates/sftp`

Tokio runtime、russh adapter、物理连接池、浏览 actor、传输 planning/执行、host trust 和认证请求。生产 API 只构造真实 SSH runtime；mock actor 和 mock 构造器必须受 `#[cfg(test)]` 限制，不能进入发布构建或公共 API。该 crate 不能持有 GPUI Context、Window 或 Entity。

### `crates/storage`

版本化 JSON、原子写入、跨实例锁、Profile/Keychain 协调和 migration。敏感值不能进入普通配置。

### `crates/platform`

macOS 与本地文件系统边界：应用路径、本地目录读取、编辑器打开、权限/mtime/symlink 和诊断文件。

### `crates/test_support`

真实 sshd 测试夹具。它不进入产品行为。

## 3. 状态所有权

每个 `Workspace` 拥有一个窗口的：

- `AppState` / `TabStore`；
- window-local modal 队列；
- focus、popover、filter、scroll handle 等 view state。

进程级 GPUI globals 拥有：

- Profile、配置、最近连接和 residual-temp stores；
- 全局 `TransferStore` 与速率采样；
- 远程编辑 session；
- runtime event receiver；
- 多窗口 session checkpoint 协调。

传输 event 只能在进程级边界 reduce 一次，再由所有窗口读取共享快照。tab event 可以广播到窗口，但必须先经过 core 的陈旧事件校验。

process-owned 的 transfer、residual-temp 和远程编辑校验 event 不能在 `Workspace` 再保留第二套生产 reducer；新增 event 必须进入显式路由分支，不能依靠 wildcard 静默忽略。

长期业务状态不得只存在 view struct；hover、输入草稿、popover 展开状态等短期状态可以放在 view。

## 4. GPUI 与 Tokio 桥接

GPUI 主线程和 Tokio runtime 之间只通过 bounded `flume` command/event channel 通信。

```text
GPUI action
  -> RuntimeClient::try_send(AppCommand)
  -> Tokio command dispatcher
  -> browsing actor / TransferManager
  -> AppEvent
  -> process-wide AppEventCoordinator
  -> global reducer or owning Workspace
  -> cx.notify / refresh_windows
```

约束：

- GPUI 主线程不能 await 网络或执行大目录扫描；
- GPUI 发送 command 必须非阻塞；
- Tokio task 不能调用 GPUI API；
- event channel 满时由背压保护不可丢的状态转换；
- progress 在生产端节流，不能通过 UI 侧随机丢事件降载；
- runtime shutdown 必须取消 actor 和 transfer、拒绝 pending request，并在有界超时内结束；
- tab 关闭或 reconnect 必须让仍在握手/channel 初始化阶段的 session task 观察 cancellation；`JoinHandle` 必须被有界等待或最终 abort，不能只 drop 后让任务脱离所有权；
- command dispatcher 必须穷尽匹配，禁止兜底吞掉新增 command。

## 5. Command / Event 契约

`AppCommand` 只包含 runtime 实际消费的操作：连接生命周期、远端读取、文件操作、传输、host trust、keyboard-interactive、远程编辑校验和 shutdown。

本地目录读取由 app/platform 后台任务处理，不进入 SFTP runtime 协议。tab 的创建和关闭先更新 UI/core；只有需要释放 runtime session 时才发送关闭 command。

`AppEvent` 分三类：

1. 带 `RemoteEventScope` 的 browsing-session event；
2. 不依赖 tab 存活的全局 transfer event；
3. 只按 tab/epoch/check-id 相关联、但尚未到达 actor 的 dispatch failure。

不要为未来功能预留 enum variant。新增 variant 时必须同时存在生产者、消费者和测试。

## 6. ID 与陈旧事件

跨线程对象使用稳定 ID，例如 `TabId`、`SessionId`、`TransferId`、`TransferPlanId`、`TrustRequestId`、`ConflictRequestId` 和远程编辑 check ID。

`SessionId` 和 `session_epoch` 的唯一分配方是 UI/core。runtime 和 actor 只能回显 command 中的值。

远端事件只有同时满足以下条件才能修改 tab：

```text
tab 仍存在
AND event.session_id == 当前连接尝试
AND event.session_epoch == tab.session_epoch
```

reconnect 必须递增 epoch；tab 关闭后的迟到 event 必须被忽略。host-key 和 keyboard-interactive modal 还必须绑定各自 request ID，过期确认不能作用于替换 session。

## 7. SSH 连接与认证

每个 browsing tab 持有独立 actor，但可以通过 `ConnectionManager` 复用已认证的物理 SSH 连接。transfer 持有自己的 lease、队列和取消域；关闭 tab 不能取消已经进入全局队列的传输。

物理连接 key 包含目标、用户名和不含 secret 的保存 Profile 身份。Profile revision、jump profile revision 和稳定 Profile ID 参与复用判断；修改 Profile 后不能错误复用旧连接。

认证支持：

- Password：值只驻留于 zeroize 容器或 Keychain；
- Private key：Ed25519/ECDSA 由 ssh-key/russh 路径处理，RSA-SHA2 私钥签名使用 AWS-LC；
- Keyboard-interactive：支持多轮 prompt，每轮 request 绑定 tab/session/epoch，回答跨 runtime channel 后仍需清零；
- SSH agent：支持默认 `SSH_AUTH_SOCK` 或显式 socket，遍历普通 key 和 certificate；agent comment、key blob、签名输入输出禁止记录。

route 支持：

- Direct；
- 单跳 saved-profile JumpHost，jump profile 自身必须 Direct；
- ProxyCommand，通过 `/bin/sh -c` 执行用户显式配置的命令，并安全替换 `%h`、`%p`、`%r`、`%%`。

ProxyCommand 是明确的代码执行能力。UI 必须显示风险提示，日志不能记录完整命令。

## 8. 传输模型

目录传输必须使用 `TransferPlan`，不能伪装成单文件 job。planning 流式发布 child jobs；首个 child 立即可见，后续批量发布。

`TransferManager` 是进程级 owner。传输状态至少区分 planning、queued、running、waiting-for-conflict、completed、skipped、cancelled 和 failed。

规则：

- conflict 是等待用户决定的状态，不是普通失败；
- `apply_to_all` 只绑定当前 `TransferPlanId`；
- running transfer 使用显式 cancellation token；
- retry 重新获得可用连接，不长期强持有失败连接；
- `.macsftp-part-*` 不是正常目标冲突；
- temp cleanup 失败写入 `ResidualTempStore`，后续只清理由 macSFTP 记录的路径；
- residual cleanup command 无法路由到 live actor 时保留记录并写结构化 WARN，等待下一次连接重试，不能静默当作成功；
- 权限或 mtime 保留失败通常产生 warning，不推翻已成功的数据传输；
- symlink 默认复制 link 本身，不解引用。

## 9. 远程编辑

远程编辑 session 属于进程级状态，因为外部编辑器和传输可能跨窗口存活。tab 关闭必须清理该 tab 的 session 和临时目录；应用启动时使用新的 run namespace，避免外部编辑器缓存旧路径。

应用不监视外部编辑器的保存动作。用户在选中已打开编辑的远端文件后，通过明确的 **Upload Modified File** 操作发起上传。上传不能依赖 UI 缓存的目录 listing 授权覆盖远端文件。流程为：

```text
Editing
  -> 用户选择 Upload Modified File
  -> CheckingRemote（分配唯一 check id）
  -> actor 对目标执行实时 metadata 查询
  -> 快照一致：UploadingBack
  -> 快照不同：RemoteConflict
  -> 路由/检查失败：回到 Editing，保留本地文件并等待用户重试
```

结果应用前必须完整匹配 edit session、tab、session epoch、check ID、remote path 和发起检查时的本地 mtime。检查完成后还要再次 stat 本地文件；若用户在检查期间再次保存，本次结果不能授权上传，必须回到 Editing 等待用户再次点击。

已知限制：远端快照只比较 size 和整秒 mtime。同一秒内发生、且 size 不变的并发远端修改无法识别。没有内容哈希或服务端版本号前，不得声称已解决这一限制。

## 10. Known Hosts

host-key mismatch 必须阻断连接，不提供一键覆盖。unknown key 可以通过绑定 request/session 的 modal 选择信任并写入应用 known_hosts。

解析目标是 OpenSSH-compatible 子集，包含项目测试覆盖的普通、hashed 和 revoked entry。无法解析的行按行忽略并记录 WARN，不能使整个文件不可用。

比较必须只比较 key 内容，例如 `key_data()`；不能比较带 comment 等展示字段的整个对象。

## 11. Profile、Keychain 与持久化

普通配置只保存非敏感字段和 `SecretRef`。password 和 private-key passphrase 只能进入 Keychain；agent socket、私钥路径和 ProxyCommand 也不能出现在默认诊断日志中。

Profile 更新由 `ProfileStore` 协调：

1. 校验请求和 route；
2. 写入或读取 Keychain；
3. 原子替换版本化 profiles 文件；
4. 在未提交失败时补偿回滚 Keychain；
5. 提交成功后 best-effort 删除孤立 secret，并将失败作为 warning 返回。

Profile 写入只有一个产品入口：Settings → Profiles 将编辑草稿转换为 `ProfileSaveRequest`，storage 通过 `ProfileStore::save_request` 提交。Connect 表单只选择已有 Profile 或建立临时连接，不创建、更新或删除 Profile；它通过 “Manage…” 进入 Settings。禁止为 Connect 或其他 UI 再增加并行的保存适配器。

`ProfileStore::save_request` 是唯一公开保存入口；更底层的 profile-file 写入只允许 storage 内部调用，测试夹具只能使用 `#[cfg(test)]` helper。Connect 中手工输入的 credential 只用于当前连接，不得因曾选择 Profile 而回写持久化状态。

`profiles.json` v5 删除了从未有生产消费者的 `group_id` 预留字段；读取 v4 时忽略该字段，并在下一次成功写入时升级为 v5。未来真正实现 Profile group/folder 时必须重新设计模型和 migration，不能复用历史占位语义。

损坏或未来版本文件不能被空默认值静默覆盖。session 文件恢复失败时必须先保存原始 corrupt backup，成功后才重新开放 checkpoint。

跨启动只恢复窗口、tab 和非敏感连接元数据；不恢复 transfer catalog 或 secret。

## 12. UI 约束

- 第一屏是可操作的文件工作区；
- 目录列表必须虚拟化，10k entries 不创建长期 row entity；
- 可滚动 surface 使用统一的 theme-aware scrollbar；虚拟列表和普通 scroll container 分别绑定各自 handle，但共享 overflow、drag、track paging 和 resize 语义；
- selection 存稳定 path，不存易漂移的 row index；
- icon-only button 必须有 tooltip 或 label；
- modal 必须有标题、主操作、取消路径和 request 绑定；
- 慢操作必须显示 loading、progress 或状态文本；
- 用户可见错误由 app/ui 映射，core 使用稳定 error code 和参数；
- UI 改动检查窄窗口、短窗口、Retina、明暗主题、键盘和焦点状态；
- 可见改动提供截图或明确的视觉验证说明。

## 13. 错误与日志

可恢复错误不能 panic。异步失败必须进入 UI 或经过审计的日志路径。

日志允许记录 host、port、username、作用域 ID 和枚举化失败标签；禁止记录：

- password、passphrase、keyboard-interactive answer；
- secret ref 的值；
- 完整私钥路径；
- ProxyCommand 内容；
- agent socket、identity blob、comment、签名输入或输出；
- 未审核的第三方错误原文或完整 event Debug。

默认过滤器只开启经过审计的连接生命周期 target。开发者可以通过 `RUST_LOG` 临时覆盖，但代码本身仍不能包含敏感 tracing 参数。

## 14. 测试与交付

最低验证按改动风险选择：

- core 状态机：unit tests；
- SFTP adapter、认证、jump host、ProxyCommand：真实 sshd integration tests；
- password 和 keyboard-interactive：Docker/PAM gate；
- Keychain：隔离 macOS Keychain gate；
- runtime bridge：bounded channel、背压、shutdown、陈旧事件；
- UI：action、modal、tab、多窗口、focus、transfer drawer；
- 性能：10k entries、并发 transfer、快速切 tab、窗口 resize；
- 发布：`scripts/check.sh`、依赖策略、app bundle 构建和 release evidence。

并发测试必须使用带测试名、进程或序列号的唯一临时路径。无 Docker 时允许本地 integration test 明确 skip，但 CI 必须运行完整门禁。

## 15. 需要显式决策的扩展

下列功能不能通过“预留字段/空 enum variant”提前设计；真正实施时再增加模型、migration 和测试：

- Profile group/folder；
- OpenSSH config 导入；
- 多跳 jump-host；
- 传输跨启动恢复；
- 自动更新；
- 内容哈希级远程编辑冲突检测。

新增中等以上功能前，设计说明只需记录当前问题、所有权、持久化/安全边界、用户可见取舍和验证方式。功能交付后应把仍有效的约束合并回本文，并删除已完成的逐步实施计划。
