# 远程编辑功能 — Bug 分析报告

- **日期**: 2026-07-16
- **范围**: 远程编辑功能(15 个提交,`127854a..200151b`,已合并到 master)
- **方法**: 3 个独立深挖 agent(并发/生命周期、状态机/边界、文件系统/资源/安全)并行审查,每项发现由控制者对抗性验证——附具体触发场景与代码指针,过滤误报与 style 问题
- **背景**: 该功能已过三轮开发内审查(每任务审查 + 全分支审查修了 3 个集成 bug + 修复审查)。本报告是**合并后的新鲜视角复查**,专找存活于前序审查的深层问题

> **状态**: 原为纯分析报告。**2026-07-17 已按严重度依次修复全部 11 项可修 bug**(#12 是用户明确选择的 `(size, mtime)` 无哈希检测的固有限制,不修,仅记录)。每项修复附带回归测试,`bash scripts/check.sh` 全绿(fmt + 架构检查 + 敏感日志检查 + 全工作区测试 + clippy `-D warnings`)。详见文末「修复记录」。

---

## 摘要

| # | 严重度 | 问题 | 后果 | 修复 |
|---|--------|------|------|------|
| 1 | 🔴 **Critical** | 通过远程文件名的 `open` 分发代码执行(RCE) | 单次点击任意代码执行,默认配置即脆弱 | ✅ `open -t` 强制文本编辑器,永不按内容类型执行 |
| 2 | 🟠 **High** | 重连后保存丢失,会话永久搁浅 | 用户保存的编辑静默永不上传,无报错 | ✅ 重连时 `update_epoch_for_tab` 刷新会话 epoch |
| 3 | 🟡 Medium | 标签/窗口/断连关闭泄漏 + 幽灵阻塞重新编辑 | 关标签后该文件永远"已在编辑中" | ✅ 关标签/关窗清理会话+临时目录(断连按设计保留) |
| 4 | 🟡 Medium | 传输按本地路径字符串误关联 | 同路径的普通传输可驱动编辑会话/删其临时目录 | ✅ 关联时校验方向+远程端点,不止本地路径 |
| 5 | 🟡 Medium | 瞬时 stat 失败误删活跃会话 | 编辑器原子保存触发瞬时 NotFound → 会话被删 → 之后保存全丢 | ✅ 连续 3 轮 miss 才回收;成功 stat 清零计数 |
| 6 | 🟡 Medium | 列表外文件的盲覆盖 | 文件导航出可见列表 → 冲突检测跳过 → 静默覆盖并发改动 | ✅ 仅在远程确认未变时回传,否则标记冲突 |
| 7 | 🟠 **High** | 忽略 `send_command`/`try_send` 失败 → 会话卡在不可见相位 | 通道满时会话永久搁浅,文件无法重新编辑 | ✅ 三个派发站点失败时回退相位/移除会话 |
| 8 | 🟡 Medium | 大文件确认 modal 持陈旧 `PendingEdit`,不重新校验 | modal 开着时重连 → 确认后用陈旧 epoch → 搁浅 `Downloading` | ✅ `confirm_large_edit` 重新校验连接并刷新 epoch |
| 9 | 🟢 Low | 删临时文件泄漏空目录 | `<edits>/<id>/` 空目录残留到退出 | ✅ 回收会话时 `remove_dir_all` 父目录 |
| 10 | 🟢 Low | DiscardLocal 孤儿目录 | 每次 Discard 泄漏一个旧会话目录 | ✅ Discard 时删旧会话临时目录 |
| 11 | 🟢 Low | 双击"Overwrite"重复派发 | 一次冗余上传(第二个完成被相位守卫拒绝) | ✅ `resolve_edit_conflict` 加 `RemoteConflict` 相位守卫 |
| 12 | 🟢 Low | 同秒同大小并发改动漏检 | `(size, 整秒 mtime)` 检测的固有窗口 | ⚪ 不修:用户明确选择无哈希方案的固有限制 |

**并发模型已确认无数据竞争**:两个异步循环(事件泵、`EditWatcher` 轮询)都跑在 GPUI 主线程,`edit_sessions` 串行化;传输管线在 tokio 线程但只通过 `AppEvent` 通信,回到主线程才碰会话状态。所有问题都是**逻辑交织**和**生命周期搁浅**,不是内存竞争。

---

## 🔴 #1 Critical — 通过远程文件名的代码执行(RCE)

**位置**: `crates/platform/src/platform.rs:266`(sink)+ `crates/app/src/workspace/remote_edit.rs:114`(文件名派生)

**触发**(单次交互):
1. 受害者连到恶意/被攻陷的服务器,或任何攻击者能投放文件的目录
2. 攻击者放一个名为 `quarterly-report.command`(或 `.terminal`/`.webloc`/`.url`/`.workflow`)、远程权限 `0755`、内容为脚本的文件
3. 受害者双击它(远程非目录 → `open_entry_at` → `begin_edit`,panes.rs:340 把**每个**远程非目录都路由到 `begin_edit`)或选 Edit / 按回车
4. 文件下载到 `<edits>/<id>/quarterly-report.command`,`advance_downloading` 调 `open_in_editor(temp, None)`

**验证过的完整链条**:
- **文件名连扩展名原样保留**:`start_edit_download` 用 `Path::file_name()` 派生临时名(remote_edit.rs:114)。前序审查正确指出它剥离了路径分隔符——但**不剥离扩展名**。`evil.command` 仍是 `evil.command`
- **可执行位被保留**:编辑下载用 `MetadataPolicy::default()`(remote_edit.rs:147),`preserve_permissions` 默认 `true`(core.rs)。`preserve_download_metadata` 把远程 mode 通过 `set_permissions` 套到本地临时文件(session_actor.rs:2145)。远程 `0755` → 本地**可执行**临时文件
- **无隔离标记**:临时文件用 `tokio::fs` 写(session_actor.rs:1875),全代码库从不设 `com.apple.quarantine`。应用自己写的文件不带 Gatekeeper 隔离标记 → **Gatekeeper 不拦截**
- **裸 `open` 按扩展名分发**:默认配置(`external_editor = None`)下 `build_open_command` 跑 `/usr/bin/open <temp>`,**无 `-a` 无 `-t`**(platform.rs:266)。`open` 按扩展名交给 LaunchServices 默认处理器:`.command`/`.terminal` → Terminal 执行;`.webloc`/`.url` → 打开攻击者 URL;`.workflow` → Automator

**后果**: 受害者 Mac 上任意代码执行,一次点击即可。**默认配置就是脆弱配置。**

**为何前序审查漏掉**: 它们查了 shell/命令注入(`open -a` 用独立 argv,确实安全)和路径注入(`file_name()` 剥离 `/`,安全)。没人查 `open` 本身是内容类型分发器、会执行某些扩展名。

**修复建议**(任选或组合):
- 在 `None` 分支强制文本语义:`open -t <file>`(默认**文本**编辑器打开)或 `open -e`(TextEdit)
- 无视远程 mode,剥离编辑临时文件的可执行位
- 危险扩展名(`.command`/`.app`/`.webloc`/`.workflow`/`.terminal` 等)拒绝或中和
- 设置 quarantine xattr
- 注:配置路径 `open -a <app> <file>` 已安全(app 强制了处理器),只有 `None` 分支需修

---

## 🟠 #2 High — 重连后保存丢失,会话永久搁浅

**位置**: `crates/app/src/edit_watcher.rs:116` + `crates/sftp/src/runtime.rs:727,836`

**触发**(精确序列):
1. 用户编辑远程文件;会话 `Editing`,`session_epoch = 1`(在 `begin_edit` 时捕获,存入 `EditSession.session_epoch`)
2. 连接断开后标签页(自动/手动)重连。重连把 epoch +1:`mod.rs:717` `let next_epoch = tab.session_epoch + 1;`。runtime 里该 `TabId` 的 `RemoteSessionHandle` 现在 `session_epoch = 2`;标签页**仍连着、仍拥有该标签**
3. 用户保存。下一轮询:文件变了、远程未偏离 → 轮询置 `phase = UploadingBack`、更新 `local_mtime`、用**陈旧的 `epoch = 1`** 派发 `build_edit_upload_command`
4. `dispatch_edit_command` 找到拥有窗口(存在)、`try_send` 成功 → **不回退到 `Editing`**
5. runtime 里 `StartTransfer` 按 `tab_id` 关联会话后 `.filter(|session| session.session_epoch == command.session_epoch)`(runtime.rs:727)。`1 != 2` → `transfer_request_tx = None` → 命中 `let Some(request_tx) = transfer_request_tx else { return; }`(runtime.rs:836)→ **静默返回,无 `TransferCompleted`、无 `TransferFailed`**

**后果**: `advance_uploading_back` 永不被调用(无终止事件到达)。会话永卡 `UploadingBack`。因不再是 `Editing`,轮询永久跳过它(`editing_sessions()` 过滤)→ **后续保存也被静默忽略**。因 `find_active` 匹配任何活跃相位,用户也无法重新打开该文件("This file is already open for editing")。用户保存的编辑静默永不上传,无任何报错。

**根因**: 编辑会话在 `begin_edit` 时捕获一次 `session_epoch` 并终身复用,但 runtime 把重连后的 epoch 不匹配当作陈旧命令丢弃、**且不发终止事件**。轮询只在 `try_send` 失败或无窗口拥有标签时才回退 `UploadingBack→Editing`——这里两者都不发生。`UploadingBack` 无超时/看门狗。

**修复建议**: 重连时刷新会话 epoch;或 epoch 不匹配丢弃时发终止事件;或给 `UploadingBack` 加超时看门狗。

---

## 🟡 #3 Medium — 标签/窗口/断连关闭泄漏 + 幽灵阻塞重新编辑

**位置**: `crates/app/src/workspace/mod.rs`(`close_tab_by_id`)+ `crates/core/src/core.rs`(`find_active`)

**触发**: 打开一个远程文件编辑(创建会话 + `<edits>/<id>/` 目录),然后关闭该标签页

**后果**:
- `close_tab_by_id` 移除标签(及 MRU/导航/恢复条目)但**从不移除**该标签的编辑会话或临时目录(已验证:只碰 `tab_nav`/`restored_targets`)。`<id>/` 目录和文件残留到退出兜底清理。轮询每秒继续 `stat` 它们
- **更糟(功能 bug)**:`find_active` 只按 `(profile_id, remote_path)` 去重、**不含 `tab_id`**(已验证 core.rs)。孤儿会话活得比标签久 → 在新标签/窗口(同 profile)重新打开同一文件时命中"This file is already open for editing"(remote_edit.rs:91)——永久,除了退出应用别无解法
- 同样泄漏适用于**窗口关闭**(`checkpoint_after_window_closed` 不碰编辑临时文件)和**断连**(会话存活、临时文件孤立)

**根因**: diff 的清理故事覆盖了下载失败、上传失败、退出、启动——但标签/窗口/断连生命周期从未接到编辑会话拆除。

**修复建议**: 标签/窗口关闭、断连时移除该标签的编辑会话 + 临时目录。

---

## 🟡 #4 Medium — 传输按本地路径字符串误关联

**位置**: `crates/app/src/event_coordinator.rs:147`(`find_by_temp_path`)

**触发**: 用户编辑 `/srv/config.json`(临时路径 `<edits>/<id>/config.json`);独立地,某个普通传输的本地端点恰好等于该临时路径(用户把普通下载目标选到 edits 目录子树,或 `Downloading` 相位路径碰撞)。该无关传输完成/失败 → `advance_edit_sessions` 提取其本地端点,`find_by_temp_path` 按字符串相等匹配到编辑会话 → 若会话在 `Downloading`/`UploadingBack` 则推进它。

**后果**: 一个外来传输的 `TransferCompleted` 能驱动编辑会话状态机:如无关完成让 `Downloading` 会话提前开编辑器、进 `Editing`(真下载还没落地),或无关失败在活跃编辑下把会话拆掉、删其临时目录(`remove_dir_all`)。

**根因**: 关联只靠本地路径字符串。`active_transfer: Option<TransferId>` 字段正是为按传输 id 关联而存在、`find_by_transfer` 也已实现——但生产中从不填充 `active_transfer`(`start_edit_download` 在传输 id 存在前就注册),所以稳健的键未用、脆弱的路径字符串键是唯一关联。

**修复建议**: 填充并使用 `active_transfer`(传输 id)做关联。

---

## 🟡 #5 Medium — 瞬时 stat 失败误删活跃会话

**位置**: `crates/app/src/edit_watcher.rs:89`

**触发**: 用户正在编辑;某一轮询的 `std::fs::metadata(temp)` 因**瞬时**原因返回 `Err`——不是"文件被删"。macOS 上发生于 `EINTR`,或编辑器原子保存(rename-over:有些编辑器短暂 `unlink`+`rename`,留下亚毫秒路径不可解析窗口),或卷/FUSE 挂载抖动。`metadata.is_err()` → `edit_sessions.remove(id)`,会话没了。

**后果**: 活跃编辑会话在第一次 stat 错误时被静默销毁。用户继续在编辑器里编辑、保存——但没有会话检测保存,**永不上传**,无提示。临时文件留在盘上。(vim 带 `writebackup`、TextEdit 等原子保存编辑器正是做这种 rename 舞蹈。)

**根因**: 代码把"任何 metadata 错误"等同于"文件永久消失"。注释称"session dir wiped, user deleted it",但 metadata 错误不持久——原子替换期间的 `NotFound` 是瞬时的、下一轮就自愈。第一次错误就删会话不可恢复。

**修复建议**: 要求连续 N 轮 NotFound 才删;或只在确认持久的条件下删,其他错误类型不动。

---

## 🟡 #6 Medium — 列表外文件的盲覆盖(静默数据丢失窗口)

**位置**: `crates/app/src/edit_watcher.rs:106`

**触发**: 打开文件编辑,然后把该标签导航到别的远程目录(文件掉出 `tab.remote.entries`),再本地编辑 + 保存。`current_remote_snapshot` 返回 `None`;`is_some_and(...)` 短路为 `false`(edit_watcher.rs:106)→ 偏离检查**被跳过** → 上传用 `ConflictPolicy::OverwriteAll` 直接进行。

**后果**: 若期间别人改了远程,他们的改动被**静默覆盖**——无冲突弹窗。文档说"匹配 brief",但 brief 的"无法判定 → 不误报冲突"用假阳性换了一个静默数据丢失窗口。

**修复建议**: `None` 快照时,回传前发一次远程 `stat`(而非依赖当前列表)以判定偏离;或对列表外的文件拒绝自动回传、提示用户刷新。

---

## 🟠 #7 High — 忽略 `send_command`/`try_send` 失败 → 会话卡在不可见相位

**位置**: `crates/app/src/workspace/remote_edit.rs`(`start_edit_download`、`resolve_edit_conflict`)+ `crates/app/src/edit_watcher.rs`(`dispatch_edit_command`)

前序审查加固了**一种**派发失败(无窗口 → 回退 `Editing`),但漏了**通道发送失败**(`ChannelFull`/`ChannelClosed`)的**三个**站点。因为 `Downloading` 和 `UploadingBack` **只**由传输终止事件推进(`advance_edit_sessions` 经 `find_by_temp_path`),而这两个相位既不被轮询(`editing_sessions()` 只含 `Editing`)也不被渲染(`conflict_sessions()` 只含 `RemoteConflict`),一个没有在途传输的会话就永久搁浅且不可见——`find_active` 随即永久阻塞重新编辑该文件("This file is already open for editing"),除非重启应用。

- **站点1 `start_edit_download`**(已验证):会话在 `send_command` **之前**就 `register` 为 `Downloading`,`bool` 结果只用于状态消息门控
  - 触发:用户点 Edit 时命令通道饱和(排队的批量传输)
  - 后果:幽灵 `Downloading` 会话;状态显示"Busy — try again",但该文件现在永久无法重新编辑、永不被轮询/渲染/清理
- **站点2 `resolve_edit_conflict(Overwrite)`**(已验证):置 `phase = UploadingBack` 后调 `send_command` 并丢弃结果
  - 触发:用户点"Overwrite remote"时通道满
  - 后果:永卡 `UploadingBack`
- **站点3 `dispatch_edit_command`**(已验证):`client.try_send(command)` 返回 `Err` 时只 `warn!`,不回退。`poll_edit_sessions` 在调它之前已置 `UploadingBack`
  - 触发:轮询驱动的回传时通道满
  - 后果:永卡 `UploadingBack`。注意该函数自己的文档注释解释了为何要回退(正是为避免这个)——但只对 `None`/无窗口分支回退,漏了两行之上完全相同的 `try_send`-`Err` 分支

**为何是新发现**: 已知 #2 是陈旧 **epoch** 路径(命令被通道接受、被 runtime 丢弃)。这个是**通道级发送失败**(命令根本没进通道)——不同触发、相同终止症状,且两个同步站点甚至不检查发送结果。

**修复建议**: 三个站点在发送失败时回退相位(`Downloading`/`UploadingBack` → 移除会话或回 `Editing`)并清理临时目录,与 `dispatch_edit_command` 的无窗口分支对称。

---

## 🟡 #8 Medium — 大文件确认 modal 持陈旧 `PendingEdit`,不重新校验

**位置**: `crates/app/src/workspace/remote_edit.rs`(`confirm_large_edit`)

`begin_edit` 在点击时把 `session_epoch`/`profile_id` 快照进 `large_edit_confirm`;`confirm_large_edit` → `start_edit_download` 原样使用,**不重新检查连接或 epoch**(已验证:函数体只是 `take()` + `start_edit_download`)。

- **触发**:用户让大文件确认 modal 开着;标签页重连(epoch +1)或断连;然后点"Edit"
- **后果**:下载用陈旧 `session_epoch` 派发。由与已知 #2 相同的机制(runtime 丢弃陈旧-epoch 命令、无终止事件),会话搁浅在 `Downloading`——不可见、阻塞重新编辑
- **为何是新发现**: modal 把 #2 那个可忽略的竞争窗口放大成**用户交互宽度**的窗口,且发生在**下载**相位(非回传)。`begin_edit`/`start_edit_download` 在 modal 之后从不重跑 `find_active` 去重或连接检查

**修复建议**: `confirm_large_edit` 重新校验连接/epoch(或重跑 `begin_edit` 的前置检查),陈旧则放弃并提示。

---

## 🟢 Low

- **#9 删临时文件泄漏空目录**(edit_watcher.rs:89): 用户只删临时文件(非 `<id>/` 目录)→ 轮询 `remove(id)` 但不 `remove_dir_all(parent)` → 空 `<edits>/<id>/` 目录残留到退出。与 `advance_downloading`(失败时清 parent)不对称
- **#10 DiscardLocal 孤儿目录**: `ConflictChoice::DiscardLocal` 做 `remove(id)`(丢旧会话、**不删**其 `<old_id>/` 目录)再 `start_edit_download` 生成**新** id → 新目录。旧目录永不删,每次 Discard 泄漏一个
- **#11 双击"Overwrite"重复派发**(remote_edit.rs:180,已验证): `resolve_edit_conflict` 只有存在性守卫(`get(id).cloned()`),无相位守卫(`phase == RemoteConflict`)。冲突 modal 重绘移除前的两次点击都跑 `Overwrite` 分支 → 两个 `StartTransfer(Upload)`。**有界危害**:第二个 `TransferCompleted` 时相位已 `Editing`,被 `Downloading|UploadingBack` 守卫拒绝,净代价一次冗余上传。`DiscardLocal`/`Later` 双击安全(`remove` 后 `get→None`,或幂等)
- **#12 同秒同大小并发改动漏检**: `(size, 整秒 mtime)` 检测的固有限制,被 `truncated_to_secs` rebase 略微加剧。窗口 ~1s 且需同大小并发写

---

## ✅ 已检查且安全(排除的疑点)

- **并发/内存竞争**: 两循环都在 GPUI 主线程,`edit_sessions` 串行化;传输线程只发 `AppEvent`,回主线程才碰会话。无数据竞争、无跨线程会话变更、无借用跨 await
- **双重上传**: 不会发生。轮询在派发**前**翻 `phase = UploadingBack`,且只轮询 `Editing` 会话;在途中的第二轮见 `UploadingBack` 跳过
- **轮询关闭泄漏**: 安全。循环 `await` timer(让出、不忙等),`cx.update` 出错即 break;`EditWatcher` 是 global,`_task` 在拆除时 drop、取消 spawn;不阻塞退出
- **后台标签冲突可恢复**: `render_edit_conflict_modal` 按活跃标签过滤——后台标签的 `RemoteConflict` 不渲染但**会话保留**,切回该标签即弹窗。**不是搁浅,是延迟**(已验证 remote_edit.rs:295)
- **配置写原子性**: `config.rs` → `atomic_file.rs`:`create_new` 临时文件(0600)→ `write_all` → `sync_all` → `rename` → 父目录 fsync。崩溃不会损坏 `config.json`。每键写是性能问题非损坏
- **`"file"` 回退无碰撞**: 每会话唯一 app 生成的 `u64` id,`<id>/file` 跨会话不碰撞。远程无法影响目录部分
- **`open -a` argv 安全**: `open -a <app> <file>` 用离散 argv 槽;以 `-` 开头的编辑器串落在 app 名槽(open 报"app 未找到")而非新增 flag。无 shell 无注入。是用户配置非远程控制
- **权限**: `edits` 根强制 `0700`(platform.rs:241),阻止其他本地用户遍历其下一切。敏感内容保持私密(一个小注意:若 `edits` 缺失时 `create_dir_all` 会以 `0755` 重建,但会话中无物删它,低概率)
- **上传截断**: 上传先完整写远程 `.macsftp-part` 临时文件、验证硬链接支持,**之后**才删目标(session_actor.rs:1502)。本地内容确认可读/已拷贝才碰远程
- **上传失败保留本地编辑**: `advance_uploading_back` 回 `Editing`、保留临时文件、不 rebase(测试 `upload_back_failure_returns_to_editing_without_rebasing` 确认)。可重试、无静默丢失
- **`remove_dir_all(parent)` 安全**: `advance_downloading` 用 `Path::parent()` 守卫良构的 `<edits>/<id>/<file>` 路径,parent 恒为 `<id>/` 目录,绝非 `edits` 根或 `/`
- **`TransferSkipped` 不可达**: 编辑传输用 `OverwriteAll`,actor 不对存在冲突发 `TransferSkipped`;它只在取消时触发,而编辑上传无用户取消路径
- **DiscardLocal 在途冲突不可达**: `RemoteConflict` 只从轮询的偏离分支进入,该分支**不派发上传**,所以冲突会话无在途传输;`DiscardLocal` 分配新 id → 新临时目录,任何游离的旧传输完成 `find_by_temp_path(old)` → `None`,无碰撞
- **会话 id 无复用**: `next_id` 从 1 单调 `u64`,`remove` 从不回收,分配在单次调用内主线程串行。无复用(溢出按 brief 忽略)
- **None-mtime 无上传风暴**: 轮询变更判定 `(Some(last),Some(now)) => now>last, _ => false`;任一侧 `None` ⇒ `changed=false` ⇒ 不派发
- **Some↔None mtime 变更被检测**: 偏离用 `remote_now.is_some_and(|now| now != snapshot)`,`RemoteSnapshot` 派生 `PartialEq`,`Some != None` **会**被标记;`agrees_at_second_granularity` 对 `None→None` 截断、不会在刷新时采纳 `Some↔None` 差异,真实变更保持可检测

---

## 修复优先级建议

1. **#1 RCE(Critical)** — 发布前必修。最小改动:`None` 分支改 `open -t` 强制文本编辑器 + 剥离编辑临时文件的可执行位。这是前 3 轮审查结构上无法发现的(它活在 `open` 的内容分发行为里,不在字符串构造里)
2. **#2 重连搁浅 + #7 发送失败搁浅(High)** — 同一根本病灶:会话卡在无人轮询、无人渲染的相位(`Downloading`/`UploadingBack`),`find_active` 随即永久阻塞重新编辑。建议一并修:所有派发/终止路径失败时回退相位或移除会话
3. **#3 关闭泄漏/幽灵阻塞(Medium)** — 标签/窗口/断连关闭时移除该标签的编辑会话+临时目录
4. **#5 瞬时 stat 误删** — 常见编辑器原子保存即触发,影响面广
5. **#4 误关联 / #6 盲覆盖 / #8 大文件 modal 陈旧** — 需特定条件,但后果是破坏性的(误删目录 / 静默覆盖 / 搁浅)
6. **#9-#12 Low** — 清理/边界,可批量处理

> **根本模式**: 除 #1 RCE 外,最严重的一类(#2、#3、#7、#8)共享同一病灶——**会话如何卡进一个既无 watcher 轮询、又无 modal 渲染的相位**。`Downloading` 和 `UploadingBack` 只由传输终止事件推进;任何"命令没成功送达但相位已推进"的路径都会造成永久不可见搁浅 + `find_active` 阻塞重新编辑。系统性修法:为这两个相位加超时/看门狗,或保证每条派发路径在失败时都回退相位。

---

## 附:审查过程说明

本报告基于 3 个独立深挖 agent(并发/生命周期、状态机/边界、文件系统/资源/安全)+ 控制者对每项发现的对抗性代码验证。状态机维度的首个 agent 静默失败(零产出),已重派替代 agent 补齐,其 3 项新发现(#7 High、#8 Medium、#11 Low)均经验证后纳入。共 12 项真实 bug(1 Critical、2 High、3 Medium、6 Low),另排除 17 类疑点。

---

## 修复记录(2026-07-17)

按严重度依次修复。每项附回归测试;`bash scripts/check.sh` 全绿。

| # | 改动文件 | 关键改动 | 新增测试 |
|---|---------|---------|---------|
| 1 | `platform/src/platform.rs` | `build_open_command` 的 `None` 分支改用 `open -t`(强制默认**文本**编辑器,不按内容类型分发执行)。保留 `open -a` 指定应用分支。**不**剥离临时文件执行位——那会经上传回传剥掉远程脚本的 `+x`。 | `build_open_command_uses_text_editor_when_no_editor` |
| 2 | `core/src/core.rs`、`app/src/workspace/mod.rs` | 新增 `EditSessionStore::update_epoch_for_tab`;`connect_with` 在 epoch bump 后刷新该标签所有编辑会话的 epoch。断连保留会话(按设计),重连后刷新使保存回传被运行时接受而非静默丢弃。 | `store_update_epoch_for_tab_refreshes_only_that_tab` |
| 7 | `app/src/workspace/remote_edit.rs`、`app/src/edit_watcher.rs` | 三个派发站点失败时回退:`start_edit_download` 移除 `Downloading` 会话+删临时目录;`resolve_edit_conflict(Overwrite)` 回退 `UploadingBack`→`RemoteConflict`;`dispatch_edit_command` 的 `try_send` Err 与无窗口分支统一回退 `Editing` 并**恢复 pre-save mtime**(否则下轮轮询看不到改动、永不重试)。 | `poll_reverts_to_editing_when_command_channel_full` |
| 3 | `core/src/core.rs`、`app/src/workspace/{mod,remote_edit}.rs`、`app/src/main.rs` | 新增 `remove_for_tab`/`session_tab_ids`。关标签→`cleanup_edit_sessions_for_tab`;关窗→`cleanup_orphaned_edit_sessions`(GC 无窗口拥有的会话)。均删临时目录。**断连不清理**(按 spec 保留会话)。 | `close_tab_tears_down_its_edit_sessions_and_temp_dir`、`store_remove_for_tab_*` |
| 5 | `core/src/core.rs`、`app/src/edit_watcher.rs` | `EditSession` 加 `missing_ticks` 字段 + `EDIT_MISSING_TICKS_LIMIT=3`。stat 失败累加计数,连续 3 轮才回收;任一成功 stat 清零。容忍编辑器原子保存的瞬时 NotFound。 | `poll_tolerates_transient_stat_miss_then_reaps_after_limit` |
| 4 | `app/src/event_coordinator.rs` | `advance_edit_sessions` 关联时除本地临时路径外,再校验传输**方向**+**远程端点**匹配会话相位预期。外来同路径传输不再驱动/拆除编辑会话。 | `unrelated_transfer_sharing_temp_path_does_not_drive_edit_session` |
| 6 | `app/src/edit_watcher.rs` | 回传门控反转:仅当远程**确认**等于基线才上传;偏离**或**列表外(标签拥有但文件不在列表)都标记 `RemoteConflict`。用 `tab_is_owned` 区分「文件离开列表」(标冲突)与「无窗口拥有标签」(留 `Editing` 重试)。 | `poll_flags_conflict_when_file_left_the_listing` |
| 8 | `app/src/workspace/remote_edit.rs` | `confirm_large_edit` 用 live 标签重新校验连接、刷新 epoch/profile 后再下载;标签已断连/关闭则放弃并提示。 | `confirm_large_edit_uses_refreshed_epoch_after_reconnect` |
| 9 | `app/src/edit_watcher.rs` | 达到 miss 上限回收会话时同时 `remove_dir_all` 父目录,不留空 `<edits>/<id>/`。 | (覆盖于 #5 测试的删除断言) |
| 10 | `app/src/workspace/remote_edit.rs` | `DiscardLocal` 在 `start_edit_download` 铸新 id 前删旧会话临时目录。 | (覆盖于 `resolve_conflict_discard_local_redownloads`) |
| 11 | `app/src/workspace/remote_edit.rs` | `resolve_edit_conflict` 入口加相位守卫:非 `RemoteConflict` 即返回,双击第二次成为 no-op。 | `resolve_conflict_double_overwrite_dispatches_once` |
| 12 | — | 不修。用户在设计阶段明确选择「仅 `(size, mtime)`、无哈希」(方案 A),同秒同大小并发改动漏检是该方案的固有限制。若未来要闭合此窗口需引入哈希,属功能变更而非 bug 修复。 | — |

**设计一致性说明**: #3 的断连维度**刻意不拆除**会话——spec §断线处理明确要求「保留会话+临时文件,重连后可继续保存」。#2 的 epoch 刷新正是让这个保留的会话在重连后真正可用。二者配合实现设计意图,而非各自为战。

**测试计数**: 新增 10 个回归测试(platform 1、core 2、event_coordinator 1、edit_watcher 3、workspace 3)。全工作区测试通过,clippy `-D warnings` 无告警。

