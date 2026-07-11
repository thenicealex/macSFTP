# macSFTP UI/UX 设计准则

> 状态：强制规范。
> 适用范围：所有 GPUI 界面、组件、交互、文案、状态展示和视觉验收。
> 设计目标：学习 Zed 的产品气质，但不复制 IDE。macSFTP 是一个高密度、低延迟、键盘友好的 SFTP 桌面工具。

## 0. 基本假设

- 本项目第一版是单窗口 macOS 桌面应用，使用 GPUI 自绘 UI。
- 第一版核心界面是：顶部 tab bar、左右文件 pane、底部 transfer drawer、底部 status bar。
- Zed 风格在本项目中的含义是：快、克制、密集、明确、可键盘驱动、状态透明。
- 本规范不引入额外功能；只约束已经在 `docs/gpui-russh-plan.md` 中定义的产品表面。

## 1. Zed 风格转译

Zed 给本项目的参考不是"深色编辑器皮肤"，而是以下产品原则：

- **速度可感知**：启动、切换 tab、滚动、选择、打开目录、显示传输状态都必须即时反馈。
- **单窗口上下文**：用户应在同一个工作面里完成连接、浏览、传输、冲突处理和错误恢复。
- **命令优先**：核心动作必须有稳定 action，可由 command palette 和快捷键触发。
- **面板式密集信息**：信息分布在 tab、pane、drawer、status bar 中，避免大面积空白和营销式布局。
- **状态透明**：连接、加载、错误、传输、冲突、安全决策必须在原位置或紧邻工作流处表达。
- **视觉克制**：UI 应用细线、低对比面、有限强调色，不使用装饰性大色块、渐变、插画或卡片堆叠。

参考资料：

- Zed 首页：<https://zed.dev/>
- Zed Getting Started：<https://zed.dev/docs/>
- Zed Appearance：<https://zed.dev/docs/appearance>
- Zed Visual Customization：<https://zed.dev/docs/visual-customization>
- Zed Command Palette：<https://zed.dev/docs/command-palette>
- Zed Project Panel：<https://zed.dev/docs/project-panel>
- Zed Tab Switcher：<https://zed.dev/docs/tab-switcher>
- Zed Agent Panel：<https://zed.dev/docs/ai/agent-panel>

## 2. 强制设计原则

### 2.1 信息密度优先

- 必须把首屏用于真实工作区，不允许做欢迎营销页、hero 区、说明卡片或大面积品牌展示。
- 必须优先显示用户当前能操作的对象：连接 tab、local pane、remote pane、transfer drawer。
- 空状态必须只给下一步动作，例如 `Open Connection`、`Choose Local Folder`、`Retry`，不得写长篇功能解释。
- 文件列表、传输列表、tab、路径栏、状态栏必须保持紧凑；组件高度应稳定，避免 hover、loading、progress 导致布局跳动。

### 2.2 快速路径优先

- 每个高频动作必须有三条入口中的至少两条：按钮、快捷键、command palette action。
- 高频动作包括：新建 tab、关闭 tab、切换 tab、连接、刷新、返回上级、上传、下载、取消传输、重试传输、打开 transfer drawer。
- 命令入口的 action 名称必须稳定，不得把 action 绑定到临时文案。
- UI 主线程不得等待网络、磁盘扫描、SFTP 操作或传输 planning。

### 2.3 位置稳定

- tab bar 必须固定在顶部。
- local pane 必须默认在左，remote pane 必须默认在右。
- transfer drawer 必须默认在底部，可折叠但不能遮挡主文件列表。
- status bar 必须在底部展示轻量全局状态，不得替代错误详情、冲突决策或安全确认。
- modal 只用于必须阻塞当前流程的决策；普通错误、加载、传输进度不得用 modal。

### 2.4 安全决策显式

- Host key unknown 必须展示 host、port、key type、fingerprint、known_hosts 写入目标和主操作。
- Host key mismatch 必须阻断连接，主操作只能是取消或查看详情，不得提供"继续连接"作为默认操作。
- 密码、passphrase、私钥完整路径不得出现在 UI 错误、日志预览、tooltip 或复制文本中。
- 所有安全 modal 必须绑定 request id 和 session epoch；过期 modal 的确认按钮必须失效。

## 3. 视觉系统

### 3.1 颜色

- 必须支持 dark 和 light 两套 theme token。
- 默认界面应以中性灰阶为主，强调色只用于焦点、主操作、连接成功、选中态和进度。
- 禁止使用大面积紫色/蓝紫渐变、彩色背景、装饰性光斑、拟物阴影。
- 错误、警告、成功、信息色必须有固定语义，不得把同一种颜色同时用于无关含义。
- 颜色不能是唯一状态表达；连接状态、错误和传输状态还必须有文本或图标。

### 3.2 层级

- 主窗口层级必须靠边线、背景面、间距和文字权重表达，不靠厚重阴影。
- 面板之间使用 1px 分隔线或相邻背景面区分。
- 卡片只允许用于 modal、popover、重复列表项的必要容器；禁止在 page section 内套卡片。
- 圆角应克制：工具按钮、输入框、popover、modal 可使用小圆角；文件行、tab、状态条不应有大圆角。

### 3.3 字体

- UI 字体和等宽数据字体必须分开 token：`ui_font_family`、`mono_font_family`。
- 文件名、路径、权限、大小、mtime、速度、ETA 等表格数据应使用可读性高的 UI 字体；需要列对齐的数字可使用 tabular numbers 或等宽字体。
- 正文和控件文案不得使用 hero 级字号。
- 文件列表、传输列表、状态栏必须用固定行高。

### 3.4 间距与尺寸

- 使用小步进 spacing token，例如 4、6、8、12、16、24。
- icon button 必须有稳定点击区域，视觉尺寸可小，但命中区不得过小。
- 文件列表 row 高度、transfer row 高度、tab 高度、toolbar 高度必须有固定 token。
- 文本必须在窄窗口下截断、中间省略或换行；不得溢出按钮、tab、路径栏和 modal。

## 4. 主窗口布局

主布局必须保持：

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

要求：

- local/remote pane 必须视觉对称，但状态可以不同。
- 当前聚焦 pane 必须有清晰但克制的 active indicator。
- pane resize 不得破坏路径栏、表头、滚动区域和空状态布局。
- 用户收起 transfer drawer 后，active/failed transfer 必须仍在 status bar 有轻量提示。

## 5. Tab 设计

- tab 必须显示连接名或 host，必要时附加路径上下文。
- tab 必须显示连接状态：empty、connecting、awaiting host key、connected、disconnected、failed。
- tab 错误 indicator 必须可被键盘 focus 并打开详情。
- tab 关闭行为必须尊重传输状态：若 transfer 会因关闭 tab 而取消，必须明确确认；若 transfer 独立继续，则不能误导用户。
- tab switcher 必须按最近使用排序，而不是只按创建顺序。

## 6. File Pane 设计

### 6.1 Path Bar

- path bar 必须支持复制当前路径。
- local 和 remote path bar 必须保留 back、up、refresh。
- 路径过长时必须中间省略，末尾文件夹优先可见。
- loading 时 path bar 不得消失；只显示局部 spinner 或 progress indicator。

### 6.2 文件列表

- 文件列表必须虚拟化，10k entries 不得创建 10k 个长期 row entity。
- 表头必须支持排序状态展示。
- selection 必须绑定稳定 id/path，禁止只绑定 row index。
- keyboard navigation 必须覆盖上/下、page up/down、home/end、shift 多选、enter 默认动作。
- directory row、file row、symlink row、hidden row 必须有可区分但克制的 icon 或 marker。
- hover action 必须可发现但不能造成列宽变化或 row 高度变化。

### 6.3 状态

remote pane 必须覆盖这些状态：

- `Disconnected`：给连接或重连动作。
- `Connecting`：显示目标 host 和可取消动作。
- `AwaitingHostKey`：列表区域保持上下文，modal 承担安全决策。
- `LoadingDirectory`：保留旧列表时必须标记为 refreshing；首次加载可显示 skeleton 或 spinner。
- `Loaded`：显示列表、排序、selection、路径。
- `Error`：展示用户可理解错误和恢复动作。

## 7. Transfer Drawer 设计

- transfer drawer 是传输工作的唯一主视图，不得把进度散落到多个无关位置。
- active、queued、completed、failed 必须分组；completed/failed 默认可折叠。
- 每个 transfer row 必须显示方向、源、目标、状态、进度、速度、ETA、主操作。
- cancel 和 retry 必须是 icon button + tooltip；危险操作必须有确认或可撤销语义。
- progress 更新必须节流；视觉上应平滑，但不得用动画掩盖真实停滞。
- planning 阶段必须流式显示已发现数量和估算总字节，不得长时间空白。
- conflict 不是错误；冲突等待用户决策时 row 状态必须是 `WaitingForConflictDecision`，不能显示成 failed。

## 8. Modal 与 Popover

- modal 只允许用于 host key、credential、transfer conflict、destructive confirm、error details。
- modal 必须有标题、简短说明、主操作、取消/关闭操作。
- 主操作必须放在固定位置，危险主操作必须使用危险语义色。
- transfer conflict modal 必须显示源、目标、大小、mtime、决策选项和 apply-to-all 作用域。
- credential modal 不得在标题或说明中暴露 secret 值。
- popover 用于轻量选择或详情；不得承载必须完成的阻塞决策。

## 9. Command Palette 与快捷键

- 所有 app-level action 必须可被 command palette 搜索。
- action 名称必须使用稳定动词短语，并优先沿用 `docs/gpui-russh-plan.md` 中的 action 名称，例如 `NewTab`、`RefreshPane`、`RetryTransfer`。
- 快捷键可以后续调整，但 action id 不得随 UI 文案变化。
- command palette 中不得暴露内部 runtime、channel、actor 等实现细节。
- 用户忘记快捷键时，command palette 必须能完成同一动作。

### 9.1 Settings 与 About

- Settings 使用主窗口内的独立 surface，不创建第二个窗口，不使用阻塞 modal；关闭后
  必须恢复原 tab、pane、selection 和 transfer drawer 状态。
- Settings 必须可通过 `macSFTP > Settings…`、`⌘,` 和稳定的 `OpenSettings` action
  打开，并可通过 Done 或 Escape 返回工作区。
- 外观选择必须立即预览；`System` 跟随窗口 appearance，固定 Light/Dark 时不得被系统
  外观变化覆盖。
- 配置保存失败必须在 Settings 内行内展示，不用 toast，不静默丢弃。
- About 是非阻塞轻量浮层，只显示图标、应用名、从构建元数据派生的版本、简短说明和
  复制版本信息操作；不得承担设置或营销页面职责。

## 10. 错误与恢复

- UI 错误必须来自 `UserFacingError`，不得直接展示 `russh`、`russh-sftp`、IO debug 字符串。
- 每个错误都必须有 title、short message、detail、recovery action、retryable。
- 可恢复错误应就地显示恢复动作；不可恢复错误要解释为什么无法继续。
- 状态栏只能放摘要，错误详情必须在相关 pane、transfer row 或 modal detail 中展开。
- 错误文案必须说明下一步，不得只说 `failed` 或 `unknown error`。

## 11. 文案

- 第一版可以单语，但同一构建内必须统一语言。
- 文案必须短、具体、面向用户动作。
- 禁止在 UI 中解释架构、runtime、session epoch、actor、channel、crate 等内部概念。
- 安全和 destructive 文案必须明确后果。
- 日志文案和 UI 文案必须分离。

## 12. 可访问性与键盘

- 所有 icon-only button 必须有 tooltip/label。
- modal 必须有可测试 focus order，打开后 focus 到第一个合理控件，关闭后恢复到触发控件。
- 文件列表 selection 必须可纯键盘完成。
- tab、pane、transfer drawer 必须能通过快捷键聚焦。
- 颜色对比必须满足基础可读性；低对比文本只可用于次要信息，不可用于错误、警告、主操作。
- loading、error、success 不得只靠颜色或动画表达。

## 13. 性能与动效

- UI 动效必须短、轻、可跳过；不得影响输入、滚动、切换和选择响应。
- 目录滚动、tab 切换、pane focus、drawer 展开必须无明显卡顿。
- 10k local entries、10k remote entries、4 active transfers、3 connected tabs 是最低性能烟测场景。
- transfer progress 最多以 UI 可感知频率更新；不得逐 chunk 触发重绘。
- hover、selection、focus 不得触发昂贵 layout。

## 14. 禁止模式

- 禁止 landing page、hero section、营销插画、装饰性渐变背景。
- 禁止用 toast 代替必须解决的错误或安全决策。
- 禁止在文件传输主流程中弹出非必要 modal。
- 禁止让 loading 状态清空用户已有上下文，除非旧数据确实不可用。
- 禁止在主线程阻塞等待网络、磁盘或 runtime 响应。
- 禁止大面积品牌色、厚阴影、玻璃拟态、彩色卡片墙。
- 禁止把实现术语暴露给用户。
- 禁止为了视觉统一牺牲安全信息、错误恢复和传输可观测性。

## 15. UI 评审清单

任何 UI/UX PR 合并前必须回答：

- 是否保留了单窗口工作上下文？
- 是否有 command palette action 或快捷键路径？
- 是否有 loading、empty、error、disabled、focused、hover、selected 状态？
- 是否在窄窗口下检查了 tab、路径、按钮、modal 文本不溢出？
- 是否避免了无关卡片、渐变、装饰图形和大面积空白？
- 是否不阻塞 GPUI 主线程？
- 是否能处理 10k entries 和多 transfer 场景？
- 是否为 icon-only button 提供 tooltip/label？
- 是否没有泄露 secret、私钥完整路径或内部 debug 字符串？
- 是否覆盖了 request id/session epoch 相关 modal 过期场景？

未满足以上任一项，必须在 PR 中说明取舍；涉及安全、阻塞、secret 泄露、modal 过期误确认的项不得豁免。
