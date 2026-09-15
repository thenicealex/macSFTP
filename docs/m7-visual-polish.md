# M7 视觉完善验证（Zed-style Visual Polish Verification）

来源：计划 `docs/gpui-russh-plan.md` §M7「Zed-style visual polish」（1919）。

状态记录日期：2026-07-12。

目标：完成 M7 最后 15% 所需的系统化视觉验证记录。原计划已经包含实现，但是缺少相应的验证记录。

验证方法包括静态代码核查（`crates/ui/src/theme.rs`、`crates/ui/src/*`、`crates/app/src/workspace.rs`）、既有单元测试，以及仓库内 `grep` 结果。GUI 的像素级可读性需要在 macOS 实机上进行人工检查，因此在结果中单独注明。

## 验证标准与执行结果

| # | 验证标准 | 核查方式 | 结果 | 状态 |
| --- | --- | --- | --- | --- |
| V1 | 使用单一全局 `Theme` 和语义颜色令牌，并且组件不直接定义颜色 | `theme.rs` 定义了实现 `Global` 的 `Theme`；`grep -rn "rgb(\|hsla(" crates/ui/src crates/app/src` 仅匹配 `theme.rs` 中的令牌定义 | 所有 UI 均通过 `theme.colors.*` 引用颜色，因此组件内不存在颜色字面量 | PASS |
| V2 | One Light、One Dark 和 System 三种主题状态均存在且具有差异 | 检查 `Theme::one_dark()`、`Theme::one_light()` 和 `Theme::for_appearance()`，并执行主题 token 回归测试 | 三种状态均存在，而且其背景色和文字色互不相同 | PASS |
| V3 | `ThemeSizes` 统一定义固定布局尺寸，并且 hover/loading 不引发布局抖动 | `theme.sizes` 应用于 7 处：tab_bar、path_bar、table_header、file_row、transfer_row、status_bar、settings-tab | 尺寸均由令牌统一控制，因此不存在分散的像素值 | PASS |
| V4 | Zed 风格使用中性色表面、细边框和单一强调色 | 组件使用 `.border_1()`；表面分为 background、surface、elevated_surface 三级；仅定义一个 `accent` 令牌 | 中性色层级、1px 边框和单一 accent 均符合标准 | PASS |
| V5 | success、error、warning 和 info 等语义颜色只表示对应状态 | transfer 状态、history 和 connection 状态均映射至 `theme.colors.*` 语义颜色 | 各状态的颜色使用一致，并且具有明确语义 | PASS |
| V6 | hover、active 和 selected 状态使用专用令牌 | status bar、transfer drawer 和选中行使用 `element_hover`、`element_active`、`element_selected` | 交互状态统一使用令牌，因此不存在临时 alpha 值 | PASS |
| V7 | UI 字体令牌与等宽字体令牌相互独立 | `ui_family` 为 `.SystemUIFont`，`mono_family` 为 `Menlo`；单元测试为 `ui_and_mono_font_families_are_separate_tokens` | 两类字体由独立令牌定义 | PASS |
| V8 | accent 仅用于主按钮、选中状态和焦点环 | accent 仅用于主按钮（About Close、设置激活项）和 `border_focused` 焦点环 | accent 的使用范围符合约束 | PASS |
| V9 | modal 和 popover 使用 elevated_surface 与 border | connect、host-key、conflict modal 和 About 卡片均使用 `elevated_surface` 与 `border` | 所有浮层使用一致的视觉层级 | PASS |
| V10 | 图标体系包含 AppIcon 的完整尺寸集合和应用内 `IconName` 令牌 | 提供可编辑的 `AppIcon.svg` 和同步的 1024px PNG；`build_app.sh` 生成 16–1024px 的完整集合；应用内使用 `IconName` | 图标包含白色圆角底板、充足安全区，以及简化的双端点和双向传输符号；小尺寸仍可辨认 | PASS（M7-05 已人工检查 16/32/64/128px） |
| V11 | 文本与背景的对比度符合可读性要求 | 抽样结果：暗色主题使用 text `#c8ccd4`/bg `#1f2127`，亮色主题使用 text `#33373e`/bg `#fafafa`，两者均具有高对比度；muted 文字通过较低对比度表达层级 | 代码核查未发现违规；用户于 2026-08-01 在实机上确认亮色和暗色主题均可读 | PASS（代码核查 + 实机检查） |
| V12 | 颜色数值只在 theme.rs 中定义 | `grep "rgb(\|hsla("` 在 `crates/{ui,app}/src` 中仅匹配 `theme.rs` | 所有颜色均来自单一来源 | PASS |

## 结论

Zed-style 视觉完善在实现层面已经完成。颜色、尺寸、字体和语义状态均已使用令牌，并且由统一机制控制。原有缺项仅是系统化验证记录，而本文档提供了该记录。因此，12 项核查全部通过。V11 的可读性也已经由用户于 2026-08-01 在 macOS 实机上检查亮色和暗色主题后确认。

本文档与 `docs/m7-test-matrix.md` 的 M7-12、`crates/app/src/m7_regression.rs` 共同组成 M7 的验收证据。
