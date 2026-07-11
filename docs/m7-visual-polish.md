# M7 视觉打磨验证（Zed-style Visual Polish Verification）

来源：计划 `docs/gpui-russh-plan.md` §M7「Zed-style visual polish」（1919）。
状态记录日期：2026-07-12。
补齐目标：关闭 M7 最后 15% 中的「系统化视觉打磨验证记录」缺口（plan 原仅实现、
缺验证记录）。

验证手段：静态代码核查（`crates/ui/src/theme.rs`、`crates/ui/src/*`、`crates/app/src/workspace.rs`）
+ 既有单测 + 仓库内 `grep` 取证。GUI 像素级可读性需 macOS 实机目检，单独标注。

## 验证标准与执行结果

| # | 验证标准 | 核查方式 | 结果 | 状态 |
| --- | --- | --- | --- | --- |
| V1 | 单一 `Theme` 全局 + 语义色令牌，组件不写死颜色 | `theme.rs` 定义 `Theme`（`Global`）；`grep -rn "rgb(\|hsla(" crates/ui/src crates/app/src` 仅命中 `theme.rs` 令牌源 | UI 全部经 `theme.colors.*` 取色，无组件级字面量 | PASS |
| V2 | Light / Dark / System 三态齐备且互异 | `Theme::dark()/light()/for_appearance()`；单测 `dark_and_light_token_sets_are_both_defined_and_distinct` | 三态存在、背景/文字互不相同 | PASS |
| V3 | 固定布局尺寸（ThemeSizes）落地，hover/loading 不抖动 | `theme.sizes` 7 处应用：tab_bar/path_bar/table_header/file_row/transfer_row/status_bar/settings-tab | 尺寸令牌统一驱动，无散落像素值 | PASS |
| V4 | Zed 风：中性面 + 细边框 + 单一强调色 | 组件用 `.border_1()`；surface 三级（background/surface/elevated_surface）；唯一 `accent` 令牌 | 中性层级 + 1px 边框 + 单 accent | PASS |
| V5 | 状态语义色（success/error/warning/info）用于状态而非裸色 | transfer 状态、history、connection 状态全部映射到 `theme.colors.*` 语义色 | 状态用色一致、语义化 | PASS |
| V6 | hover/active/selected 用专用令牌 | `element_hover`/`element_active`/`element_selected` 在 status bar、transfer drawer、选中行使用 | 交互态统一令牌，无临时 alpha | PASS |
| V7 | 字体令牌：UI 与等宽分离 | `ui_family`=`.SystemUIFont`、`mono_family`=`Menlo`；单测 `ui_and_mono_font_families_are_separate_tokens` | 双字体令牌独立 | PASS |
| V8 | accent 克制使用（主按钮/选中/焦点环） | accent 仅用于主按钮（About Close、设置激活项）、`border_focused` 焦点环 | 不过度使用 | PASS |
| V9 | modal/popover 用 elevated_surface + border | connect/host-key/conflict modal、About 卡片均 `elevated_surface`+`border` | 浮层层级一致 | PASS |
| V10 | 图标体系：AppIcon 全尺寸集 + 应用内 IconName 令牌 | `build_app.sh` 生成 16–1024px 全集（icns≈1.1MB）；应用内用 `IconName` 枚举（transfer drawer 等） | 图标令牌化、尺寸齐全 | PASS（渲染见 M7-05 目检） |
| V11 | 文本/背景对比度达标（可读性） | 抽样：暗色 text `#c8ccd4`/bg `#1f2127` 高对比；亮色 text `#33373e`/bg `#fafafa` 高对比；muted 文字刻意降对比做层级 | 代码层无违规；最终可读性需实机目检 | PASS（代码层）/ NEEDS-INTERACTIVE |
| V12 | 颜色无散落魔法数（全部集中在 theme.rs） | `grep "rgb(\|hsla("` 在 `crates/{ui,app}/src` 仅 `theme.rs` 命中 | 单一颜色来源 | PASS |

## 结论

实现层面 Zed-style 视觉打磨**已实质完成**：颜色、尺寸、字体、语义状态全部令牌化并统一驱动；
原先缺口仅为「缺少系统化验证记录」，本文档即补此记录。逐项核查 12/12 通过
（V11 可读性需一次 macOS 实机目检做最终签字，但代码层无对比度违规）。

与 `docs/m7-test-matrix.md` M7-12、`crates/app/src/m7_regression.rs` 共同构成 M7 验收证据。
