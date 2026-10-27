# macSFTP 文档索引

工作树只维护当前约束和仍有操作价值的记录。已完成的设计、实施计划、阶段进度和架构评审通过 Git 历史查询，不再与当前文档并列维护。

## 当前文档

| 文档 | 用途 |
| --- | --- |
| [`architecture.md`](architecture.md) | 当前架构、所有权、协议、安全、UI/UX 和测试边界；架构或界面改动前必读 |
| [`adr/`](adr/) | 已接受且仍约束当前实现的架构决策；避免后续评审重复讨论同一取舍 |
| [`release-process.md`](release-process.md) | tag、版本、构建和 GitHub Release 流程 |
| [`release-evidence/`](release-evidence/) | 已发布版本的验收记录及新版本模板 |
| [`screenshots/`](screenshots/) | `README.md` 使用的产品截图 |
| [`visual-evidence/`](visual-evidence/) | 界面改动的视觉验收记录 |

仓库根目录还维护以下当前文档：

| 文档 | 用途 |
| --- | --- |
| [`../README.md`](../README.md) | 当前产品能力、用户流程、构建和 crate 总览 |
| [`../README.zh-CN.md`](../README.zh-CN.md) | README 的简体中文版本 |
| [`../README.ja.md`](../README.ja.md) | README 的日文版本 |
| [`../README.ko.md`](../README.ko.md) | README 的韩文版本 |
| [`../CHANGELOG.md`](../CHANGELOG.md) | 已发布及未发布的用户可见、安全和兼容性变化 |
| [`../CONTRIBUTING.md`](../CONTRIBUTING.md) | 开发、验证、架构边界和文档同步要求 |
| [`../SECURITY.md`](../SECURITY.md) | 漏洞报告方式和必须保持的安全边界 |

## 维护规则

- 代码是行为事实来源；文档只记录代码无法强制的约束和取舍。
- 中等以上架构或 UI/UX 变化更新 `architecture.md`；功能交付后把仍有效的约束并回当前文档，并删除已完成的逐步实施计划。
- 已接受且仍约束当前实现的架构取舍记录在 `adr/`，一条决策一个文件；被新决定取代时标记 Superseded 并保留原文，不删除历史。
- 发布证据按版本保留，不把旧版本的结果改写成当前状态。
- 用户流程改变时同步 `README.md`、其三种语言版本和 `CHANGELOG.md`；持久 UI 规则改变时同步 `architecture.md` 第 12 节。
- 历史材料需要追溯时使用 `git log -- docs`、`git show <commit>:<path>` 或 GitHub commit history。
