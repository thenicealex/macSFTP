# 阶段 1 设计 — 核心文件操作（统一本地/远程 + 删除确认）

**Date:** 2026-07-13 GMT+8
**来源：** `docs/ux-improvement-plan.md` 阶段 1 和 `docs/ui-ux-guidelines.md` 规范。
**方法：** 本设计采用 brainstorming 流程，并且 5 个决策点均已与用户确认，详情见末尾的“决策记录”。

---

## 决策摘要（已确认）

1. **统一抽象形态：** 使用单一 `FsCommand` 枚举和 `side` 判别（`Local | Remote`），因此统一 dispatcher 可以根据 side 将命令路由至 `platform`（本地，同步）或 `RemoteSessionActor`（远程）。
2. **确认交互：** 使用危险样式的居中 modal，并且默认焦点位于 `Cancel`，而 `Delete` 使用危险色。
3. **确认策略：** 所有删除操作都显示确认 modal；modal 包含「不再询问」复选框，因此用户选中该项后，应用将 `AppConfig.confirm_delete=false` 写入持久化配置。
4. **递归执行：** 用户确认后立即执行。远程 actor 按深度优先顺序逐项枚举并删除；但是出现首个错误时立即停止，并报告尚未删除的项目（fail-fast）。本地操作使用 `std::fs::remove_dir_all`。
5. **统一范围：** 采用最小统一范围，因此**仅**让新增的增删改操作（`Delete`/`Rename`/`CreateDirectory`）使用统一的 `FsCommand` 和新的 `FsOperationFailed` 事件。现有读取路径（`ReadRemoteDir`/`ReadLocalDir`、`RemoteDirLoaded`/`LocalDirLoaded`/`RemoteOperationFailed`）保持不变，从而避免修改已经有效的代码。

---

## 1. 命令与事件模型（最小统一）

在 `crates/core/src/core.rs` 中新增以下类型，并且在 `AppCommand` 中增加 `Fs` 变体：

```rust
/// 操作目标后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsSide { Local, Remote }

/// 将一次 Fs 操作或事件关联到指定 tab；远程作用域额外包含 session_epoch，以便执行 stale-event 校验。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsScope {
    Local { tab_id: TabId },
    Remote { tab_id: TabId, session_epoch: u64 },
}
impl FsScope {
    pub fn tab_id(&self) -> TabId { /* 两个分支均返回 tab_id */ }
    pub fn session_epoch(&self) -> Option<u64> { /* 仅 Remote 返回 Some */ }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntryRef { pub path: FsPath, pub is_dir: bool }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsOp {
    Delete { entries: Vec<FsEntryRef> },
    Rename { from: FsPath, to: FsPath },
    CreateDirectory { parent: FsPath, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsCommand { pub scope: FsScope, pub op: FsOp }
```

`FsPath` 是枚举，并且其变体与 `FsScope` 的 side 对应，因此 side 与路径类型不会由两个互不关联的来源定义：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsPath {
    Local(crates::platform::LocalPath),
    Remote(crates::core::RemotePath),
}
```

`AppCommand` 增加 `Fs(FsCommand)` 变体。
`AppEvent` 增加以下变体，供本地和远程操作共享失败事件：

```rust
FsOperationFailed { scope: FsScope, failure: UserFacingError },
```

**成功语义：** 增删改成功后，系统**自动为当前路径执行一次 ReadDir 刷新**，并且复用现有的 `ReadRemoteDir`/`ReadLocalDir` 命令与 `RemoteDirLoaded`/`LocalDirLoaded` 事件。因此，读取路径保持不变。

---

## 2. Dispatcher 路由

在现有的 `AppCommand` 分派位置（runtime 或 app workspace 的 match）增加 `AppCommand::Fs(cmd)` 分支：

```
match cmd.scope {
  FsScope::Local { tab_id } => {
     let r = match cmd.op {
        Delete { entries }      => entries.iter().map(|e| platform::delete_entry(&e.path.local()?, e.is_dir)).collect::<Result<_,_>>(),
        Rename { from, to }    => platform::rename_entry(&from.local()?, &to.local()?),
        CreateDirectory { parent, name } => platform::create_directory(&parent.local()?, &name),
     };
     match r {
        Ok(_)  => refresh_local(tab_id),                       // -> LocalDirLoaded
        Err(e) => event_tx.send(FsOperationFailed{ scope, failure: e.into() }),
     }
  }
  FsScope::Remote { tab_id, session_epoch } => {
     actor_tx.send(FsRequest { scope, op: cmd.op }).await;     // 转交 RemoteSessionActor
  }
}
```

本地分支采用**同步**执行方式，这与现有 `ReadLocalDir` 一致。但是远程分支会将消息发送至 actor，并由 actor 在异步任务中执行操作和返回事件。

---

## 3. 远程 actor 实现（`session_actor.rs`）

在 `handle_request` 现有的 `ReadRemoteDir` 分支旁增加 `Fs` 分支，并且使用已有的 `SftpSession`：

- `Rename` → `sftp.rename(from, to)`
- `CreateDirectory` → `sftp.create_dir(parent.join(name))`
- `Delete` 单文件 → `sftp.remove_file(path)`
- `Delete` 目录（递归）→ 使用**深度优先遍历**：
  ```
  async fn remove_dir_recursive(sftp, path):
      for entry in sftp.read_dir(path):
          child = path.join(entry.name)
          if entry.is_dir: remove_dir_recursive(sftp, child)   // 先删子树
          else:             sftp.remove_file(child)
      sftp.remove_dir(path)                                   // 子项均删除后再删除该目录
  ```
  出现首个 `SftpError` 时立即 `return Err`（fail-fast），因此终止遍历并返回 `FsOperationFailed`。
- 成功 → 复用现有 read_dir 逻辑返回 `RemoteDirLoading`/`RemoteDirLoaded`，从而刷新当前路径。
- 错误 → 执行 `event_tx.send(FsOperationFailed{ scope, failure: sftp_error.into() })`。

> 注：russh-sftp 不提供原生递归删除功能，因此必须在服务端逐项删除，这与计划 §1a 一致。

---

## 4. 本地 platform API（`crates/platform/src/platform.rs`）

新增以下三个同步函数。当前仅有 `read_local_directory`，而且删除和重命名仅在测试中直接调用 `std::fs`：

```rust
pub fn delete_entry(path: &LocalPath, is_dir: bool) -> std::io::Result<()> {
    if is_dir { std::fs::remove_dir_all(path.as_str()) }
    else      { std::fs::remove_file(path.as_str()) }
}
pub fn rename_entry(from: &LocalPath, to: &LocalPath) -> std::io::Result<()> {
    std::fs::rename(from.as_str(), to.as_str())
}
pub fn create_directory(parent: &LocalPath, name: &str) -> std::io::Result<LocalPath> {
    let p = parent.join(name);
    std::fs::create_dir(&p)?;
    Ok(p)
}
```

---

## 5. 删除确认 modal（`crates/app/src/workspace/modals.rs`）

**显示时机：** UI 侧识别删除意图（菜单 / `cmd-delete` / 按钮）后，按照以下配置处理：
- 若 `AppConfig.confirm_delete == true` → 显示确认 modal，并汇总待删除条目；用户选择 `Delete` 后才分派 `FsCommand::Delete`。
- 若该值为 `false` → 直接分派命令，并且不显示 modal。

**modal 内容（危险样式）：**
- 标题：`Delete N item(s)?`
- 正文：最多列出 K 个名称；如果数量超过 K，则显示 `and M more`。
- 如果任一待删除条目的 `is_dir` 为 true，则显示红色警示区块：`Includes M director(y/ies). Recursive delete removes all contents permanently.`
- 按钮：`Cancel`（默认获焦）/ `Delete`（危险红）。
- 「Don't ask again」复选框仅在 `confirm_delete` 当前为 true 时显示；用户选中该项并确认后，应用将 `AppConfig.confirm_delete = false` 写入持久化配置（storage crate）。
- 多选删除：根据当前 selection 创建 `entries`，并且由**单个 modal 汇总**所有选中项。

**配置（`crates/storage/src/config.rs`）：** `AppConfig` 增加以下字段：
```rust
pub confirm_delete: bool,   // Default = true
```
struct 已经具有 `#[serde(default)]`，因此还需要在 `Default` 实现中增加该字段。

---

## 6. 重命名与新建文件夹（行内交互）

**重命名（`F2` / 菜单）：**
- 用户启用重命名后，pane 将状态设置为 `editing_entry_id`，并将对应行渲染为 `InputState`（默认值为当前名称，并预选 basename）。
- `Enter` 执行校验（非空、不含 `/`、不与同级条目重名）→ 分派 `FsCommand::Rename{from,to}` → 成功后刷新；但是失败时在当前位置显示错误，并复用 `FsOperationFailed`。`Esc` 取消操作。

**新建文件夹（`cmd-shift-n` / 菜单 / 空白区右键）：**
- 用户启用新建文件夹后，状态为 `creating_folder_in = Some(dir)`，并且行内 `InputState` 的默认值为 `Untitled Folder`。
- `Enter` → `FsCommand::CreateDirectory{parent,name}` → 成功后刷新。

---

## 7. UI 入口（包含三个入口，符合规范 §2.2）

- **右键上下文菜单**（`crates/ui` 中的新组件）：
  - 文件行右键：`Rename / Delete / New Folder / Download(remote) / Upload(local) / Copy Path / Reveal(仅本地)`。
  - 空白区右键：`New Folder / Refresh`。
- **快捷键**（`crates/app/src/workspace/app_actions.rs`）：采用稳定的 action 名，因此阶段 4 的 palette 可以复用这些名称。
  - `cmd-delete` → `DeleteSelection`
  - `enter` / `F2` → `RenameEntry`
  - `cmd-shift-n` → `NewFolder`
- **按钮：** path bar / toolbar 提供 `New Folder` 和 `Delete`，并且 `Delete` 作用于 selection。
- 菜单项与 action 使用同一个稳定 action，因此键盘、鼠标和 palette 三个入口的行为一致。

---

## 8. 错误处理与刷新

- `FsOperationFailed` 处理（`event_handling.rs`）：在对应 tab pane 内显示错误状态。阶段 2 将增加 Retry 按钮；但是本阶段仅显示错误标题和信息，并允许用户重新发起操作。
- 成功路径：dispatcher / actor 自动为当前路径重新执行 `ReadDir`，因此列表在当前位置刷新，无需用户执行 refresh。
- 递归删除部分失败：actor 在首个错误处停止。已经删除的条目不可撤销，因为远程操作不提供 undo；因此 modal 中的前置警示需要说明该风险，并且返回的 `FsOperationFailed` 携带对应错误。

---

## 9. 测试计划

- **platform 单测：** 删除文件 / 删除空目录 / 删除非空目录（递归）/ 重命名 / 新建目录；错误用例包括权限不足和路径缺失。
- **core 单测：** `FsCommand`/`FsScope`/`FsPath` 构造与 side/path 映射。
- **session_actor 单测：** 验证递归删除的深度优先遍历顺序，并且使用临时 sshd 或伪造的 `SftpSession` 验证中途错误时的 fail-fast 行为。
- **UI 测试（`crates/app`）：** 验证确认 modal 的数量和递归警示；验证「不再询问」配置可以持久化，并且下一次不再显示 modal；验证批量汇总、行内重命名的提交与取消，以及新建文件夹。
- **人工测试：** 连接真实服务器，并验证远程目录和文件的新建、删除及重命名；对本地目录和文件执行相同验证；验证递归目录删除需要确认；验证失败后可以在当前位置重试。

---

## 10. 改动文件清单（map）

| 文件 | 改动 |
| --- | --- |
| `crates/core/src/core.rs` | 新增 `FsSide`/`FsScope`/`FsEntryRef`/`FsOp`/`FsCommand`、`AppCommand::Fs`、`AppEvent::FsOperationFailed` 和 `FsPath` 枚举 |
| `crates/storage/src/config.rs` | `AppConfig.confirm_delete: bool`（默认 true） |
| `crates/platform/src/platform.rs` | 新增 `delete_entry`/`rename_entry`/`create_directory` |
| `crates/sftp/src/session_actor.rs` | `handle_request` 新增 `Fs` 分支：执行 rename/create_dir/递归 delete 遍历；错误时发送 `FsOperationFailed` |
| `crates/app/src/workspace/event_handling.rs` | 分派 `AppCommand::Fs`（本地同步 / 远程转交）；处理 `FsOperationFailed` |
| `crates/app/src/workspace/modals.rs` | 删除确认 modal（+「不再询问」） |
| `crates/app/src/workspace/panes.rs` / `render.rs` | 上下文菜单启用操作、行内重命名、New Folder 行内编辑、危险样式 |
| `crates/ui/src/*` | 上下文菜单组件 |
| `crates/app/src/workspace/app_actions.rs` | 快捷键绑定（`DeleteSelection`/`RenameEntry`/`NewFolder`） |

---

## 11. 开放问题与不在范围

- **dispatcher 位置：** 可以使用 runtime.rs 或 app workspace `mod.rs` 中现有 `AppCommand` match 的位置，因此以当前的命令分派位置为准。
- **Reveal：** 本地操作表示在 Finder 中显示；但是远程端没有对应操作，因此该菜单项仅在本地端显示。
- **读取路径保持不变**（决策 5）：`RemoteOperationFailed` 继续表示读取失败，而新的 `FsOperationFailed` 仅用于增删改操作。后续阶段可以统一两类事件，但是本阶段不修改读取路径。
- **阶段 2 的 Retry 按钮 / 当前位置错误恢复 UI**：本设计仅保证错误事件可达和列表刷新。完整的错误状态 UI 将在阶段 2 完成。

---

## 决策记录（brainstorming 5 问）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 本地/远程如何统一 | 单枚举 `FsCommand` + `side` 判别（推荐） |
| 2 | 确认交互形态 | 危险 modal（推荐） |
| 3 | 确认策略 | 所有删除均需确认，并且持久化「不再询问」配置（推荐） |
| 4 | 递归删除执行模型 | 确认后立即执行，采用深度优先顺序，并在出现错误时停止（推荐） |
| 5 | 统一范围 | 最小统一（仅修改增删改，保留读取路径）（推荐） |
