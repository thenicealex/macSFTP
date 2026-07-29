# Cancel All Transfers & Clear Transfer Records

## 背景

用户希望一键取消所有活跃/排队中的 transfer，以及一键删除所有已终态的 transfer 记录（Completed、Skipped、Failed），解决逐个点击效率低的问题。

## 设计

### 方案：视图侧批量操作（零 runtime 改动）

#### 1. 新 Trash 图标

- 在 `crates/ui/src/icon.rs` 的 `IconName` 枚举新增 `Trash` 变体
- 新增 `crates/app/assets/icons/trash.svg`
- 在 `crates/app/src/assets.rs` 的静态 SVG 数组和测试中注册

#### 2. core: TransferStore::clear_terminal()

- 新增方法，移除所有 Completed/Skipped/Failed 状态的 jobs
- 级联移除关联的 plans（plan 的所有 job 都被清除后删除 plan）
- 返回 bool 表示是否有变化

#### 3. app: ActiveTransfers 扩展

- 在 `ActiveTransfers` trait 新增 `clear_terminal_transfers() -> bool` 方法
- 实现：清除 TransferStore 的终态 jobs + plans，同时 clean up RateBook 中的对应条目
- 调用 `refresh_windows()` 刷新 UI

#### 4. app: Workspace 新增方法

- `cancel_all_transfers(cx)`: 遍历 `visible_transfer_jobs()`，对所有 Queued/Running/Planning/WaitingForConflictDecision 状态的 job 调用 `cancel_transfer(transfer_id, cx)`
- `clear_transfer_records(cx)`: 调用 `cx.clear_terminal_transfers()`，重置 completed/failed section 折叠状态

#### 5. UI: 抽屉标题栏两个按钮

在 `transfer_render.rs` 的 header 行（`Transfers` 标签右侧）放两个 icon button：
- **Cancel All**: `IconName::Close`，tooltip `"Cancel All Transfers"`，条件渲染（active + queued > 0）
- **Clear Records**: `IconName::Trash`，tooltip `"Clear Transfer History"`，条件渲染（completed + failed > 0）

### 不改的

- 不新增 AppCommand
- 不修改 runtime / TransferManager
- 不新增确认弹窗
- Cancel All 复用已有 per-transfer 取消路径

### 验证

- Cancel All: 有活跃 + 排队任务时点击，所有任务变为 Cancelling/Skipped
- Clear Records: 有终态记录时点击，记录消失，section 折叠重置
- 无任务时按钮不显示
- 按钮 hover 有 tooltip
- `every_icon_name_resolves_to_an_embedded_asset` 测试通过
