# macSFTP 当前架构与 UI/UX 约束

本文记录 macSFTP 当前实现中代码本身不容易表达、但修改时必须保持的边界和取舍。已经完成的里程碑、评审过程和逐步实施清单保留在 Git 历史中，不在工作树内重复维护。

代码是行为事实来源；架构改动前必读本文。

## 1. 产品边界

macSFTP 是使用 GPUI 和 `russh + russh-sftp` 构建的 macOS 原生 SFTP 客户端。

当前能力：

- 多窗口、每窗口多标签；本地与远端目录浏览；
- 文件和目录上传、下载、取消、重试及冲突处理；
- 密码、私钥、keyboard-interactive 和 SSH agent 认证；
- 单跳 saved-profile jump host 和显式 ProxyCommand；
- OpenSSH-compatible `known_hosts` 子集；
- 外部编辑器远程编辑与显式保存回传；
- Profile、最近连接、窗口会话和残留临时文件持久化。

当前非目标（不得用预留字段或空 enum variant 提前设计）：FTP/WebDAV/S3 等其他协议、内置终端、目录同步、多跳 jump-host 链、自动解析完整 `~/.ssh/config`、完整 OpenSSH `known_hosts` grammar、跨启动恢复传输、App Store、自动更新和后台常驻传输。

公开构建目前可以是 unsigned ZIP。Developer ID 签名、公证和正式二进制发布属于独立发布门禁，不能由普通构建成功替代。

## 2. Workspace 边界

```text
app -> ui, core, storage, sftp, native-menu
ui -> core
sftp -> core, storage
storage -> core
platform -> core
test_support -> core
```

- `core`：纯业务模型和状态机（tab、连接状态、command/event、传输、冲突、远程编辑、排序、错误码）。禁止依赖 GPUI、russh、Tokio runtime 或 macOS API。
- `app`：GPUI 入口、窗口、Entity、Action、事件协调和 UI 状态。可以发送 runtime command 并消费 event，不能直接调用 russh 或 Keychain。
- `ui`：主题和可复用 GPUI 组件。只接收展示模型，不拥有远端 session 或持久化状态。
- `native-menu`：macOS AppKit 菜单呈现器（Base `NativeMenu` 模型 → NSMenu），只被 `ui` 的输入右键菜单和 `app` 的文件列表右键菜单调用，不持有 GPUI Entity、Window 借用或业务草稿。
- `sftp`：Tokio runtime、russh adapter、物理连接池、浏览 actor、传输 planning/执行、host trust 和认证请求。不能持有 GPUI Context、Window 或 Entity。生产 API 只构造真实 SSH runtime；mock actor 和 mock 构造器必须受 `#[cfg(test)]` 限制，不能进入发布构建或公共 API。
- `storage`：版本化 JSON、原子写入、跨实例锁、Profile/Keychain 协调和 migration。敏感值不能进入普通配置。
- `platform`：macOS 与本地文件系统边界：应用路径、本地目录读取、编辑器打开、权限/mtime/symlink 和诊断文件。
- `test_support`：真实 sshd 测试夹具，不进入产品行为。

## 3. 状态所有权

每个 `Workspace` 拥有一个窗口的 `AppState`/`TabStore`、window-local modal 队列，以及 focus、popover、filter、scroll handle 等 view state。

进程级 GPUI globals 拥有 Profile、配置、最近连接和 residual-temp stores、全局 `TransferStore` 与速率采样、远程编辑 session、runtime event receiver 和多窗口 session checkpoint 协调。

- 传输 event 只能在进程级边界 reduce 一次，再由所有窗口读取共享快照；tab event 可以广播到窗口，但必须先经过 core 的陈旧事件校验。
- process-owned 的 transfer、residual-temp 和远程编辑校验 event 不能在 `Workspace` 再保留第二套生产 reducer；新增 event 必须进入显式路由分支，不能依靠 wildcard 静默忽略。
- 长期业务状态不得只存在 view struct；hover、输入草稿、popover 展开状态等短期状态可以放在 view。

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

- GPUI 主线程不能 await 网络或执行大目录扫描；发送 command 必须非阻塞。
- Tokio task 不能调用 GPUI API；event channel 满时由背压保护不可丢的状态转换。
- progress 在生产端节流，不能通过 UI 侧随机丢事件降载。
- runtime shutdown 必须取消 actor 和 transfer、拒绝 pending request，并在有界超时内结束。
- tab 关闭或 reconnect 必须让仍在握手/channel 初始化阶段的 session task 观察 cancellation；`JoinHandle` 必须被有界等待或最终 abort，不能只 drop 后让任务脱离所有权。
- command dispatcher 必须穷尽匹配，禁止兜底吞掉新增 command。

## 5. Command / Event 契约

`AppCommand` 只包含 runtime 实际消费的操作：连接生命周期、远端读取、文件操作、传输、host trust、keyboard-interactive、远程编辑校验和 shutdown。本地目录读取由 app/platform 后台任务处理，不进入 SFTP runtime 协议。tab 的创建和关闭先更新 UI/core；只有需要释放 runtime session 时才发送关闭 command。

`AppEvent` 分三类：带 `RemoteEventScope` 的 browsing-session event；不依赖 tab 存活的全局 transfer event；只按 tab/epoch/check-id 相关联、但尚未到达 actor 的 dispatch failure。

不要为未来功能预留 enum variant。新增 variant 时必须同时存在生产者、消费者和测试。

## 6. ID 与陈旧事件

跨线程对象使用稳定 ID：`TabId`、`SessionId`、`TransferId`、`TransferPlanId`、`TrustRequestId`、`ConflictRequestId` 和远程编辑 check ID。

`SessionId` 和 `session_epoch` 的唯一分配方是 UI/core；runtime 和 actor 只能回显 command 中的值。

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

route 支持 Direct、单跳 saved-profile JumpHost（jump profile 自身必须 Direct）和 ProxyCommand。ProxyCommand 通过 `/bin/sh -c` 执行用户显式配置的命令，并安全替换 `%h`、`%p`、`%r`、`%%`；它是明确的代码执行能力，UI 必须显示风险提示，日志不能记录完整命令。

## 8. 传输模型

目录传输必须使用 `TransferPlan`，不能伪装成单文件 job。planning 流式发布 child jobs；首个 child 立即可见，后续批量发布。`TransferManager` 是进程级 owner，传输状态至少区分 planning、queued、running、waiting-for-conflict、completed、skipped、cancelled 和 failed。

传输完成后必须让文件列表跟上磁盘事实：一个 plan 进入终态（completed / failed / cancelled，单文件 plan 由 `TransferCompleted` 触发）时，`AppEventCoordinator::reload_panes_for_finished_plan` 取该 plan 根 job 与全部 child job 的目标路径的父目录，只重读当前活动 tab 中仍显示在那些目录的 pane。重读走 `reload_local_directory` / `reload_remote_directory`，与用户主动导航的区别是保留 selection、scroll 与状态文案，且不推送导航历史。禁止用 `request_local_directory` / `request_remote_directory` 直接做这件事——它们会清空 selection 并重置滚动位置，用户刚看到结果就丢了上下文。pane 显示其它目录时不得被拉回文件落地的目录。

- conflict 是等待用户决定的状态，不是普通失败；
- `apply_to_all` 只绑定当前 `TransferPlanId`；
- running transfer 使用显式 cancellation token；
- retry 重新获得可用连接，不长期强持有失败连接；
- `.macsftp-part-*` 不是正常目标冲突；
- temp cleanup 失败写入 `ResidualTempStore`，后续只清理由 macSFTP 记录的路径；
- residual cleanup command 无法路由到 live actor 时保留记录并写结构化 WARN，等待下一次连接重试，不能静默当作成功；
- 权限或 mtime 保留失败通常产生 warning，不推翻已成功的数据传输；
- symlink 默认复制 link 本身，不解引用；下载时 target 为空、绝对路径、含 `\` 或含 `..` 路径分量的一律拒绝（`UnsupportedSymlink`），因为本地写入无法依赖服务端解析来保证 target 仍留在下载树内；
- 浏览时 link 仍显示为 symlink，列表不做 per-entry stat；用户打开时由 actor 解析一次类型：目录 link 按逻辑路径进入（服务端自行解析），文件 link 进入编辑且后续下载、上传、远端快照检查都使用解析后的目标路径（写穿透），链接本身保持不变。文件 link 的写穿透起点按三条路径确定：(1) 服务端 `REALPATH` 返回绝对路径时直接使用；(2) 返回相对路径（不符合 SFTP v3）时拒绝；(3) 服务端不支持 `REALPATH` 时退回 `readlink`，但 link target 含 `..`（无论相对还是绝对）一律拒绝。安全依据是「客户端不做 `..` 折叠，父路径由服务端解析」，不是「已经 canonicalize」。若 link 指向的目标又落在目录上，按目录打开；断链、环、无权限只报状态，不移动 pane。

## 9. 远程编辑

远程编辑 session 属于进程级状态，因为外部编辑器和传输可能跨窗口存活。tab 关闭必须清理该 tab 的 session 和临时目录；应用启动时使用新的 run namespace，避免外部编辑器缓存旧路径。

应用监视编辑会话临时目录中的保存动作，但只用于在文件列表和状态栏显示 dirty 标记；它绝不触发上传，也不因此放宽远端一致性检查。应用自身写入临时副本（下载完成、`DiscardLocal` 重新下载）必须按 size+mtime baseline 抑制，不能被误报成本地修改。用户在选中已打开编辑的远端文件后，通过明确的 **Upload Changes** 操作发起上传。上传不能依赖 UI 缓存的目录 listing 授权覆盖远端文件。流程为：

```text
Editing
  -> 用户选择 Upload Changes
  -> CheckingRemote（分配唯一 check id）
  -> actor 对目标执行实时 metadata 查询
  -> 快照一致：UploadingBack
  -> 快照不同：RemoteConflict
  -> 路由/检查失败：回到 Editing，保留本地文件并等待用户重试
```

结果应用前必须完整匹配 edit session、tab、session epoch、check ID、remote path 和发起检查时的本地 mtime。检查完成后还要再次 stat 本地文件；若用户在检查期间再次保存，本次结果不能授权上传，必须回到 Editing 等待用户再次点击。

文件软链的编辑是写穿透：session 的 `remote_path` 保留用户看到的链接路径（选择、dirty 标记、状态文案），`io_path` 保存服务端解析后的目标路径，下载、`CheckRemoteEditSnapshot`、冲突比较下载和上传全部使用 `io_path`。目标路径的来源按服务端能力分三种：`REALPATH` 返回绝对路径时直接使用；返回相对路径（不符合 SFTP v3）时拒绝；不支持 `REALPATH` 时退回 `readlink` + 词法拼接，但 target 含 `..` 时拒绝。去重键是 `(connection_key, io_path)`，因此先后打开链接和它的目标只会得到一个编辑 session；若同一链接在会话存活期间被重新指向不同目标，再次打开会被拒绝并提示先结束已有编辑，而不是静默产生两个共享 `remote_path` 的会话。目录刷新不会用链接自身的 lstat `(size, mtime)` 覆盖写穿透 session 的 baseline。

已知限制：远端快照只比较 size 和整秒 mtime。同一秒内发生、且 size 不变的并发远端修改无法识别。没有内容哈希或服务端版本号前，不得声称已解决这一限制。

## 10. Known Hosts

host-key mismatch 必须阻断连接，不提供一键覆盖。unknown key 可以通过绑定 request/session 的 modal 选择信任并写入应用 known_hosts。

解析目标是 OpenSSH-compatible 子集，包含项目测试覆盖的普通、hashed 和 revoked entry。无法解析的行按行忽略并记录 WARN，不能使整个文件不可用。

比较必须只比较 key 内容，例如 `key_data()`；不能比较带 comment 等展示字段的整个对象。

## 11. Profile、Keychain 与持久化

普通配置只保存非敏感字段和 `SecretRef`。password 和 private-key passphrase 只能进入 Keychain；agent socket、私钥路径和 ProxyCommand 也不能出现在默认诊断日志中。

`config.json` 的 `metadata_policy.preserve_mtime` / `.preserve_permissions` 是新建用户上传、下载的默认元数据策略；旧文件缺少该字段时按 `true` 读取，因此新增字段只使用 `serde(default)`，不 bump `CURRENT_VERSION`。偏好只影响新 plan：retry 复用 job 已捕获的策略；remote-edit 回传固定保留 mtime，因为其冲突基线由本地临时文件的 mtime 推导。UI 不暴露没有生产消费者的 `preserve_symlinks`。目标端无法应用元数据时只能记录 warning，已成功传输的数据不能因此变成整体失败。

Profile 更新由 `ProfileStore` 协调：校验请求和 route → 写入或读取 Keychain → 原子替换版本化 profiles 文件 → 未提交失败时补偿回滚 Keychain → 提交成功后 best-effort 删除孤立 secret，并把失败作为 warning 返回。

- Profile 写入只有一个产品入口：Settings → Saved Connections 将编辑草稿转换为 `ProfileSaveRequest`，storage 通过 `ProfileStore::save_request` 提交。Connect 表单只选择已有 Profile 或建立临时连接，并通过 “Manage Connections…” 进入 Settings；禁止为 Connect 或其他 UI 增加并行的保存适配器。
- `ProfileStore::save_request` 是唯一公开保存入口；更底层的 profile-file 写入只允许 storage 内部调用，测试夹具只能使用 `#[cfg(test)]` helper。Connect 中手工输入的 credential 只用于当前连接，不得因曾选择 Profile 而回写持久化状态。
- `profiles.json` v5 删除了从未有生产消费者的 `group_id` 预留字段；读取 v4 时忽略该字段，并在下一次成功写入时升级为 v5。未来真正实现 Profile group/folder 时必须重新设计模型和 migration，不能复用历史占位语义。
- 损坏或未来版本文件不能被空默认值静默覆盖。session 文件恢复失败时必须先保存原始 corrupt backup，成功后才重新开放 checkpoint。
- 跨启动只恢复窗口、tab 和非敏感连接元数据；不恢复 transfer catalog 或 secret。

## 12. UI 与 UX 约束

设计目标参考 Zed 的产品特征但不复制 IDE：macSFTP 是高密度、低延迟、键盘友好的 SFTP 桌面工具，界面应响应快速、视觉克制、信息密集、语义明确、状态透明。本节只约束第 1 节已定义的产品界面，不引入新功能。

### 12.1 布局与信息密度

布局固定为：顶部 tab bar、左右文件 pane、底部 transfer drawer、底部 status bar。

```text
+------------------------------------------------------+
| Tab Bar                                              |
+----------------------+-------------------------------+
| Local Pane           | Remote Pane                   |
| path bar             | path bar                      |
| table/list           | table/list                    |
+----------------------+-------------------------------+
| Transfer Drawer                                      |
+------------------------------------------------------+
| Status Bar                                           |
+------------------------------------------------------+
```

- 第一屏必须是可操作的文件工作区，优先显示连接 tab、local pane、remote pane、transfer drawer；不得出现欢迎页、hero 区、说明卡片或大面积品牌展示。
- local pane 默认在左、remote pane 默认在右，两者视觉结构对称但可以处于不同状态；当前聚焦 pane 有清晰而克制的 active indicator。
- transfer drawer 默认位于底部，可以折叠，不能遮挡主文件列表；折叠后 status bar 仍须简要提示 active 或 failed transfer。
- 空文件夹、加载和无匹配状态填满列表 viewport 后居中；首次未连接的远端 pane 居中显示连接入口及说明，最近连接列表保持靠上以便扫描，并用统一的 scrollbar 保证短窗口中仍可访问全部入口。
- 调整 pane 或窗口尺寸时，路径栏、表头、滚动区域和空状态布局必须保持完整。
- 路径栏按功能成组：导航（后退、前进、上级、刷新）、面包屑、文件操作（复制路径、隐藏文件、新建文件夹、删除）、传输（上传或下载）；组间用 1px 竖线分隔，图标命中区保持冻结的 26×22，窄 pane 下优先保证面包屑的末段（当前目录）可见。
- 空状态只提供下一步操作（如 `Open Connection`、`Choose Local Folder`、`Retry`），不含长篇功能说明。
- 文件列表、传输列表、tab、路径栏和状态栏保持紧凑；组件高度稳定，hover、loading、progress 不引发布局变化。
- 状态栏消息是单次动作的瞬时结果，必须过期（`StatusLine::LIFETIME`，6s）：`show_status` 在写入时启动一次性 timer，到点用 generation guard 清除自己的那条，更晚的消息不能被更早的 timer 清掉。后台目录读取只允许退役自己设置的 "Loading files on your Mac…" 行，不得清除用户在加载期间得到的其它消息。

### 12.2 交互入口

- 高频操作（新建/关闭/切换 tab、连接、刷新、返回上级、上传、下载、取消传输、重试传输、打开 transfer drawer）至少提供按钮、menu bar item、快捷键、command palette action 中的两种入口。
- 作用于 tab / pane / 传输 / 设置的 app-level action 必须可被 command palette 搜索，action 名称使用稳定动词短语（如 `NewTab`、`RefreshPane`、`NavigateBack`、`OpenSettings`），优先沿用本文中的名称；action id 不随 UI 文案变化。窗口与应用生命周期命令（New Window、Close Window、Minimize、Zoom、Bring All to Front、Hide/Quit）留在 macOS 标准菜单位置，不进入 palette。
- 菜单栏由 `app_menus::menu_bar()` 单一定义，菜单项只能引用与快捷键、palette 相同的 action，禁止在菜单里另造一套；`app_menus` 的一致性测试把「菜单项 ↔ 快捷键绑定 ↔ palette id」三者钉在一起，改名或删除命令时三处必须同时更新。
- 菜单文案遵循 HIG 省略号：会打开需要输入或确认的面板的命令带省略号（New Folder…、Rename…、Delete…、Go to Folder…），即时完成的命令不带。右键菜单与菜单栏对同一命令使用同一写法。
- Edit 菜单使用 `MenuItem::os_action` 走 AppKit 标准 selector（`cut:`/`copy:`/`paste:`/`selectAll:`/`undo:`/`redo:`），dispatch 的仍是 `gpui-base` input action，不得自造文本编辑 action（§12.14）。
- 平台支持 `MenuItem::checked`，但菜单栏是应用级的一份，在启动时构建一次（`main.rs` 的 `set_menus(menu_bar())`），而排序状态属于每个 tab（两个 pane 共用），单个 checkmark 无法表达多 tab 下的当前状态——要显示它就必须在排序、tab 或窗口焦点变化时重建整个菜单栏，目前不做。有状态的命令因此写成显式取值（例如 Sort By ▸ Name (A→Z)/(Z→A)）；当前排序状态的显示位置只有文件列表表头（§12.3）。
- command palette 不得暴露 runtime、channel、actor、crate 等内部实现细节；用户忘记快捷键时必须能用 command palette 完成同一操作。
- path bar 必须支持复制当前路径，local 和 remote 都保留 back、up、refresh；路径过长从中间省略，loading 时不得消失，只增加局部 spinner 或 progress indicator。Copy Path 复制的是选中条目自身的完整路径（多选时每行一条），只有没有选中任何条目时才回落到当前目录路径——右键菜单会先选中指针下的行，因此右键某个文件再 Copy Path 得到的必须是该文件的路径，而不是它所在目录。

### 12.3 文件列表与滚动

- 文件列表必须虚拟化，10k entries 不创建长期 row entity；表头显示排序状态；selection 绑定稳定 id/path，禁止绑定 row index。
- 键盘导航覆盖上/下、page up/down、home/end、shift 多选和 enter 默认动作；directory、file、symlink 和 hidden row 有可区分但克制的 icon 或 marker；hover action 易于发现但不改变列宽或 row 高度。
- 聚焦的 pane 里键入字母是 Finder 式 type-select：前缀累积约 1 秒（连续按键继续同一前缀；超时、导航、切 tab、Escape 后重置），跳到第一个匹配项并滚动到它，从当前 selection 之后扫描并在末尾回绕，所以重复按同一字母会依次跳过匹配项；前缀匹配不到时退回最后键入的字符、不改变 selection。列表不缩小、不出现筛选栏。筛选只保留显式入口：⌘F（Filter Files）、菜单/palette 命令或筛选字段本身，筛选栏出现后键入才进入筛选。
- Enter 打开选中项、F2 重命名，不采用 Finder 的 Return=重命名：行内编辑需要 macOS 完整键盘访问的路径，且会让 Enter 的语义随 selection 状态漂移；重命名另有 File 菜单项与 palette 命令，F2 不是唯一入口。
- 可滚动 surface 使用统一的 theme-aware scrollbar：虚拟列表和普通 scroll container 各自绑定 handle，但共享 overflow、drag、track paging 和 resize 语义。
- 只在内容溢出时显示 track/thumb，thumb 必须可拖动、track 点击按 viewport 翻页、滚轮与键盘滚动后位置同步；隐藏原生滚动条避免双滚动条。
- 窄 pane、短窗口和动态内容变化后，thumb 大小与位置必须重新反映真实 viewport，不能遮挡主要操作或造成列表宽度跳变。
- 跨 pane 拖拽携带的是整个选区，而不是被抓住的那一行：拖拽 payload 在抓取行属于当前选区时展开为分区的全部选中路径，否则只携带该行。拖到对侧 pane 时必须搬走全部路径，不能只传第一个文件；资源管理器里拖一个已选项就是搬整组选中项。上传把整组交给同一个 plan（本地遍历不需要连接）；下载因为远端 planner 只解析一棵远端树，必须**每个源各发一个单源 plan**，目标为「本地目录/源名」。

### 12.4 安全决策显式

- Host key unknown modal 必须展示 host、port、key type、fingerprint、known_hosts 写入目标和主操作。
- Host key mismatch 必须阻断连接，主操作只能是取消或查看详情，不得把“继续连接”作为默认操作。
- 所有安全 modal 绑定 request id 和 session epoch；modal 过期后确认按钮必须失效。
- 密码、passphrase、私钥完整路径不得出现在 UI 错误、日志预览、tooltip 或复制文本中；credential modal 不得在标题或说明中暴露 secret 值。

### 12.5 视觉系统

- 支持 dark 和 light 两套 theme token，默认值分别采用 One Dark 和 One Light；界面以中性灰阶为主，强调色只用于焦点、主操作、连接成功、选中态和进度。
- 错误、警告、成功和信息色具有固定语义，同一颜色不得表示无关含义；颜色不能作为唯一状态表达，状态还必须包含文本或图标。
- 层级通过边线、背景、间距和文字权重表达，不依赖厚重阴影；面板之间用 1px 分隔线或相邻背景面区分；page section 内禁止嵌套卡片。
- 阴影只能来自 theme 的 elevation scale，按浮层等级命名，不允许调用点自行拼 shadow；三个等级有固定顺序，低等级的浮层不得看起来比高等级的更远：
  - `Theme::shadow_chip`：贴在表面上的单行小浮层，目前是 tooltip。
  - `Theme::shadow_popover`：独立的短命浮层，目前是 command palette、tab switcher 和拖拽预览。
  - `Theme::shadow_modal`：modal / sheet 卡片。
  `elevation_shadow_token_stays_light` 测试守住等级顺序以及透明度、模糊半径的上限。
- 需要压暗窗口背景的浮层（modal、command palette、tab switcher）背后的遮罩统一取 `Theme::scrim()`；调用点不写裸色值，一处变暗处处一致。
- 窗口顶边那一行 chrome 用 `Theme::chrome_highlight()` 画 1px 高光边线（dark 取亮线、light 取暗线），这是 chrome 唯一的材质提示；chrome 本身保持不透明，不做玻璃或半透明（§12.15）。
- 卡片内部的列表、选择面板不叠加 elevation：它们与卡片同层，用 1px 边线区分；浮层阴影只给真正浮在窗口之上的表面，不在卡片里再叠一层卡片。
- 空态分两行表达：lead 行（13px / MEDIUM / 主文本色）给结论，detail 行（12px / muted / 居中）给原因或恢复提示；单行空态只保留 lead 行。错误的 title 与 message 不得再用破折号拼成一行。层级关系对齐首次连接卡片（`remote_empty_with_recents` 的标题与说明），尺寸整体小一档，因为空态位于 pane 内。
- 圆角克制：工具按钮、输入框、popover、modal 使用小圆角；文件行、tab、状态条不用大圆角。
- 字体分开 `ui_font_family` 和 `mono_font_family`；文件名、路径、权限、大小、mtime、速度、ETA 等表格数据使用 UI 字体，需要按列对齐时使用 tabular numbers 或等宽字体；正文和控件不用 hero 级字号；文件列表、传输列表和状态栏使用固定行高。
- 间距使用小步进 token（4、6、8、12、16、24）；icon button 命中区域不得过小；文件行、传输行、tab 和 toolbar 高度使用固定 token。
- 文本在窄窗口中必须截断、中间省略或换行，不得溢出按钮、tab、路径栏和 modal 边界。

### 12.6 远端 pane 状态

remote pane 必须覆盖：`Disconnected`（提供连接或重新连接）、`Connecting`（显示目标 host 和可取消操作）、`AwaitingHostKey`（保留列表上下文，由 modal 提供安全决策）、`LoadingDirectory`（保留旧列表时标记 refreshing，首次加载可以显示 skeleton 或 spinner）、`Loaded`（列表、排序、selection、路径）和 `Error`（可理解的错误信息和恢复操作）。

### 12.7 Tab 设计

- tab 显示连接名称或 host，需要区分时可以附加路径上下文。
- tab 显示连接状态：empty、connecting、awaiting host key、connected、disconnected、failed；错误 indicator 支持键盘 focus 并能打开详情。
- 关闭 tab 的行为必须符合传输状态：会取消 transfer 时必须要求明确确认，transfer 独立继续时不得显示错误暗示。
- 非活动 tab 的关闭钮只在指针位于该 tab 上时出现（Safari 习惯），隐藏时不可点击，避免误关；活动 tab 常显；26×22 的关闭钮槽位恒定占位，出现或消失不得引起标题位移。
- tab switcher 按最近使用时间排序，不能只按创建顺序。
- 窗口标题跟随活动 tab 的标题（无 tab 时用 app 名）：标题栏透明不显示标题，但 Mission Control 和 Window 菜单用它命名窗口，不得留空标题或让非活动 tab 的名字覆盖它。

### 12.8 Transfer Drawer

- transfer drawer 是传输任务的唯一主视图，不得在多个无关位置分别显示完整进度；active、queued、completed、failed 分组，completed/failed 默认可折叠。
- 每个 transfer row 显示方向、源、目标、状态、进度、速度、ETA 和主操作；cancel 和 retry 使用 icon button 与 tooltip，危险操作要求确认或支持撤销。
- progress 更新节流，视觉变化平滑，但不得用动画隐藏真实停滞；planning 阶段持续显示已发现数量和估算总字节。
- conflict 不是错误；等待用户决策时 row 状态必须是 `WaitingForConflictDecision`，不能显示为 failed。
- 面板默认展开 160px，保留手动调整、最小高度和全局队列所有权；面板内提供收起按钮，底部常驻 Transfers 入口，不改变传输执行或连接安全语义。
- 空队列提示在可用空间居中；面板高度至少 90px 时显示上传/下载说明，更小高度隐藏说明并裁切内容，保留调整入口。

### 12.9 Modal 与 Popover

- modal 只用于 host key、credential、transfer conflict、destructive confirm 和 error details；必须有标题、简短说明、主操作、取消/关闭操作和 request 绑定。
- 主操作位于固定位置，具有危险性时使用危险语义色。
- transfer conflict modal 显示源、目标、大小、mtime、决策选项和 apply-to-all 作用域。
- popover 用于简短选择或详情，不得包含必须完成的阻塞决策；普通错误、加载状态和传输进度不得使用 modal。

### 12.10 Settings、About 与 Profile UI

- Settings 使用主窗口内的独立 surface，不创建第二个窗口、不使用阻塞 modal；关闭后恢复原 tab、pane、selection 和 transfer drawer 状态。
- Settings 可通过 `macSFTP > Settings…`、`⌘,` 和 `OpenSettings` action 打开，并通过 Done 或 Escape 返回工作区。
- Settings 固定为三个分类：General（外观、行为、传输偏好、诊断）、Editor（外部编辑器与关联规则）、Saved Connections（Profile 管理）；不按单项设置拆分更多分类。
- 外观选择立即预览；`System` 跟随窗口 appearance，固定为 Light 或 Dark 时不被系统外观变化覆盖。
- General 中的 `Confirm before delete` 是删除 modal 里 “Don’t ask again” 的唯一恢复入口；`Show hidden files` 与路径栏、菜单和快捷键读写同一份配置。
- 传输元数据偏好（保留 mtime / permissions）适用于用户发起的上传和下载；remote-edit 回传固定保留 mtime，因为其冲突基线由本地临时文件的 mtime 推导。
- 配置保存失败必须在 Settings 内行内展示，且每个分类都要能看到该错误，不能使用 toast，也不能静默丢弃错误。
- 离散配置写入统一经过 `Workspace::finish_config_change`：成功后清除错误并 `refresh_windows()`，让其他窗口的开关和菜单不保留旧快照；失败时写入同一行内错误。调用点不得各自复制 `match` 和错误文案；高频的编辑器输入通过同一错误收口，但不强制跨窗口重绘。
- 文件表头的排序列使用嵌入的 `arrow_up.svg` / `arrow_down.svg` 图标，不用文本箭头；排序按钮的无障碍 label 必须保留方向和列名。
- Settings → Saved Connections 是创建、编辑和删除保存 Profile 的唯一入口；Connect modal 只能选择已有 Profile 或填写临时连接，`Manage Connections…` 关闭 Connect 并进入 Settings。
- Profile 列表使用紧凑双行条目：名称是主层级，`username@host` 是可截断的次层级，port 独立显示；选中、hover 和焦点不能改变条目高度或挤压地址文本。
- Profile editor 明确显示认证与 route 字段；密码和 passphrase 输入遮罩，保存失败不得清空用户仍可修正的草稿。
- About 是非阻塞浮层，只显示图标、应用名、从构建元数据派生的版本、简短说明和复制版本信息操作，不含设置功能或营销内容。

### 12.11 远程编辑 UI

- 双击远端文件只下载临时副本并打开外部编辑器；应用只监视本地保存以显示 dirty 标记，不自动上传。
- 冲突状态下 **Compare Changes…** 每次重新下载远端快照到会话目录的独立子目录，不复用旧快照，避免把过期内容当作当前远端版本对比。
- 用户重新选中该远端文件时，status bar 提供明确的 **Upload Changes** 操作，并在 checking、uploading 和 conflict 阶段显示不可歧义的状态。
- 上传前必须读取实时远端 metadata；缓存目录 listing 不能授权覆盖；远端变化时进入冲突决策，不能静默覆盖。
- 校验或上传失败后保留本地临时副本并允许重试；tab 或窗口关闭后结束其编辑 session 并清理临时目录。

### 12.12 错误、文案与可访问性

- UI 错误来自 `UserFacingError`，不得直接展示 `russh`、`russh-sftp` 或 IO debug 字符串；每个错误有 title、short message、detail、recovery action 和 retryable。
- 可恢复错误在原位置显示恢复操作，不可恢复错误解释无法继续的原因；status bar 只显示摘要，错误详情在相关 pane、transfer row 或 modal detail 中完整展示；错误文案说明下一步操作，不得只显示 `failed` 或 `unknown error`。
- 慢操作必须显示 loading、progress 或状态文本；loading、error 和 success 不得只通过颜色或动画表达。
- 文案当前可以只使用一种语言，但同一构建内保持语言一致；文案简短、具体、面向操作，不得在 UI 中解释架构、runtime、session epoch、actor、channel、crate 等内部概念；安全操作和 destructive 操作明确说明后果；日志文案与 UI 文案分离。
- 所有 icon-only button 必须有 tooltip 或 label。
- modal 有可测试的 focus order：打开后焦点移到第一个合理控件，关闭后恢复到触发控件。
- 文件列表 selection 可以纯键盘完成；tab、pane、transfer drawer 可以通过快捷键聚焦。
- 文件列表的可访问性结构固定且可 headless 断言：列表容器是 `Role::List`（label 为 "Local files"/"Remote files"），每个文件行是 `Role::ListItem`，label 依次读名称、类型（file/folder/symlink/item）、hidden 标记、大小、修改时间，跳过空列与 `—` 占位，并携带 selection 状态；表头三列是 `Role::Button`，label 形如 "Sort by Name, currently ascending"；状态栏是 `Role::Status`，label 汇总连接状态、选中数量和 status message。
- 需要被测试查询的组件在构造点调用 `TestSupportExt::test_support()`（非 test-support 构建下是零成本的恒等包装），测试用 `gpui_kit::test` 的 `ElementSnapshot` 断言 `role()`/`label()`/`selected()`，不依赖 VoiceOver 手工冒烟。
- 颜色对比满足基本可读性，低对比文本只用于次要信息。

### 12.13 性能与动效

- UI 动效简短、克制并且可以跳过，不影响输入、滚动、切换和选择响应。
- 目录滚动、tab 切换、pane focus、drawer 展开不得有明显卡顿；hover、selection、focus 不触发昂贵 layout。
- transfer progress 的更新频率不得超过 UI 可感知范围，不得由每个 chunk 触发重绘。
- 性能烟测的最低场景：10k local entries、10k remote entries、4 active transfers、3 connected tabs。
- 允许的动效只有下面这些；全部经 `gpui_kit::base::motion` 驱动，不手写弹簧或时间线，hover / press / selection / focus 保持即时。
  - transfer drawer 展开与折叠：`spring(("transfer-drawer", "height"))`，response 300ms、临界阻尼、收敛容差 0.5px。拖动调整高度期间 `with_travel(false)` 把目标钉住，高度 1:1 跟随指针，松手后恢复弹簧。收敛时弹簧精确等于目标，所以 `open == false && height == 0` 才是卸载条件（动画期间必须继续挂载）。给容器 `.h()` 的是动画值，容器自身不能带最小高度，否则关闭时会在最小高度处跳一下；最小高度由存储值上的 clamp 保证。内容判断（例如空态提示是否显示）读存储高度，不读动画值。
  - 浮层（command palette、tab switcher、About、Go to Path 和状态型对话框）：进入用 150ms 透明度淡入（`workspace::overlay::OverlayFade`，内部为 `Presence`）；关闭瞬时，不做镜像退出。关闭后的采样走零时长过渡，收敛到全透明，因此退出不绘制、也不请求任何动画帧。关闭时浮层内容已被清空（palette 查询、go-to-path 输入、确认请求模型），淡出一个已经没有内容的表面只会出现“卡片已消失只剩余晖”，而保留已取消的请求模型会让陈旧操作仍可生效（§6）；macOS 的 alert 同样是立即消失。
  - 该 GPUI 快照没有任何元素级 transform（只有 `Svg` 带 `Transformation`），浮层只能用透明度表达；计划中的 0.96→1.0 scale 无法实现，也没有用位移或裁剪替代。
  - transfer 进度条：`transition(("transfer-progress", job_id), 200ms)` 让绘制值平滑逼近真实 fraction；文本百分比仍显示真实值，卡顿立刻可见。
  - loading 占位：`icons/refresh.svg` 经 `animate_keyframes` + 无限 `Timing` 旋转（900ms/圈）替代静态 “↻”。
- 动效状态在 render 期间采样，且每帧都要采样：浮层即使这一帧不渲染也必须调用一次采样，否则时间线错过关闭事件（关闭帧的采样把状态立即收回起点），下次打开不会重新从透明淡入。
- reduced motion：上述动效全部来自 gpui-base motion，自动跟随系统开关并直接返回目标值或静止帧，不需要额外设置项；新增动效不得绕过它。

### 12.14 GPUI Base 组件约束

App 和 UI 通过 workspace 的 `gpui-kit = "=0.7.3"`（gpui-pre snapshot）引入 GPUI 和 `gpui-base`，不启用 Kit 的 component 和 asset 层；组件和 GPUI 类型不能来自不同版本。

- 每个生产窗口以 `gpui-base::Root` 承载 `Workspace`，Root 只管理组件浮层和交互；事件分发、远程编辑清理和 session checkpoint 必须读取其中的 Workspace，不能仅按窗口根类型筛选。
- `Theme::install` 从 macSFTP 主题 token 派生 Base 语义主题并刷新所有窗口；项目的 `WindowPresentation` Root 插件统一设置 13px rem、字体、背景和文字色，明暗切换只通过该入口更新。
- 普通输入由 `ui::OrdinaryTextField` 使用 `gpui-base::InputBase` / `Input` 绘制：Settings 外部编辑器、Connect、Saved Connections、路径跳转、重命名、新建目录、文件/Profile 筛选和 command palette 复用同一个 Base Entity。macSFTP theme 拥有尺寸、内边距、边框、焦点环、占位符、光标和选区色，保持冻结的紧凑高度（13px UI 字体下 19.5px），不读取 Component 输入外观 token。
- `IconButton` / `TextButton` 使用 `gpui-base::Button`，保留 keyed focus、Enter/Space、Tab、无障碍 label 和禁用行为；macSFTP theme 拥有按钮尺寸、圆角、文字和焦点环，鼠标点击保留 pane/input 焦点；图标按钮保持冻结的 26×22px 尺寸，tooltip 使用项目 `Tooltip`。
- Settings 的开关使用 `gpui-base::Toggle`：Base 负责 Tab、Enter/Space 和 `aria_toggled`，macSFTP theme 负责 track/knob 外观；不得用裸 `div().on_click` 实现设置开关。
- Assets 只加载项目嵌入图标，未知资源返回 None，不回退到另一套控件资源。
- 输入右键菜单使用 Base 的 `NativeMenu` 模型和编辑 action；`macsftp-native-menu` 只呈现 macOS AppKit 菜单，不持有输入 Entity 或业务草稿。NSView 在释放 GPUI Window 借用前被 retain，AppKit tracking loop 在 foreground task 中执行且不持有 GPUI update 借用；呈现前检查窗口/焦点，返回后只向仍存在且焦点未变的窗口派发 action，取消后刷新窗口；非 macOS 返回明确 Unsupported 错误。
- 文件列表右键菜单同样走 `macsftp-native-menu`（`workspace/context_menu.rs` 构建纯数据菜单模型）：右键在 mouse-up 时先聚焦并选中指针下的行，再以指针位置弹出；菜单条目复用既有 workspace action（Open/Open for Editing、Upload、Download、Rename…、New Folder…、Delete…、Copy Path、Show in Finder、Refresh），暂时不可用的命令以禁用态呈现而不是隐藏，只有 side/kind 不适用的命令才不出现；菜单模型必须可在无窗口环境下单测。
- 用户已授权 `macsftp-native-menu` 的 `src/macos.rs` 包含必要 unsafe：该 crate 默认 deny，其余 workspace crate 继续 inherit forbid。unsafe 边界检查递归扫描其 Rust 源文件，只允许规范路径 `src/macos.rs` 和唯一 macOS-gated module 例外，拒绝其他 unsafe 语法、lint 覆盖、外部 include/path 和符号链接模块；回归 fixture 覆盖嵌套路径、unsafe fn/impl/extern/attribute、注释与字面量。菜单调度层使用真实 GPUI 窗口更新验证关闭、焦点变更及重复取消，原生界面截图见 `docs/visual-evidence/gpui-base/README.md`。
- 表单草稿仍由 Workspace 持有；`PlainInput` 只保留选区、撤销和 IME 编辑状态，Change 事件写回草稿，程序修改草稿时只在文本不同时同步控件；关闭 surface 时释放绑定，替换表单或冲突请求后旧控件事件必须被忽略。
- Connect/Profile 表单的 Tab 使用 scoped action 跨普通和敏感字段移动实际焦点，普通字段的 Enter 通过 Base `PressEnter` 转发原有提交入口；端口、路径、名称及认证校验仍由原有业务入口统一执行，无效提交不能清空草稿。
- 密码、私钥 passphrase 和 keyboard-interactive answer 保留不可 Clone 的 `SecretInputState` 及清零路径，不进入普通 Base 输入或其撤销/剪贴板状态。
- 首次连接入口保留 160×34px 外观，复用 Base 按钮焦点、无障碍 label 和 Enter/Space 激活；`ConnectionEntry` 上下文排除文件 pane 的 Enter 打开快捷键。最近连接行高 30px，分组标题使用完整 muted token。

### 12.15 禁止模式

- 禁止 landing page、hero section、营销插画、装饰性渐变背景、大面积品牌色、厚阴影、玻璃拟态和彩色卡片墙。
- 禁止用 toast 代替必须解决的错误或安全决策，禁止在文件传输主流程中弹出非必要 modal。
- 禁止让 loading 状态清除用户已有的上下文，除非旧数据确实不可用。
- 禁止在主线程阻塞等待网络、磁盘或 runtime 响应。
- 禁止向用户展示实现术语。
- 禁止为了视觉统一而减少安全信息、错误恢复能力和传输可观测性。

### 12.16 UI 评审清单

任何 UI/UX 变更在合并前必须回答：

- 是否保持单窗口内工作上下文完整，并且没有破坏多窗口间的状态隔离？
- 是否提供 command palette action 或快捷键入口？
- 菜单栏、快捷键和 command palette 是否仍然描述同一条命令（`app_menus` 的一致性测试覆盖了菜单项、chord 与 palette id）？
- 是否覆盖 loading、empty、error、disabled、focused、hover、selected 状态？
- 是否在窄窗口和短窗口下检查 tab、路径、按钮、modal 文本不溢出？
- 是否避免无关卡片、渐变、装饰图形和大面积空白？
- 是否不阻塞 GPUI 主线程，并能处理 10k entries 和多个并行 transfer？
- 是否为 icon-only button 提供 tooltip 或 label？
- 是否没有泄露 secret、私钥完整路径或内部 debug 字符串？
- 是否覆盖 request id/session epoch 相关的 modal 过期场景？
- Profile 写入是否仍然只发生在 Settings → Saved Connections？
- 远程编辑是否仍由用户明确触发回传，并在上传前执行实时冲突检查？
- 所有可滚动区域是否在溢出、拖动、track 点击、滚轮和窗口 resize 下保持同步？

未满足任一项时必须在变更说明中记录取舍；涉及安全、阻塞、secret 泄露或 modal 过期误确认的项目不得豁免。

## 13. 错误与日志

可恢复错误不能 panic，异步失败必须进入 UI 或经过审计的日志路径。

日志允许记录 host、port、username、作用域 ID 和枚举化失败标签；禁止记录 password、passphrase、keyboard-interactive answer、secret ref 的值、完整私钥路径、ProxyCommand 内容、agent socket/identity blob/comment/签名输入输出，以及未审核的第三方错误原文或完整 event Debug。

默认过滤器只开启经过审计的连接生命周期 target；开发者可以通过 `RUST_LOG` 临时覆盖，但代码本身仍不能包含敏感 tracing 参数。

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

并发测试必须使用带测试名、进程或序列号的唯一临时路径。无 Docker 时允许本地 integration test 明确 skip，但 CI 必须运行完整门禁。可见 UI 改动必须提供截图或明确的视觉验证说明。

## 15. 需要显式决策的扩展

下列功能不能通过“预留字段/空 enum variant”提前设计；真正实施时再增加模型、migration 和测试：Profile group/folder、OpenSSH config 导入、多跳 jump-host、传输跨启动恢复、自动更新、内容哈希级远程编辑冲突检测。

新增中等以上功能前，设计说明只需记录当前问题、所有权、持久化/安全边界、用户可见取舍和验证方式。功能交付后把仍有效的约束合并回本文，并删除已完成的逐步实施计划。
