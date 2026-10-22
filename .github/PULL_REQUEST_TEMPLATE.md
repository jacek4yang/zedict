## 变更

<!-- 说明为什么要改，以及用户可见的行为变化。 -->

## 验证

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --all-targets --all-features -- -D warnings`
- [ ] `cargo test --locked --all-features`
- [ ] `cargo test --locked --no-default-features`
- [ ] `python -m unittest discover -s tests`
- [ ] 涉及真实语料时已显式运行 `local_data` 测试并记录结果

## 数据与许可

- [ ] 未提交 `data/`、`store/`、`out/`、`extracted/`、词库正文、密钥或个人信息
- [ ] 变更用户可见行为时已更新 `README.md`，必要时更新 `CHANGELOG.md`
- [ ] 新增依赖或改变 CLI/JSON 兼容性已在描述中说明
