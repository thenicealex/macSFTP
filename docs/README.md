# macSFTP 文档索引

工作树只维护当前约束和仍有操作价值的记录。已完成的设计、实施计划、阶段进度和架构评审通过 Git 历史查询，不再与当前文档并列维护。

## 当前文档

| 文档 | 用途 |
| --- | --- |
| [`gpui-russh-plan.md`](gpui-russh-plan.md) | 当前架构、所有权、协议、安全和测试边界；架构改动前必读 |
| [`ui-ux-guidelines.md`](ui-ux-guidelines.md) | 当前 UI/UX 强制规范和评审清单 |
| [`release-process.md`](release-process.md) | tag、版本、构建和 GitHub Release 流程 |
| [`release-evidence/`](release-evidence/) | 已发布版本的验收记录及新版本模板 |

仓库根目录还维护以下当前文档：

| 文档 | 用途 |
| --- | --- |
| [`../README.md`](../README.md) | 当前产品能力、用户流程、构建和 crate 总览 |
| [`../CHANGELOG.md`](../CHANGELOG.md) | 已发布及未发布的用户可见、安全和兼容性变化 |
| [`../CONTRIBUTING.md`](../CONTRIBUTING.md) | 开发、验证、架构边界和文档同步要求 |
| [`../SECURITY.md`](../SECURITY.md) | 漏洞报告方式和必须保持的安全边界 |

## 维护规则

- 代码是行为事实来源；文档记录代码无法强制的约束和取舍。
- 中等以上架构变化更新 `gpui-russh-plan.md`，或在实现期间增加临时设计说明。
- 功能合并后，把仍有效的约束并回当前架构文档，并删除已完成的逐步实施计划。
- 发布证据按版本保留，不把旧版本的结果改写成当前状态。
- 用户流程改变时同步 `README.md` 和 `CHANGELOG.md`；所有权或协议改变时同步架构文档；持久 UI 规则改变时同步 UI/UX 准则。
- 历史材料需要追溯时使用 `git log -- docs`、`git show <commit>:<path>` 或 GitHub commit history。

当前目录结构：

```text
docs/
  README.md
  gpui-russh-plan.md
  ui-ux-guidelines.md
  release-process.md
  release-evidence/
```
