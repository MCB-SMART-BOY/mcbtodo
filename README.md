# mcbtodo

面向 Linux 的 Markdown 个人工作台，当前发行构建目标为 x86_64 和 AArch64。默认打开月历，使用 Helix 风格的键位管理日期选择，在同一工作台查看任务层级、依赖和日程。后台守护进程保存专注计时；尚未声明或验证其他操作系统支持。

## 开始使用

在 Linux ARM64 或 x86_64 上使用 Rust 1.100 或更高版本。构建守护进程时需要 C 编译工具链来编译随包提供的 SQLite。

```sh
cargo build --locked --workspace --bin mcbtodo --bin mcbtodo-daemon --bin mcbtodo-worker
./target/debug/mcbtodo init
./target/debug/mcbtodo
```

根 `Cargo.toml` 将 `apps/mcbtodo` 设为唯一 `default-member`，所以开发时在仓库根目录直接运行 `cargo run` 会启动 TUI；Cargo 参数与程序参数用 `--` 分隔，例如 `cargo run -- --help` 或 `cargo run -- init`。要启动另一个 workspace binary，明确选择包：

```sh
cargo run --locked -p mcbtodo-daemon
cargo run --locked -p mcbtodo-worker
```

`mcbtodo-worker` 是由 Daemon 按工作流节点启动的内部进程，通常不需要手动运行。日常使用前先构建三个 binary 并运行一次 `mcbtodo init`；之后可直接运行 `cargo run`，或从 `target/debug/mcbtodo` 启动 CLI/TUI。专注计时、提醒和调度需要另外运行 `mcbtodo-daemon`。

| 位置 | 用途 |
| --- | --- |
| `~/.config/mcbtodo/config.toml` | 用户配置。`timezone` 接受 IANA 时区名称，默认 `UTC`；`home_view` 可设为 `month`（默认）或 `restore_last_view`。 |
| `~/.config/mcbtodo/agents.toml` | 可选的工作流 Agent 策略。每条授权绑定能力和发布摘要。 |
| `~/.config/mcbtodo/keymap.toml` | 可选 TUI 键位覆盖；未创建时使用内置默认键位。 |
| `~/.mcbtodo/vault/` | 默认 Vault，存放可直接编辑的 Markdown。 |
| `~/.mcbtodo/state/` | 计时和工作流 SQLite 状态、提案队列、工作流 revision 快照、进程锁与 Unix socket。工作流产物保存在工作流 SQLite 状态中。 |
| `~/.mcbtodo/cache/agents/` | 由 `xtask` 生成的 AI 上下文与固定摘要的下载缓存。 |

`init` 可重复运行，现有配置文件会保留。修改显示时区后重新打开客户端，也可用 `--timezone Asia/Shanghai` 为单次调用指定时区。
这些路径固定相对于用户 home；当前版本不读取 `XDG_CONFIG_HOME` 或 `XDG_DATA_HOME` 进行重定位。

配置示例：

```toml
timezone = "Asia/Shanghai"
home_view = "restore_last_view"
```

`home_view = "month"` 每次从月历进入；`restore_last_view` 在正常退出或编辑器/提案审批交还终端时恢复并保存上次视图。保存值位于 `~/.mcbtodo/state/last-view`，不属于用户配置；缺失或不识别的值回退到月历。强制结束进程时不保证保存本次视图。

初始化及每次用户配置读取都会检查 `.config` 与 `mcbtodo` 配置目录不是符号链接、属主正确且不允许其他用户写入；配置文件也须由当前用户所有且不可被组/其他用户写入。配置读取有 1 MiB 上限并通过 no-follow 文件描述符打开。应用数据目录另有私有权限校验；这些检查不构成对所有祖先路径、同一用户并发替换、任意外部 Vault 或全部运行时文件的完整隔离。

## 日历和任务

```sh
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai tui --date 2026-11-20
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai calendar --date 2026-11-25 --json
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai \
  calendar export-ical --from 2026-11-20 --through 2026-12-01 --output week.ics
cargo run --locked -- --vault examples/vault \
  calendar import-ical calendar.ics
cargo run --locked -- --vault examples/vault outline
cargo run --locked -- --vault examples/vault graph
```

`calendar export-ical` 输出选定日期窗内的标准 iCalendar：日期区间用排他式 `DTEND`，精确时段统一输出 UTC，截止日输出为 `VTODO/DUE`。活动期、任务发生项、活动、时间块与期限会保留 `X-MCBTODO-KIND` 和 `X-MCBTODO-ID` 来源属性。省略 `--output` 时写入 stdout。`calendar import-ical <PATH>` 只接受可表达为独立日历活动的 `VEVENT`，支持全天日期、UTC 和 IANA 时区；本项目导出的两个 `X-MCBTODO-*` 来源属性可识别但不影响 UID 身份映射，其它未知扩展属性、嵌入式 `VTIMEZONE`、重复属性、重复规则、浮动时刻、DST 歧义/缺口、VTODO 及其他无法映射的属性会拒绝。导入 UID 映射为稳定 UUID，整批先验证，再以一次目录交换发布；任何校验失败都不会导入部分事件。重复导入相同 UID 会因稳定 UUID 已存在而整体失败，不会创建重复事件。

`hjkl` 或方向键移动，`v` 开启日期范围选择，`Enter` 查看议程。`Space u` 打开视图选择，可从月历切换到周历、日历、议程、大纲、任务图、Timeline 或工作流。Timeline 展示焦点日期所在周起始的 14 天计划；`h/l` 移动日期，`j/k` 选择项目，横条区分计划区间、精确时段、任务实例和期限。在 Graph 中用 `v/x` 选择依赖源节点，移动焦点到目标节点后按 `a` 新增边或按 `d` 移除边；预览会显示边方向和受影响的阻塞状态，按 `y` 确认、`Esc` 取消。依赖边方向为前置节点指向依赖它的目标节点。在月历选中日期后按 `Space t b`，为 WorkUnit 安排精确时段。按 `Space c r` 创建单次提醒，并输入带偏移的 RFC 3339 时间戳，例如 `2026-11-21T09:00:00+08:00`；偏移必须与 Vault 时区相符。在 Outline 选中 Task 或 WorkUnit 后创建，提醒会自动关联该对象。周期提醒仍通过 `mcbtodo vault create-reminder` 或 MCP 提案创建。`e` 编辑当前选中项的源文档，按 `$VISUAL`、`$EDITOR`、`hx` 的顺序选择编辑器可执行文件。`r` 重载 Vault。若编辑后校验失败，TUI 保留上一次有效数据并显示错误。

键位可在 `~/.config/mcbtodo/keymap.toml` 中按动作覆盖，例如：

```toml
[keys]
quit = "Q"
move_left = "H"
move_down = "J"
move_up = "K"
move_right = "L"
view_month = "1"
view_week = "2"
view_day = "3"
view_agenda = "4"
view_outline = "5"
view_graph = "6"
view_workflow = "7"
```

配置只接受列出的动作名和单个可打印字符。常用导航键只在主视图生效，视图键只在 `Space u` 菜单生效；表单和确认键保持固定。`x`、`a`、`d` 保留给 Graph 操作，`v` 保留给选择；冲突、未知动作或无效绑定会阻止 TUI 启动并指出配置错误。

按 `/` 可模糊搜索 Project、Task 和 WorkUnit；输入名称片段后按上下键选择，Enter 会切换到 Outline 并聚焦该对象，Esc 取消且保留原焦点。

按 `:` 打开命令模式：`:view month|week|day|agenda|outline|graph|timeline|workflow` 切换视图，`:search <文字>` 打开对象搜索，`:timer status|start|pause|stop` 操作计时器，`:proposals` 打开提案审阅；`:today`、`:date YYYY-MM-DD`、`:reload`、`:help` 和 `:quit` 提供日期、重载、帮助与退出操作。直接输入视图名称也可切换视图。

在 Outline 中选中 Project 后按 `Space c t` 可捕获 Task；创建成功后会切换到 Outline 并聚焦新 Task。Project 在表单打开期间被外部修改时，写入会因 revision 冲突被拒绝，不会覆盖外部内容。

将 `examples/vault/` 中的 `projects/` 与 `calendar/` 作为编辑参考。每个项目目录包含 `project.md`、`tasks/*.md` 和 `units/*.md`，独立日历活动在 `calendar/*.md`；未分类事项保存在 `inbox/*.md`。使用 `mcbtodo.project/v1`、`mcbtodo.task/v1`、`mcbtodo.work-unit/v1`、`mcbtodo.calendar-event/v1` 和 `mcbtodo.inbox-entry/v1` 作为 `schema`，为每个对象分配唯一 UUID，正文第一条非空行写 `# 标题`。修改文件名后，基于 UUID 的引用保持有效。`mcbtodo validate` 校验 YAML、时间范围、引用和依赖环。

快速捕获不要求先选择 Project；`list-inbox` 输出 JSON Lines。分流到已有 Project 时会从 Inbox 标题、正文和标签创建 Task，并在原 Inbox 条目写入 `project_id` 作为来源记录。Task UUID 从 Inbox UUID 稳定派生；若在发布 Task 后中断，重试会验证并复用完全相同的 Task。若目标文件已被修改或与预期不符，操作失败且不会覆盖。每次分流都基于当前 Inbox Markdown revision，过期编辑会被拒绝。

```sh
mcbtodo vault capture-inbox --title "Check the backup design" --body "Review recovery cases"
mcbtodo vault list-inbox
mcbtodo vault triage-inbox --id INBOX_UUID --project PROJECT_UUID
```

活动期与截止日作为日历标记。全天日期区间包含首日和末日，精确时间段的结束时刻不计入占用。精确时间使用带 UTC 偏移的时间戳和 IANA 时区，夏令时重复时刻由偏移确定。

日历事件可用 `recurrence` 声明有限或持续的 `daily` / `weekly` 重复规则，`interval` 是正整数，`through`（若提供）为包含的最后一个发生日期。规则的 `start` 必须与事件 `when.start` 一致。当前重复投影支持全天/多日 `date_span` 事件；精确时段事件暂不支持重复。日期视图按查询窗口展开，不写入额外 Markdown 实例；查询范围最多 366 天。

重复人工任务与日历事件不同：任务规则保存在 Task 模板中，明确物化后每个发生日期成为独立 Task Markdown 文件，实例有自己的 UUID、状态和截止日期。模板和实例以 `recurrence` / `occurrence` front matter 字段区分；实例 UUID 由模板 UUID 和日期通过 UUID v5 稳定计算。物化范围最多 366 天，重复运行会返回既有实例而不覆盖其状态。

```sh
mcbtodo vault capture --project PROJECT_UUID --title "Weekly review" \
  --recurrence '{"start":"2026-11-20","through":"2027-12-31","frequency":"weekly","interval":1,"dst":{"gap":"skip","fold":"earlier"}}'
mcbtodo vault materialize-task-occurrences --template TASK_UUID \
  --from 2026-11-20 --through 2026-12-31
```

实例可以像普通 Task 一样编辑和完成；Vault 验证器会校验实例是否引用有效模板、是否与模板同属一个 Project，以及稳定 UUID 是否与模板和日期匹配。任务发生项会以非忙碌的日期标记出现在日历中。

## 专注计时

在终端运行 `./target/debug/mcbtodo-daemon`，或按 [systemd 用户服务指南](deploy/systemd/README.md) 启动。在另一个终端运行以下命令。

```sh
./target/debug/mcbtodo timer start
./target/debug/mcbtodo timer status --json
./target/debug/mcbtodo timer pause
./target/debug/mcbtodo timer stop
./target/debug/mcbtodo timer report
./target/debug/mcbtodo timer report --date 2026-11-20 --json
```

也可从 TUI 按 `Space t` 操作计时器。关闭客户端后计时继续。守护进程重启时，运行中的会话进入 `needs_review`，保留已确认工时。

每日报告按 WorkUnit UUID 汇总会话，并单独列出未关联的会话。省略 `--date` 时查询配置时区（可用 `--timezone` 覆盖）的今天。整个会话归到开始的本地日期，跨午夜或次日继续的时间也归到这一天。运行中的会话包含当前计时进度，暂停与 Linux 系统休眠期间不累计工时，守护进程重启后只保留上次持久化的工时。报告只读取计时记录。

`mcbtodo vault --help` 列出项目、任务、工作单元和时间块的写入命令。`vault bind-workflow` 绑定发布摘要，`vault complete --unit UUID --evidence RUN_UUID:STEP` 在验收时附加运行步骤引用；只有状态为 `succeeded` 的已记录步骤可作为证据，失败、跳过或格式异常的 trace 会被拒绝。`vault completion-events` 以 JSONL 输出 Vault 中已提交的 WorkUnit 完成事件。

用 `mcbtodo vault set-deadline --id UUID --date YYYY-MM-DD` 设置 Project、Task 或 WorkUnit 的日期截止日；用 `mcbtodo vault clear-deadline --id UUID` 清除。写入会基于当前读取到的文档 revision 检查冲突。

用 `mcbtodo vault create-reminder --title "Review" --schedule '{"kind":"once","at":{"value":"2026-11-21T09:00:00+08:00","timezone":"Asia/Shanghai"}}'` 创建一次性提醒。周期提醒使用 `{"kind":"recurring","rule":{"start":"2026-11-21","time":"09:00:00","timezone":"Asia/Shanghai","frequency":"weekly","interval":1,"dst":{"gap":"skip","fold":"earlier"}}}`；`--target UUID` 可关联现有 Task 或 WorkUnit。提醒定义保存在 Vault 的 `reminders/<uuid>.md`，目标关闭后会停止投递。

## 工作流

```sh
cargo xtask workflow-example
cargo run --locked -- workflow validate target/examples/workflow/graph.yaml
cargo run --locked -- workflow simulate target/examples/workflow/graph.yaml --fixture examples/workflow/fixtures/passed.json
```

编译器检查图结构、输入输出类型和权限声明。发布摘要固定图文件、脚本和声明的可执行文件字节。`cargo xtask workflow-example` 构建本机静态 ELF，供示例中的 Nushell 脚本检查 Rust 页表模型。启动 `mcbtodo-daemon` 后可运行发布版本。失败分支提出的后续任务需要默认 Vault 包含示例 Rust OS 项目。

```sh
revision="$(./target/debug/mcbtodo workflow publish target/examples/workflow/graph.yaml)"
./target/debug/mcbtodo workflow authorize-script "$revision" test
./target/debug/mcbtodo workflow run "$revision"
./target/debug/mcbtodo workflow schedule cron "$revision" '0 23 * * *' \
  --zone Asia/Tokyo --missed run-once
./target/debug/mcbtodo workflow runs
./target/debug/mcbtodo notifications
```

脚本执行需要宿主机具备可用的 Bubblewrap 和所选的 Bash 或 Nushell。授权绑定发布摘要和节点。沙箱隔离宿主 HOME 和网络。节点可以使用空能力集，或声明一项 `process.exec`，用图目录的相对路径指定一个本机 ELF。守护进程将发布时固定的 ELF 以只读方式交给脚本，并通过 `PROD_EXECUTABLE` 提供沙箱内路径。运行详情由 `workflow inspect`、`trace`、`artifact` 和 TUI 工作流视图提供。守护进程重启后，尚未确认结果的活动工作节点进入 `outcome_unknown`，不会自动重放。操作者核对外部效果后，可运行 `mcbtodo workflow resolve <run-id> --outcome completed|failed|not-started --note "核对依据"` 记录一次性审计结论。该操作不把运行改写为普通成功/失败、不生成步骤证据、不触发依赖工作流；若确认未启动且要重试，请单独提交新的运行。失败分析可接入本机 Agent。 [Agent 适配器](docs/agent-adapter.md)

`mcbtodo notifications` 以 JSON 返回持久化的工作流终态通知，最新记录优先；`mcbtodo notifications --reminders` 查询提醒 Inbox，记录含 `created_at`（入 Inbox 时间），不代表系统桌面通知已经送达。用上一页的 `next_before` 作为 `--before UUID` 继续查询。桌面通知不可用时仍可查询这些记录。桌面 helper 失败会按有界指数退避重试最多五次，重启会恢复进行中的投递；进程可能在 helper 已显示通知、但成功状态尚未落盘时崩溃，因此这种极少数窗口按至少一次语义处理，可能重复。停机超过 24 小时的旧 occurrence 不会自动补发。

`mcbtodo workflow schedule dependency <下游发布摘要> <上游发布摘要> [--id UUID]` 为上游之后的每次成功排队一个下游运行。同一规则和上游运行 UUID 只处理一次。`workflow inspect` 的 `source_run_id` 指向触发该运行的上游，`workflow schedule disable` 停止后续触发，已排队的运行继续保留。每个上游版本最多有 16 条启用的依赖规则。级联最多延伸 8 层，每个根运行最多产生 128 个下游运行，同一链路内不会再次访问已经运行过的版本。`workflow schedule inspect` 的 `cascade_skipped` 统计被这些级联保护跳过的次数。

`mcbtodo workflow schedule work-unit-completed <发布摘要> <WorkUnit UUID> [--id UUID]` 订阅默认 Vault 中指定 WorkUnit 的后续完成事件。规则以注册时读取的最后一个已提交事件为起点，使用相同 `--id` 重试会保留原起点。守护进程重启后继续读取，即使事件在离线期间写入。在保留的运行数据库中，每条规则对每个事件 UUID 最多排队一次，`workflow inspect` 的 `source_event_id` 指向原始事件 UUID。已有历史事件、手动编辑 Markdown 状态和对已完成节点补证据均不会触发此规则。禁用规则后，已排队的运行继续保留。

## MCP 提案与备份

将 `mcbtodo mcp serve` 配置为本机 stdio 服务。客户端须支持 MCP `2026-07-28`，并在每个请求中携带该版本所需的元数据。客户端能读取 Vault 和提交提案。用 TUI 或 `mcbtodo proposal show <UUID>` 查看提案，CLI 用 `mcbtodo proposal apply <UUID>` 交互批准，或用 `mcbtodo proposal reject <UUID>` 拒绝。

MCP 可读取 Project、Task、WorkUnit、Inbox 和日期范围内的日历投影；Inbox 查询返回捕获时间、标签、Project 分配及当前 revision。MCP 还可提议捕获 Task、把 Inbox 条目转换为指定 Project 下的 Task、完成 WorkUnit、创建时间块、设置或清除 Project/Task/WorkUnit 的日期截止日、为 Task/WorkUnit 创建一次性或周期提醒、批量调整 WorkUnit 依赖图，以及提交工作流快照供人工审核。工作流工具接收 YAML `graph` 和以标准 Base64 编码的 `sources` 映射；重用相同 `request_id` 与相同输入会返回原提案。TUI 审核页展示图、文本源码及源码 SHA-256/字节数；二进制源码以摘要和长度标识。批准只发布不可变 revision，不授权也不执行脚本，授权和运行仍使用独立命令。提案进入 `applying` 后可对内容寻址的工作流发布安全重试。Inbox 分流提案使用 `list_inbox` 提供的条目 revision，并在人工审批时保留来源条目及其捕获内容；生成 Task 使用稳定 ID，过期修订会被拒绝。截止日和提醒提案必须附带 `list_projects`、`list_tasks` 或 `list_work_units` 返回的当前 revision；提交和批准时都会检查，过期提案不会覆盖修改。提醒由人工审批后才写入 Vault。Graph Diff 批准时重新校验审核过的修订、UUID 和候选 DAG，在 Linux 上一次发布整批 WorkUnit。若发布后无法确认结果，提案会停留在 `applying`，需要人工核对。项目内 `units/` 使用平铺的常规文件布局，项目目录中的 `.mcbtodo-graph-*` 暂存目录由 Vault 保留。

停止守护进程后运行 `mcbtodo backup <新备份目录>`。`mcbtodo restore <备份目录> <新家目录>` 验证后发布恢复结果。备份包含默认 Vault、用户配置、SQLite 权威状态、已发布版本和绑定默认 Vault 的提案。默认 Vault 按 canonical 路径识别：若 `~/.mcbtodo/vault` 是指向其他目录的符号链接，指向的目录仍视为默认 Vault，其提案也会纳入备份；恢复时内容写入真实目录，不保留原主机上的符号链接。Vault 内容树内部的符号链接仍会被拒绝，显式选择的外部 Vault 不纳入默认备份。显式选择外部 Vault 的 CLI/TUI 操作不会切换后台 Daemon 的 workspace；提醒扫描和 WorkUnit 完成事件触发器仍服务于默认 Vault。因此 CLI、TUI 和 MCP 都拒绝为外部 Vault 创建提醒，避免显示保存成功但实际不会触发；默认 Vault 的 canonical 符号链接别名仍可使用。Agent 适配器启动时会重建它的 socket。参见 [架构文档](docs/architecture.md)。

## 开发

```sh
cargo xtask check
cargo build --release --locked --workspace --bin mcbtodo --bin mcbtodo-daemon --bin mcbtodo-worker
cargo xtask smoke
cargo xtask smoke --sandbox
cargo xtask smoke-systemd
cargo xtask package
cargo xtask agent prepare
cargo xtask agent prepare --harness codex --workflow review --offline
```

`cargo xtask check` 运行格式检查、Clippy 与 workspace 测试。`smoke` 用临时用户目录和发行二进制，覆盖 Rust OS 示例的 MCP 规划、Graph Diff 与工作流快照提案审批、TUI 时间块、计时、调度和跨目录恢复；工作流快照获批后仍须单独授权脚本，且不会自动排队执行。`--sandbox` 在隔离环境执行本机编译的页表检查，使用测试适配器覆盖失败分析和人工决策，并验证完成证据与进程中断恢复。两种 Linux 架构的 CI 均运行 sandbox smoke。开发用的 AI 注册表、harness 与 workflow 集中在仓库根目录。[`.agents/`](.agents/README.md) · [设计书](docs/design.md)

文档总览、各文档职责以及同步更新规则见[文档索引](docs/README.md)。提交代码前还应按变更类型遵循 `.agents/workflows/develop.md` 和 `.agents/workflows/review.md`；版本准备遵循 `.agents/workflows/release.md`。

贡献代码、文档或其他材料前，请先阅读[贡献指南](CONTRIBUTING.md)并按
[贡献接收工作流](.agents/workflows/contribution.md)记录来源与授权边界。
项目按 [Apache-2.0](LICENSE) 许可证发布；贡献材料须有清晰来源，并可
依照该许可证及其适用条款接收和分发。

`cargo xtask smoke-systemd` 通过临时用户服务验证计时恢复，依赖可访问的 systemd 用户管理器、Bubblewrap 和 `jq`。`cargo xtask package` 使用 `tar` 将同架构的发行二进制和服务文件打包到 `target/dist/`。

## 支持范围与发布状态

当前受支持的发行目标是 Linux x86_64 和 AArch64。仓库 CI 在对应原生 Linux runner 上运行 workspace 检查、release 构建和打包二进制 smoke；systemd 用户服务仅适用于提供 systemd user manager 的 Linux 环境。此验证不代表已在所有发行版、桌面环境或其他操作系统上验收。

当前项目具备端到端可演示的规划、计时、提案审批、工作流执行及备份恢复基础，但还未达到可靠日用级别。生产 Daemon 已为脚本和 Agent attempt 启动并监督独立 `mcbtodo-worker` 进程，使用绑定 workflow revision 的输入、attempt 身份校验、取消回收和 `outcome_unknown` 重启恢复；测试也覆盖 Daemon/Worker 中断后的本地进程回收及不重放。新解压的 Linux 发行归档已通过 sandbox smoke，其中用归档内 Worker 运行 `exit 23` 脚本并验证失败步骤与错误原因被持久化；打包 Worker 的畸形协议帧及组合故障仍未验收。数据库 epoch 不是可过期租约，也不能撤销或核对外部副作用。桌面提醒已采用持久 outbox、最多五次有界退避及重启恢复，但 helper 副作用与成功落盘之间仍存在可能重复的 at-least-once 窗口，停机超过 24 小时的 occurrence 不会补发；`outcome_unknown` 已支持一次性人工核对审计，但持续运行恢复与全副作用边界故障注入仍不完整。首页月历/恢复上次视图策略已有配置及持久化实现，但完整跨日历视图交互、干净系统安装和持续运行可靠性也尚未验收。详细证据与架构差距见[架构验收评估](docs/architecture-assessment.md)。

仓库包含 GitHub Actions 构建与归档验证流程，但目前流程不会创建 GitHub Release，也不会发布到 crates.io。Cargo workspace 包已声明 Apache-2.0、canonical repository、包描述和 docs.rs 文档地址；发布前仍需逐个检查准备发布的包内容。`mcbtodo-xtask` 是仓库专用工具，不发布。不要把本地打包成功视为已经完成上架。项目许可证及第三方材料规则见[许可与第三方材料](docs/licensing.md)。
