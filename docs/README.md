# macSFTP 文档索引

本项目文档按用途分层。核心架构与贡献规则见 [`docs/gpui-russh-plan.md`](gpui-russh-plan.md) 与根目录 `AGENTS.md`。

## 当前状态入口

| 文档 | 用途 |
| --- | --- |
| [`progress-analysis-2026-08-01.md`](progress-analysis-2026-08-01.md) | **最新进展总览**（推荐从这里读起）：07-14 之后的功能、修复、验证现状、遗留项 |
| [`release-evidence/v0.1.0.md`](release-evidence/v0.1.0.md) | 0.1.0 发布证据（已签字，Status: PASS） |

## 架构与设计

| 文档 | 用途 |
| --- | --- |
| [`gpui-russh-plan.md`](gpui-russh-plan.md) | **架构主文档**（强制）：crate 划分、运行时模型、里程碑、ADR |
| [`architecture-review-v2.md`](architecture-review-v2.md) | 架构评审 v2（对照 v1 的修订） |
| [`architecture-review-v3.md`](architecture-review-v3.md) | 架构评审 v3（最终评审） |
| [`archive/`](archive/) | 已归档的历史文档（v1 评审、遗留问题分析、workspace 重构设计） |

## 计划（docs/plans/）

按日期命名的设计/实现计划。已完成阶段的计划保留作历史参考：

- **进行中/近期**：`2026-07-29-detailed-code-implementation-plan.md`（三项高严重度缺陷修复合同）、`2026-07-28-*`（传输终态 / host-key scope / 编辑校验）、`2026-07-29-scrollbar*.md`
- **已完成阶段**：`2026-07-13/14-phase*-*.md`（UX 六阶段）、profile management、connect picker、transfer drawer
- 完整列表见 [`docs/plans/`](plans/)

## 进度分析（历史快照）

按时间排序，每篇是当时的项目总览：

| 日期 | 文档 |
| --- | --- |
| 2026-07-12 | [`progress-analysis-2026-07-12.md`](progress-analysis-2026-07-12.md) |
| 2026-07-13 | [`progress-analysis-2026-07-13.md`](progress-analysis-2026-07-13.md) |
| 2026-07-13（多窗口） | [`progress-analysis-2026-07-13-multiwindow.md`](progress-analysis-2026-07-13-multiwindow.md) |
| 2026-07-14 | [`progress-analysis-2026-07-14.md`](progress-analysis-2026-07-14.md) |
| 2026-08-01 | [`progress-analysis-2026-08-01.md`](progress-analysis-2026-08-01.md)（最新） |

## UX 规范与计划

| 文档 | 用途 |
| --- | --- |
| [`ui-ux-guidelines.md`](ui-ux-guidelines.md) | **强制 UX 规范**（所有界面改动必须遵守） |
| [`ux-improvement-plan.md`](ux-improvement-plan.md) | UX 提升分阶段计划（阶段 1–6 已完成，见 status update） |

## 发布

| 文档 | 用途 |
| --- | --- |
| [`release-process.md`](release-process.md) | 发布流程：tag 规则、版本单一来源、校验脚本 |
| [`release-evidence/`](release-evidence/) | 每个版本的发布证据清单（`vX.Y.Z.md` + `template.md`） |

## 验收证据

| 文档 | 用途 |
| --- | --- |
| [`m7-test-matrix.md`](m7-test-matrix.md) | M7 手动测试矩阵（全部通过） |
| [`m7-visual-polish.md`](m7-visual-polish.md) | M7 Zed-style 视觉打磨验证（12/12） |

## 专项分析

| 文档 | 用途 |
| --- | --- |
| [`remote-editing-bug-analysis-2026-07-16.md`](remote-editing-bug-analysis-2026-07-16.md) | 远程编辑 post-merge 11 bug 分析（已修复） |

## 目录结构

```
docs/
  README.md                      本索引
  gpui-russh-plan.md             架构主文档（强制）
  architecture-review-v2.md      v2/v3 评审
  architecture-review-v3.md
  plans/                         按日期的设计/实现计划
  progress-analysis-*.md         进度快照
  ui-ux-guidelines.md            UX 强制规范
  ux-improvement-plan.md         UX 阶段计划
  release-process.md             发布流程
  release-evidence/              发布证据
  m7-test-matrix.md              M7 验收
  m7-visual-polish.md
  remote-editing-bug-analysis-*.md
  superpowers/                   SDD 工作流产物
  archive/                       归档的历史文档
```
