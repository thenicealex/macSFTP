# AGENTS.md

macSFTP 的强制编码准则。所有代码、测试、文档和 agent 修改都必须遵守本文件；如果用户的明确要求与本文件冲突，先指出冲突并等待确认。

本文件借鉴 Zed 的工程风格：规则应该是避坑清单，不是架构地图；正确性和清晰度优先；小步、可验证、少做无关改动。

## 1. 开工前必须做

- 先读上下文，再写代码。涉及架构、crate 划分、运行时、SFTP、传输、host key、UI 的任务，必须先读 `docs/gpui-russh-plan.md` 的相关章节。
- 明确成功标准。每个非平凡任务开始前要能说清：要改什么、如何验证、哪些行为不能变。
- 有歧义就说出来。存在多种解释时，不要悄悄选一个；说明取舍。若错误选择会造成返工或安全风险，先问。
- 先找简单方案。禁止为了“未来可能需要”添加抽象、配置项、插件点、泛型或跨层框架。
- 只改和目标直接相关的文件。发现无关问题可以记录或告诉用户，不要顺手重构。
- 两处必须保持一致但类型系统无法强制关联的值（例如同一路径/常量在两个 crate 里各写一份），要么改成从单一来源派生，要么在两处都写注释互相指向；不能任其独立演化，否则会在某一处改动后悄悄产生不一致（例如 mock 数据的远端根路径和 mock actor 的默认配置曾经对不上，导致连接成功但目录为空）。

## 2. Zed 风格的工程纪律

- 正确性和可读性优先于速度；性能优化必须有测量或明确的产品约束。
- 一个改动只解决一个问题。不要把 bugfix、功能、格式化、重命名和重构混在一起。
- 优先在现有文件内实现，只有出现新的清晰逻辑组件时才创建新文件。
- 新规则、新抽象、新 crate 必须满足三点：非显而易见、会重复踩坑、具体到可以执行。
- 不写“整理性注释”。注释只解释不明显的原因、约束或取舍，不复述代码做了什么。
- 变量名用完整单词，避免 `q`、`cfg`、`tmp` 这类上下文不充分的缩写；惯用缩写如 `id`、`url` 可以使用。
- Rust 模块禁止使用 `mod.rs` 路径。使用 `src/session.rs`，不要使用 `src/session/mod.rs`。
- 新 crate 的 library root 优先在 `Cargo.toml` 里显式声明 `[lib] path = "...rs"`，使用能表达 crate 语义的文件名。

## 3. 项目架构边界

详细架构以 `docs/gpui-russh-plan.md` 为准。本节只列硬约束。

- `core` 是纯业务模型和状态机，禁止依赖 GPUI、russh、Tokio runtime 实现或 macOS 平台 API。
- `ui` 只做可复用 GPUI 组件和表现层，禁止直接发起网络请求或持有远端 session。
- `app` 负责 GPUI 应用入口、窗口、Entity、Action 和 UI 事件转发，禁止直接调用 `russh` 或 Keychain。
- `sftp` 负责 `russh + russh-sftp` adapter、Tokio runtime、session actor、TransferManager，禁止直接操作 GPUI Entity。
- `storage` 负责 profiles、known_hosts、Keychain 引用和 migration，敏感值不能写入普通配置。
- `platform` 封装 macOS/local filesystem 边界，业务层不能散落平台调用。
- view state 不保存长期业务状态；长期状态必须进入 `AppModel`、`TabStore`、`TransferStore`、`ModalStore` 或 `core` 模型。

禁止的依赖方向：

```text
core -> gpui
core -> russh
core -> tokio runtime details
ui -> sftp
sftp -> gpui
storage -> gpui
```

## 4. 异步与运行时

- GPUI 主线程不能 await 网络、远端 SFTP、本地大目录扫描或其他长耗时工作。
- `russh` 只能运行在 Tokio runtime 上，不能放到 GPUI executor 上跑。
- GPUI 和 Tokio 之间只能通过 bounded command/event channel 通信。
- GPUI 侧发送 command 必须非阻塞；禁止在主线程调用阻塞 `send`。
- Tokio task 只能发 `AppEvent`，不能持有 GPUI `Context`、`Window`、Entity update handle，也不能直接 `cx.notify()`。
- event drain task 只负责接收 event、进入 GPUI update closure、更新 Entity state、调用 `cx.notify()`。
- progress event 必须在源头节流或合并，禁止用 unbounded channel 承接高频进度。
- runtime shutdown 必须有超时；禁止依赖 Tokio runtime 的默认 `Drop` 行为无限等待。
- async context 中为了缩小 borrow 生命周期，可以用变量 shadowing 显式 clone。

## 5. 错误处理与安全

- 禁止用 `unwrap()`、`expect()`、数组越界索引等可 panic 操作处理可恢复错误。测试中的 `expect` 需要说明断言含义。
- 禁止用 `let _ =` 静默丢弃 fallible operation 的结果。要么 `?` 传播，要么 `match` 处理，要么记录可见日志。
- 所有异步失败必须能传播到 UI 或日志，用户可见路径要转成 `UserFacingError`。
- secret value 永不进入日志、错误、UI detail、普通配置文件或测试快照。
- password、private key passphrase 只能存 Keychain；配置文件只存 `SecretRef`。
- 私钥路径默认不要完整记录；需要诊断时使用文件名、hash 或脱敏路径。
- host key mismatch 必须阻断连接，禁止提供一键覆盖。
- unknown host key 必须绑定 `TrustRequestId` 和 `session_epoch`，过期、tab 关闭或 reconnect 后确认按钮不能生效。
- 不要自己实现完整 OpenSSH `known_hosts` parser。MVP 只支持文档声明的子集，优先使用现成 crate；无法解析的行按行忽略并写 WARN。
- known_hosts / host key 比对必须只比较 key 内容本身（例如 `key_data()`），禁止直接比较整个 `PublicKey`/`Entry` 对象。这类类型的相等性通常会带上 comment 等展示字段，服务器握手发来的 wire key 从不带 comment——直接比较会把合法的已知 host 误判成 mismatch，把最安全的路径错误地变成最危险的阻断路径。踩过一次：集成测试连真实 sshd 时才发现，编译器和 clippy 都不会提示。

## 6. 状态、ID 与陈旧事件

- 跨线程、跨 session、跨 modal 的对象必须有稳定 ID：`TabId`、`SessionId`、`TransferId`、`TrustRequestId`、`ConflictRequestId`。
- 每种跨线程 ID 动手前先确定唯一权威分配方，不能两侧各分配一套。`SessionId`/`session_epoch` 的权威方是 UI/core（`AppState`/`TabState` 持有长期状态），runtime 和 actor 只能回显 command 里携带的值，不得自行生成——如果 runtime 自己分配一套序号，陈旧事件防护表面成立，实际比较的是两个不同源的计数器，形同虚设。
- 来自远端 browsing session 的 event 必须携带 `RemoteEventScope { tab_id, session_id, session_epoch }`。
- event 进入状态机前必须先做陈旧事件校验；校验逻辑放在 `core`，禁止散落在 view。
- tab 关闭后的远端迟到 event 必须被忽略，不能重新创建 tab 或覆盖新状态。
- reconnect 后旧 session 的失败、断开、目录结果不能覆盖新 session。
- transfer event 不依赖 tab 存活；进入全局队列后由 `TransferStore` / `TransferManager` 管。

## 7. 传输与文件语义

- 目录上传/下载必须建模为 `TransferPlan`，不能把递归目录偷换成单个文件 job。
- planning 必须流式产出进度；禁止先完整扫描 10k 文件再让 UI 第一次更新。
- conflict 是状态，不是错误；除非用户取消或 resolve 后执行失败，否则不要进入 `Failed`。
- `apply_to_all` 的作用域必须绑定到 `TransferPlanId`，禁止做成全局开关。
- 取消 running transfer 必须使用显式 cancellation token；禁止依赖 drop future 作为主要取消机制。
- `.macsftp-part` 不能当作正常目标冲突。清理失败要记录 residual temp file，后续只清理 macSFTP 记录过的残留文件。
- metadata preservation 失败通常是 warning，不应让已成功传输的数据变成整体失败，除非用户要求严格模式。
- symlink 要复制 link 本身，不默认解引用。

## 8. GPUI 与 UI 规则

- UI 第一屏必须是可用产品界面，不做营销式 landing page。
- 文件列表必须虚拟化；10k entries 不能创建 10k 个长期 row entity。
- row 渲染从 snapshot + index 派生；selection 存稳定 id/path，不存 row index。
- 所有 icon-only button 必须有 tooltip 或 label。
- modal 必须有明确标题、主操作、取消路径，并绑定 request id。
- 所有用户动作要有即时反馈；慢操作必须显示 loading、progress 或状态文本。
- UI 文案可以先单语，但业务层不能硬编码最终展示文案；`core` 输出 error code 和参数，`app/ui` 映射展示文本。
- UI 改动必须检查窄 pane、短窗口、Retina、高亮/暗色主题、键盘可达性和焦点状态。
- 可见 UI 改动完成后必须提供截图或视觉验证说明。

## 9. 测试与验证

- bugfix 优先先写能复现 bug 的测试，再修到测试通过。
- 新状态机逻辑必须有 `core` unit tests。
- SFTP adapter 行为必须有 integration tests；无 Docker 时可以 skip，但 skip 信息必须明确，CI 必须跑完整集成测试。
- runtime bridge 必须覆盖 bounded channel、progress 节流、shutdown timeout、stale event guard。
- UI 行为必须覆盖 action dispatch、modal dispatch、tab switching、focus 和 transfer drawer 关键状态。
- 性能相关改动必须至少做 smoke test：10k entries、快速切 tab、并发 transfer、窗口 resize。
- 每次交付前运行和改动风险匹配的最小验证命令；不能运行时要说明原因和剩余风险。
- 并发运行的测试之间共享的临时文件/目录路径必须带唯一标识（测试名、label 或类似区分符），禁止多个测试用同一路径模板互相覆盖——这类问题只在并发/多次运行时随机触发，单独跑测试或串行跑往往通过，是最容易被误判为"环境问题"而不是代码问题的一类 flaky。

## 10. 文档与 ADR

- 架构变化必须更新 `docs/gpui-russh-plan.md` 或新增 ADR，不能只改代码。
- 新增中等以上功能前，先写清：解决什么问题、为什么现在做、影响哪些 action/settings/persistence/security/performance/accessibility。
- 文档要记录取舍，不堆“最佳实践”。
- 不要把 crate 的完整架构说明塞进本文件；本文件只放会反复踩坑的强制规则。

## 11. 提交与评审 hygiene

- 提交或 PR 标题用祈使句，清楚说明行为变化；不要用模糊标题。
- PR/变更说明必须包含：问题、方案、验证、风险或未做事项。
- UI 可见变化附截图或录屏；不可截图时说明用什么方式验证。
- AI 生成的代码必须由提交者理解并能解释。不能提交自己无法维护的代码。
- 不做 drive-by cleanup。格式化只覆盖被本次任务实际修改的文件，除非用户明确要求全仓格式化。

