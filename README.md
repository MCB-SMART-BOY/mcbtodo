# mcbtodo

以 Markdown Vault 为基础的个人工作台。默认打开月历，使用 Helix 风格的键位管理日期选择，在同一工作台查看任务层级、依赖和日程。后台守护进程保存专注计时。当前发行构建目标为 Linux x86_64 和 AArch64。macOS ARM64 和 Windows x64 的前台 CLI、TUI 渲染和 Vault 已在原生 CI 测试。后台服务目前在 Linux 上运行。

## 开始使用

在 Linux ARM64 或 x86_64 上使用 Rust 1.100 或更高版本。构建守护进程时需要 C 编译工具链来编译随包提供的 SQLite。

```sh
cargo run
```

根 `Cargo.toml` 将 `apps/mcbtodo` 设为唯一 `default-member`，所以在仓库根目录直接运行 `cargo run` 会启动 TUI。首次启动会安全创建默认配置、状态目录和空 Vault；现有配置不会被覆盖。`mcbtodo init` 仍可用于提前初始化。Cargo 参数与程序参数用 `--` 分隔，例如 `cargo run -- --help` 或 `cargo run -- init`。要启动另一个 workspace binary，明确选择包：

### 后台服务

`mcbtodo-daemon` 是后台服务。可以用 `cargo install` 将程序安装到 `~/.cargo/bin`，并配置为登录时启动的 systemd 用户服务。服务运行后，在任意终端执行 `mcbtodo` 打开 TUI。按 `q` 退出终端界面后，服务继续保存计时、处理提醒和运行工作流。再次打开 TUI，用 `:timer status` 查看同一计时会话，或打开 Workflow 查看后台运行。Cargo 和发行包安装步骤见 [systemd 用户服务指南](deploy/systemd/README.md)。

从源码开发时，可以在一个终端启动 Daemon，在另一个终端运行 `cargo run`：

```sh
cargo run --locked -- init
cargo build --locked --workspace --bin mcbtodo --bin mcbtodo-daemon --bin mcbtodo-worker
cargo run --locked -p mcbtodo-daemon
```

TUI 和 Daemon 使用同一用户的本地 socket。服务停止时，计时和 Workflow 界面会提示启动 Daemon，日历和 Markdown Vault 编辑仍可独立使用。Daemon 按工作流节点启动 `mcbtodo-worker` 处理脚本和 Agent 请求。

Daemon 默认使用一个 Worker 槽。在 TUI 设置的 Daemon 分组中可以选择
1 到 4 个槽，重启 Daemon 后生效。启动参数 `--worker-slots` 优先于保存的设置。

```sh
mcbtodo-daemon --worker-slots 2
```

槽位已占满时，后续工作保持 `pending`，不会提前领取 Attempt 或消耗 Agent
预算。每个 Worker 的取消和监督彼此隔离。Daemon 重启后，已经 Started 的执行仍
按 `outcome_unknown` 恢复，尚未领取的工作继续保留。多槽能力是本地有界并发，
不代表已经提供通用的跨资源调度器。

运行 TUI 只需要应用本身。Daemon 执行工作流脚本时需要 Linux Bubblewrap，解释器由脚本的 `runner` 决定。CI 安装 Bubblewrap 和 Nushell，以实际运行脚本的沙箱测试。`cargo xtask smoke-systemd` 使用 Rust 解析 JSON，也可以在没有 `jq` 的环境中运行。

可以在 TUI 设置中选择默认、高对比度或单色主题。非空 `NO_COLOR` 强制使用
单色主题。更完整的操作提示可在 TUI 中按 `?` 查看。
终端太窄或太矮时，月历会用日期列表保持选中日期可见。详见 [TUI 设计系统](docs/tui-design.md)。

| 位置 | 用途 |
| --- | --- |
| `~/.config/mcbtodo/config.toml` | 用户偏好。`timezone` 默认 `UTC`，`home_view` 可设为 `month`（默认）或 `restore_last_view`，`tui_theme` 可设为 `default`、`high-contrast` 或 `monochrome`。Linux 上的 `worker_slots` 是 1 到 4 之间的整数，默认 1。 |
| `~/.config/mcbtodo/agents.toml` | 可选的工作流 Agent 策略。每条授权绑定能力和发布摘要。 |
| `~/.config/mcbtodo/codex.toml` | 可选 Codex Provider 配置。包含受保护的可执行路径、能力、指令和输出 schema。 |
| `~/.config/mcbtodo/keymap.toml` | 可选 TUI 键位覆盖，可在设置页中编辑。未创建时使用内置默认键位。 |
| `~/.mcbtodo/vault/` | 默认 Vault，存放可直接编辑的 Markdown。 |
| `~/.mcbtodo/state/` | 计时和工作流 SQLite 状态、提案队列、工作流 revision 快照、进程锁与 Unix socket。工作流产物保存在工作流 SQLite 状态中。 |
| `~/.mcbtodo/cache/agents/` | 由 `xtask` 生成的 AI 上下文与固定摘要的下载缓存。 |

在 TUI 中按 `Space` → `Enter` 打开设置，也可以输入 `:settings`。General 包含时区和首页，Appearance 包含主题，Linux 上的 Daemon 包含 Worker 槽位。用 `Tab` 或 `Shift+Tab` 跨分组切换字段。时区输入 IANA 名称（例如 `Asia/Shanghai`），用 `Delete` 清空输入。用 `←` / `→` 调整其他字段。`Enter` 保存本页设置，`Esc` 放弃。

每次打开设置页都会读取当前的 `config.toml`。手工编辑时只需填写想改的字段，其余项使用应用默认值。值未改动时按 `Enter` 会保留原文件。如果打开设置后文件有任何修改（包括注释），保存会提示冲突。按 `F5` 可原地读取新版本并重置当前页的草稿。

如果 `config.toml` 可读但内容无效，`cargo run` 会先打开设置页，使用默认值作为草稿。检查并修改各字段后，按 `Enter` 替换整份文件并继续打开日历，或按 `Esc` 保留原文并退出。如果配置在 TUI 运行期间变得不可读，偏好页会显示原因，`F5` 可重试。此时仍可按 `F2` 编辑键位、按 `F3` 查看 Agents。

保存后主题立即生效。终端设置了非空 `NO_COLOR` 时，本次会话继续以单色显示，选中的主题仍会保存。旧的 `MCBTODO_TUI_THEME` 已移除。

时区和启动视图在下次启动时应用。选择恢复上次视图后，正常退出会记录当前视图。如果无法读取上次视图，TUI 会从月历启动并提示，可以在设置中改回固定月历。显式 `--timezone` 只覆盖当前会话，若要使用保存的时区，重启时去掉该参数。

保存修改时会重新生成 TOML，原有注释和排版可能丢失。使用显式 `--vault` 时也可编辑这份用户配置。

设置中按 `F2` 打开键位页。用方向键或 `Tab` 选择动作，直接按下要绑定的字符键，`Delete` 恢复该动作的默认键位。`Enter` 保存并即时应用，`Esc` 放弃键位草稿并返回设置。键位页单独保存到 `keymap.toml`，未修改时保留文件原文或缺省状态。遇到同一作用域内的重复绑定会留在当前页并显示冲突。遇到文件修改冲突时，`F5` 只重读键位页，偏好草稿会保留。

如果手工修改的 `keymap.toml` 无效，TUI 会提示并使用默认键位启动。进入键位页后，可以从默认键位开始编辑，按 `Enter` 替换无效文件，或按 `Esc` 保留原文件。文件无法读取时，键位页会显示错误。恢复文件访问后可在原页按 `F5` 重试，按 `Esc` 回到原先的偏好草稿。

设置中按 `F3` 打开 Agents，查看已配置的 Provider、默认路由和授权条数。在 Linux 上，如果已配置多个 Provider，用 `↑` / `↓` 选择，按 `Enter` 保存为默认路由。页面会提示此操作将重定向的授权条数。没有指定 Provider 的授权会跟随默认路由，带有 Provider 的授权继续使用指定路由。

选中已配置的 Provider 后按 `F6` 可编辑其 socket 路径。路径相对于 `~/.mcbtodo/`，例如 `state/agent.sock`。`Delete` 清空，`Enter` 保存，`F5` 重读，`Esc` 取消。新增 Provider、移除 Provider 和增删 revision 授权仍通过 `agents.toml` 完成。

在 Linux 上，从 Agents 页按 `F4` 可编辑 `codex.toml` 中的可执行路径、能力名、指令和输出字段。用 `Tab` 或方向键切换字段，`Delete` 清空当前字段，`Enter` 保存，`F5` 重读当前页，`Esc` 取消编辑。输出字段可写成 `title=string, count=integer`。新路由、端点和 Codex 参数在下一次执行尝试中读取。本地安装诊断由 `mcbtodo doctor --json` 提供。

`init` 可重复运行，现有配置文件会保留。
这些路径固定相对于用户 home；当前版本不读取 `XDG_CONFIG_HOME` 或 `XDG_DATA_HOME` 进行重定位。

配置示例：

```toml
timezone = "Asia/Shanghai"
home_view = "restore_last_view"
```

`home_view = "month"` 每次从月历进入。`restore_last_view` 在启动时恢复上次视图，并在正常退出时记录当前视图。应用把这个运行状态保存在 `~/.mcbtodo/state/last-view`。文件缺失或视图名称无法识别时，从月历启动。强制结束进程时不保证保存本次视图。

在 Unix 上，配置读取逐层打开 `.config`、`mcbtodo` 和配置文件，拒绝符号链接、非当前用户属主以及允许组或其他用户写入的权限。普通偏好和键位文件有 1 MiB 上限，Agent 策略和 Codex 配置有 64 KiB 上限。普通偏好还要求应用配置目录为私有目录。

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

单独按 `Space` 可浏览操作菜单。方向键滚动，`Home` 和 `End` 跳到首尾，
`Esc` 返回原视图。`Space u`、`Space t` 等快捷键仍可直接执行。

也可以直接在 `~/.config/mcbtodo/keymap.toml` 中按动作覆盖。

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

每个动作接受单个可打印字符，大小写分别绑定。视图键的作用域为 `Space u` 菜单。原默认键在未分配给其他动作时仍可作为快捷键。`Ctrl+C` 始终退出，设置导航和保存键是固定键。`x`、`a`、`d` 保留给 Graph 操作，`v` 保留给选择。手工配置中的绑定冲突、未知动作或无效值会在启动时报告。

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

日历事件可用 `recurrence` 声明有限或持续的 `daily` / `weekly` 重复规则，`interval` 是正整数，`through`（若提供）为包含的最后一个锚定日期。规则的 `start` 必须与事件 `when.start` 的当地日期一致。`date_span` 保持日期跨度。`date_time_span` 按模板时区的本地开始时刻重复，并保持实际经过时长。首个实例保留模板的显式偏移。后续实例遇到 DST 跳时，由 `dst.gap` 选择 `skip` 或 `next_valid`，其中 `next_valid` 移到缺口后的第一个有效时刻。时钟回拨时，由 `dst.fold` 选择 `earlier` 或 `later`，只生成一个实例。查询按显示时区返回与窗口相交的完整实例。查询起止日期相差最多 366 天。每条重复规则在单次查询中最多展开 1024 个相交实例，超过时会报告错误。投影不会写入额外 Markdown 实例。

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

`mcbtodo vault completion-events --after 0` 分页输出最多 64 条记录，每条包含 `sequence`、`observed_revision` 和 `event`。CLI 直接完成和应用完成提案会记录 `origin_device_id`，旧事件可能缺少该字段。后续页将最后一条的 `sequence` 传给 `--after`。空输出表示当前没有更新。游标仅适用于同一个本地事件日志。安装身份和备份规则见 [存储与同步模型](docs/storage-and-sync.md)。

使用 `mcbtodo vault completion-events --after 0 --logical` 导出版本化事件。每行的 `envelope` 携带事件 UUID、工作单元 UUID、来源、观察到的 revision 和完成证据。`sequence` 位于外层，只能用于继续读取该 Vault 的日志。

可在另一份 Vault 手动回放可信的导出记录。目标 WorkUnit 必须保留完成前的 Markdown，导入不会触发工作流。导入逐条提交，遇到冲突会报告行号，已导入的事件可以安全重试。自动同步、设备授权和冲突合并仍在规划中。详见[存储与同步模型](docs/storage-and-sync.md#manual-completion-import)。

```bash
mcbtodo --vault /path/to/source vault completion-events --after 0 --logical > completions.jsonl
mcbtodo --vault /path/to/destination vault import-completion-events < completions.jsonl
```

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

审批前先执行 `mcbtodo workflow inspect <run-id>`，阅读等待节点 `current_node` 和完整的 `detail`。确认后，从**同一次**输出复制 `approval_review`，运行 `mcbtodo workflow approve <run-id> <node> --review <value>`。如果内容已刷新，重新检查并审阅后再批准。自动化集成也须把审批凭据和实际展示的详情一起保存。升级时请同时更新 CLI、TUI 和 Daemon。`workflow reject` 用法保持不变，已有运行记录可直接检查。

Linux 上可运行 `mcbtodo doctor --json` 查看 `agent_policy` 检查和配置所选的后端。检查缺失策略时显示 `skipped`，策略不可用时显示 `warning`。选择 Codex 时，`agent_codex` 会检查受保护的配置和可执行文件。协议兼容性与模型连通性由运行时探测验证。

同一命令也会预检 Bubblewrap、Bash 和 Nushell 的固定系统路径、文件所有权及执行权限。`ok` 表示该二进制通过安装预检。沙箱启动与隔离需通过实际执行验证。可选工具的 `warning` 不影响 Vault 检查或 TUI 启动。

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

`cargo xtask smoke-systemd` 通过临时用户服务验证退出并重开 TUI 后计时继续，以及服务崩溃恢复。该测试需要可访问的 systemd 用户管理器和 Bubblewrap。`cargo xtask package` 使用 `tar` 将同架构的发行二进制和服务文件打包到 `target/dist/`。

## 支持范围与发布状态

当前受支持的发行目标是 Linux x86_64 和 AArch64。仓库 CI 在对应原生 Linux runner 上运行 workspace 检查、release 构建和打包二进制 smoke；systemd 用户服务仅适用于提供 systemd user manager 的 Linux 环境。此验证不代表已在所有发行版、桌面环境或其他操作系统上验收。

macOS ARM64 和 Windows x64 的前台应用正在验证。CI 已配置原生 CLI 和 Agent 工作流准备门禁。两者的 `mcbtodo doctor --json` 将 Linux 专用的 Daemon 传输、Workflow Agent、沙箱、桌面通知适配器和 systemd 报告为 `unsupported`。Windows 上的备份、恢复及私有提案队列会明确拒绝。各平台的实测范围见[平台能力矩阵](docs/architecture.md#平台验证范围)。

Linux 的 `mcbtodo doctor --json` 通过 `notification_desktop` 报告当前进程的桌面通知环境。未配置会话总线时为 `skipped`，已配置总线但 `PATH` 中缺少可执行的 `notify-send` 时为 `warning`。检查只读取环境和文件状态。若 Daemon 由 systemd 启动，请以服务进程的环境为准。

当前项目具备端到端可演示的规划、计时、提案审批、工作流执行及备份恢复基础，但还未达到可靠日用级别。生产 Daemon 已为脚本和 Agent attempt 启动并监督独立 `mcbtodo-worker` 进程，使用绑定 workflow revision 的输入、attempt 身份校验、取消回收和 `outcome_unknown` 重启恢复；测试也覆盖 Daemon/Worker 中断后的本地进程回收及不重放。新解压的 Linux 发行归档已通过 sandbox smoke，其中用归档内 Worker 运行 `exit 23` 脚本并验证失败步骤与错误原因被持久化；打包 Worker 的畸形协议帧及组合故障仍未验收。数据库 epoch 不是可过期租约，也不能撤销或核对外部副作用。桌面提醒已采用持久 outbox、最多五次有界退避及重启恢复，但 helper 副作用与成功落盘之间仍存在可能重复的 at-least-once 窗口，停机超过 24 小时的 occurrence 不会补发；`outcome_unknown` 已支持一次性人工核对审计，但持续运行恢复与全副作用边界故障注入仍不完整。首页月历/恢复上次视图策略已有配置及持久化实现，但完整跨日历视图交互、干净系统安装和持续运行可靠性也尚未验收。详细证据与架构差距见[架构验收评估](docs/architecture-assessment.md)。

仓库包含 GitHub Actions 构建与归档验证流程，但目前流程不会创建 GitHub Release，也不会发布到 crates.io。2026 年 11 月 23 日在 Linux AArch64 上已完成首发包准备检查，13 个可发布包均通过归档生成、Apache-2.0 元数据、归档许可证文件和 `cargo publish --dry-run` 检查。该结果仍不代表已经上架，首次发布还需要按依赖顺序执行并确认 crates.io 中的包名和版本可用。`mcbtodo-xtask` 是仓库专用工具，不发布。项目许可证及第三方材料规则见[许可与第三方材料](docs/licensing.md)。
