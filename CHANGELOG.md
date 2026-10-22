# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

## [0.3.0] - 2026-10-22

首个公开发布版本。

### Added

- 离线英汉词卡查询：精确词头、前缀、包含、模糊、词形、繁简折叠、拼音与中文释义反查。
- 从 `combined.bin` 与 `phrase.bin` 的精确索引导入全量词头，并提供合并、分片、校验与打包工具。
- 可选例句索引、词形关系、词级表、拼写补位、拼音索引与近义/反义/联想资料。
- 生词本、复习计划、评分日志，以及 Anki CSV/TSV 与不依赖第三方库的 `.apkg` 导出。
- 管道、JSONL 与 HTTP 兼容服务；联网翻译、讲解与 TTS 均为可选能力，失败时退回本地结果。
- Windows x64、macOS ARM64、Linux x64 的 CI 与标签触发的 Release 构建。

### Changed

- 源码按 `cli`、`app`、`core`、`data`、`net`、`render` 六个领域重组，`lib.rs` 只导出这些顶层模块。
- CLI 拆分为命令定义、查询输出和输入/导出/联网辅助三部分；生成字形表并入 `core::variant`。

### Security

- 词库始终只读；学习状态独立写入 `study.sqlite`，联网结果只落用户缓存目录。
- 仓库与发布包均不包含欧陆词典词库内容；词库需使用者从本地资料自行构建。

[Unreleased]: https://github.com/jacek4yang/zedict/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/jacek4yang/zedict/releases/tag/v0.3.0
