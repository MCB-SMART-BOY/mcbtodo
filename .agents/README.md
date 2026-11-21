# 项目 AI 工作流

`.agents/registry.toml` 声明可用的 harness、workflow 与各自的 Markdown 来源。`AGENTS.md` 是从仓库根目录进入项目约定的入口。

```sh
cargo xtask agent prepare
cargo xtask agent prepare --harness codex --workflow review --offline
cargo xtask agent prepare --workflow architecture --offline
cargo xtask agent prepare --workflow contribution --offline
cargo xtask agent run --workflow develop '为日期投影补充一个闰日测试'
cargo xtask agent prepare --workflow documentation --offline
cargo xtask agent prepare --workflow release --offline
```

- Harness 依次来自显式 `--harness`、`MCBTODO_HARNESS`、当前会话标记、`PATH` 中的客户端和 `generic`。自动探测顺序由 `auto_order` 管理。`prepare` 可以为尚未安装的客户端准备上下文；`run` 自动探测时跳过未安装的客户端，显式指定的客户端不可用时会报错。xtask 不会安装或下载 harness 客户端。
- Workflow 依次来自 `--workflow`、`MCBTODO_WORKFLOW` 和注册表中的 `default_workflow`。
- Harness 操作约定见注册表引用的指南：
  - [`codex`](harness/codex.md)
  - [`claude`](harness/claude.md)
  - [`opencode`](harness/opencode.md)
  - [`pi`](harness/pi.md)
  - [`generic`](harness/generic.md)，未配置或未安装专用客户端时的通用说明
- [`develop`](workflows/develop.md) 规定实现分层、测试与交付检查。
- [`contribution`](workflows/contribution.md) 在接收代码、文档、夹具或其他材料前核查来源、权利主体与允许用途。
- [`architecture`](workflows/architecture.md) 从具体使用场景选择最小架构切片并维护层边界。
- [`review`](workflows/review.md) 规定缺陷优先的代码审查及验收。
- [`documentation`](workflows/documentation.md) 维护文档事实、索引和同步。
- [`release`](workflows/release.md) 只准备发布并验证输入，不执行实际发布。
- `release` workflow 仅执行发布准备和审查；它不会发布 crates.io 包或创建公开的 GitHub Release。项目使用 Apache-2.0，发布前仍需检查包内容、第三方通知和目标版本。
- `documentation` workflow 用于审查文档、示例和工作流之间的一致性；文档中的目标、实现事实和验收证据必须分别标明。
- 软件许可与第三方材料政策以 [`docs/licensing.md`](../docs/licensing.md) 为准；项目按 Apache-2.0 发布，完整文本见根目录 `LICENSE`。第三方材料仍须单独检查来源、兼容许可证与通知义务。
- Python 与 JavaScript/TypeScript 的强制工具规则见通用项目指令 `.agents/instructions.md`，所有 harness 和 workflow 均应遵循该单一来源。
- 上下文依次包含根目录 `AGENTS.md`、注册表中的通用指令、所选 harness 和所选 workflow。选择规则只决定加载哪些已注册说明，不会自动安装客户端。`prepare` 输出合并后的文件路径。`run` 从仓库根目录启动客户端，将上下文和任务直接放入首条提示。提示超过 120 KiB 时，`run` 改为在提示中提供文件路径，由客户端读取。直接启动客户端时，请按客户端的约定加载项目指令，或使用 `prepare` 生成的文件。客户端和登录由用户管理。
- 当前 harness 和 workflow 来源均随仓库检出，本机默认 `prepare` 无需联网；注册表也支持通过 `{ url = "https://…", sha256 = "…" }` 声明固定摘要的远程 Markdown。首次检索所选远程条目需要 `curl`。每次使用缓存时重新校验 SHA-256。在线模式可重新下载损坏的缓存。离线模式只读取本地文件和已校验的缓存，缺失或损坏的远程条目会报错。缓存位于 `~/.mcbtodo/cache/agents/`。Unix 平台 API 以目录文件描述符逐级打开并验证缓存目录，读写均相对已打开目录执行；静态符号链接、非普通文件、多硬链接及不安全权限会被拒绝，写入通过同目录私有临时文件原子替换。该实现将缓存操作锚定到已验证的目录对象，不依赖后续路径重新解析；非 Unix 构建保留路径操作实现。xtask 不负责安装 harness 客户端；`--offline` 只控制指令检索，客户端自行管理模型和网络。

CI 在两种 Linux 架构上运行 `cargo xtask check`、示例 Vault 校验、包校验和发行构建，并从归档解出二进制运行包含 `--sandbox` 的端到端 smoke。CI 归档附带 `.tar.gz.sha256`；将两个文件放在同一目录，运行 `sha256sum -c mcbtodo-linux-<arch>.tar.gz.sha256` 校验归档。Agent 注册表来源/摘要规则有单元测试，但当前 CI 不运行冷缓存远程 harness 下载流程。文档职责和同步更新要求见 [`docs/README.md`](../docs/README.md)。

打 `v*` 版本标签会触发发布准备流水线：检查 workspace、验证示例 Vault、运行 crates.io dry-run，并构建及验证各架构发行归档。该流水线也可通过 `workflow_dispatch` 手动启动；手动运行仅验证 GitHub Actions 中选定的 ref，不会自动创建正式发布。流水线只上传 GitHub Actions 构建产物，不会执行实际发布。

`cargo xtask agent validate` 会校验根 `AGENTS.md`、注册表、每个已注册来源，以及注册 workflow 在本文件和 `docs/README.md` 中的链接与本地来源路径；集成测试在冷离线 HOME 下覆盖全部已注册 harness × workflow 组合。添加或删除 workflow 时仍须同步更新两个索引。远程内容更新需要在注册表中更新摘要。
