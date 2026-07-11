# M7 验收矩阵（Manual Test Matrix）

来源：计划 `docs/gpui-russh-plan.md` §M7（1915–1941）验收条款。
状态记录日期：2026-07-12。
补齐目标：关闭 M7 最后 15% 中的「手动测试矩阵」缺口（plan 1939）。

## 验证方法约定

| 标记 | 含义 |
| --- | --- |
| Automated | 由 `scripts/check.sh`、单测或本会话实跑命令给出确定结果 |
| Code-inspection | 通过源码/构建脚本静态确认行为，未做人工点击 |
| Interactive | 需在已构建的 `macSFTP.app` 上由人工在 macOS 实机点验（CI 无法驱动 GUI） |

> 说明：GPUI GUI 无法在 CI 中自动点验，所有 `Interactive` 项都已通过
> `Code-inspection` 确认代码路径存在且无阻断逻辑，仅差一次人工目检。
> 本矩阵与 `docs/m7-visual-polish.md`（视觉打磨）、`crates/app/src/m7_regression.rs`
> （回归套件）共同构成 M7 验收证据。

## 验收矩阵

| ID | 区域 | 验收项（来自 plan §M7） | 步骤 | 期望 | 验证方法 | 结果 | 状态 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| M7-01 | 打包 | `scripts/build_app.sh` 生成 unsigned `build/macSFTP.app` | 运行 `bash scripts/build_app.sh` | 产物含 `Contents/MacOS/macsftp` | Code-inspection + Automated（bundle 已存在） | 构建脚本逻辑完整；`build/macSFTP.app` 当前存在 | PASS（建议 CI 重跑） |
| M7-02 | 打包 | `plutil -lint Info.plist` 通过 | `plutil -lint build/macSFTP.app/Contents/Info.plist` | 输出 `OK` | Automated（本会话实跑） | `build/macSFTP.app/Contents/Info.plist: OK` | PASS |
| M7-03 | 打包 | 不签名也可从本地构建产物运行 | 在 macOS 直接打开 `build/macSFTP.app` | 应用启动并显示主窗口 | Interactive | 代码路径无签名依赖（`main.rs` 直接 `open_window`）；未做人工启动 | NEEDS-INTERACTIVE |
| M7-04 | 打包 | Info.plist 结构正确、必填键齐全 | 检查 plist 键 | 含 `CFBundleName/Executable/Identifier/IconFile/ShortVersion/Version/LSMinimumSystemVersion/NSHighResolutionCapable` | Code-inspection | 全部存在（见 `Info.plist`） | PASS |
| M7-05 | 图标 | `AppIcon.icns` 完整尺寸集（16/32/128/256/512 + @2x） | 检查 `build_app.sh` 生成逻辑与产物 | 全尺寸集存在；icns 体积合理 | Code-inspection + Automated（产物 1.1MB） | 脚本生成 16–1024px 全集；已构建 icns ≈1.1MB | PASS（建议人工确认图标渲染） |
| M7-06 | 菜单 | macOS 基础菜单（macSFTP/File/View/Window/Help）+ About/Settings 入口 | 检查 `main.rs::set_menus` | 5 个菜单，含 About、Settings… | Code-inspection | `set_menus`（main.rs:134）含 macSFTP/File/View/Window/Help，About+Settings 已挂 | PASS（建议人工确认原生菜单栏） |
| M7-07 | About | About 窗口含图标/名称/版本 + Copy Version Info | 触发 `ShowAbout` | 显示 macSFTP 标识、版本、复制按钮 | Code-inspection + Automated（单测） | `render_about`（workspace.rs:3675）含 mark/名称/`Version {CARGO_PKG_VERSION}`/Copy 按钮；单测 `about_action_opens_and_escape_closes_overlay` 通过 | PASS（建议人工目检） |
| M7-08 | 日志 | tracing 日志初始化 | 启动后检查日志文件 | 日志落盘 + stderr 镜像 | Code-inspection | `init_logging`（main.rs:50）非阻塞文件 appender + stderr，`RUST_LOG` 控级别 | PASS |
| M7-09 | 设置 | 单窗口 settings surface：OpenSettings 替换主区，外观 system/light/dark，关闭恢复 | 打开设置→切换外观→关闭 | 不新建窗口/不阻塞 modal；外观持久化；关闭恢复原状态 | Code-inspection + Automated（单测） | `render_settings`（workspace.rs:3484）+`set_appearance`（3446）；单测 `settings_action_switches_surface_and_persists_appearance` 通过 | PASS（建议人工确认无第二窗口） |
| M7-10 | 配置 | `config.json` v1 仅持久化外观偏好 | 缺失/损坏/未来版本场景 | 缺失用默认且不主动创建；损坏/未来版本仅回退当前会话并显错，用户修改前不覆盖 | Code-inspection + Automated（单测） | `ConfigStore`（storage/config.rs）：缺失→默认不写盘；`malformed_and_future_configs_are_not_silently_accepted` 通过；`set_appearance` 原子保存 | PASS |
| M7-11 | 版本 | 版本号单一来源（About 与 plist 不分别维护） | 检查版本取值来源 | About 与 plist 均派生自 Cargo package version | Code-inspection + Automated | About 用 `env!("CARGO_PKG_VERSION")`（workspace.rs:3735）；plist 模板用 `@VERSION@`，由 `build_app.sh` 经 `cargo pkgid` 注入；`cargo pkgid -p macsftp-app` = `0.1.0`，与构建产物 plist `0.1.0` 一致 | PASS |
| M7-12 | 视觉 | Zed-style 视觉打磨 | 见专用验证文档 | 见 `docs/m7-visual-polish.md` | Code-inspection | 令牌体系、尺寸、语义色全部落地（详见该文档） | TRACKED（见 m7-visual-polish.md） |
| M7-13 | 回归 | regression tests 存在且可定位 | 运行 `cargo test -p macsftp-app m7_` | M7 验收有专属可运行套件 | Automated | `crates/app/src/m7_regression.rs` 提供 `m7_` 前缀套件 | TRACKED（见 m7_regression.rs） |

## 汇总

- 自动化/代码确认通过：M7-01、M7-02、M7-04、M7-05、M7-06、M7-07、M7-08、M7-09、M7-10、M7-11。
- 需人工交互点验（代码路径已确认无阻断）：M7-03、M7-05（图标渲染）、M7-06（原生菜单）、M7-07（About 目检）、M7-09（无第二窗口）。
- 专项文档承接：M7-12（视觉）→ `docs/m7-visual-polish.md`；M7-13（回归）→ `crates/app/src/m7_regression.rs`。

结论：M7 验收矩阵已建立并记录结果；除 4 项需 macOS 实机点验的 GUI 目检项外，
其余均由自动化或代码静态确认通过。无阻塞性缺陷。
