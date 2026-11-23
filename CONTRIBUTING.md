# 贡献指南

本项目欢迎可审查、范围清楚的改进。项目由维护者原创编写，并按根目录
`LICENSE` 中的 Apache-2.0 许可证发布；不要求维护者提供所有权证明、
权利声明或私密文件。

## 提交前

1. 阅读 [项目说明](README.md)、[设计约束](docs/design.md) 和
   [开发工作流](.agents/workflows/develop.md)。架构边界变更还需遵循
   [架构工作流](.agents/workflows/architecture.md)。
2. 将改动限制在一个可描述的行为或问题中；说明用户场景、验证方法和
   未覆盖的风险。
3. 如果改动引入第三方代码、文档、测试夹具、图片或生成资产，列出其
   来源、许可证和所需署名/通知。来源或许可不明确时，不要将材料纳入
   仓库。
4. `examples/vault/` 中受 Git 跟踪的内容是供公开使用的合成夹具。
   需要复现问题或演示可写操作时，使用临时目录中的合成数据副本。
   不要将真实 Vault、备份、凭据、本地配置或运行状态放入提交、PR
   或附件。该目录中新建的文件默认被 Git 忽略。经审阅确需添加公开
   样例时，用 `git add -f -- <路径>` 逐个暂存。

## Pull Request

- 填写仓库 PR 模板，并说明改动范围、测试及是否引入第三方材料。
- 第三方材料的许可或通知问题未解决时，不将该材料纳入公开发布范围。
- 贡献按项目 Apache-2.0 许可证的条款提交和分发；不要求单独签署所有权
  证明或提交私密法律文件。
- 可复现的一般缺陷请使用 GitHub Bug report 表单；不要在公开 Issue 中提交
  凭据、私人 Vault 数据或漏洞细节。漏洞披露渠道与当前状态见 [SECURITY.md](SECURITY.md)。

许可与第三方材料规则见[许可与第三方材料](docs/licensing.md)。
发布准备另按[发布工作流](.agents/workflows/release.md)执行。

## 本地验证

```sh
cargo xtask check
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate
```

根据变更范围，再运行相关集成测试、示例、`cargo xtask smoke` 或
`cargo xtask smoke --sandbox`。说明未运行的检查及原因。Python 与
JavaScript/TypeScript 工具规则见 [.agents/instructions.md](.agents/instructions.md)。
