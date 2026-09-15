# macSFTP UI/UX 设计准则

> 状态：强制规范。
> 适用范围：所有 GPUI 界面、组件、交互、文案、状态展示和视觉验收。
> 设计目标：参考 Zed 的产品特征，但是不复制 IDE。macSFTP 是一个高密度、低延迟、键盘友好的 SFTP 桌面工具。

## 0. 基本假设

- 本项目第一版是单窗口 macOS 桌面应用，使用 GPUI 自绘 UI。
- 第一版核心界面是：顶部 tab bar、左右文件 pane、底部 transfer drawer、底部 status bar。
- Zed 风格在本项目中的含义是：响应快速、视觉克制、信息密集、语义明确、支持键盘操作、状态透明。
- 本规范不引入额外功能，因此只约束已经在 `docs/gpui-russh-plan.md` 中定义的产品界面。

## 1. Zed 风格转译

本项目不照搬 Zed 的深色编辑器外观，但是参考以下产品原则：

- **速度可感知**：启动、切换 tab、滚动、选择、打开目录、显示传输状态都必须即时反馈。
- **单窗口上下文**：用户应在同一个工作界面内完成连接、浏览、传输、冲突处理和错误恢复。
- **命令优先**：核心操作必须有稳定 action，并且可以由 command palette 和快捷键触发。
- **面板式密集信息**：信息分布在 tab、pane、drawer、status bar 中，因此界面应避免大面积空白和营销式布局。
- **状态透明**：连接、加载、错误、传输、冲突、安全决策必须在原位置或紧邻工作流处表达。
- **视觉克制**：UI 使用细线、低对比背景和有限的强调色，因此不得使用装饰性大色块、渐变、插画或密集卡片布局。

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

- 首屏必须用于真实工作区，因此不允许设置欢迎营销页、hero 区、说明卡片或大面积品牌展示。
- 必须优先显示用户当前能操作的对象：连接 tab、local pane、remote pane、transfer drawer。
- 空状态必须只提供下一步操作，例如 `Open Connection`、`Choose Local Folder`、`Retry`，不得包含长篇功能说明。
- 文件列表、传输列表、tab、路径栏、状态栏必须保持紧凑；同时，组件高度应保持稳定，避免 hover、loading、progress 引发布局变化。

### 2.2 快速路径优先

- 每个高频操作必须具备以下三种入口中的至少两种：按钮、快捷键、command palette action。
- 高频操作包括：新建 tab、关闭 tab、切换 tab、连接、刷新、返回上级、上传、下载、取消传输、重试传输、打开 transfer drawer。
- 命令入口的 action 名称必须稳定，因此不得将 action 绑定到临时文案。
- UI 主线程不得等待网络、磁盘扫描、SFTP 操作或传输 planning，因为这些任务可能阻塞界面响应。

### 2.3 位置稳定

- tab bar 必须固定在顶部。
- local pane 必须默认在左，remote pane 必须默认在右。
- transfer drawer 必须默认位于底部；它可以折叠，但是不能遮挡主文件列表。
- status bar 必须在底部展示简要的全局状态，但是不得替代错误详情、冲突决策或安全确认。
- modal 只用于必须阻塞当前流程的决策，因此普通错误、加载状态和传输进度不得使用 modal。

### 2.4 安全决策显式

- Host key unknown 必须展示 host、port、key type、fingerprint、known_hosts 写入目标和主操作，用户因此可以核对信任对象。
- Host key mismatch 必须阻断连接，因此主操作只能是取消或查看详情，不得将“继续连接”作为默认操作。
- 密码、passphrase、私钥完整路径不得出现在 UI 错误、日志预览、tooltip 或复制文本中。
- 所有安全 modal 必须绑定 request id 和 session epoch；如果 modal 已经过期，那么确认按钮必须失效。

## 3. 视觉系统

### 3.1 颜色

- 必须支持 dark 和 light 两套 theme token；其中，默认值分别采用 One Dark 和 One Light。
- 默认界面应以中性灰阶为主，强调色只用于焦点、主操作、连接成功、选中态和进度。
- 禁止使用大面积紫色/蓝紫渐变、彩色背景、装饰性光斑、拟物阴影。
- 错误、警告、成功和信息色必须具有固定语义，因此同一种颜色不得同时表示无关含义。
- 颜色不能作为唯一的状态表达，因此连接状态、错误和传输状态还必须包含文本或图标。

### 3.2 层级

- 主窗口层级必须通过边线、背景、间距和文字权重表达，不得依赖厚重阴影。
- 面板之间使用 1px 分隔线或相邻背景面区分。
- 卡片只允许作为 modal、popover 和重复列表项的必要容器；因此，page section 内禁止嵌套卡片。
- 圆角应克制：工具按钮、输入框、popover、modal 可使用小圆角；文件行、tab、状态条不应有大圆角。

### 3.3 字体

- UI 字体和等宽数据字体必须分开 token：`ui_font_family`、`mono_font_family`。
- 文件名、路径、权限、大小、mtime、速度、ETA 等表格数据应使用可读性高的 UI 字体；如果数字需要按列对齐，那么可以使用 tabular numbers 或等宽字体。
- 正文和控件文案不得使用 hero 级字号。
- 文件列表、传输列表、状态栏必须用固定行高。

### 3.4 间距与尺寸

- 使用小步进 spacing token，例如 4、6、8、12、16、24。
- icon button 必须具有稳定的点击区域；其视觉尺寸可以较小，但是命中区域不得过小。
- 文件列表 row 高度、transfer row 高度、tab 高度、toolbar 高度必须有固定 token。
- 文本在窄窗口中必须截断、中间省略或换行，因此不得超出按钮、tab、路径栏和 modal 的边界。

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

- local pane 和 remote pane 的视觉结构必须对称，但是两者可以处于不同状态。
- 当前聚焦 pane 必须有清晰但克制的 active indicator。
- 调整 pane 尺寸时，路径栏、表头、滚动区域和空状态布局必须保持完整。
- 用户折叠 transfer drawer 后，status bar 仍须简要提示 active 或 failed transfer。

## 5. Tab 设计

- tab 必须显示连接名称或 host；如果存在区分需求，那么可以附加路径上下文。
- tab 必须显示连接状态：empty、connecting、awaiting host key、connected、disconnected、failed。
- tab 错误 indicator 必须支持键盘 focus，并允许用户打开详情。
- tab 的关闭行为必须符合传输状态：如果关闭 tab 会取消 transfer，那么必须要求用户明确确认；如果 transfer 会独立继续，那么界面不得显示错误暗示。
- tab switcher 必须按最近使用时间排序，因此不能只采用创建顺序。

## 6. File Pane 设计

### 6.1 Path Bar

- path bar 必须支持复制当前路径。
- local 和 remote path bar 必须保留 back、up、refresh。
- 路径过长时必须从中间省略，因此末尾文件夹应优先保持可见。
- loading 时 path bar 不得消失，而应只增加局部 spinner 或 progress indicator。

### 6.2 文件列表

- 文件列表必须虚拟化，因此 10k entries 不得创建 10k 个长期 row entity。
- 表头必须支持排序状态展示。
- selection 必须绑定稳定的 id 或 path，因此禁止仅绑定 row index。
- keyboard navigation 必须覆盖上/下、page up/down、home/end、shift 多选、enter 默认动作。
- directory row、file row、symlink row 和 hidden row 必须具有可区分但是克制的 icon 或 marker。
- hover action 必须易于发现，但是不能导致列宽或 row 高度变化。

### 6.3 状态

remote pane 必须覆盖这些状态：

- `Disconnected`：提供连接或重新连接操作。
- `Connecting`：显示目标 host 和可取消操作。
- `AwaitingHostKey`：列表区域保持当前上下文，同时由 modal 提供安全决策界面。
- `LoadingDirectory`：如果保留旧列表，那么必须标记为 refreshing；首次加载时可以显示 skeleton 或 spinner。
- `Loaded`：显示列表、排序、selection、路径。
- `Error`：展示用户可以理解的错误信息和恢复操作。

## 7. Transfer Drawer 设计

- transfer drawer 是传输任务的唯一主视图，因此不得在多个无关位置分别显示完整进度。
- active、queued、completed、failed 必须分组；completed/failed 默认可折叠。
- 每个 transfer row 必须显示方向、源、目标、状态、进度、速度、ETA、主操作。
- cancel 和 retry 必须采用 icon button 与 tooltip；如果操作具有危险性，那么必须要求确认或支持撤销。
- progress 更新必须节流；视觉变化应保持平滑，但是不得用动画隐藏真实的停滞状态。
- planning 阶段必须持续显示已发现数量和估算总字节，因此界面不得长时间没有状态信息。
- conflict 不是错误；因此，冲突等待用户决策时，row 状态必须是 `WaitingForConflictDecision`，不能显示为 failed。

## 8. Modal 与 Popover

- modal 只允许用于 host key、credential、transfer conflict、destructive confirm 和 error details。
- modal 必须有标题、简短说明、主操作、取消/关闭操作。
- 主操作必须位于固定位置；如果主操作具有危险性，那么必须使用危险语义色。
- transfer conflict modal 必须显示源、目标、大小、mtime、决策选项和 apply-to-all 作用域。
- credential modal 不得在标题或说明中暴露 secret 值。
- popover 用于简短选择或详情，因此不得包含必须完成的阻塞决策。

## 9. Command Palette 与快捷键

- 所有 app-level action 必须可被 command palette 搜索。
- action 名称必须使用稳定的动词短语，并优先采用 `docs/gpui-russh-plan.md` 中的名称，例如 `NewTab`、`RefreshPane`、`RetryTransfer`。
- 快捷键可以后续调整，但是 action id 不得随 UI 文案变化。
- command palette 中不得暴露内部 runtime、channel、actor 等实现细节。
- 如果用户忘记快捷键，那么 command palette 必须可以完成同一操作。

### 9.1 Settings 与 About

- Settings 使用主窗口内的独立 surface，因此不创建第二个窗口，也不使用阻塞 modal；关闭后
  必须恢复原 tab、pane、selection 和 transfer drawer 状态。
- Settings 必须可通过 `macSFTP > Settings…`、`⌘,` 和稳定的 `OpenSettings` action
  打开，并可通过 Done 或 Escape 返回工作区。
- 外观选择必须立即预览；`System` 跟随窗口 appearance，但是固定为 Light 或 Dark 时不得被系统
  外观变化覆盖。
- 配置保存失败必须在 Settings 内行内展示，因此不能使用 toast，也不能静默丢弃错误。
- About 是非阻塞的简洁浮层，因此只显示图标、应用名、从构建元数据派生的版本、简短说明和
  复制版本信息操作；但是不得包含设置功能或营销页面内容。

## 10. 错误与恢复

- UI 错误必须来自 `UserFacingError`，因此不得直接展示 `russh`、`russh-sftp` 或 IO debug 字符串。
- 每个错误都必须有 title、short message、detail、recovery action、retryable。
- 可恢复错误应在原位置显示恢复操作；但是对于不可恢复错误，界面必须解释无法继续的原因。
- 状态栏只能显示摘要，因此错误详情必须在相关 pane、transfer row 或 modal detail 中完整展示。
- 错误文案必须说明下一步操作，因此不得只显示 `failed` 或 `unknown error`。

## 11. 文案

- 第一版可以只使用一种语言，但是同一构建内必须保持语言一致。
- 文案必须简短、具体，并且面向用户操作。
- 禁止在 UI 中解释架构、runtime、session epoch、actor、channel、crate 等内部概念。
- 安全操作和 destructive 操作的文案必须明确说明后果。
- 日志文案和 UI 文案必须分离。

## 12. 可访问性与键盘

- 所有 icon-only button 必须有 tooltip/label。
- modal 必须具有可测试的 focus order；打开后，focus 移至第一个合理控件；关闭后，focus 恢复至触发控件。
- 文件列表 selection 必须可纯键盘完成。
- tab、pane、transfer drawer 必须能通过快捷键聚焦。
- 颜色对比必须满足基本可读性；低对比文本只可用于次要信息，因此不可用于错误、警告或主操作。
- loading、error 和 success 状态不得只通过颜色或动画表达。

## 13. 性能与动效

- UI 动效必须简短、克制并且可以跳过，因此不得影响输入、滚动、切换和选择响应。
- 目录滚动、tab 切换、pane focus、drawer 展开必须无明显卡顿。
- 10k local entries、10k remote entries、4 active transfers、3 connected tabs 是最低性能烟测场景。
- transfer progress 的更新频率不得超过 UI 可感知的范围，因此不得由每个 chunk 触发重绘。
- hover、selection、focus 不得触发昂贵 layout。

## 14. 禁止模式

- 禁止 landing page、hero section、营销插画、装饰性渐变背景。
- 禁止用 toast 代替必须解决的错误或安全决策。
- 禁止在文件传输主流程中弹出非必要 modal。
- 禁止让 loading 状态清除用户已有的上下文，除非旧数据确实不可用。
- 禁止在主线程阻塞等待网络、磁盘或 runtime 响应。
- 禁止大面积品牌色、厚阴影、玻璃拟态、彩色卡片墙。
- 禁止向用户展示实现术语。
- 禁止为了视觉统一而减少安全信息、错误恢复能力和传输可观测性。

## 15. UI 评审清单

任何 UI/UX PR 在合并前必须回答以下问题：

- 是否保留了单窗口工作上下文？
- 是否提供了 command palette action 或快捷键入口？
- 是否有 loading、empty、error、disabled、focused、hover、selected 状态？
- 是否在窄窗口下检查了 tab、路径、按钮、modal 文本不溢出？
- 是否避免了无关卡片、渐变、装饰图形和大面积空白？
- 是否不阻塞 GPUI 主线程？
- 是否能够处理 10k entries 和多个 transfer 并行存在的场景？
- 是否为 icon-only button 提供 tooltip/label？
- 是否没有泄露 secret、私钥完整路径或内部 debug 字符串？
- 是否覆盖了 request id/session epoch 相关 modal 过期场景？

如果未满足以上任一项，那么必须在 PR 中说明取舍；但是，涉及安全、阻塞、secret 泄露或 modal 过期误确认的项目不得豁免。
