# mcbtodo 架构

`mcbtodo` 默认打开 TUI，也提供 CLI 和 stdio MCP。Linux 上，
`mcbtodo-daemon` 独立运行，管理专注计时、提醒、调度和工作流运行状态。
TUI 退出后后台任务继续运行。Daemon 委派脚本和 Agent 节点给本机
`mcbtodo-worker`，Markdown Vault 仍是业务文档的权威来源。

## Cargo workspace

| 组件 | 职责 |
| --- | --- |
| `mcbtodo-domain` (`crates/domain`) | UUID、Inbox entry、任务状态、时间语义、日历投影和 WorkUnit DAG。边从前置单元指向依赖它的单元。 |
| `mcbtodo-vault` (`crates/vault`) | Markdown 与 YAML front matter、Inbox/任务等引用校验、预览和带修订条件的写入。Vault 持有写锁并协调 Markdown 发布与完成事件恢复。 |
| `mcbtodo-workflow` (`crates/workflow`) | 类型化图编译、发布快照、确定性轨迹重放和模拟。文件发布后由摘要标识。 |
| `mcbtodo-execution` (`crates/execution`) | Linux Agent 适配器装配、Bubblewrap 脚本沙箱和 `notify-send` 适配器。 |
| `mcbtodo-agent-codex` (`crates/agent-codex`) | Codex app-server 协议、Linux 隔离进程及模型请求转发。 |
| `mcbtodo-ports` (`crates/ports`) | Agent、本地传输、进程控制、脚本隔离、工作流 Attempt 持久化、完成事件读取、桌面通知和 Provider 选择的业务契约。 |
| `mcbtodo-platform` (`crates/platform`) | 用户路径和配置、Linux Unix socket 客户端及 peer 身份验证。 |
| `mcbtodo-protocol` (`crates/protocol`) | 本机 Unix socket 的计时、日历查询和工作流等请求与响应，以及 Daemon 与单次 Worker 之间有界、带 attempt 身份的消息契约。 |
| `mcbtodo-storage-sqlite` (`crates/storage-sqlite`) | Daemon 使用工作流运行、调度、通知和专注会话的持久化接口。Vault 使用完成意图、提交、游标读取和快照验证接口。SQLite 连接由适配器持有。 |
| `mcbtodo-application` (`crates/application`) | 共享 Vault 业务命令、查询投影，以及工作流校验、模拟和 revision 发布用例。依赖 domain、Vault 和 workflow。 |
| `mcbtodo-client` (`crates/client`) | CLI 与 TUI 共用的 Daemon 调用、提案队列、Agent 配置摘要及少量共用格式化。 |
| `mcbtodo-tui` (`crates/tui`) | 终端会话、按键与命令输入、展示模型、Ratatui 渲染和交互表单。 |
| `mcbtodo` (`apps/mcbtodo`) | 命令行参数解析、默认 TUI 装配、CLI、MCP 和备份恢复入口。 |
| `mcbtodo-daemon` (`apps/mcbtodo-daemon`) | 单用户守护进程。管理计时、持久化调度、审批和工作流执行；为每个可执行节点启动并监督一个 Worker 子进程，独占运行队列及结果提交。 |
| `mcbtodo-worker` (`apps/mcbtodo-worker`) | 按单次 attempt 运行的 Linux Worker。通过受限协议接收固定输入，复用 Agent 与脚本执行适配器，不拥有队列、Vault 或运行数据库。 |
| `mcbtodo-xtask` (`xtask`) | 仓库检查、隔离及 systemd 冒烟测试、发行打包、开发用 AI harness 与 workflow 的选择和准备；仅供仓库开发，不发布到 crates.io。 |

crate 的完整命名规则见[命名规范](naming.md)。
所有文档的用途和变更同步规则见[文档索引](README.md)。

定时规则定义和编译由 `mcbtodo-workflow` 持有。Daemon 注册时与 SQLite
恢复调度时共用 `Trigger::clock_schedule`，协议使用同一套可序列化类型。
SQLite 将生成运行和推进调度游标放在同一事务内。

默认 TUI 首次启动会调用 `WorkspacePaths::initialize` 准备配置、状态和空 Vault；
显式 `init` 使用同一幂等路径。每次通过平台 API 读取用户配置时，都会检查
`.config` 与 `mcbtodo` 配置目录是当前用户拥有的真实目录，且不允许组/其他用户写入；
`mcbtodo` 配置目录还必须对组/其他用户不可访问。配置文件以 no-follow
方式打开，并检查属主与禁止组/其他用户写入。Unix 上主配置、键位和 Agent
相关配置共用描述符相对的读取实现。主配置与键位文件上限为 1 MiB，Agent
策略与 Codex 配置上限为 64 KiB，并各自执行内容校验。Agent 连接独立校验
socket 和对端身份。
首次初始化的默认配置通过同目录临时文件与 no-replace 原子重命名发布，
避免并发初始化时将暂存文件与正式配置同时保留为硬链接；已存在的有效配置不被覆盖。
`.mcbtodo` 下私有应用目录也检查属主、目录类型和 Unix 权限。首页状态文件仅能
读写为 `state/` 的直接子项；读取采用 no-follow、有界打开并检查属主和权限，
写入通过同目录临时文件原子替换。这些检查不等于
对所有祖先目录竞态、硬链接、任意外部 Vault 或所有运行时文件读写提供完整隔离；
相关限制与后续验收项见[架构验收评估](architecture-assessment.md)。

CLI、TUI 和 Daemon 在入口解析 `WorkspacePaths` 并传给下游组件。
Daemon 的 watcher、提醒和 WorkUnit 完成事件触发器绑定默认 Vault。
Daemon 和本地模式的 TUI 分别使用 Vault 的去抖文件监听，校验并保留最后有效快照。连接模式的 TUI 在后台查询 Daemon 的 Vault 变更标记。
Daemon 的提醒扫描从其监听快照取得定义，每次客户端投影查询则单独加载
当前 Markdown。工作流的 `task.create` 由 Daemon 事件循环通过共享应用
用例写入默认 Vault。

## 执行进程边界

Daemon 为每个 Script 或 Agent 节点的 attempt 启动独立的本机 Worker。
Daemon 负责排队、授权、预算、状态推进和结果提交。它通过有界协议提供绑定
不可变 workflow revision 的源码字节。RunStore 持久化 Started 后，Daemon
才发送 Start。结果须匹配 attempt 的 run、revision、node、步骤与 epoch，
由 Daemon 持久化。Worker 通过共享执行适配器执行节点，队列、Vault 写入与
运行数据库由宿主或 Vault 自身管理。

Daemon 监督子进程、取消并回收 Linux 进程组。Worker 启动时安装父进程死亡
信号并核验 Daemon PID。未决 attempt 会在启动恢复时失效，已释放的工作若
结果不明则记录 `outcome_unknown` 并禁止自动重放。epoch 只约束状态提交，
已发生的外部副作用仍需核对。进程故障的本机测试、打包二进制与替身的覆盖范围
见[架构验收评估](architecture-assessment.md#milestones)。协议与单机目标
见[设计第 11.3 节](design.md#113-首发-worker-协议与故障边界)。

WorkUnit 完成时，`mcbtodo` CLI 会先核对每条工作流步骤引用的 revision 绑定和已持久化 trace；仅 `trace.status = "succeeded"` 可作为完成证据。失败、跳过、缺失或畸形 trace 在调用应用层写入命令前拒绝，因此不会改变 WorkUnit 状态。

CLI 的 Vault 写命令从已加载快照取得乐观并发控制 token 时使用 application 的 `document_revision_in_snapshot` 投影；写命令仍将该 token 传给 Vault，由 Vault 在 writer lock 下重验。该投影避免适配器直接访问 Vault revision API，也避免为读取 token 加载第二份快照。

焦点报告的 Project 存在性检查、WorkUnit 所属信息和估时现在由 `VaultQueries::focus_report_context` 从同一个已验证快照提供，避免 CLI 在连续查询间读取到不同版本的 Markdown。按日向守护进程读取计时记录、跨日聚合和 Markdown/CSV/JSON 呈现仍属于报告适配器。

## 适配器边界

| 入口 | Vault 访问路径 |
| --- | --- |
| CLI | `validate`、`outline`、`graph`、`vault` 和默认的 `calendar` 使用本地 Vault。`calendar --daemon` 只查询默认 Vault 的单日投影，并拒绝与 `--vault` 或 iCalendar 导入导出组合。计时、通知和工作流运行命令使用 IPC，工作流校验、模拟和发布通过本地应用用例。 |
| Linux TUI，默认 Vault | 启动时建立服务投影缓存。Month、Week、Day、Agenda、Timeline、大纲、全局搜索、Inbox 列表及分流、Graph 浏览及依赖预览和确认走 IPC。Task 捕获、Project、Deadline 快速创建、全天事件、单次提醒、WorkUnit 时间块和现有截止日期编辑也走 IPC。默认 Vault 的文件监听由 Daemon 持有。 |
| Linux TUI，外部 Vault，以及非 Linux TUI | 使用本地 Vault 投影和可用的前台编辑功能。外部 Vault 没有独立后台服务。 |
| MCP | stdio 服务通过共享用例读取选定 Vault。在支持私有队列的平台，业务写入提案需人工批准。批准工作流快照只发布内容寻址 revision，执行授权与入队各有独立入口。 |

CLI 和 TUI 共用[客户端 IPC 模块](../crates/client/src/daemon_client.rs)中的
请求处理。TUI 通过该模块判断当前 Vault 是否由 Daemon 管理。写入处理保留
修订冲突、拒绝和提交结果未知的区别。
帧编解码和同用户身份检查仍由协议和平台层负责。查询缓存和请求分派见
[TUI 的查询缓存](../crates/tui/src/tui/daemon_views.rs)与
[Daemon 的请求分派](../apps/mcbtodo-daemon/src/server.rs)。

截止日期表单接收已读出的日期和 revision，表单按键只改变草稿或发出刷新和
提交动作。TUI 交互循环处理打开、刷新和提交，连接会话通过 Daemon 查询和提交，
本地会话通过 Application 执行对应操作。修订冲突后按 `r` 从当前数据来源
重读 revision，保留用户草稿并要求再次确认。见
[表单](../crates/tui/src/tui/deadline.rs)和
[动作处理](../crates/tui/src/tui/deadline_io.rs)。

服务端每个投影查询重新加载一次有效 Vault 快照。TUI 按查询参数缓存行或错误，
供渲染与选择共用。手动重载、写入后重载或收到 Daemon 变更标记会清除缓存。
服务不可用时这些视图报告错误。日历查询最多覆盖 366 个含首尾日期，结果
超出单个 64 KiB 帧时返回错误。大纲、Inbox 和 Graph 可在同一连接传送
有序多帧，客户端收齐后才交付结果。查询索引、服务端变更订阅、GUI 和
桌面启动器入口仍待实现。帧契约见 [IPC 编解码](../crates/protocol/src/wire.rs)
与[客户端](../crates/protocol/src/client.rs)。

## 应用边界的演进

`mcbtodo` 继续作为 Cargo 默认入口，在终端中启动 `mcbtodo-tui`。Linux 默认工作区的
业务读写逐步集中到 Daemon，Vault 继续持有 Markdown 的解析和一致性规则。
TUI 与未来的 GUI 共用面向业务的客户端调用，分别维护导航、选中项、草稿与
渲染。显式外部 Vault 和现有非 Linux 会话保留本地入口，复用 Application
用例。连接模式遇到服务错误时展示连接状态。

连接会话用文档 UUID 向 Daemon 查询当前 Markdown 路径。打开编辑器和恢复终端
由 TUI 负责。本机 IPC 保留原始 Unix 路径字节，搜索列表中的显示路径用于匹配。
手动刷新和 Daemon 变更标记都会使服务投影缓存失效，并按 UUID 或日历实例恢复焦点。
Daemon 每次启动生成新的标记，检测到文件变化后更新标记，包括 Markdown 校验失败时。
客户端以约一秒间隔在后台查询标记，断线和重连也触发刷新。每个投影仍独立查询，
文档写入继续使用所见 revision。显式外部 Vault 由本地监听触发快照重验。
提案的终端审阅与确认由 `mcbtodo-tui` 提供，CLI 共用这一入口。客户端的队列
接收已确认的记录，持锁复核记录并提交。

当前 TUI 会话负责管理终端、分派动作和
处理编辑器交接。[数据收集](../crates/tui/src/tui/screen_data.rs)访问当前
视图所需的行和表单上下文，[ScreenModel](../crates/tui/src/tui/screen_model.rs)
接收这些数据及交互状态。[Ratatui 绘制](../crates/tui/src/tui/render.rs)
使用模型与主题。数据收集仍在终端线程同步运行，缓存未命中时会等待 IPC。
见[终端会话](../crates/tui/src/tui/session.rs)。

### 命令与事件

TUI 的快捷键、菜单和 `:` 命令逐步映射到同一组类型化动作。比如 `:view workflow`
和视图菜单调用相同的视图切换，`创建 → 时间块`、`时间操作 → 时间块` 和
`:time-block` 打开同一个表单。导航、焦点和草稿由各前端维护。CLI、TUI 与未来
GUI 的业务写入复用应用用例或已有的 Daemon 请求，由界面解析选中对象后明确
传递 UUID、操作参数和所见 revision。终端尺寸变化、文字输入、文件监听和
异步响应仍由各自的事件处理路径消费。
见[TUI 命令](../crates/tui/src/tui/command.rs)。

### 插件扩展

内建 Agent 和其他适配器通过 `mcbtodo-ports` 扩展。独立进程扩展有真实调用方后，
拟由 `mcbtodo-plugin` 定义宿主与插件之间的消息契约。首个扩展点可以是提供
一种 Agent 能力的独立进程。宿主通过 stdin/stdout 发送有大小上限的 JSON
消息，用请求 ID 关联应答和取消，并在启动时协商协议版本与能力。诊断走
stderr。Daemon 负责授权、预算、修订校验及结果提交。CLI、TUI 和未来 GUI
使用独立的业务客户端契约。

按一条真实的读写路径推进，每阶段校验数据来源、修订冲突、断线行为和现有
快捷键。

1. **表单和客户端。** 截止日期和时间块已分别复用 Daemon 的编辑上下文和
   大纲投影。时间块表单在打开时保存候选行，输入、渲染和提交共用这份快照。
   写入冲突时保留草稿，重新打开表单后可按刷新后的大纲重新选择。启动计时
   和创建提醒也用大纲判定所选对象的类型，读取失败时显示错误。打开编辑器前
   通过服务查询当前文档路径。
2. **查询和呈现。** `ScreenModel` 已从内存数据构造。下一步由交互循环持有
   展示和选中共用的数据，将请求移到终端循环之外。切换视图时只采用与当前
   查询对应的响应。读取失败可重新加载，提交结果未知时引导用户核对数据。
3. **连接模式的启动和刷新。** 会话启动时选择本地 Vault 快照或服务投影。
   连接模式可在本地 Markdown 校验失败时进入界面，显示服务的查询结果或连接
   状态。Daemon 提供统一的变更信号。客户端主题、按键和窗口
   状态由本机管理。提案队列、Agent 策略和备份各自保有独立权限和一致性边界。

CLI 入口 [main.rs](../apps/mcbtodo/src/main.rs)负责参数解析、工作区初始化及
命令分派。按业务用例收敛命令实现和客户端接口。未来 GUI 和第二个真实调用方
出现时，再从应用内提取实际共用的客户端接口。

本地写入和 IPC 写入共用 `VaultCommands` 与 Vault 的写锁及 revision 校验。
例如 Task 捕获使用大纲行里的 Project revision，Graph 确认与时间块写入
使用所观察的 WorkUnit revision。截止日期表单由服务一次读取原值和 revision，
写入时重验 revision。Daemon 写入尝试遇到锁争用时报告冲突。
单次提醒和时间块以 Daemon 配置时区检查偏移。提交结果不明时，TUI 提示
核对 UUID 或时间范围。Inbox 分流的两文件发布和中断恢复见
[存储与同步](storage-and-sync.md#transactions-and-compatibility)。

其余入口按实际场景共享用例：

| 场景 | 共享边界 | 入口差异 |
| --- | --- | --- |
| Vault 查询 | CLI、MCP 和 Daemon 使用 `VaultQueries` 的拥有型投影。本地 TUI 使用快照投影函数。 | 各入口选择过滤参数、交互方式和输出格式。`doctor` 与 `validate` 共用 `VaultQueries::validate`。 |
| Graph Diff | application 负责候选 DAG、预览及发布前验证；Vault 在锁内再次验证并发布。 | MCP 可预览并提交提案；TUI 展示和审批；CLI 暴露相应命令。 |
| iCalendar 导入 | application 对传输无关的事件草稿执行文本约束，Vault 原子发布整批事件。 | RFC 5545 解析和文件读写仅属于 CLI 导入/导出适配器。 |
| 专注报告 | application 从同一 Vault snapshot 提供并校验 Project/WorkUnit 元数据。 | CLI 读取 daemon 计时记录、聚合日期范围并渲染报告。 |

不把“每个适配器都暴露同一功能”作为架构目标。某入口缺少某项能力只有在产品明确承诺该入口支持该场景时才是缺口；新增入口时，应对它实际暴露的共享业务场景补充一致性测试。守护进程的事件分页、SQLite 状态、提案队列、协议解码、终端呈现和文件格式解析分别留在其拥有层，不为形式上的统一而移入 application。

默认 Daemon 只监控默认 Vault，因此 CLI、TUI 与 MCP 在创建提醒前都要求所选 Vault 的 canonical 路径与默认 Vault 完全一致。外部 Vault 的其他前台编辑仍可使用；默认 Vault 的符号链接别名不构成外部 Vault。

## 稳定端口与适配器契约

| 端口 | 生产适配器与调用方 | 契约边界 |
| --- | --- | --- |
| `AgentProvider` | Worker 中的 Linux `UnixSocketAgentProvider` 与 Codex app-server 适配器 | capability、revision、结构化输入、期限和宿主取消状态进入同步调用。输出归一化后由业务层校验。 |
| `LocalTransport` | Linux Unix socket 适配器，供 Worker 连接本地 Agent、CLI 和 TUI 连接 Daemon，以及 Daemon 包装已接收的连接 | 适配器提供系统验证的对端身份、绝对期限和有界读写。调用者授权同 UID 后才传输数据。Daemon 客户端整次请求使用三秒期限。Daemon 保有监听端点、连接授权和请求调度，服务端的请求读取和响应写入各有一秒期限。 |
| `ProcessLauncher` / `ProcessController` | Daemon 使用 Linux Worker 进程控制器 | 启动、退出检查、进程组终止与回收。Daemon 决定何时发布 Started、发送 Start 和接收结果。 |
| `Sandbox` | Worker 注入 `Bubblewrap`，工作流脚本执行路径消费该端口 | 同步执行已发布的脚本与固定可执行字节。适配器负责隔离、执行限额和进程回收。可证明脚本未释放的启动错误以失败步骤记录。释放令牌传输或后续基础设施错误如果导致结果丢失，Daemon 记录 `outcome_unknown`。 |
| `WorkflowRunStore` | Daemon 事件循环调用 `mcbtodo-storage-sqlite::run_store::RunStore` | 单一宿主串行调用。脚本领取和 Agent 带预算领取均走此端口。领取、启动、提交及结束核对 Attempt 身份。轨迹、工件、预算结算和状态推进共同提交。 |
| `WorkflowQueueStore` | Daemon 事件循环调用 SQLite `RunStore` | 在同一读快照计算最早唤醒时间。Worker 忙碌时仍交付到期时钟规则。单次至多推进一个规则，原子提交运行、计数和游标。读取到期运行后仍需经过宿主校验和 Attempt 领取。 |
| `WorkflowScheduleStore` | Daemon 管理命令调用 SQLite `RunStore` | 原子注册规则。停用在写事务内按 UUID 读取最新状态，保留已有运行、回执和计数。宿主负责发布版本校验和完成事件水位。 |
| `WorkflowProgressStore` / `ScriptAuthorizationStore` | Daemon 调用 SQLite `RunStore` | 宿主推进依据带 revision 的运行快照。活跃 Attempt 的结果由 Worker 提交路径推进，宿主仍可请求同节点取消。脚本授权绑定发布 revision 和节点。 |
| `CompletionEventStore` | Daemon 完成事件调度使用默认 Vault 适配器 | 注册读取的水位在锁争用时立即返回 `Busy`。后台读取最多 64 条已提交事实，游标排除起点。 |
| `CompletionDeliveryStore` | Daemon 完成事件轮询器使用 SQLite `RunStore` | 事件循环读取最早的活跃游标。交付方重新核对规则，将运行、去重回执和游标合并在同一事务中。提交失败时从持久化游标重读。 |
| `NotificationStore` | Daemon IPC、提醒扫描和通知调度调用 SQLite `RunStore` | 分页读取、终态水位和持久化提醒 outbox。提醒消息与待投递条目原子创建，工作流终态通知随运行状态提交。 |
| `NotificationSink` | Daemon 投递线程使用 `NotifySend` | 适配器投递单条纯文本通知。成功表示通知程序确认提交。Daemon 通过仓储领取、确认、重试和恢复，故障恢复可能重复投递。 |
| `FocusSessionStore` | Daemon Timer 使用 SQLite `FocusStore` | 原子保存检查点，并按会话开始时间的本地日期生成报告。持久化成功后 Timer 才切换会话状态。 |

Worker 的 Agent 宿主读取用户策略，核对 capability 与 revision，并收紧调用
deadline。`UnixSocketAgentProvider` 负责 socket 路径和同用户身份校验、协议帧
大小、限时 I/O 和响应结构。Daemon 保持预算、幂等、结果提交和
`outcome_unknown` 的所有权。Worker 按授权解析出的路由构造适配器，由适配器在启动
或连接时检查执行条件。连接失败会终止调用。

Daemon 对 Script 和 Agent 使用共享的有界 Worker 槽集合。默认配置为一个槽，
Linux 上 Daemon 启动时读取 `config.toml` 的 `worker_slots`（1 到 4）。启动参数
`--worker-slots` 优先。读取错误会记录在服务日志中，Worker 容量采用默认值，
Vault 校验仍使用原有配置规则。空闲时按到期时间及入队顺序
读取下一项 `pending` 运行，由宿主提供当前时间。运行状态及摘要由
`mcbtodo-workflow` 拥有，IPC 沿用原有 JSON 字段。`Prepare`、`Ready` 和 `Start`
使用同一个五秒握手期限。后台线程写入 `Prepare` 并等待 `Ready`，Daemon
循环轮询握手结果，继续服务 IPC 和计时检查点。Daemon 负责持久化 Started
并发送有界的 `Start`。未完成 `Prepare` 或 `Ready` 的 Attempt 记录为失败。
等待期间接受取消时，仓储在同一事务内作废未 Started 的 Attempt 并提交
Cancelled，Daemon 回收 Worker。持久化 Started 后的发送失败进入
`outcome_unknown`。活跃 Worker 的取消使用有界 `Cancel` 帧，发送失败时
回收进程并记录未知结果。

Daemon 在持久化 Started 后、发送 Start 前确定两类执行期限的共同计时起点。
Agent 使用事务中预留的精确时间额度。脚本使用节点超时加两秒上报窗口，
供沙箱完成清理和返回诊断。额度包含 Start 发送时间。Start 帧传输使用握手、
控制帧与执行截止时间中最早的一项。超过宿主
期限后，Daemon 拒收结果并发送 Cancel。Worker 在两秒内仍未退出时，Daemon
终止进程组。已释放的 Attempt 记为 `outcome_unknown`，释放执行槽并禁止重放。
Daemon 循环取正常轮询间隔和当前监督阶段剩余时间的较小值。发送取消后，
退出宽限期取代执行期限参与唤醒。回收后读取终态期间仍检查执行期限。
进程回收时间取决于操作系统。

Daemon 启动时打开工作流和专注数据库，并向 Runtime 和 Timer 注入业务仓储。
`RunStore` 组合调度、完成事件消费、依赖交付、预算、工件和通知持久化，
保持原有事务范围。`FocusStore` 保存专注会话及中断恢复检查点。计时模式和
实时工时计算由 Timer 管理。
Daemon 通过 `WorkflowRunCatalog` 查询、分页和幂等入队。SQLite 在写事务中
创建或读取同一 UUID 的运行。Daemon 校验发布版本，并检查返回的运行与请求
在 revision、计划时间和触发来源上是否一致。
`WorkflowHistoryStore` 返回有类型的轨迹、工件元数据和有界数据块。
`NotificationStore` 提供分页、提醒领取和终态通知水位。通知 DTO 由端口定义，
IPC 沿用已有 JSON。Daemon 的生产依赖无需直接引入 `rusqlite`。
Vault 内的 SQLite 事件日志仍随 Markdown
写入协调，CLI 备份和恢复保留独立的快照校验。
Agent 领取使用宿主从当前修订和步骤历史推导的已执行标记。没有预算账户且已有
Agent 执行记录时，仓储拒绝领取。预算拒绝类型由端口定义，SQLite 适配器
在同一事务内验证步骤数、预留预算并发布领取结果。

`mcbtodo-agent-codex` 实现同一个端口。生产 Worker 支持在策略中显式选择 Codex，
通过 Bubblewrap 启动 app-server，执行协议握手，并把隔离回环上单次有界模型请求
转交给已认证的同用户 Unix 模型服务。Daemon 保留审批及 durable Attempt 的控制权。
Linux AArch64 离线探测覆盖生产链路。`AgentPolicy` 为每个 capability/revision
授权解析唯一 Provider 路由，Worker 只构造被选中的适配器。这个路由边界
只提供选择，不授予 Policy、revision、budget、deadline、cancellation 或最终结果提交
权限。Daemon 的 Worker 执行槽统一限制 Script 和 Agent 的并发。跨请求优先级、
独立资源预算、在线上游凭据和独立进程插件协议仍有各自的验收门禁。
Worker 在共享的 Agent 结果接纳路径执行可选的 `max_output_bytes` 策略。
额度按归一化后的紧凑 JSON `outputs` 对象计 UTF-8 字节。超额结果进入
`outcome_unknown`，Daemon 保留 Attempt 与预算预留。
参见 [Agent 适配器配置](agent-adapter.md#codex-app-server)。

### Provider 路由

当前路由边界由 `AgentPolicy` 和 Worker 共同承担。策略可以在
`[providers]` 中配置 `unix_socket` 与 `codex`，每个 `[[allow]]` 可以显式指定
Provider。未指定时使用顶层默认 Provider。Worker 在授权后只构造选中的适配器，
由适配器在启动或连接时检查执行条件。Codex 在启动前校验配置绑定的 capability，
Unix socket 将已授权的请求交给通过身份校验的 peer。Provider metadata
用于描述能力和标识不确定结果。
Linux TUI 可以修改默认 Provider、预览跟随默认路由的授权数，并编辑已有
Provider 的 socket 路径。两项编辑都通过同一个配置发布入口写入 `agents.toml`，
由配置快照识别并发编辑。显式授权仍通过 `agents.toml` 手工配置。
Provider 失败不会触发隐式 fallback，宿主仍保留 revision 授权、预算预留、
deadline、cancellation、幂等语义、`outcome_unknown` 判断和最终结果提交。

当前路由是同步且按调用创建的边界，没有 Provider fallback、重试或生命周期监督。
SQLite、Sandbox 和 Notification 仍由各自的拥有层管理。Daemon 循环直接使用共享
Worker 槽集合的占用状态计算队列唤醒时间，并在读取下一条待执行运行前判断是否有
空位。所有槽被占用时，后续任务保持 `pending`，尚未领取 Attempt 或预留 Agent
预算。每个槽的取消、进程监督和结果接纳彼此隔离。当前调度能力是本地有界并发，
不是通用 ResourceScheduler。
SQLite 负责持久排队，`WorkflowRunStore` 原子预留和结算 Agent 预算。
独立进程插件协议仍需验证实际的进程扩展需求。

### SQLite 仓储能力

Daemon 启动时要求 `WorkflowRepository` 保证同一操作内的多记录原子提交，
以及完成事件游标随对应运行和收据持久化。缺少任一保证时，启动错误会指出
后端与缺失能力。SQLite 是目前接入该门禁的后端。

Daemon 作为单写者顺序调用运行仓储。仓储不提供 Watch 或同步导出接口。
事务、数据库版本和备份快照规则见[存储与同步](storage-and-sync.md)。

### 平台验证范围

| 能力 | Linux AArch64 / x86_64 | macOS ARM64 | Windows x64 |
| --- | --- | --- | --- |
| 前台 CLI、Vault 和 TUI | 本地路径和默认 Vault IPC 路径 | 离线前台路径 | 离线前台路径 |
| 私有提案与备份、恢复 | Unix 实现 | Unix 实现 | 明确拒绝。MCP 工具发现仅提供只读工具 |
| Daemon、Worker、IPC 与 systemd 用户服务 | Linux 实现 | 不支持 | 不支持 |
| 工作流 Agent、Bubblewrap 与桌面通知 | Unix socket、Codex app-server、Bubblewrap 与 `notify-send`，可用性取决于本地配置及环境 | 不支持这些 Linux 后端 | 不支持这些 Linux 后端 |

[CI 配置](../.github/workflows/ci.yml)包含两种原生 Linux 发行架构、macOS 与
Windows 原生前台测试，以及独立的交叉类型检查。各测试实际覆盖范围与运行记录见
[架构验收评估](architecture-assessment.md#native-ci-verification-follow-up-2026-11-23)。
`notify-send` 的 helper 测试覆盖提交行为。实际桌面通知显示需要桌面会话验收。
Linux Daemon 和 `doctor` 共用通知适配器的会话检测。`doctor` 被动检查当前进程的
会话总线配置和 `PATH` 中的可执行 helper。Daemon 继续在后台队列中按需启动
`notify-send`，提醒 outbox 负责重试及重启恢复。
Linux `doctor` 与执行适配器共用 Bubblewrap、Bash 和 Nushell 的安装预检。
预检检查固定路径、所有权及执行权限。依赖加载、命名空间启动和隔离由实际沙箱执行验证。
macOS 和 Windows 的 `doctor` 明确报告 Linux 专用检查为 `unsupported`，
离线前台功能仍可根据 Vault 自身的校验结果使用。

Linux Daemon 每次接纳一条本机 IPC 连接。该连接的线程负责同 UID 校验和帧读写，
请求与响应各有一秒的传输期限。事件循环接收已解码的请求并执行命令，
并在客户端传输期间继续监督 Worker、保存计时检查点和调度提醒。其他客户端
在监听队列中等待当前连接释放。

## 数据权威与恢复

| 路径 | 内容 |
| --- | --- |
| `~/.config/mcbtodo/config.toml` | 日历显示时区、首页策略和 TUI 主题。省略字段时使用 `AppConfig` 的默认值。`--timezone` 可覆盖指定客户端的投影时区。 |
| `~/.config/mcbtodo/agents.toml` | 本机 Agent 适配器的能力、发布摘要、调用时限和可选的结果字节额度。 |
| `~/.config/mcbtodo/codex.toml` | 可选 Codex 后端的可执行路径、单一能力、指令与输出 schema。备份恢复后单独配置。 |
| `~/.config/mcbtodo/keymap.toml` | TUI 全局导航和视图切换键位覆盖。 |
| `~/.mcbtodo/vault/` | 默认 Markdown Vault 与该 Vault 的完成事件日志。 |
| `~/.mcbtodo/state/` | 专注与工作流 SQLite 库、已发布 revision、私有提案和本机运行文件。Daemon socket 与进程锁也在此目录。 |
| `~/.mcbtodo/state/last-view` | 仅在 `home_view = "restore_last_view"` 时使用的最近一次正常 TUI 视图。缺失或值未知时从 Month 启动。 |
| `~/.mcbtodo/cache/agents/` | `cargo xtask agent` 生成的上下文和经过摘要验证的文档下载缓存。Unix 平台 API 使用目录文件描述符逐级验证并相对已打开目录读写，拒绝静态符号链接、非普通文件、多硬链接和不安全权限；非 Unix fallback 仍使用路径检查。 |

上述配置与应用数据路径固定相对于用户 home；当前实现不读取 `XDG_CONFIG_HOME` 或 `XDG_DATA_HOME` 来重定位。

TUI Settings 是 `config.toml` 中普通偏好的交互式编辑入口。General、
Appearance 和 Linux 专有的 Daemon 分组共用一个 `AppConfig` 草稿和一套字段焦点。时区文本在提交时解析，
校验失败时焦点回到时区字段，便于修正原始输入。保存动作直接从当前草稿生成
配置，平台层使用打开设置时读取的配置作为并发比较基线。
打开子页、保存和重载通过同一个设置命令入口。`keymap.toml` 的键位草稿由
Keyboard 页独立保存。文件发生冲突时，`F5`
从磁盘重建当前页草稿。Agents 页的 `F5` 刷新 `agents.toml` 的路由摘要。
Linux 上从 Agents 按 `F4` 可编辑 `codex.toml`。编辑器与 Worker 共用配置类型
及字段校验，本地安装诊断由显式运行的 `doctor` 提供。运行期间无法读取
`config.toml` 时，仅偏好页不可编辑，
Keyboard 和 Agents 会独立读取各自的文件。启动修复直接使用偏好编辑器。

由 Vault 重建 Inbox 列表、日历、大纲和任务图。CLI `vault capture-inbox` 无需 Project 即可捕获，`vault triage-inbox` 以 revision 条件将条目转换为 Task，保留来源条目及其原始内容。备份范围、恢复与设备间交换见[存储与同步](storage-and-sync.md#backup-versus-multi-device-exchange)。

守护进程重启时，未完成的计时会话保留最后一次检查点并进入 `needs_review`。正在执行的工作节点进入 `outcome_unknown`，等待用户核对可能已经发生的副作用；`mcbtodo workflow resolve <run-id> --outcome completed|failed|not-started --note <说明>` 会向独立审计表写入一次性人工观察，运行生命周期仍保持 `outcome_unknown`。核对不是机器验证，不补造步骤轨迹、不重放任务，也不触发 Dependency 调度；确认未启动时，操作者可另行创建新运行。运行摘要含核对结果、说明与记录时间。运行中的取消请求会尝试中止执行；若取消与活动工作竞争，无论执行端没有返回还是迟到返回结果，Daemon 都不提交该结果、不继续后续节点，而将运行置为 `outcome_unknown`，因为取消请求和进程停止都不能证明外部副作用已撤销。结果写入事务失败时，attempt、产物、轨迹及运行推进一并回滚；运行时随即将仍为 Running 的运行转为 `outcome_unknown`，重启后也不会重放。待定计划与等待人工决策的节点可继续。Dependency 调度只在运行真实进入成功终态的事务内写入下游队列，并按规则和上游运行去重。

完成事件调度在注册时记录默认 Vault 的事件水位。游标交付、事件导入导出
及各库的事务边界见[存储与同步](storage-and-sync.md)。

## 执行与部署

运行绑定发布摘要。脚本的执行许可同时绑定节点与摘要。`mcbtodo-execution` 的 Bubblewrap 沙箱在宿主 Linux 上隔离运行 Bash 或 Nushell，执行配置限制输入、输出、时间与可见资源。`process.exec` 允许节点使用一个图目录内的本机 ELF。发布时将字节写入快照，执行时将其只读挂载到 `/work/executable`。脚本与可执行文件合计上限为 2 MiB，其中脚本合计上限为 1 MiB。Agent 通过同 UID 的本机 Unix socket 接收类型化绑定输入。Agent 的网络和凭据由适配器进程管理。Daemon 通过独立 Worker 启动这些受限执行器并监督结果；仍未验收的进程故障边界见上文及[架构验收评估](architecture-assessment.md)。[Agent 适配器](agent-adapter.md)

GitHub Actions 为两种架构配置原生 Linux runner，执行 `cargo xtask check`、示例 Vault 校验和锁定依赖的发行构建。CI 安装 Bubblewrap 和 Nushell，以覆盖脚本沙箱及其受支持的解释器；从发行包解出应用和守护进程后运行 `cargo xtask smoke --sandbox`，并核对打包的 [systemd 用户服务](../deploy/systemd/README.md)。可选的 `cargo xtask smoke-systemd` 使用 Bubblewrap 和可访问的 systemd 用户管理器，JSON 校验由 `xtask` 完成。Sandbox smoke 覆盖隔离、Agent 决策和进程中断恢复；它仍是确定性 fixture，不替代真实桌面环境和长期使用验收。

构建与验收由 CI 执行，发布由独立流程授权。发布版本及各渠道状态见
[发行说明](../distro/README.md#release-status)。第三方材料审查及包元数据
要求见[许可与第三方材料](licensing.md)和[命名规范](naming.md)。

TUI 的呈现、导航和焦点规则见 [TUI 设计系统](tui-design.md)。日历与任务
操作见[用户指南](../README.md#日历和任务)，交互测试与剩余验收见
[架构验收评估](architecture-assessment.md#milestones)。
