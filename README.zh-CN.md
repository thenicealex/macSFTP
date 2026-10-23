# macSFTP

[English](README.md) | **简体中文** | [日本語](README.ja.md) | [한국어](README.ko.md)

使用 Rust 构建的原生 macOS SFTP 客户端。macSFTP 把本地和远端文件放在同一个窗口里：顶部是连接标签页，中间是左右两个文件面板，底部是传输抽屉。界面由 GPUI 与 GPUI Base 绘制，SSH 和 SFTP 通过 `russh`、`russh-sftp` 在 Tokio 上运行。

macSFTP 仍处于预发布阶段。它可以构建未签名的应用包供本地使用，公开发布只提供源码。Developer ID 签名、公证、自动更新以及 SFTP 之外的协议都不在当前范围内。

## 截图

**本地与远端面板**

![macSFTP 深色工作区与示例文件](docs/screenshots/workspace-dark.jpg)

**使用已保存的连接配置或临时连接**

![macSFTP 连接对话框与示例连接信息](docs/screenshots/connect-dark.jpg)

**浅色外观与外部编辑器设置**

![macSFTP 浅色外观与外部编辑器设置](docs/screenshots/settings-light.jpg)

截图使用隔离的预览环境与本地示例文件。右侧服务器面板处于未连接状态；`demo.example` 与 `deploy` 均为示例连接信息。

## 功能

**连接与安全**

- 多窗口，每个窗口拥有独立的连接标签页
- 已保存的连接配置，以及不会进入配置列表的临时连接
- 密码、多轮 keyboard-interactive、SSH agent 和私钥认证
- 支持 Ed25519、ECDSA 与 RSA-SHA2 客户端密钥；RSA 直接签名使用 AWS-LC
- 已保存配置的单跳 jump host，以及显式配置的 OpenSSH 风格 `ProxyCommand`
- 兼容 OpenSSH 的 host key 校验，支持 `known_hosts` 子集
- 未知主机需要用户确认；主机 key 与已保存的不一致时阻断连接
- 密码与私钥口令保存在 macOS Keychain，内存中只保留短期副本

**文件浏览**

- 本地与远端面板，支持前进/后退历史、面包屑与路径跳转
- 虚拟化文件列表，1 万条记录下仍保持响应
- 筛选、排序、多选、Finder 式键入跳转以及隐藏文件
- 重命名、新建文件夹、删除、复制路径；本地文件还可「在 Finder 中显示」
- 文件列表与输入框的右键菜单由原生 AppKit 菜单呈现

**传输**

- 目录上传/下载以 plan 建模，并流式发布进度
- 流水线下载：16 个在途读取请求、256 KiB 分块，每个 plan 最多 4 个并发任务，全局最多 6 个
- 冲突处理支持按 plan 生效的「全部应用」、取消与重试
- 进程级传输队列，所有窗口共享同一个传输抽屉
- 元数据保留失败以警告呈现，已传输的数据仍视为成功
- 符号链接按链接本身复制，默认不解引用

**远端编辑**

- 打开远端文件（双击、`↩` 或右键菜单）时，在外部编辑器中打开临时副本
- 被本地修改的副本在列表中显示 `●`，上传只由显式操作触发
- 明确执行 **Upload Changes**，上传前先读取实时远端元数据
- 远端发生变化时进入冲突决策，可执行 **Compare Changes…**
- 支持按通配符/扩展名指定编辑器，状态栏显示编辑状态

**界面**

- One Light 与 One Dark 两套主题，可跟随系统或固定指定
- 命令面板覆盖应用级操作；窗口与应用生命周期命令保留在标准菜单位置
- 文件列表、modal、标签页与设置均可用键盘操作
- 传输进度、错误与空态都会说明下一步该做什么

## 连接服务器

按 `⌘T` 新建连接标签页，然后在服务器面板点击 **Connect…**，填写服务器、端口、用户名与登录方式。选择 **Enter Details** 时，连接保持临时状态：不会保存为配置，也不会保存凭据，但该端点会出现在 Recent Connections 中。若要将其保存为配置，请选择已有配置或打开 **Manage Connections…**；连接配置在 **Settings → Saved Connections** 中创建和编辑。

成功连接后会记录在服务器面板的 **Recent Connections** 列表中（最多 6 条），包含显示名称、`user@host:port` 与上次的远端目录。若对应的已保存配置仍有可用凭据，选择后会直接重连；否则会打开已预填的 Connect 表单。

首次连接未知主机时会显示服务器地址、key 类型、SHA-256 指纹，以及将要写入的 `known_hosts` 位置。主操作 **Trust and Save** 需要用户明确决定；若主机 key 与已保存的不一致，连接会被阻断。

## 键盘快捷键

| 快捷键 | 操作 |
| --- | --- |
| `⌘N` / `⌘⇧W` | 新建窗口 / 关闭窗口 |
| `⌘T` / `⌘W` | 新建连接标签页 / 关闭标签页 |
| `⌘⇧]` / `⌘⇧[` | 下一个 / 上一个标签页 |
| `⌃Tab` | 标签页切换器，按最近使用排序 |
| `⌘1` / `⌘2` | 聚焦本地 / 服务器面板 |
| `⌘↑` | 返回上级目录 |
| `⌘[` / `⌘]` | 后退 / 前进 |
| `⌘⇧G` | 跳转到路径 |
| `⌘F` | 筛选文件 |
| `⌘⇧.` | 显示隐藏文件 |
| `↩` / `F2` | 打开 / 重命名 |
| `⌘⇧N` / `⌘⌫` | 新建文件夹 / 删除 |
| `⌘⇧C` | 复制路径 |
| `⌘U` / `⌘D` | 上传 / 下载选中项 |
| `⌘⇧U` | 上传已编辑文件 |
| `⌘J` | 显示传输抽屉 |
| `⌘R` / `⌘⇧R` | 刷新 / 重新连接 |
| `⌘⇧P` | 命令面板 |
| `⌘,` | 设置 |

多数命令同时提供菜单栏项；命令面板（`⌘⇧P`）可搜索当前窗口的应用级操作。

## 远端编辑流程

远端编辑保持显式：在编辑器中保存文件不会触发上传。

1. 打开远端文件（双击、`↩` 或右键菜单）。macSFTP 下载临时副本并在外部编辑器中打开。
2. 编辑并保存副本。副本发生变化时，文件列表会以 `●` 标记。
3. 重新选中该远端文件，在状态栏点击 **Upload Changes**，或使用 **Upload Edited File**（`⌘⇧U`）。
4. macSFTP 读取实时远端元数据。若服务器文件已变化，会在覆盖前询问并提供 **Compare Changes…**。
5. 上传失败时保留本地副本，可以再次重试。

**Compare Changes…** 每次都把远端文件重新下载到独立目录，旧快照不会被视为当前服务器版本。关闭标签页或窗口会结束对应编辑会话并清理临时目录。

## 系统要求

- macOS 13 或更高版本。arm64 是经过验证的架构：工具链固定该目标，发布记录也覆盖它
- Xcode
- Rust `1.96.1`（由 `rust-toolchain.toml` 固定），并安装 `rustfmt` 与 `clippy`
- 架构检查需要 Python `3.11` 或更高版本（CI 使用 `3.13`）
- 真实会话测试需要 `/usr/sbin/sshd`、`ssh-keygen` 和 `ssh-keyscan`

Metal shader 在应用启动时编译，因此构建 macSFTP 不再需要单独安装 Metal Toolchain 组件。

## 构建与运行

```bash
cargo run                 # 从 workspace 直接运行
bash scripts/check.sh     # fmt、架构、日志安全、测试与 clippy
bash scripts/build_app.sh # 在 build/macSFTP.app 生成未签名应用包
```

用 `open build/macSFTP.app` 打开应用包。它未签名，适用于本地测试；分发需要 Developer ID 签名与公证。

`check.sh` 运行与 CI 相同的格式、架构、日志安全、测试与 Clippy 门禁。SFTP 集成测试会连接本机 `sshd`：本地不可用时会带明确说明跳过，而 CI 设置 `MACSFTP_REQUIRE_SSHD=1`，遇到同样情况会直接失败。修改 `Cargo.toml`、`Cargo.lock` 或 vendor 代码时还需要运行：

```bash
cargo deny --locked check advisories bans licenses sources
```

## 数据存储位置

| 路径 | 内容 |
| --- | --- |
| `~/Library/Application Support/macSFTP/config.json` | 偏好设置 |
| `~/Library/Application Support/macSFTP/profiles.json` | 已保存的连接配置，不含敏感值 |
| `~/Library/Application Support/macSFTP/known_hosts` | 你已信任的 host key |
| `~/Library/Application Support/macSFTP/session.json` | 窗口与标签页恢复信息 |
| `~/Library/Application Support/macSFTP/recents.json` | 最近连接目标 |
| `~/Library/Application Support/macSFTP/edits/` | 远端编辑使用的临时副本 |
| `~/Library/Application Support/macSFTP/residual_temp.json` | 中断的传输遗留的临时文件记录 |
| `~/Library/Logs/macSFTP/macsftp.log` | 诊断日志 |

密码和私钥口令保存在 macOS Keychain 中，配置文件只保存引用。可以用命令面板中的 **Show Logs** 打开日志目录。写入日志前会脱敏密钥值、完整私钥路径与 ProxyCommand 内容。

## 架构

Workspace 按职责拆分：

| Crate | 职责 |
| --- | --- |
| `macsftp-core` | 纯模型与状态机 |
| `macsftp-ui` | 项目风格的 GPUI Base 控件、主题与表现层 |
| `macsftp-native-menu` | 隔离的 macOS AppKit 菜单呈现器，服务输入框与文件面板右键菜单 |
| `macsftp-app` | 窗口、Action、UI 状态与事件协调 |
| `macsftp-sftp` | Tokio runtime、russh adapter、会话与传输 |
| `macsftp-storage` | 连接配置、known hosts、Keychain 引用与迁移 |
| `macsftp-platform` | macOS 与本地文件系统边界 |
| `macsftp-test-support` | 共享集成测试夹具 |

GPUI 与 Tokio 通过有界的 command/event channel 通信，网络工作不在 UI 线程上运行。来自已关闭标签页或过期会话的事件会在修改状态前被 `core` 拒绝。依赖规则、运行时模型与 UI 约束记录在 [`docs/architecture.md`](docs/architecture.md)。

生产环境的 SFTP runtime 基于真实 SSH 后端构建；mock actor 与 mock 构造器仅用于 crate 测试。

## 不在范围内

FTP、WebDAV、S3、内置终端、目录同步、多跳 jump host 链、完整解析 `~/.ssh/config`、跨启动恢复传输、App Store 分发、已公证的正式发布，以及退出后继续运行的后台传输。

## 安全

请将凭据、私钥内容、host 指纹等敏感数据留在公开 issue 之外。私有报告流程见 [`SECURITY.md`](SECURITY.md)。

## 参与贡献

修改 runtime、SFTP、传输、host key、持久化或 UI 行为之前，请先阅读 [`AGENTS.md`](AGENTS.md) 和 [`CONTRIBUTING.md`](CONTRIBUTING.md)。Pull request 应保持小而聚焦，说明验证方式，可见的 UI 变化需附视觉证据。

## 许可证

macSFTP 使用 [MIT License](LICENSE)。原生菜单适配器使用 [Apache-2.0 License](crates/native-menu/LICENSE-APACHE)，并保留其改编的 AppKit 实现的署名。
