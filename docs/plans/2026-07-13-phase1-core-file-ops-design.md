# 阶段 1 设计 — 核心文件操作（统一本地/远程 + 删除确认）

**Date:** 2026-07-13 GMT+8
**来源：** `docs/ux-improvement-plan.md` 阶段 1；`docs/ui-ux-guidelines.md` 规范。
**方法：** brainstorming 流程，5 个决策点已与用户确认（见末尾 §决策记录）。

---

## 决策摘要（已确认）

1. **统一抽象形态：** 单一 `FsCommand` 枚举 + `side` 判别（`Local | Remote`），由统一 dispatcher 按 side 路由到 `platform`（本地，同步）或 `RemoteSessionActor`（远程）。
2. **确认交互：** 危险样式居中 modal（`Cancel` 默认获焦，`Delete` 危险色）。
3. **触发策略：** 所有删除都弹确认 modal；带「不再询问」复选框，勾选后写入 `AppConfig.confirm_delete=false` 持久化。
4. **递归执行：** 确认即执行，远程 actor 深度优先逐项枚举+删除，遇首个错误即停止并报告剩余（fail-fast）。本地用 `std::fs::remove_dir_all`。
5. **统一范围：** 最小统一——**只**把新增的增删改（`Delete`/`Rename`/`CreateDirectory`）走统一 `FsCommand` + 新 `FsOperationFailed` 事件；现有读取路径（`ReadRemoteDir`/`ReadLocalDir`、`RemoteDirLoaded`/`LocalDirLoaded`/`RemoteOperationFailed`）原样保留，不动已工作的代码。

---

## 1. 命令与事件模型（最小统一）

在 `crates/core/src/core.rs` 新增类型，并把 `Fs` 变体加进 `AppCommand`：

```rust
/// 操作目标后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsSide { Local, Remote }

/// 把一次 Fs 操作/事件钉到某个 tab；远程额外带 session_epoch 以过 stale-event 守卫。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsScope {
    Local { tab_id: TabId },
    Remote { tab_id: TabId, session_epoch: u64 },
}
impl FsScope {
    pub fn tab_id(&self) -> TabId { /* 两分支取 tab_id */ }
    pub fn session_epoch(&self) -> Option<u64> { /* Remote 才 Some */ }
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

`FsPath` 为枚举，与 `FsScope` 的 side 对应，避免「side 与路径类型」两处来源不一致：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsPath {
    Local(crates::platform::LocalPath),
    Remote(crates::core::RemotePath),
}
```

`AppCommand` 新增：`Fs(FsCommand)`。
`AppEvent` 新增（两侧共享失败事件）：

```rust
FsOperationFailed { scope: FsScope, failure: UserFacingError },
```

**成功语义：** 增删改成功后**自动触发一次当前路径的 ReadDir 刷新**（复用现有 `ReadRemoteDir`/`ReadLocalDir` 命令与 `RemoteDirLoaded`/`LocalDirLoaded` 事件）。读取路径完全不动。

---

## 2. Dispatcher 路由

在现有 `AppCommand` 派发点（runtime 或 app workspace 的 match）新增 `AppCommand::Fs(cmd)` 分支：

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
     actor_tx.send(FsRequest { scope, op: cmd.op }).await;     // 转发给 RemoteSessionActor
  }
}
```

本地分支**同步**执行（与现有 `ReadLocalDir` 一致）；远程分支把消息发给 actor，由 actor 在异步任务里执行并回发事件。

---

## 3. 远程 actor 实现（`session_actor.rs`）

在 `handle_request` 现有 `ReadRemoteDir` 分支旁新增 `Fs` 分支，使用已有 `SftpSession`：

- `Rename` → `sftp.rename(from, to)`
- `CreateDirectory` → `sftp.create_dir(parent.join(name))`
- `Delete` 单文件 → `sftp.remove_file(path)`
- `Delete` 目录（递归）→ **深度优先遍历**：
  ```
  async fn remove_dir_recursive(sftp, path):
      for entry in sftp.read_dir(path):
          child = path.join(entry.name)
          if entry.is_dir: remove_dir_recursive(sftp, child)   // 先删子树
          else:             sftp.remove_file(child)
      sftp.remove_dir(path)                                   // 空了再删自己
  ```
  遇首个 `SftpError` 即 `return Err`（fail-fast），停止遍历并回发 `FsOperationFailed`。
- 成功 → 复用现有 read_dir 逻辑回发 `RemoteDirLoading`/`RemoteDirLoaded` 刷新当前路径。
- 错误 → `event_tx.send(FsOperationFailed{ scope, failure: sftp_error.into() })`。

> 注：russh-sftp 无原生递归删除，必须服务端逐项删（与计划 §1a 一致）。

---

## 4. 本地 platform API（`crates/platform/src/platform.rs`）

新增三个同步函数（当前仅有 `read_local_directory`，删除/重命名只在测试里用 `std::fs` 直调）：

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

**触发时机：** UI 侧捕获删除意图（菜单 / `cmd-delete` / 按钮）后——
- 若 `AppConfig.confirm_delete == true` → 打开确认 modal，汇总待删条目；用户点 `Delete` 才派发 `FsCommand::Delete`。
- 若为 `false` → 直接派发，不弹窗。

**modal 内容（危险样式）：**
- 标题：`Delete N item(s)?`
- 正文：列出至多 K 个名称（超出显示 `and M more`）。
- 若任一待删条目 `is_dir`：红色警示区块——`Includes M director(y/ies). Recursive delete removes all contents permanently.`
- 按钮：`Cancel`（默认获焦）/ `Delete`（危险红）。
- 「Don't ask again」复选框（仅当 `confirm_delete` 当前为 true 时显示）；勾选且确认后写入 `AppConfig.confirm_delete = false` 并持久化（storage crate）。
- 多选删除：由当前 selection 构建 `entries`，**单个 modal 汇总**所有选中项。

**配置（`crates/storage/src/config.rs`）：** `AppConfig` 增加字段
```rust
pub confirm_delete: bool,   // Default = true
```
`#[serde(default)]` 已在 struct 上，`Default` 实现需补该字段。

---

## 6. 重命名与新建文件夹（行内交互）

**重命名（`F2` / 菜单）：**
- 触发后 pane 状态置 `editing_entry_id`，对应行渲染为 `InputState`（默认值=当前名、basename 预选）。
- `Enter` 校验（非空、不含 `/`、不与同级重名）→ 派发 `FsCommand::Rename{from,to}` → 成功刷新；失败就地报错（复用 `FsOperationFailed`）。`Esc` 取消。

**新建文件夹（`cmd-shift-n` / 菜单 / 空白区右键）：**
- 触发后 `creating_folder_in = Some(dir)`，行内 `InputState` 默认 `Untitled Folder`。
- `Enter` → `FsCommand::CreateDirectory{parent,name}` → 刷新。

---

## 7. UI 入口（三入口齐全，规范 §2.2）

- **右键上下文菜单**（新组件，`crates/ui`）：
  - 文件行右键：`Rename / Delete / New Folder / Download(remote) / Upload(local) / Copy Path / Reveal(仅本地)`。
  - 空白区右键：`New Folder / Refresh`。
- **快捷键**（`crates/app/src/workspace/app_actions.rs`，稳定 action 名供阶段 4 palette 复用）：
  - `cmd-delete` → `DeleteSelection`
  - `enter` / `F2` → `RenameEntry`
  - `cmd-shift-n` → `NewFolder`
- **按钮：** path bar / toolbar 提供 `New Folder`、`Delete`（作用于 selection）。
- 菜单项与 action 共用同一稳定 action，保证键盘 / 鼠标 / palette 三入口一致。

---

## 8. 错误处理与刷新

- `FsOperationFailed` 落地（`event_handling.rs`）：在对应 tab pane 内展示错误态（阶段 2 补 Retry 按钮；本阶段先展示错误标题+信息，可重新发起操作）。
- 成功路径：dispatcher / actor 自动重新 `ReadDir` 当前路径 → 列表原地刷新，无需手动 refresh。
- 递归删除部分失败：actor 在首个错误处停止，已删条目不可撤销（远程无 undo）——modal 前置警示已说明此风险；回发的 `FsOperationFailed` 携带该错误。

---

## 9. 测试计划

- **platform 单测：** 删文件 / 删空目录 / 删非空目录（递归）/ 重命名 / 新建目录；错误用例（权限不足、路径缺失）。
- **core 单测：** `FsCommand`/`FsScope`/`FsPath` 构造与 side/path 映射。
- **session_actor 单测：** 递归删除遍历顺序（深度优先）、中途错误 fail-fast（用临时 sshd 或伪造 `SftpSession`）。
- **UI 测试（`crates/app`）：** 确认 modal 显示数量 + 递归警示；「不再询问」持久化并抑制下次弹窗；批量汇总；行内重命名提交/取消；新建文件夹创建。
- **手测：** 连真实服务器建/删/重命名远程目录与文件；本地同样；递归目录删除走确认；失败就地可重试。

---

## 10. 改动文件清单（map）

| 文件 | 改动 |
| --- | --- |
| `crates/core/src/core.rs` | 新增 `FsSide`/`FsScope`/`FsEntryRef`/`FsOp`/`FsCommand`；`AppCommand::Fs`；`AppEvent::FsOperationFailed`；`FsPath` 枚举 |
| `crates/storage/src/config.rs` | `AppConfig.confirm_delete: bool`（默认 true） |
| `crates/platform/src/platform.rs` | 新增 `delete_entry`/`rename_entry`/`create_directory` |
| `crates/sftp/src/session_actor.rs` | `handle_request` 新增 `Fs` 分支：rename/create_dir/递归 delete 遍历；错误发 `FsOperationFailed` |
| `crates/app/src/workspace/event_handling.rs` | 派发 `AppCommand::Fs`（本地同步 / 远程转发）；处理 `FsOperationFailed` |
| `crates/app/src/workspace/modals.rs` | 删除确认 modal（+「不再询问」） |
| `crates/app/src/workspace/panes.rs` / `render.rs` | 上下文菜单触发、行内重命名、New Folder 行内、危险样式 |
| `crates/ui/src/*` | 上下文菜单组件 |
| `crates/app/src/workspace/app_actions.rs` | 快捷键绑定（`DeleteSelection`/`RenameEntry`/`NewFolder`） |

---

## 11. 开放问题与不在范围

- **dispatcher 落点：** runtime.rs 还是 app workspace `mod.rs` 的现有 `AppCommand` match——取现有派发点即可。
- **Reveal：** 本地=在 Finder 中显示；远程无对应操作（菜单项仅本地出现）。
- **读取路径不改动**（决策 5）：`RemoteOperationFailed` 仍用于读取失败，新 `FsOperationFailed` 仅用于增删改；后续阶段可再统一，本阶段不碰。
- **阶段 2 的 Retry 按钮 / 就地错误恢复 UI**：本设计只保证错误事件可达 + 列表刷新，完整错误态 UI 在阶段 2 补齐。

---

## 决策记录（brainstorming 5 问）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 本地/远程如何统一 | 单枚举 `FsCommand` + `side` 判别（推荐） |
| 2 | 确认交互形态 | 危险 modal（推荐） |
| 3 | 确认触发策略 | 全部确认 + 「不再询问」持久化（推荐） |
| 4 | 递归删除执行模型 | 确认即执行，深度优先，遇错停止（推荐） |
| 5 | 统一范围 | 最小统一（只动增删改，保留读取路径）（推荐） |
