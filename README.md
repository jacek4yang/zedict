# ZEDICT

[![CI](https://github.com/jacek4yang/zedict/actions/workflows/ci.yml/badge.svg)](https://github.com/jacek4yang/zedict/actions/workflows/ci.yml)
[![Release](https://github.com/jacek4yang/zedict/actions/workflows/release.yml/badge.svg)](https://github.com/jacek4yang/zedict/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可)

离线英汉命令行词典。支持完整词卡、中文释义反查、词形候选、拼写建议、拼音检索、例句整句与中文关键词检索（可选索引）、超纲词统计、生词本与复习计划、Anki 卡片表与 `.apkg` 导出、管道和 JSONL。查词无需网络、API key、原词典程序或项目工作目录。

本文里的“词卡”指一个词条的全部词典内容，“词头”指词条的标准英文写法；命令行界面统一显示为“词条”“词条记录”，不使用这两个内部叫法。

默认全量导入从 `combined.bin` 与 `phrase.bin` 精确提取并合并的 `data/eudic-merged/all.jsonl`（522,675 条，每条都有词头），不要求人工审批。旧 `data/dict_full.jsonl` 的 375 条词头修正仍可作为可选覆盖层，修正后的同名词卡优先用于查询。所有原始记录（包括缺少词头的旧记录）都会压缩保存到词库；缺少词头的记录不能凭空转为英文查询入口。`doctor` 分别报告记录数、词卡数和去重词头数。

当前本地构建（`data/eudic-store-phrase/dict.sqlite`，841,277,440 B）：**522,661 个规范化词头**、522,675 张词卡、615,703 个义项。522,675 条原始记录全部归档，缺少词头 0 条、非法词头 0 条；14 个词头在规范化后发生键碰撞，所以去重词头比词卡少 14。这些词头直接来自 `combined.bin` 的条目索引和 `phrase.bin` 的词头缓冲，不再依赖启发式自证；自证分档仍按第六轮规则统计：例句高亮 53,674 张、仅释义出现 3,063 张、无自证 465,938 张——无自证在新通路里只说明这条记录没有例句也没有把词头写进自己的释义（汉语条目和短语/专名大多如此），不代表词头是错的，默认 `--key-evidence all` 全收。可选资料规模：例句 64,403 行 / 545,943 条链、词形关系 17,366 条（7 条原形未命中）、近义/反义/联想中文 746,205 条、词级 72,970 行、拼写补位 5,062,201 行（覆盖 515,829 个词头）、拼音索引 159,848 条读音键（159,846 张词卡、136,835 个 distinct flat 键）、参考词表 68,089 行。仅 `combined.bin` 的旧构建（`data/eudic-store/dict.sqlite`，693,010,432 B；428,668 个词头 / 428,682 张词卡 / 520,261 个义项）保留作对照；旧 `data/dict_full.jsonl` 通路的 69,608 个词头 / 109,969 张词卡 / 183,783 个义项与逐项测量保留在[实现边界](#实现边界)供复现。

## 安装和首次构建

需要 Rust 1.98.1；构建依赖使用 Cargo.lock 固定。SQLite 随程序编译，用户不需要安装 SQLite。

```sh
cargo build --locked --release

# 1) combined.bin → 22 个分片（可加 --inflated data/combined.inflated 复用缓存）
python3 tools/eudic_extract.py --source /path/to/dic/combined.bin \
  --out data/eudic --merge

# 2) phrase.bin → 111,302 条短语/专名（可加 --inflated data/phrase.inflated）
python3 tools/eudic_phrase.py --source /path/to/dic/phrase.bin \
  --out data/eudic-phrase

# 3) 合并：重叠键只补新义项，其余成为独立词卡
python3 tools/eudic_merge.py --combined data/eudic/all.jsonl \
  --phrase data/eudic-phrase/phrase.jsonl --out data/eudic-merged/all.jsonl

# 4) 导入合并后的全量词头；22 个分片也可按顺序逐个导入，但只有合并文件是全量
cargo run --locked --release -- compile --input data/eudic-merged/all.jsonl \
  --inflections data/inflect_aliases.jsonl \
  --related data/eudic/related.tsv \
  --levels data/word_levels.jsonl \
  --examples --spell --pinyin --vocab data/spell_words.txt \
  --out data/eudic-store-phrase
cargo run --locked --release -- --db data/eudic-store-phrase/dict.sqlite robust
```

`data/eudic/`、`data/eudic-phrase/`、`data/eudic-merged/` 与 `store/` 都在 `.gitignore` 里：仓库只托管代码、提取器和格式说明，不分发欧陆词库内容。旧的 `data/dict_full.jsonl` 通路仍保留（可配 `--corrections reviews/local.manifest.json` 复现 375 条人工修正），但它的词头来自启发式，新构建应优先使用 `data/eudic-merged/all.jsonl`。

`data/` 是已有本地资料，不包含在程序发布包中。核验清单只适用于 [source-inventory.json](reviews/source-inventory.json) 指定哈希的资料；源文件变化后必须重新核验。清单中的 `external/` 只是“仓库外来源”的标记（本机欧陆安装目录、OpenCC 包等），并非仓库内路径，SHA-256 才是核验依据。`--related`、`--levels` 都是可选资料，不带就少相应的能力，词库照样能用（见[词级与超纲词](#词级与超纲词)）。

安装两个命令入口并将新词库安装到用户目录：

```sh
cargo install --locked --path . --bins
python3 tools/eudic_phrase.py --source /path/to/dic/phrase.bin --out data/eudic-phrase
python3 tools/eudic_merge.py --combined data/eudic/all.jsonl \
  --phrase data/eudic-phrase/phrase.jsonl --out data/eudic-merged/all.jsonl
zedict compile --input data/eudic-merged/all.jsonl \
  --inflections data/inflect_aliases.jsonl --related data/eudic/related.tsv \
  --levels data/word_levels.jsonl --examples --spell --pinyin --vocab data/spell_words.txt --install
zedict robust
zedict doctor
```

也可以直接运行 `target/release/zedict`（Windows 为 `zedict.exe`）。旧 `dict-english` 命令名仍可用。旧的 `store/dict.db` 和原始数据不会被构建器覆盖；旧 `ZEDICT01` 和 schema 1 SQLite 数据须从结构化源重新构建。

## 仓库、数据与发布

仓库只托管代码、测试、生产构建脚本和审计资料，**不托管词库内容**。`data/`、`store/`、`out/`、`extracted/`、`target/`、`review-work/`、`research/` 与 `tools/_probe/` 都被 `.gitignore` 排除；其中 `data/` 与 `store/` 是欧陆词库及派生 SQLite，没有可再分发授权，不能上传到 GitHub，也不能放进公开 Actions artifact。`research/` 只保存本机历史实验脚本，不进入仓库。

独立编译与运行：

- Rust 版本由 `rust-toolchain.toml` 固定为 1.98.1，依赖由 `Cargo.lock` 锁定。
- SQLite 通过 `rusqlite` 的 `bundled` 特性编译进程序，TLS 使用 rustls；构建和运行都不依赖系统 SQLite、OpenSSL 或欧陆词典程序。
- Python 只在提取/导出资料时需要；`zedict` 运行时不需要 Python，也不依赖当前工作目录。
- `tools/package.py` 只打包两个可执行文件和 README，不含词库；把本地构建的 `dict.sqlite` 放到配置目录（或 `--db` 指向它）即可查词。
- CI 覆盖 Windows x64、macOS ARM64、Linux x64；打 `v*` 标签会触发 release workflow，构建并上传三个平台的压缩包与 SHA-256。它不构建、也不上传词库。

词库想“像源码一样分片、由 Actions 编译”时，分片只解决单文件体积，不解决版权：

```sh
# 切成 24 MiB 一片（按完整 JSONL 行切，绝不切断一行）
python3 tools/dict_shards.py split --input data/eudic-merged/all.jsonl --out data/shards
# 在另一台机器或 CI 里按 manifest.json 逐片校验并原样合并
python3 tools/dict_shards.py join --input data/shards --out data/eudic-merged/all.jsonl
```

`manifest.json` 记录原文件与每片的字节数、行数、SHA-256；合并时任何一片被改动都会拒绝。只有**你拥有或已获授权**的词库才能把分片放进私有仓库或私有 Actions artifact；欧陆派生内容仍受原授权限制，公开仓库和公开 artifact 都不能放。公开 Release 只发布程序，不发布词库。

Windows 上如果 `python` 指向应用商店占位符，用 `py -3` 执行 README 里的 Python 命令。

## 日常查询

```sh
zedict robust
zedict "as well as"
zedict love
zedict 苹果
zedict 測試
zedict running
zedict query saw
zedict query runn --mode prefix --limit 20
zedict query wor --mode prefix --limit 20
zedict query wrold --mode fuzzy
zedict query 苹果 --mode reverse
zedict query xian --mode pinyin
zedict query "xi'an" --mode pinyin
zedict robust --brief
zedict robust --json
zedict robust --theme rich
zedict query query
```

默认完整展示所选词头的全部内容。`--brief` 显示前 3 个义项并隐藏例句与扩展资料；`--full` 覆盖配置中的简洁模式。`--limit` 默认 8，范围 1–1000，控制搜索词头数量，不截断词卡义项。全量模式保留源数据中的例句、英英释义和词形；已修正词条使用修正后的字段。完整词卡另起一行给出音节切分（`音节: com·pu·ter`），单音节词和不规则形状不给假切分。

文本排版只用 ASCII 装饰符（`=`、`[ ]`、`-`、`>`），不依赖制表符或 box-drawing 字符；标题、音标、义项编号、例句引用各有固定位置，窄终端按可见宽度折行。近义、反义、相关逐条计数，释义随条目显示；宽度允许时排成两列，窄终端自动退回单列折行，词条和释义都不会省略。`--theme auto`（默认）只在交互终端且终端支持 ANSI 时上色；`--theme ascii` 始终无色；`--theme rich` 强制彩色（Markdown 风格：标题、音标、词性、义项编号、例句、拼写建议分色）。`NO_COLOR`、`CLICOLOR=0`、`TERM=dumb` 会关闭 `auto`；Windows 下只对 Windows Terminal、VS Code、ConEmu、Git Bash 等已知支持 ANSI 的终端自动着色，旧 conhost 保持纯文本。`--json` 与管道输出不受主题影响。

颜色只使用终端标准的 16 色（ANSI SGR 索引），不写死 RGB，换终端主题时颜色由终端自己的调色板决定。`auto` 还会判断背景深浅，深色和浅色终端各用一套对比度足够的索引：优先级是 `ZEDICT_BACKGROUND=dark|light` → `COLORFGBG` → Windows 控制台属性 → 深色兜底。浅色终端如果自动判断不准则显式设置 `ZEDICT_BACKGROUND=light`。

`auto` 是默认模式：英文先精确查询，并另列原形候选；含汉字时先精确匹配中文词头（新精确词库的汉语条目），再补中文释义反查，没有中文词头时才整段走反查。没有精确/词形命中、也没有英文词首候选的纯字母查询会再试拼音；带调号或数字调的查询直接试拼音，所以 `xiàn`、`xian4` 都会进入拼音检索。无声调的 `xian` 因存在 `xianggang` 等英文词首候选，按设计不抢英文前缀——Auto 不回退到拼音，也不把词首候选当命中，只给拼写建议；查拼音写 `--mode pinyin`。`exact` 只做精确词头查询。其他模式是 `prefix`、`contains`、`fuzzy`、`reverse`、`pinyin`。错拼建议不会冒充精确命中，派生词不会被自动当作屈折原形。

拼音检索使用 `--pinyin` 构建的索引：无声调、`ü` 归一为 `v`，`lü`/`lv` 同义；多音字（`shé，zhé，zhē`）每个读音各建一行。结果按读音吻合度分四档：整串音节相等（`xi'an` 命中西安，同音的“烯胺/锡安”也在同一档）→ 连写整串相等（`xian`）→ 查询是完整音节（`an` 找到“西安”、`xian` 找到“先后”）→ 仅是连写前缀（`xian` 找到“想”，因 `xiang` 以 `xian` 开头）。档内先短词后长词；词库没有词频，同音词按字形稳定排序。分档保证完整音节命中不会被前缀词淹没：全量库 `an` 的“西安”在 `--limit 1000` 中排第 296，`xian` 的“先后”排第 88；默认 `--limit 8` 会先显示同音单字，词组类边界命中要放大 limit 才看得到。本机全量库实测 159,846 张词卡、159,848 条读音键、136,835 个 distinct flat 键。

错拼建议的距离同小时按常见度排序（词级表里档位靠前的先出，`axs` 给 `axis` 而不是 `axe`），没有词级表时退回字典序，结果仍然稳定。`rnning` 这类**拼错的屈折形式**既给出同距离的 `running`，也会经词形表把原形 `run` 端上来；命中的词形记在 `via` 里并显示成 `run (距离 1，词形 running)`。绕词形表来的候选一律排在同距离的直达候选之后，原形自己已有候选时不重复添加，因此 `running` 在完整词库里仍按自己的词卡出现。

带 `--spell` 构建的词库还会多做一步**兜底补位**：穷举扫描和词形表都排完了还没凑满 `--limit` 条时，才从删字符索引里按"距离放宽一步"补候选（`thsi` 会多出 `thin/thus/thir`）。补位一律接在已有候选之后，不重排、不替换，所以不带这张表的词库结果与今天逐条一致；它的可达范围很小，实测对拼写建议的 recall@k 没有改动，理由和数字见"实现边界"。

中文反查查询的是已有中文义项的连续子串，不是在线翻译，也不是语义搜索。支持一字、二字、长词；不会把“学，习”误认为“学习”。精确命中的汉语词头（`呕`、`酢浆草科`）排在反查结果之前，所以新词库既能当汉英词典用，也保留原有的英汉反查。繁体查询先折成简体再查，词组优先、再逐字（`測試` 与 `测试`、`一目瞭然` 与 `一目了然` 命中同一批词头）；折叠只作用于查询串，库里的键不变，因此不需要重建词库。

`prefix` 同时扫描词头和词形表：只在词形表里出现、没有独立词卡的形式也会补出来，并指向它的原形（`runn` 能补出 `running`）。

用 `--vocab data/spell_words.txt` 构建过的词库，在未命中而该词又在参考词表里时，会在结果后面追加一行“该词是很常见的英文词，但本地词库暂未收录”。这行只走文本输出，`--json` 的形状不变。它把"这份词库没有这个词"和"英语里没有这个词"分开——两者的差别见[实现边界](#实现边界)。

### 英英释义翻译（默认自动）

交互终端里查询时，英英释义和例句会逐句补中文，译文以缩进的 `译：` 行紧跟在原文下面：

```text
[英英]
  n.
  1. a strong positive emotion of regard and affection
     译：一种强烈的积极关怀和情感
     > his love for his work
       译：他对工作充满热爱
```

词性标题和 `syn:` 词表不翻译（它们不是句子）。自动翻译是**有网就补**的增益：配了 LLM 密钥（`LLM_API_KEY`）先走大模型，失败或没配再走与 `translate --engine edge` 同一条免密钥 Edge 通路；断网、超时或两条通路都失败时安静显示英文原文，查询照常成功，退出码不变。自动模式最多等 8 秒；显式 `--en-zh` 会等完整网络超时，并在失败时于 stderr 说明原因。

为了让脚本输出保持确定，**管道/重定向、`--stdin` 批量和 `--json` 默认不翻译**。需要在这些场景翻译时：

- `--en-zh`：强制翻译（`--json` 仍不翻译，只提示）；
- `--no-en-zh`：关闭自动翻译；
- 配置 `en_zh = true`：连管道输出也自动翻译；`en_zh = false`：从不自动翻译。命令行开关优先于配置。

两条通路都会把英英释义和例句文本发给你配置的 LLM 端点或微软 Edge 翻译端点，回复只用于本次文本渲染，不写回词库。Edge 通路按原文逐条缓存在用户缓存目录，重复查询不重复请求。译文由机器生成，只作辅助理解；免密钥 Edge 逐条翻译没有上下文，孤立短语可能译得生硬或不准，需要更高质量就配置 LLM。

### 记忆提示（构词联想）

完整文本输出会在词卡后追加 `[记忆提示]`，把词拆成“常见前缀 + 本地词库里能查到的完整基础词”。例如：

```text
[记忆提示]
  按常见前缀拆开，和已经认识的基础词挂钩；这是联想记忆，不是严格词源。
  un-（不）+ happy（幸福的） → 不幸福的
```

前缀名称和释义来自本地词库自带的词缀词条，基础词和整词释义也来自本地词卡，不引入外部资料。为了不硬凑，提示要同时满足这些条件：

1. 前缀在常用表里，剩下部分能在词库里精确查到完整词条；
2. 基础词有实义词性（缩写如 `est` 不算），且基础词和整词至少有一族词性重合；
3. 整词释义和基础词释义对得上：整词直接包含基础词义（`unhappy` ⊃ `happy`），或整词里出现该前缀常见的语义标记（`preview` 的“预”、`subway` 的“地”），或两者共享实义汉字（`overcoat` 与 `coat` 共享“衣”）。只靠“词形看起来像”的拆分会安静跳过，所以 `profile` 不会拆成 `pro- + file`，`translate` 不会拆成 `trans- + late`。

最后一级证据（只有前缀标记和常见基础词）还要求基础词在词级表里是 1–5 级；没有词级表的老词库退化为“基础词至少 4 个字母”。常见后缀在本地库几乎没有词条（只有 `-natured`），所以不会编造后缀拆分。`--brief` 不显示，`--json` 的字段和形状完全不变。

这不是词源：它不解释历史演变，也不声称构词关系一定成立；本地词库没有结构化的词根/词源字段。欧陆的在线 `prefixwiki` 词根/词源接口需要授权签名，本工具不调用，也不复用其服务端凭据。

## 管道和输出契约

PowerShell：

```powershell
Get-Content words.txt -Encoding UTF8 | zedict query --stdin --json
```

Bash / Zsh：

```sh
zedict query --stdin --json < words.txt
```

标准输入每个非空行是一条查询，保留顺序；`--json` 输出 JSONL，每行一个完整对象。单条 JSON 查询也输出一个对象，所有查询模式使用同一结构：

```json
{
  "schema_version": 1,
  "query": "missing-word",
  "normalized_query": "missing-word",
  "status": "not_found",
  "hits": [],
  "suggestions": []
}
```

`hits[].match_type` 为 `exact / lemma / prefix / contains / reverse`。中文结果附 `matched_senses`，词形结果附 `relations`；每张词卡带可追溯的 `source`。`suggestions[]` 是 `{word, distance, via?}`，`via` 只在建议来自某个拼错词形时出现，其余情况整个键省略。显式 fuzzy 查询有建议时状态为 `suggestions`，其他命中为 `found`。

| 退出码 | 含义 |
|---|---|
| 0 | 成功；显式 fuzzy 找到建议 |
| 1 | 文件、数据或网络等运行错误 |
| 2 | 参数错误或非法查询 |
| 3 | 无结果；批量查询有任一未命中 |
| 130 | Ctrl+C 中断 |

结果输出 stdout，诊断输出 stderr。无结果也产生合法 JSON。批量读取到非法查询时停止，之前输出的完整行仍有效。关闭下游管道不产生 panic。正文默认使用 ASCII 排版，管道输出和 `--json` 永远不含 ANSI；交互终端是否上色由 `--theme auto` 判断，`rich` 需要显式指定；终端按可用宽度换行，不强制启动分页器。文本查询的自动英英翻译只发生在交互终端；管道、`--stdin` 与 `--json` 默认不发翻译请求，需要时显式 `--en-zh` 或配置 `en_zh = true`。

## 配置与诊断

优先级：命令参数 → 环境变量 → 配置文件 → 默认值。

- `--db` / `ZEDICT_DB`：覆盖词库路径。
- `--config` / `ZEDICT_CONFIG`：覆盖配置文件位置。
- `--theme` / `ZEDICT_THEME`：终端配色 `auto|ascii|rich`；命令行优先于环境变量和配置文件。
- `en_zh`：英英释义自动翻译。缺省只在交互终端翻译；`true` 连管道输出也翻译，`false` 从不自动翻译（显式 `--en-zh` 仍可强制）。命令行 `--en-zh` / `--no-en-zh` 优先于配置。
- `ZEDICT_BACKGROUND=dark|light`：指定终端背景深浅，供 `auto` 选择对比度合适的 16 色；不设置时依次尝试 `COLORFGBG`、Windows 控制台属性，最后按深色处理。
- Windows 默认使用用户 LocalAppData 下的 `zedict` 配置/数据目录；macOS 使用用户 Library/Application Support；Linux 遵循 XDG 配置/数据目录。
- 精确路径通过 `zedict doctor` 查看；缺库时错误会显示预期的文件路径与构建方法。
- 配置中的相对路径相对于配置文件所在目录；命令参数中的相对路径相对于当前目录。

```toml
db = "dict.sqlite"
brief = false
# 缺省只在交互终端自动翻译；true 连管道也翻，false 关闭自动翻译
# en_zh = true
theme = "auto"
llm_base_url = "https://api.deepseek.com/v1"
llm_model = "deepseek-chat"
```

`doctor` 只检查本地数据：SQLite 身份/版本、完整性、词卡解压、词头与来源、悬空词形关系（带例句索引时还包括悬空的例句链与拼写候选；带词头档位表时还检查每条 `entries.id` 在 `key_evidence` 里都有行）、构建报告，以及这次构建附加了哪些资料表（`datasets`），不会联网或输出密钥。六张可选表在不在另有 `tables.levels` / `tables.examples` / `tables.spell` / `tables.pinyin` / `tables.key_evidence` / `tables.vocab` 六个布尔值：`datasets` 里有记录不等于表非空，也不能反推表里有没有数据，所以这一层要单独说清。`tables.key_evidence` 为 `false` 就是"这份库按默认阈值 `all` 构建，没有净化过，也就不需要存档位"。

```sh
zedict completions bash
zedict completions zsh
zedict completions fish
zedict completions powershell
```

## 屈折词形

`ran`、`running`、`children` 这类词形经 `aliases` 表指向原形词卡；派生词（`happiness`、`worker`）永远不作为原形命中。词形表由本地资料生成，不联网：

```sh
python tools/inflect_aliases.py --db store/dict.sqlite --stats
python tools/inflect_aliases.py --out data/inflect_aliases.jsonl
```

生成器只保留 Hunspell `data/spell/en_US.aff` 的屈折规则类（D/G/R/S/T），用 `en_US.dic` 的 `st:` 交叉引用补充不规则和双写字形，再按以下证据过滤：

- 词性决定 `-s` 是复数还是第三人称单数，两种读法都成立时同时保留；
- 规则生成的词形必须在 `data/combined.inflated` 的英文正文中出现，规则本身会造出 `stoped`、`runing` 这类不存在的词形；
- 若双写拼写（`planned`）在正文中多于规则拼写（`planed`），丢弃后者；
- 规则 `-er` 词形必须在正文中出现于 `than` 之前，否则它是 `defaulter`、`airer` 这类施事名词，不是比较级；`-est` 同理要求该词存在比较级。

旧 `data/dict_full.jsonl` 与 `store/dict.sqlite` 构建产出 17,338 条词形关系，覆盖 12,659 个不同词形。另有约 26,500 条候选词形的原形不在词库词头中，生成阶段就已丢弃；编译同样只接受原形已有词卡的行，其余计入 `inflections_unmatched`。这些缺口来自旧通路的启发式词头（74,124 条记录无词头，另有 6,445 个词头其实是屈折形式原文）。新通路的精确词头（`data/eudic/all.jsonl`，428,682 条记录、428,682 个词头）不再有这个问题；第六轮“正文里没有词头、也没有条目索引”的结论已在第七轮被推翻，见[实现边界](#实现边界)。用 `--key-evidence some|strong` 净化旧通路词头会让 `inflections_unmatched` 变得更大：被挡掉的错词头本来能接上一些词形关系，接不上是净化错词的应付代价，不是回归。

## 近义词中文与资料来源

源词库的近义/反义词只有英文。`data/semantic_related.tsv` 是 `词<TAB>中文` 的对照表（581,335 行，但只有 24,525 个不同的词），构建时带上它，扩展资料里的词就跟着中文：

```sh
zedict compile --input data/dict_full.jsonl --install \
  --inflections data/inflect_aliases.jsonl \
  --related data/semantic_related.tsv
zedict query address --full      # 近义: address（写姓名地址）, speech（演说）
```

- 只补中文，不增删词条、不改动任何键；不带 `--related` 时构建产物与带默认 `Extras` 的产物逐字节一致。
- 一个词在表里出现多次时取第一条非空释义；只有词没有释义的行（如 `Zune<TAB>`）跳过，不让整次构建失败。
- 表里没有的词仍然只显示英文（`computer address`）。旧通路按上面的完整命令构建，109,969 张词卡共配上 155,042 条中文，构建报告的 `近义/反义中文: N 条` 就是这个数；新通路带 `--related data/eudic/related.tsv`（源词库自带的词-中文关系）构建，报告 746,205 条；phrase 记录本身没有关系条目，合并后这个数不变。带 `--corrections` 时它比不带的少约 2,500 条：375 条被批准的记录只保留勾选的字段，而词形/关系都不在勾选范围内，那些卡的相关列表就此为空。
- 完整词卡另有 `音节: com·pu·ter` 一行（`--brief` 不显示）；单音节和切不出来的形状保持不显示，而不是硬凑一份。实测覆盖率随词头分布差别很大：按词头顺序取前 400 个有 91% 的词卡带切分，而抽 5000/30000/55000 三段各 1,000 个词头只有 61% —— 尾部密集的是缩写和 `-hydroxy…` 这类科学词形，切分器对它们就是不给结果。
- 这次构建用了哪些资料表，记在词库的 `meta('datasets')`（路径、SHA-256、行数）；`zedict doctor --json` 原样汇报 `datasets` 字段，老词库没有这条记录时返回空对象。

## 词级与超纲词

欧陆把"这个词超不超纲"放在服务端算。本项目把它做成离线能力：`tools/word_levels.py` 读本机欧陆安装目录里的 `dic/level.bin`（1,595,613 字节），导出 `data/word_levels.jsonl`，构建时用 `--levels` 带进可选表 `word_level(key, level, tags)`。

```sh
python tools/word_levels.py --input /path/to/dic/level.bin --stats
zedict compile --input data/dict_full.jsonl --install --levels data/word_levels.jsonl   # 完整命令见"安装和首次构建"
zedict hint "The provisional arrangement delineates the scope of an essay." --level 6
zedict hint --file paper.txt --level 3 --annotate
zedict hint --file paper.txt --level 3 --only-unknown
zedict hint "the ephemeral scope is a good deal as well as durable" --include-phrase
```

源表按**词形**组织，共 72,970 个词形：57,977 个带难度档，34,683 个挂大纲标签，其中 14,993 个只有标签、没有档（`an` 就是这种）。这 72,970 个词形里只有 25,520 个在本地词库有自己的词卡——词级表覆盖的是词形，不等于覆盖词头。

- **难度档 1–10 是源表给的互不重叠词表分片**，1 档最大（12,521 个词形）、10 档最小（2,628）。档名与档内实际条数并不吻合（6 档标称"COCA等级 10000 - 12000"、源表列 5,279 条、入库 4,584 条），所以本项目只用"档号小＝更常见"这个序，不复述它的排名区间。
- 24 个大纲标签逐词形照抄源表：`junior`/`senior`/`cet4`/`cet6`/`yjs`/`tem4`/`tem8`/`pets`/`ielts`/`toefl`/`ket`/`pet`/`toeic`/`bec`/`gre`/`gmat`/`sat`/`mba`/`act`/`collins1`–`collins5`。哪个词属于哪个大纲是源表的判定，本项目不重新判定也不修改。
- `--level` 是**目标难度档**（默认 4，1–10）：比它更靠后的档算超纲。首行打印该档在源表里的自述，第二行是分类计数，明细行的方括号就是这五个状态之一：

  ```text
  目标档 ≤3（源表标称 COCA等级 4000 - 6000），1 = 最常见；11 词元 / 7 个统计单位 / 7 个不同词（短语 2）
  在纲 2；生词 5：超档 2、仅大纲 3、词库有无档 0、两处都无 0

  ephemeral            档 8     ×1 act,collins3,gmat,gre,ielts,sat,tem8  [超纲] adj. 短暂的,瞬息的
  a good deal          无档      ×1 mba,senior,toeic  [仅大纲]
  ```

- 词库里查不到的词形一律记 `?`（两处都无），不会因为"长得像"就借档位。唯一的借用是已登记在 `aliases` 表里的屈折词形：词形自己没档时退回原形的档，明细行标 `←原形`。派生词（`happiness`）不算词形，不借。
- `--include-phrase` 把词库收录的多词条目按一个单位计（最长优先贪心匹配 `entries.key`），避免把一个短语数成一串生词；上面 11 个词元合成 7 个统计单位、`短语 2`。不打开时逐词计。
- `--annotate` 把标记插回原文，标记之外的字节原样保留，可以贴回编辑器：`The〖1〗 provisional〖6〗 delineates〖4〗 an〖cet4〗 essay〖2〗.`。用 `〖〗` 而不是 `⟪⟫`——后者无法编码进 GBK，而 Windows 默认代码页正是会踩到的那个。
- `--json` 给 `threshold/band/tokens/units/distinct/phrase_units/counts/items/annotated`。`band` 是源表对该档的原样描述（`desc`、标称 `items`、源文件里的 `name`/`color` 一起带上；10 档的原文是"COCA等级 20000 以上"），词级表的 `meta` 没给这一档的描述时整个键省略。`counts` 的五个键是 `below/above/tagged/unranked/unknown`。
- 词级表是**可选资料**：旧词库没有 `word_level` 表时 `hint` 退出码 1，stderr 给出 `tools/word_levels.py` 与 `--levels` 的重建命令，其他命令不受影响。这次构建带的是哪份文件（路径、SHA-256、行数）记在 `meta('datasets')`，`zedict doctor --json` 原样汇报。
- 明细行的释义取词库该词的第一条义项，只用来"确认是不是你想的那个词"。源数据里部分短语的义项本来就错配（`reviews/cases.json` 记录的就是这类），`hint` 不修正它。

## 例句检索

欧陆把"整句检索""中文反查例句"放在服务端。这里做成离线侧表：`zedict compile --examples` 从**本来就在读的那份输入**（旧通路 `data/dict_full.jsonl`，新通路 `data/eudic/all.jsonl`）里派生两张可选表，不引入新输入文件——例句一直在词卡里，只是没有索引。

```sh
zedict compile --input data/dict_full.jsonl --install --examples
zedict examples 病假条
zedict examples "a certificate for sick leave" --full
zedict examples "in spite of" --json
zedict query 苹果 --full
```

- 三个 `examples` 不是一回事，别混：`compile --examples` 是**构建开关**（生成索引）；`zedict examples <文本>` 是**子命令**（在例句库里检索）；`query --full` 输出里的"例句:"段来自**这张词卡自带的例句**，与索引无关，本次没有改动它的行为。
- 检索走两条路，`--json` 的 `matched_by` 如实说明走了哪条：先按关键词精确命中 `example_link.key`（`key`），一个没有再全文扫描 `instr()`（`scan`）。全文扫描不用 `LIKE`，因此没有 `%`／`_` 通配转义面。
- 中文关键词反查是这张表的主要用法：`病假条` 在实测语料里只作过一次标记，`zedict examples 病假条` 就给出那一句英文原句。非词头的英文短语照样能按键查到——`in spite of` 不是任何词头的精确写法，却是 10 句例句的关键词。
- `--full` 多打一行 `相关词条: …`，因为一句例句可以挂在好几个词条下面；句子按去重后的句对存，同一条链不会重复。
- 输出末尾给一行可直接复制的 `下一步: zedict explain <词> --in-context "…"`；句子里含引号时不给这一行，否则复制回去的参数是断的。**没有**给查询自动注入 `--in-context`：`explain` 的缓存键含 `context` 却不含"这次构建带不带例句索引"，同一个词在两种库上会得到不同讲解、看起来却像命中了缓存。
- `query <词> --full` 结尾那段"其他词条中的例句（含 …）"是 `--full` 的增益而不是检索入口：只有查询**已经命中词卡**时才打印，所以 `zedict query 病假条 --full`（词库里没有"病假条"这个词头）仍然退出码 3、什么都不打——那是 `zedict examples` 的活。缺索引时这段静默消失，旧词库的查询输出与今天逐字节一致；`query --json` 的结构 `schema_version: 1` 没动，这段只出现在人类可读输出里。
- 去噪是一次**静默清洗**，所以两处都要看得见：中文关键词按**挂了多少句**计数，超过 300 句的键整条不入库。实测 64,253 句里只有 4 个这种键（最高频的挂 816 句），共去掉 2,025 条链；英文侧 0 个。被剔掉的键按句数降序最多记 12 个（连同句数）写进 `meta('datasets')`，真实总数在 `dropped_noisy_keys`；`zedict doctor --json` 原样打印，而 `zedict examples <那个键>` 会自动退回全文扫描——句子还在，只是不再冒充关键词。
- 空结果退出码 3，缺表退出码 1（stderr 给重建命令），空查询退出码 2，与 `query` 一致。

## 导出 Anki 卡片

```sh
zedict export --rows 500 --out cards.csv
zedict export --format tsv --all --out all.tsv
zedict export --random --rows 200 --out sample.csv --json
```

表头是 Anki 认的 `#separator:` / `#html:false` / `#columns:`，导出的文件直接拖进 Anki 建卡，不需要插件：

```text
#separator:comma
#html:false
#columns:Front,Back,Tags
a,"1. n. 原子能
2.  (Skyhawk“空中之鹰”)54年服役的单座轻型舰载攻击机，现仍被多国使用",zedict band-1 list-cet4 list-cet6 pos-n
```

- `Back` 每条义项一行。CSV 用 RFC4180 引号包住换行（一个记录可能占多行物理行）；TSV 不能有换行，所以把制表符和换行压成空格。
- `Tags` 是 `zedict band-1 list-cet4 list-cet6 pos-n`：`band-N` 和 `list-` 来自词级表（`list-` 只取前两个大纲标签，全量标签用 `zedict hint` 看），`pos-` 来自词卡的词性。没有词级表的旧词库只给 `zedict` 和 `pos-`。
- `--rows`（默认 200）/`--offset`/`--all`/`--random`（≤1000）只是取样，不代表学习状态。`--all` 逐条取卡，量大时慢；`--all --random` 是参数错误。
- `--json` 只在配 `--out` 时有意义（否则报告什么？），单独使用按参数错误处理；取不到任何卡片时退出码 3。
- 只读现有词库、不联网，也不写 `store/dict.sqlite`。导出的卡片是**无状态快照**：不带生词本、复习进度或来源词书。需要这些状态时用下一节的 `study`，它可以再导出带记忆进度的 `.apkg`。

## 生词本、复习与 Anki .apkg

```sh
zedict study add running          # 有独立词卡就存 running，无卡词形才归并到原形
zedict study add apple
zedict study list --due           # 只看到期的
zedict study review run --grade good
zedict study stats
zedict study export --out wordbook.apkg --deck "我的词书"
zedict study remove apple
```

学习状态写在**独立**的 `study.sqlite`（默认用户数据目录，`--file` 或 `ZEDICT_STUDY_DB` 可改路径），词典库始终只读——测试里有一条断言比较导出前后词典库的 SHA-256。`study add` 先按 Auto 查询解析词条：有独立词卡就用该卡键（全量词库里的 `running` 自己就是词头，存 `running`），只有走词形归并时才落到原形（`ran` 存 `run`）；查不到的词按规范化文本存入，导出时背面标"（词库未收录）"。

- 复习计划是简化版 SM-2：`again` 10 分钟后重来并降低 ease，`hard` 按 1.2 倍、`good` 按 ease、`easy` 按 ease×1.3 放大间隔（新词从 1/4 天起）；每次评分写入 `review_log`，`study stats` 给出新词/学习中/复习/到期/累计复习数。
- `--json` 对所有子命令有效；输出分别带 `word`、`words`、统计字段或 `notes`/`out`/`deck`。
- `study export` 生成真正的 Anki `.apkg`：ZIP 容器里是 schema ver 11 的 `collection.anki2` 和 `media`，牌组名可自定义，正面词头、背面音标/拼音/至多五条义项。不引第三方 zip/sha1 库：条目用 stored 方式写入，`csum` 用内置 SHA-1 实现并配标准向量测试；文件先写临时文件再 rename，失败不会留下半个包。
- 生词本为空、或 `--due` 下没有到期词时导出退出码 1 并提示先 `study add`；`remove` 不存在的词退出码 3。没有云同步。

## 全量导入和可选修正

`zedict compile --input data/dict_full.jsonl --out store` 不需要清单即可导入所有带有效词头的记录。加 `--corrections reviews/local.manifest.json` 保留已有修正。旧的 `--reviews` 是明确选择的筛选模式，仅导入清单中的记录；日常使用无需该选项。审计工具仅用于诊断，不是使用词库的前置步骤。

扩展资料都是可选的，前三项要外部文件，后三项**纯粹从已经在读的那一趟里派生**（`--key-evidence` 只是阈值，不加输入文件）：

| 开关 | 输入 | 建的东西 | 谁在用 |
|---|---|---|---|
| `--inflections <jsonl>` | `tools/inflect_aliases.py` 生成 | `aliases` 表（已有） | `query`/`prefix`/`fuzzy` 的词形通路 |
| `--related <tsv>` | `data/semantic_related.tsv` | `related` 侧表 | 近义词中文 |
| `--levels <jsonl>` | `tools/word_levels.py` 生成 | `word_level` 表 | `hint`、同距离建议的常见度排序 |
| `--examples` | 无（源数据本身） | `example`／`example_link` 表 | `zedict examples`、`query --full` 的例句反查 |
| `--spell` | 无（`entries.key`） | `spell_key` 表 | `query --mode fuzzy` 的兜底补位 |
| `--pinyin` | 无（中文词卡的 `pinyin`） | `pinyin` 表 | `query --mode pinyin`、Auto 的拼音回退 |
| `--key-evidence all\|some\|strong` | 无（源数据本身） | `key_evidence` 侧表（`all` 时不建表） | 词头净化，以及 `translate --glossary-auto` 的入围资格 |
| `--vocab <txt>` | `data/spell_words.txt`（一行一个英文词） | `vocab` 表 | `query` 未命中时的"常见词但未恢复到词头"提示 |

`--key-evidence` 的档位与三种阈值的实测代价见[实现边界](#实现边界)；`all` 是默认值，也是"净化前的库"，任何阈值下**原始记录归档条数都不变**。

把这些开关全部留在默认位置，构建结果与**同一二进制关掉这些功能时**逐字节一致（`tests/structured.rs` 里有一条断言专门钉这一点），所以老词库照常能开：`store.open()` 只探测 `sqlite_master` 里有没有这几张表，绝不提前 `prepare`。注意这条不变量是"新开关不改变默认路径"，不是"和上一轮发布的 `store/dict.sqlite` 逐字节相同"——`meta('report')` 里的计数字段每轮都在增加，表结构和行数才是不变的那一部分。体积与延迟的实测数字见"实现边界"。

## 资料审计和修复

```sh
zedict audit --input data/dict_full.jsonl --out review-work/new-audit
python tools/review.py inspect data/dict_full.jsonl 143946
python tools/review.py inventory data/combined.inflated data/dict_full.jsonl
python tools/review.py fragments data/combined.inflated review-work/fragments.jsonl
```

审计生成报告、未核验队列和空白核验清单，不自动批准任何候选。队列每行带 `key_tier`（`example-key` / `in-gloss` / `unevidenced`）和按档改写的 `note`，`report.json` 另给档位普查、"只有 `unevidenced` 证据的词头数"和高基数候选的档位构成——审查时先看词头是不是自证的，比先看词条长短有用。输出目录必须不存在，以免覆盖上次工作。原始记录上限 32 MiB，入库词卡解压上限 8 MiB；现有提取末尾约 13 MiB 的无词头记录作为原始数据完整归档，不进入词卡查询。`doctor` 的文本输出报告三档计数与实际被阈值挡掉的条数，`doctor --json` 的 `tables` 里能看见 `key_evidence` 与 `vocab` 两张表在不在。

详细流程及核验字段见 [资料核验说明](reviews/README.md)。原始片段工具保留 offset=0，明确把 `<FC>` 片段与“完整词条”区别开来；不会继续把相邻片段强行合并。

## 可选联网功能

显式执行 `translate`、`explain`、`speak` 时联网；`query` 在交互终端默认尝试联网补英英翻译（`--no-en-zh` 或配置 `en_zh = false` 可关闭；管道、`--stdin`、`--json` 默认不翻译）。`LLM_API_KEY` 必须自行配置；保留 `LLM_BASE_URL` / `LLM_MODEL` 及对应参数（`explain` 一定要密钥，`translate --engine edge` 与 `speak` 都不要）。调用不自动重试已经产生部分输出的请求。

```sh
zedict translate "Despite recent progress, the problem remains open."
zedict translate --file paper.txt --translation-mode explain
zedict translate --file paper.txt --translation-mode literal --chunk 1200
zedict translate --file doc.txt --bilingual
zedict translate --file doc.txt --bilingual --engine llm --glossary terms.tsv
zedict translate "hello world" --engine llm --to fr
zedict translate --file paper.txt --engine llm --glossary-auto --style-file styles/academic.txt
zedict explain robust --in-context "The economy remains robust."
zedict explain running --style examples --cefr b1
zedict explain robust --to en
zedict speak robust --out robust.mp3
zedict speak robust --rate -30%
zedict speak apple --engine youdao --accent uk --out apple-uk.mp3
```

### 整篇逐句对照

`--bilingual` 成对输出“原文行 / 译文行”，段落之间空一行；`--json` 追加 `engine`、`source_lang`、`detected`、`target_lang`、`alignment` 和 `blocks[]`，原有 `text` 键不变。

- `--engine auto`（默认）配了密钥走大模型，否则走 Edge 翻译端点。`--engine edge` 不需要任何密钥，忽略 `--glossary`、`--glossary-auto`、`--style-file`、`--no-prev-output`、`--context`、`--chunk`、`--translation-mode` 并在 stderr 说明；`--engine llm` 在没有密钥时直接报参数错误。
- `--file` 读 UTF-8 文本，自动去掉 BOM 并把 CRLF 归一为 LF，上限 4 MiB；`--to` 是任意语言代码（默认 `zh-Hans`），`--from auto` 交给服务端识别语种（仅 `--engine edge` 使用）。语言代码**两条通路都先校验**：形状不合法（含 `;`、`?`、`/`、超 20 字符等）是参数错误、退出码 2，且一个请求都不发。
- Edge 通路按 `--batch-units`（默认 3600 UTF-16 码元，200–8000）分段，一批一请求、顺序执行，不做并发抓取；端点返回 400 时自动折半重试直到单条，限流只退避一次。
- 每块译文缓存在用户缓存目录，键只含端点与语言对，不含密钥和本地路径；Ctrl-C 之后重跑不重复请求，`--no-cache` 强制重新请求，`--out` 写出的文件与 stdout 逐字节一致。
- 对齐质量在 `alignment` 中如实标注：`verified` 是有逐句句对的块，`fallback` 表示整块一对、未拆句。译文里出现 `Mr.`、`3.5` 这类句点歧义导致两侧句数不等时，该块会降级为 `fallback`——宁可整块不拆，也不给出可能错位的句对。

翻译支持流式 UTF-8、LF/CRLF 事件、错误事件与提前断流检测；连接超时 10 秒、读取超时 30 秒、单段总时限 180 秒。`--chunk` 按 Unicode 字符计数。发音校验参数和音频帧，只有收到完成消息才保存；连接/读取/总时限分别为 10/30/120 秒。无 `--out` 时音频缓存在用户缓存目录，并交给系统默认播放器；无音频关联或无桌面的系统可能无法自动播放，可用 `--out` 导出。

本机 2026-10-20 实测：Edge TTS WebSocket 返回 `HTTP 403 Forbidden`（Microsoft 端点拒绝，非本地词库或参数问题）。默认的 `--engine auto` 会在这种情况下自动改道有道（文本不超过 1000 字符时），对 `abandon` 成功导出 11,949 B、64 kbps / 48 kHz 单声道 MP3；实际用过的后端写在文本输出和 `--json` 的 `source` 里。要强制单个后端，写 `--engine edge` 或 `--engine youdao`，此时失败不再改道。

### 提示词、风格与术语

`--engine llm` 的措辞由 `--to` 决定，不再是写死的"English-to-Chinese"。这条以前是**缺陷**：`--to fr` 会被收下，然后 system、单条 user、批量 JSON 数组三处提示词一起把目标写成简体中文，端点照中文返回。现在三处都用同一个目标语言，语言代码→人类名称用一张表（`zh-Hans`/`zh-Hant`/`en`/`fr`/`de`/`es`/`it`/`ru`/`ja`/`ko`）；**表外但形状合法的代码按原样写进提示词**（`--to mi` 就要求译成 `mi`），因为悄悄退回中文比认不出名字更难查。`--from auto` 时提示词写"源语言不声明，由你判断"，不假装源语言是英语。

- `--style-file <模板文件>` 用你自己的措辞替换内置用户消息，支持的变量与欧陆模板同一套：`{{text}}`、`{{src_lang}}`、`{{src_lang_cn}}`、`{{target_lang}}`、`{{target_lang_cn}}`、`{{context}}`、`{{summary}}`、`{{prev_output}}`、`{{count}}`。载入即校验：未知变量、缺 `{{text}}`、空模板、花括号不配对都是参数错误（退出码 2，一个请求都不发）；没赋值的变量渲染成空串，不留花括号。这实现的是逆向结论"**风格 = 可编辑的提示词模板**"；欧陆服务端的 `styleId 1-6` 我们没有复刻，那需要欧陆账号（`api.frdic.com` 探测返回 401）。
- `--glossary-auto [strong|some]`（默认 `strong`）从原文探测词库收录的**短语**，把它们卡里的首条中文释义注入提示词，用来保证术语一致。它只吃自证词头：`unevidenced` 那 39,557 条记录的释义本来就是隔壁词条的词，自动钉进提示词等于把错译法固化。单词不入围（只试 2–5 词 n-gram），因为一个单词的第一释义经常不是这里要的那个意思。手工 `--glossary` 的同行优先，自动表只补空位，总量仍受 2,000 行限制。词库没有 `key_evidence` 表（即 `--key-evidence all` 构建的库）时报错退出码 1 并给出重建命令，**不**退化成"任意键都算术语"。
- 分段翻译时，从第二段起提示词带上**上一段译文的尾部**（与 `--summary` 同一个 400 字符预算），对应欧陆模板的 `prev_output` 变量——它用来保证跨段连贯。首段没有上一段，所以整行不出现，不给空占位；`--no-prev-output` 关掉。`--summary` 与 `RollingSummary` 照旧独立工作。
- 模板、术语表和词库读取全在本地，只有最终的翻译请求走网络。

### 上下文讲解

`zedict explain <词>` 让大模型讲一个词，但依据只来自本地词库：查询走 `Auto` 路由（精确 → 词形归并 → 释义反查），把命中的义项、音标、音节、词形、近反义词和例句拼成 `[本地词库]` 资料块注入提示词。模型不得引入列表之外的义项，词库里没有的含义必须回答“本地词库未收录该义项”。

- 必须配密钥（`--api-key` 或 `LLM_API_KEY`）；没配就报参数错误并指向 `zedict query`，不会静默退化成离线查询。
- `--style definition|context|examples` 换任务描述；`--in-context "整句"` 让模型判断用的是哪一条义项（上限 2000 字符，空白或超长是参数错误）；`--cefr a1..c2` 只控制**讲解用词的难易**，词库没有逐词 CEFR 标注，它不是数据标签。
- `--to <语言代码>`（默认 `zh-Hans`）决定**用哪种语言讲解**，提示词头两行 `[src_lang]` / `[target_lang]` 跟着它走。以前这里写死 `[target_lang] zh-CN`，于是 `explain --to fr` 会一边收下 `fr` 一边用中文讲。非法代码是参数错误（退出码 2）。
- 词库未收录该词时照常输出，退出码仍是 0，但 stderr 提示“本次讲解不基于词库释义”，`--json` 的 `grounded` 为 `false`。
- `--json` 字段为 `word/grounded/headword/match_type/senses/style/cefr/explanation/engine/model/cache`；`headword` 是命中词卡的词头：有独立词卡时就是原词（`running`），只有词形归并时才落到原形（`ran` → `run`）；`senses` 是实际送进提示词的义项条数。
- 缓存键含词、风格、等级、语境、**输出语言**、命中词头与模型名，不含密钥也不含端点；同一请求第二次不发网络请求（`cache: true`），`--no-cache` 强制重新生成。输出语言必须在键里，否则 `--to fr` 会读到上一次中文讲解的缓存。

### 发音后端与自动兜底

`zedict speak` 默认 `--engine auto`：先走 Edge TTS（`--voice/--rate` 可调，默认 `en-US-AriaNeural` / `+0%`）；握手或合成失败时，若文本不超过 1000 字符，自动改用有道 `dict.youdao.com/dictvoice`（`--accent us|uk`，默认 `us`），并在 stderr 说明改道。实际用过的后端出现在文本输出与 `--json` 的 `source` 中，缓存也按实际后端分别存放，不会把有道的音频记到 Edge 的键下。

- `--engine youdao` 走免密钥 GET，`--accent us|uk` 选美音或英音（默认 `us`），上限 1000 字符，只适合单词和短语。
- `--engine edge` / `--engine youdao` 是强制指定，失败不再改道。强制 Edge 时 `--accent` 越界，强制 youdao 时 `--voice/--rate` 越界，都按参数错误处理（退出码 2）——静默忽略比报错更难查。
- 超过 1000 字符的文本即使 Edge 失败也不会发给有道，错误信息会说明兜底不可用。
- 响应必须嗅探出 ID3 标签或 MPEG 帧头才落盘，网页、空响应、404 一律退出码 1，`--out` 不会留下半个文件。
- 音频按后端分别缓存：Edge 沿用历史的 `(文本, 语音, 语速)` 键形状，换后端不会作废用户已经攒下的缓存；有道另用 `(youdao, 文本, 口音)`。
- `ZEDICT_EDGE_TTS_URL` 可覆盖 Edge WebSocket 端点（必须以 `ws://` 或 `wss://` 开头），`ZEDICT_YOUDAO_URL` 可覆盖有道端点（必须以 `http://` 或 `https://` 开头），用于自建代理与离线测试。

可选 HTTP 兼容接口：

```sh
cargo build --locked --release --features server
zedict serve --addr 127.0.0.1:8686
```

保留 `/api/word/{word}`、`/api/search`、`/api/random`、`/api/translate`；词条返回词卡数组，prefix/contains 返回词头数组，fuzzy 返回 `[词头, 距离]` 数组，翻译返回 `{"translation":"..."}`。服务器仅绑定 loopback；翻译须显式 `--llm`，不默认允许跨站请求。新 CLI JSON envelope 不替代既有 HTTP 返回格式。

## 开发与验证

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo test --locked --no-default-features
python -m unittest discover -s tests
cargo build --locked --release
cargo test --release --locked --all-features --test local_data -- --ignored
python tools/validate_import.py data/dict_full.jsonl store/dict.sqlite --corrections reviews/local.manifest.json --out store/import-validation.json
python tools/benchmark.py target/release/zedict.exe store/dict.sqlite
python tools/benchmark_fuzzy.py target/release/zedict.exe --plain <不带 --spell 的库> --spell <带 --spell 的库> --timing
python tools/package.py
```

一般测试使用合成小词库和本地模拟服务，不需要私有资料、真实密钥或外网。`local_data` 是显式运行的 375 个源资料映射回归，另含已知错配、词形通路、词级表入库条数、拼写建议排序、例句索引的行数／链数／去噪结果，"卡内例句与索引两侧键永不分叉"的全库断言，以及一次**真语料三档构建**的词头档位普查（`all`/`some`/`strong` 各 184,093 条归档、52,347／18,065／39,557 三档、70,412 与 52,347 张词卡，和 `angle`/`bomb`/`promotion` 净化前后的行数）。这一条要读 `data/dict_full.jsonl` 并建三次库，debug 下慢到没法等人，所以要带 `--release` 显式运行（release 下一分钟出头）：本文件里的数字就是它断言的数字，改了语料必须重跑。基准脚本记录文件缓存预热后的新进程 P50/P95，不把它称为磁盘冷启动性能；`tools/benchmark_fuzzy.py` 要两个只差 `--spell` 的库做对照，把 recall@1/3/5/10、体积差和延迟一起打出来，并把"目标词根本没有词卡"的错词单列——那是覆盖率问题，不是候选生成问题，混在一起读数就没意义。

已配置 Windows x64、macOS ARM64、Linux x64 CI。当前工作环境实际执行的是 Windows 验证；其他平台须由 CI 运行确认。打包脚本仅收集可执行程序和说明资料，生成归档与 SHA-256，不打包原始词库。推 `v*` 标签会触发 release workflow，把三个平台的压缩包和校验和发布到 GitHub Release；词库需要使用者按上面的步骤在本地构建，或从自己获准存放的位置取得。

## 代码结构

`src/lib.rs` 只导出六个顶层模块，依赖方向由 `cli` 和 `app` 指向领域层，领域层不反向依赖界面：

| 模块 | 职责 |
| --- | --- |
| `src/cli/` | 参数定义、命令派发（`mod.rs`）、查询输出（`query.rs`）与输入/导出/联网辅助（`support.rs`） |
| `src/app/` | 配置、HTTP 兼容服务、联网缓存、学习库与讲解提示词 |
| `src/core/` | 词卡模型、词头自证、分词与词级、繁简折叠 |
| `src/data/` | 全量导入、词库编译、只读 SQLite 查询 |
| `src/net/` | Edge 翻译、LLM、HTTP、逐句对齐与 TTS |
| `src/render/` | 终端词卡、CSV/TSV 导出与 Anki `.apkg` |

## 实现边界

- `src/data/ingest.rs`：结构化全量导入、可选修正、原始数据归档、事务构建与原子替换。
- `src/data/store.rs`：只读 SQLite、FTS5、词形关系和统一查询模型。
- `src/cli/` / `src/app/config.rs` / `src/render/`：命令、路径和完整词卡。
- `src/data/compile.rs` / `src/core/model.rs`：结构化清洗及有界压缩解压。
- `src/core/evidence.rs`：词头自证分级（例句高亮词 / 自己释义里出现 / 无自证），只用已有的例句扫描器和 `data::store::normalize`，不引入第二套解析。
- `src/net/align.rs` / `src/net/edge.rs` / `src/net/llm.rs` / `src/net/http.rs` / `src/app/cache.rs`：逐句对齐纯逻辑、Edge 与 LLM 端点、共享 HTTP 客户端与联网结果缓存。联网结果只落用户缓存目录，永不写入词库。
- `src/app/explain.rs`：把本地词卡拼成提示词资料块，并硬性要求“词库没有的义项就回答未收录”。
- `src/net/tts.rs`：Edge TTS 与有道兜底两个后端、MP3 嗅探、按后端分键的音频缓存。
- `src/core/lexicon.rs`：分词、按词级表判档、`--include-phrase` 的短语合并和标注回填，纯本地不联网。
- `src/render/export.rs`：Anki 的 `#separator` 表头与 CSV/TSV 两种转义规则。
- `src/app/study.rs` / `src/render/anki.rs`：独立学习库（生词本、复习计划、评分日志）与不依赖第三方库的 `.apkg` 写出器（SQLite schema ver 11 + stored ZIP，内置 SHA-1/CRC-32 并配标准向量）。学习状态只写 `study.sqlite`，词库始终只读。
- `tools/eudic_extract.py` / `tools/eudic_phrase.py` / `tools/eudic_merge.py`：只读解出 `combined.bin` 与 `phrase.bin`，按精确索引校验词头/正文区间，再把两份词头合并成 zedict 的 JSONL。三者都是构建资料，不是运行时依赖。
- `tools/word_levels.py`：从本机欧陆的 `dic/level.bin` 导出 `data/word_levels.jsonl`（词形、难度档、大纲标签，外加一份档与标签的自述 `meta` 行）。它是构建资料，不是运行时依赖：词库建好之后不需要欧陆也不需要 Python。
- `tools/inflect_aliases.py`：屈折词形表生成器，随仓库维护；输入是本地 Hunspell 资料与正文语料。
- `tools/variant_dump.py` / `src/core/variant/`：繁体→简体折叠表的生成器与生成结果。生成器读本机 OpenCC 的 `TSCharacters.txt`（Apache-2.0，4,113 行取 4,105 对单字对应）和 `TSPhrases.txt`（Apache-2.0，277 行全收），生成结果是随仓库维护的两张常量表；构建词库不需要 Python，也不需要 OpenCC。`--stats` 会报出词组表相对单字表的净增益。
- `tools/dict_shards.py`：JSONL 词库的分片与校验合并工具，按完整行切分并核对每片字节数、行数和 SHA-256；它只解决传输体积，不改变词库的再分发授权。
- 历史上位于 `src/legacy/`、`src/aux.rs`、`src/bin/auxcheck.rs` 的代码和早期 `tools/` 实验脚本都已归档到本地 `research/`（被 `.gitignore` 排除），保留作研究资料，不进入产品运行入口，也不会随仓库上传。`src/` 只保留实际参与构建的模块，`src/bin/` 只保留 `zedict.rs`；`tools/` 只保留 README、测试和 CI 引用的生产脚本。不要直接运行带有历史写死路径的实验脚本。拼音检索走的是 `--pinyin` 从词卡 `pinyin` 字段派生的 `pinyin` 表，不读 `py.bin`，也不依赖早期分组代码。

`--engine edge` 走的是微软 `edge.microsoft.com/translate/translatetext`：**非公开接口，随时可能变更或限流**，401/403、格式变更、句长不自洽都会给出可执行提示（改用 `--engine llm` 或调小 `--batch-units`）。实测该端点的 `transSentLen` 会把句子边界算错——zh→en 会把下一句的首字母算进上一句，en→zh 会返回零长句 `[6, 8, 0]`，两种情况的累加仍等于译文长度，所以按它切片会静默错位。本项目只用 `srcSentLen` 切原文，译文按自身句末标点切，两侧句数不等即整块降级为 `fallback`；这是现有的唯一防错配手段，不做“看起来对”的猜测。

`--engine youdao` 用的 `dict.youdao.com/dictvoice` 同样是**未公开文档的第三方端点**：没有密钥也就没有配额与承诺，随时可能限流或改行为。更要紧的是它把**原文放进查询串**（`?audio=<文本>&type=1`），链路上的中间环节都可能留档，因此它只作为 Edge 失败后的兜底（`auto` 只对 ≤1000 字符自动改道，也可用 `--engine youdao` 显式指定），默认仍是 Edge TTS；整段文本不该往这个后端送（1000 字符上限也是为此）。可用 `ZEDICT_YOUDAO_URL` 指向自建端点，值必须以 `http://` 或 `https://` 开头。`explain` 会把本地释义片段与 `--in-context` 句子发给所配置的大模型端点——这是它工作的方式，不是疏漏；不希望外发就不要配密钥，改用 `zedict query --no-en-zh`。`query` 的英英翻译（交互终端自动，显式 `--en-zh` 强制）同样会把英英释义与例句文本发给所配置的大模型端点或 Edge 翻译端点；不希望外发就加 `--no-en-zh` 或配置 `en_zh = false`，管道、`--stdin`、`--json` 默认不联网翻译。

繁体折叠分两层：**先试词组表，命中就整段跳过；剩下的逐字折单字表**。两张表都来自 OpenCC：`TSCharacters.txt`（4,113 行取 4,105 对单字对应）与 `TSPhrases.txt`（277 行全收，键长 2–14 字）。词组表不是锦上添花：216 对非恒等词组里有 **205 对单字表逐字折给不出**（`一目瞭然→一目了然`——`瞭` 在字符表里排最前的候选还是 `瞭`；`上鍊→上链`——逐字折会得到 `上炼`；`乾坤一擲→乾坤一掷`——逐字折会得到 `干坤一掷`），61 对恒等词组里有 **53 对会被单字表折坏**（`乾元→乾元`，逐字折变成 `干元`；`哪吒`、`宫商角徵羽` 这类专名也在其中）。所以词组表**保留恒等行**、单字表**丢弃恒等行**。两个数字由 `python tools/variant_dump.py --stats` 现算。多候选的取舍照旧：4,113 行里 56 行、277 行里 4 行给了多个简体候选（`乾→干／乾`、`剋→克／剋`），一律取最前的一个，不做取舍；被丢掉的候选不是错，只是没有依据判定查询想要哪一个。反方向（简体→繁体）天然是一对多（`干←幹／乾`），本表不提供，简体查询也不会返回繁体词条；繁简之外的异体字也不转。折叠只作用于查询串：`normalize()` 定义库里的键，一个字节都没改，旧词库不需要重建，新旧结果也不会因为这两张表而不同。

词级数据只有**档号和大纲标签**两样，没有逐词排名：`level.bin` 里每个档就是一份按字母序排好的词形清单，1 档装了 12,521 个词形。所以拼写建议在"同距离、同档"时仍然退回长度差＋字母序——`worl` 给出 `word/wore/work/worn/world`，`teh` 给出 `tel/ten/the/tech`。这是数据粒度限制，不是排序疏漏：逐词名次要另找资料，本项目不猜。写到一半的截断型输入本来就该用 `--mode prefix`。

拼写候选另有一张删字符索引表（`spell_key`），由 `zedict compile --spell` 建，**只作补位**：`query <词> --mode fuzzy` 的三档（词头、屈折词形、词级）都扫完还没排满 `--limit`，才把索引里的候选接在已排序结果之后，所以它按构造不会挤掉任何原有候选。实测代价（旧 `dict_full.jsonl` 完整词库，两次只差 `--spell` 的构建；新通路仅 combined 时 `spell_rows=3,501,820`，并入 phrase 后 `5,062,201`）：

- 体积：279,146,496 B → 317,259,776 B，**+38,113,280 B（36.4 MiB，比不带它的库大 13.7%）**；构建多约 3 秒。
- 内容：旧通路 69,583 个词头、861,626 行删字符邻域（69,608 个词头里 25 个单字词不进表——删完就空，没有候选可生成）；新通路仅 combined 时 3,501,820 行、覆盖 421,836 个词头，并入 phrase 后 5,062,201 行、覆盖 515,829 个词头（`datasets.spell.words`）。
- 延迟：`tools/benchmark_fuzzy.py --timing`，500 个随机真词（去预热一次后 n=499）P50 64.7→65.5 ms、P95 81.3→82.3 ms；`--length 4`（补位唯一可能生效的区间，n=199）P50 28.0→28.1 ms、P95 36.4→33.2 ms。
- 召回：30 个经典错词的 recall@1/3/5/10 = 20/22/23/25，**开关前后完全相同（Δ=0）**。这是结构性的，不是实现没做到：主扫描已经覆盖距离 ≤ `max` 的全部词头，而"同删一个字符"的两个词彼此只差 ≤2，所以索引唯一能带来的是**短词的距离 2 候选**——`thsi→thin/thus/thir`、`ctx→cat` 这类；4 字以下查询在 `max=1` 里本来就扫不到它们，5 字以上查询的 `max=2` 又已经把它们覆盖了。

那 5 个连 `@10` 都进不去的错词（`recieve/acheive/embarass/harrass/recomend`）也不是候选生成的问题：`receive`、`achieve`、`embarrass`、`harass`、`recommend` **本身就不在词库里**（`not_found`），见下文词头证据分级。补位表修不了它，任何索引都修不了。

例句索引是主源那一趟生成的侧表，不引入新输入文件：`--examples` 从 `dict_full.jsonl` 已有的 `examples` 字段推出 **64,253 行句对 / 511,574 条链**（英文关键词 168,350、中文关键词 142,803、所属词头 200,421；去噪前 513,599），去噪剔掉 4 个中文高频键及其 2,025 条链（阈值 300 例句，最高的键单独挂着 816 句）。卡内例句的字节按构造不变——`parse_examples` 一行没改，两侧键用的是**同一个** `RegexLite::extract_keys`，`local_data` 在全库 232,657 条例例上断言"两半拼接 == 整段扫描"，实测 0 例外。同一份输入、同一组可选资料只多一个 `--examples` 时，库文件从 247,607,296 B 变成 279,146,496 B（+31,539,200 B ≈ 30 MiB，+12.7%）。新通路的构建报告对应 `examples=64,403`、`example_links=545,943`、`dropped_noisy_keys=4`。`meta('report')` 因此多了 `examples` / `example_links` / `dropped_noisy_keys` / `spell_rows` 四个数（都带 `#[serde(default)]`，旧库读出来是 0）：这是**预期的**报告变化，已发布的 0.3.0 产物里没有它们，`tools/validate_import.py` 只比对它关心的字段，不受影响。

两条检索通路的实测延迟（新进程、文件缓存预热、每组 30 次）：精确键命中 min/P50/P95 = 15.4/16.0/17.4 ms；全文扫描 = 78.9/80.5–85.8/89.3–92.2 ms（两句不同长度的英文整句）。没有用 FTS5 trigram：中文关键词按例句数中位长度只有 2 字，trigram 要 ≥3 字才命中，主用法直接废掉；而正文一共 6,299,712 B（英文 4,866,344 + 中文 1,433,368），全扫不过百毫秒。扫描**完全不用 `LIKE`**，走 `instr()`，因此没有 `%`/`_`/`\` 转义面。两个已知限制：全文扫描比对时，查询串走 Rust 的 Unicode 小写（`normalize()`），库内英文半区只用 SQLite 的 ASCII `lower()`，所以非 ASCII 字母的大小写变体（`İ`、希腊文 Σ/σ 这类）对不上；`examples` 的查询串**不接繁简折叠**（那张表只接在 `reverse()` 上），繁体查例句得先自己转成简体。`query --full` 末尾的例句反查段是增益，缺索引时静默不出现，不报错。

欧陆词典的 4 个 `.bin` 容器格式本轮已经解出，头部、页表和页流在四个文件上都精确吻合（`combined.bin` 页流之后还有下面的索引、词头 store 和辅助词头表）：

| 字段 | 含义 | combined / treedata / phrase / py 实测 |
| --- | --- | --- |
| `u16@0` | magic | 全是 `0x1156`（`reftree.bin` 是另一种：`0x3317` + `u32@2` 版本号） |
| `u32@2` | 头部长度 | 176 / 128 / 144 / 96 |
| `u32@6` | **正文起始偏移**，不是条目数 | 98,747 / 3,851 / 3,323 / 1,163，四处都精确等于第一个 zlib 流的起点 |
| `[u32@2, +1042)` | 高熵区 | 四个文件**都是 1,042 字节**，熵 7.78–7.85，两两只有 ≤10 字节相同 |
| 其后 `8×页数` | **明文 u64 LE 页偏移表**，严格递增，首项 = `u32@6` | 页数 12,191 / 335 / 267 / 3 |
| 其后 1 字节 | 结束标记 | 四处都是 `0x01` |
| 再往后 | 页流：每页一个独立 zlib 流 | 解出恰好 16,384 B；`treedata` 335×16384 = 5,488,640，与现有 `data/treedata.inflated` 逐字节等长 |

**第七轮修正**：上一轮说“没有条目索引”只解开了 `combined.bin` 的页流就下了结论。页流后面还有一段明文精确索引和一个词头 store，只是不在 16 KiB 页里：

| 区域 | 位置 / 大小 | 内容 |
| --- | --- | --- |
| 容器头 + 页偏移表 | 1,218 起 | 12,191 个 u64 LE；首项 98,747，末项 69,124,866 |
| zlib 页流 | 98,747–69,126,817 | 12,191 页，每页解出恰好 16,384 B，合计 199,737,344 B |
| 条目索引 | 69,126,817 起，428,682 × 16 B | `(u32 a, u32 b, u32 c, u32 d)`，`c` 恒为 0，`d` 12–21,884 |
| 词头 store | 索引之后 | 20 B store 头 + 12 B `"3G\0"` 前置 + 14,784,127 B 词头/短释义 |
| 辅助词头表 | 90,769,876 起到文件末尾，6,404,633 B | 2,799 B 前缀 + 428,682 行 `[u32 id][折叠词头 NUL]` + 61,926 B 后缀 |

`(b,d)` 排序后从 0 无缝铺满 199,729,192 B 正文，尾部 8,152 B 是 0；每个切片都以 `<FC>` 开头（428,682/428,682）。英语行 0–268,833 的 `a` 指向 store 里 NUL 结尾的词头（跳过 `<FORVO/>`/`<SYL>` 前缀），后面两个 u32 的第二个恒等于行号，再往后是 NUL 结尾的短释义；268,834 个词头全部唯一。汉语行 268,834–428,681 的 store 区是一段连续 UTF-8 词头块，`word = buf[a_k : a_{k+1}]`，末条到 `len(buf)-12`；159,848 个词头全部合法且唯一，末尾 12 B 页脚不属于词头。

辅助词头表按检索折叠键排序：小写并去掉变音符号（`cafetière→cafetiere`、`chungch'üan→chungch'uan`），每行是顺序 id 0–428,681 加 NUL 结尾字符串。按同一规则折叠后与提取结果对比：该表没有而提取有的键只有 `3G`，提取没有而该表有的键为 0——428,682 条词头在另一条独立路径上得到全量印证。两侧 2,799 B 与 61,926 B 是尚未完全解出的检索辅助数据；词条正文、词头、音标、例句都不依赖它们，提取器不读。

`tools/eudic_extract.py` 按这张表逐行解码，不猜测词头、不做启发式切分：

```sh
python3 tools/eudic_extract.py \
  --source /path/to/eudic/dic/combined.bin \
  --out data/eudic --merge
```

实测产出 22 个分片（每片 20,000 条，末片 8,682）、`all.jsonl` 279,262,590 B、`related.tsv` 24,557 行，`manifest.json` 保存源文件 SHA-256、每片 SHA-256 与偏移。检查计数：重复词头 0、非 `<FC>` 开头 0、带音标 141,536、带例句 65,095、带近反/联想 82,962、带短语 15,832；182 条空义项全部是**只有拼音**的汉语条目（源文件本身没有释义，例如“鹌”只有 `ān`），不是解析丢失。

`phrase.bin` 用同一族容器但布局不同：页表在 1,186（267 个 u64，首项 3,323），页流 3,323–1,482,590 解出 4,369,855 B；1,482,590 起是 111,302 × 16 B 记录表 `(headword_off, text_off, 0, len)`，3,263,422 处的 u64 给出词头缓冲 1,730,449 B，词头缓冲 3,263,430–4,993,879，末尾 702 × u32。记录表的 `(text_off, len)` 无缝铺满正文，111,302 个词头全部唯一。`tools/eudic_phrase.py` 逐条校验这些不变量，把正文拆成 112,494 个义项（0 条空义项、0 条残留标签）：`<div class="exp">` 是主释义，`<br>` 分隔补充义项，`<ol><li>` 逐条拆分，`<i>` 里的 POS 标记保留，而 666 个与词头仅大小写不同的展示形不当作译文；唯一一处字形占位符 `<&5&>` 由 `abductor hallucis` 的上下文确定为“拇”。产出 `data/eudic-phrase/phrase.jsonl` 9,039,927 B，检查计数：带 POS 8,774、带地域标签 6,066、带列表义项 339、规范化碰撞 0。

`tools/eudic_merge.py` 按 `store::normalize` 的同一规则合并两份词头：17,309 个键与 combined 重叠（对应 17,320 张卡），其中 15,695 个键的义项已完全覆盖，17,063 条重复义项被跳过；822 张卡补入 874 条更完整的义项。其余 93,993 条成为新词卡（94,568 条义项），合并结果 522,675 条、285,595,157 B，规范化碰撞仍是原来的 14 个，未匹配短语键 0。

把合并文件编译成 `data/eudic-store-phrase/dict.sqlite` 的实测报告：522,675 张词卡、522,661 个规范化词头、615,703 个义项，缺少词头 0 条、非法词头 0 条、未解析记录 0 条；例句 64,403 行 / 545,943 条链、词形关系 17,366 条（7 条未命中原形）、近义/反义/联想中文 746,205 条、词级 72,970 行、拼写补位 5,062,201 行（覆盖 515,829 个词头）、拼音 159,848 条读音键（159,846 张词卡、136,835 个 flat 键）、参考词表 68,089 行，`dropped_unevidenced_cards=0`。库文件 841,277,440 B。抽查：`呕`、`酢浆草科` 走 Auto 先命中汉语词头再补反查，`running`、`a cappella`、`take into account` 都是独立词卡；新增的 `aalenian stage`、`Aaron Kwok`、`abaft the beam`、`algorithm processor` 等 phrase-only 词卡可精确命中，`according to` 等重叠卡补入新义项，`郭富城`/`阿林阶` 能反查到对应词头；`examples`、`export`、`translate --engine edge` 均实测通过，发音见[可选联网功能](#可选联网功能)。仅 `combined.bin` 的构建（`data/eudic-store/dict.sqlite`，693,010,432 B）保留作对照。

第六轮的两条判断因此撤回：

| 旧结论 | 实测 |
| --- | --- |
| 正文里根本没有词头，也没有条目索引 | 膨胀页流里没有；页流之后的 6,858,912 B 索引和 14.8 MB store 里有全部 428,682 条词头 |
| 剩余 40% 词头本地不可恢复 | 全部可恢复；旧 `dict_full.jsonl` 的 74,124 条无词头、约三分之一错词头都是启发式提取的缺陷，不是源数据缺陷 |

下面“词头证据分级”只对旧 `data/dict_full.jsonl` 有意义；从精确索引通路（`combined.bin` + `phrase.bin`）构建不需要过滤，保留旧通路只是为了复现旧库和 375 条人工修正。

旧 `data/dict_full.jsonl` 的词头质量确实是那条通路的**头号缺陷**：它不是从精确索引读出来的，而是由例句高亮词、`<h4>`、`<SYN>` 自引用等启发式猜出来的。`syn_selfref` 的实测反例仍然成立——60,000 条记录样本里，`<SYN>` 第一个 `dic://` 链接与本条例句高亮词只有 **70 / 27,820 = 0.3%** 一致，所以旧文件里有大量错词头。这也是为什么新通路改用精确索引后，不应再把旧文件的统计当成源数据的事实。

旧文件仍有可复现价值：`data/dict_full.jsonl` 全部 109,969 条带词头的记录，按"这个词头有没有在本条记录里自证"分三档（`src/core/evidence.rs`，判据只用已有的例句扫描器和库里已有的 `normalize()`）：

| 档 | 判据 | 记录数 | 占比 |
| --- | --- | --- | --- |
| `example-key` | 词头 == 本记录某条例句**英文侧高亮词**（`<span class="key">`） | 52,347 | 47.6% |
| `in-gloss` | 词头作为**整词**出现在本记录自己的 `senses` 释义里 | 18,065 | 16.4% |
| `unevidenced` | 两者都没有——即 `syn_selfref`、派生字形 `forms` 值、`<h4>` 这几条规则的产物 | **39,557** | **36.0%** |

`unevidenced` 是**错的**，不是"没把握"：`angle` 那 25 条的第一条释义是 `气骨导间距`（A-B gap），词头取的是隔壁词条 `dic://angle`；`promotion` 的 8 条记录释义全是 `abbr. …` 子表；`bomb` 的 13 条 E 档记录释义是 `原子弹 / 速度特快的赛车`（真词头是 A-bomb）。`in-gloss` 只读 `senses`、**不读 `english`**，正是因为 `english` 描述的是隔壁词条。整词判据等价于 `(?<![a-z])词(?![a-z])`，所以单字母词头不会命中一切。

过滤发生在写 `entries` **之前**，代价（普查，未加 `--corrections`）：

| `--key-evidence` | 词卡 | 可检索词头 | 丢弃 | `angle` / `bomb` / `promotion` |
| --- | --- | --- | --- | --- |
| `all`（默认，行为与今天逐字节一致） | 109,969 | 69,803 | 0 | 28 / 15 / 8 |
| `some`（丢 `unevidenced`） | 70,412 | 62,499 | 39,557 | 3 / 2 / **0** |
| `strong`（只留 `example-key`） | 52,347 | 46,551 | 57,622 | 3 / 2 / 0 |

三档下 `source_records` 都归档全部 **184,093** 条原始记录，净化不伪装成"数据没了"；人工核验过的记录（`--corrections` 点名的 375 条）归入第四档 `reviewed`，在任何阈值下都入库，且不重复计入上面三档——所以上表是**不带 `--corrections`** 的普查数，带修正的构建会打印 52,216／17,822／39,556 加 375 条 `reviewed`，四个数相加仍是 109,969。`entries.id` 恒等于源行号，所以档位存成侧表 `key_evidence(line, tier)`，按键取最佳档一次 join 就够，不另存一份会漂移的词头级派生表。带 `--corrections` 的默认构建仍是 69,608 个词头 / 109,969 张词卡，表集合与每张表的行数和净化引入前完全相同——`all` 阈值下一个词都没多删、也没少删；唯一变长的是 `meta('report')` 里的档位普查字段。

`receive`、`achieve`、`embarrass`、`harass`、`recommend` 在**旧通路的**三档里都是 0——它们不在旧语料的任何自证位置；精确索引通路收录这些词头。**"字母序 + offset 单调递增 → 锚点插值指派"这条方向已撤回**：旧通路没有词头；第七轮已在页流之后找到精确索引与词头 store，不再需要插值猜测。`--vocab data/spell_words.txt` 能做的是把旧库的缺失说清楚：查询未命中而该词在参考词表里时，`zedict query` 在 stdout 追加一行"该词是常见英文词，但本词库未恢复到词头"，而不是留一个容易被读成"英语里没这个词"的 `not_found`。同一趟 `audit` 会把每条候选的档位写进队列（`key_tier`）并在 `report.json` 里给出档位普查和"只有 `unevidenced` 证据的词头数"。

本次没有接入第三方在线词库，也没有实现欧陆私有服务端（`api.frdic.com` 需账号，探测返回 401）；词级、屈折词形、繁简折叠三份资料都取自本机已有文件。没有上线 TUI、云同步或跨设备学习记录；本地生词本/复习计划与 `.apkg` 导出见“生词本、复习与 Anki .apkg”。自动校验验证结构、索引、解压及已知错配回归；不会把“结构合法”等同于源资料中每条释义均无误。

## 版本、贡献与许可

版本变化见 [CHANGELOG.md](CHANGELOG.md)。开发、测试与提交约定见 [CONTRIBUTING.md](CONTRIBUTING.md)；安全问题请按 [SECURITY.md](SECURITY.md) 私下报告。

代码以 **MIT 或 Apache-2.0** 双许可发布，任选其一，见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。仓库不包含任何欧陆词典词库内容；从本地资料构建出的词库能否再分发，取决于你取得资料时的授权。
