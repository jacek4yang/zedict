# 贡献指南

感谢参与 ZEDICT。提交改动前请先确认以下约定。

## 开发环境

- Rust 1.98.1（由 `rust-toolchain.toml` 固定），需要 `rustfmt` 与 `clippy`。
- Python 3.12，仅用于构建与测试脚本；运行时不需要 Python。
- SQLite 随程序编译，不需要单独安装。

```sh
cargo build --locked --release
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo test --locked --no-default-features
python -m unittest discover -s tests
```

需要真实词库的 `local_data` 测试标记为 `#[ignore]`，普通测试只使用合成词库和本地模拟服务，不访问外网：

```sh
cargo test --release --locked --all-features --test local_data -- --ignored
```

## 代码组织

- `src/lib.rs` 只导出 `cli`、`app`、`core`、`data`、`net`、`render` 六个顶层模块。
- 依赖方向从界面层（`cli`、`app`）指向领域层；`core`、`data`、`net`、`render` 不反向依赖 `cli`。
- 新逻辑放进对应领域模块，CLI 层只做参数解析、派发和输出编排。
- 不新增依赖或 `[[bin]]` 前请先说明理由；`SCHEMA_VERSION` 与 `QueryResponse` JSON schema 的兼容性属于发布契约。

## 数据与隐私

以下内容**不要**提交到仓库或附在 issue 中：

- `data/`、`store/`、`out/`、`extracted/` 下的原始词库、构建产物和用户缓存（已在 `.gitignore` 中排除）。
- 欧陆词典词库正文、释义、例句或任何可再分发的派生数据。
- API key、token、Cookie、个人路径和含原文的联网请求日志。

测试需要语料时请使用合成数据或本地小型 fixture。新增联网能力必须能用本地模拟服务测试，且默认不联网。

## 提交与 Pull Request

1. 从 `main` 创建分支，提交信息用一句话说明“为什么改”，不要只写“update”。
2. PR 描述包含变更动机、验证命令和结果；涉及输出格式时附上前后对比。
3. 确保 `cargo fmt`、`clippy`、全部测试与 Python 测试通过。
4. 变更用户可见行为时同步更新 `README.md`；新增版本条目时更新 `CHANGELOG.md`。
5. 一个 PR 只处理一件事，避免混入无关格式化或重构。

安全问题不要开公开 issue，请按 [SECURITY.md](SECURITY.md) 报告。

## 许可

提交即表示你同意以 MIT 或 Apache-2.0 双许可发布你的贡献。请只提交你有权贡献的代码与资料。
