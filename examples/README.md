# 可运行示例

## 用 Vault 跟踪一次发布候选

[发布检查 Vault](vault/projects/mcbtodo-release/project.md) 取材于仓库实际使用的 Cargo 检查和归档命令。大纲展示 Project、Task 和 WorkUnit 的层级。任务图展示「检查工作区」如何解锁「检查 crate 归档」。两项检查均从待办开始，日期可以按自己的发布计划设置。

在仓库根目录验证样例并查看大纲和任务图。

```sh
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai outline
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai graph
```

在 Linux 上，从当前 Git 提交取出一份可编辑副本，并在临时 home 启动 TUI。

```sh
cargo build --locked -p mcbtodo
demo_home="$(mktemp -d)"
git archive HEAD examples/vault | tar -xf - -C "$demo_home"
demo_vault="$demo_home/examples/vault"
env HOME="$demo_home" target/debug/mcbtodo init
env HOME="$demo_home" target/debug/mcbtodo --vault "$demo_vault" \
  --timezone Asia/Shanghai tui
```

1. 用 `:view outline` 查看发布项目，用 `:view graph` 查看依赖。「检查 crate 归档」此时处于阻塞状态。
2. 在仓库根目录运行 `cargo xtask check`。检查通过后，退出 TUI，在同一终端记录「检查工作区」完成。

   ```sh
   env HOME="$demo_home" target/debug/mcbtodo --vault "$demo_vault" \
     --timezone Asia/Shanghai vault complete --unit bb50ab51-4a8b-4545-8550-a1884a7347a9
   ```

3. 再次打开 TUI，进入 `:view graph`。「检查 crate 归档」现在可开始；打开该 WorkUnit 的 Markdown 可查看具体命令。也可以在大纲选中该项，按 `Space d` 为实际发布计划设置截止日期。

## 用 Workflow 路由测试结果

从 [页表检查工作流](workflow/README.md) 开始。它用仓库中的 Rust 程序执行页表检查，并演示报告、人工审批及脚本错误分支。
