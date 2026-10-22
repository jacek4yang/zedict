# 本地资料核验

本目录记录从现有原始提取中选出的 375 个词头。`approved.tsv` 是明确的源行号和义项选择，`local.manifest.json` 将决定绑定到源文件与单条记录的 SHA-256，`cases.json` 固定回归映射。原始词库内容不包含在这些清单中。

复核者为 Codex（AI）。`verified` 表示存在明确记录的本地入库决定，**不表示已完成独立人工审校**。已有词头猜测索引、拼写词表命中、近义词链接或例句关键词本身都不足以证明词头身份。本次结合源音标、中文定义和特定词条上下文逐条选定；全量导入不需要人工审批；这些记录作为已有修正覆盖层使用。

## 继续核验

1. 运行 `zedict audit --input <源.jsonl> --out <新目录>`。所有候选初始均为 `unresolved`。
2. 用 `python tools/review.py inspect <源.jsonl> <行号>` 查看原始字段、记录哈希与拆分例句。
3. 核实词头身份、释义边界、每个保留字段和词形关系。证据不足时保持 unresolved；确认错误且不能修复的记录设为 rejected。
4. 在清单中填写自己的 reviewer、具体 evidence、保留字段和状态。不要把源文件变化后的新哈希直接填进旧清单冒充重新核验。
5. `zedict compile --input <源.jsonl> --reviews <核验.json> --out <目录>`。失败不会替换上一版词库。
6. 执行 `doctor`、已知错误回归和新记录抽查，再使用 `compile --install` 安装到用户目录。

## 核验清单结构

```json
{
  "schema_version": 1,
  "source_sha256": "整个源文件的 SHA-256",
  "data_version": "my-review-1",
  "reviews": [{
    "line": 1,
    "record_sha256": "该行去除结尾 CR/LF 后的 SHA-256",
    "status": "unresolved",
    "headword": "",
    "reviewer": "",
    "evidence": "",
    "senses": [],
    "examples": [],
    "keep_phonetics": false,
    "keep_english": false,
    "keep_forms": false,
    "keep_relations": false,
    "aliases": []
  }]
}
```

`line` 从 1 开始，`senses` 是原始 senses 数组的零起点索引；其中 HTML 列表随后会拆成独立义项。`examples` 是解析后的双语例句索引：按原始数组顺序，再按 `²` 拆分；不要把一整组十条例句误认为一条例句。未明确选择的字段不会入库。

`aliases` 的每项含 `form`、`kind`、`evidence`。kind 只允许 `past / past_participle / present_participle / third_person / plural / comparative / superlative`；可对同一表面形式保留多个关系或多个原形。派生词属于展示信息，不能登记为屈折原形。

原始 `pos` 可能聚合自错误合并的多个词条，因此不直接信任；词性从选中的释义提取。原始 forms 的“时态”标签不能区分具体关系，会显示为“其他”；明确核验的 aliases 会同时生成为结构化词形展示。

## 已恢复的代表案例

| 词头 | 源行号 | 本次处理 |
|---|---:|---|
| robust | 143946 | 从 robustly 误绑定恢复；保留强健等义项与两条已查阅例句 |
| run | 144989 | 从 ran 恢复；显式记录 ran/running/runs |
| world | 182429 | 排除“工作板”，保留世界义项 |
| angle | 27834 | 使用 /æŋɡl/ 与角度释义对应记录，排除 angled 等错配 |
| city | 47962 | 保留城市义项，不接受其他城市名或缩写卡片 |
| apple | 29505 | 排除应用管理，保留苹果义项 |
| book | 38707 | 从一串屈折形式恢复词头 |

## 尚未恢复的边界

原始流 offset=0 是“铝合金”，下一 `<FC>` 才是 A-OK；只把 offset=0 插入旧条目列表仍会误合并。新的 `tools/review.py fragments` 保留每个 `<FC>` 的起止偏移及后续未绑定标记，不猜测完整条目边界。原始词头索引仍没有经过往返验证的解码器；`utf8.bin` 只作为研究线索。

清单以外的结构化记录由默认全量导入路径加载，不需要新增人工审批。缺少词头的记录保留在 source_records 原始归档中，不猜测其英文词头。查询优先采用清单已修正的同名词卡；其他词头按现有结构化字段加载。
