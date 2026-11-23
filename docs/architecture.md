# mcbtodo 架构

mcbtodo 当前面向 Linux AArch64 和 x86_64。Markdown Vault 保存用户的项目和计划。守护进程独立保存已确认的运行状态，终端客户端可随时退出。仓库 CI 在这两种 Linux 架构上构建和测试；其他操作系统尚未列为支持目标。

## Cargo workspace

| 组件 | 职责 |
| --- | --- |
| `mcbtodo-domain` (`crates/domain`) | UUID、Inbox entry、任务状态、时间语义、日历投影和 WorkUnit DAG。边从前置单元指向依赖它的单元。 |
| `mcbtodo-vault` (`crates/vault`) | Markdown 与 YAML front matter、Inbox/任务等引用校验、预览和带修订条件的写入。Vault 持有写锁并协调 Markdown 发布与完成事件恢复。 |
| `mcbtodo-workflow` (`crates/workflow`) | 类型化图编译、发布快照、确定性轨迹重放和模拟。文件发布后由摘要标识。 |
| `mcbtodo-execution` (`crates/execution`) | Linux Agent socket、Bubblewrap 脚本沙箱和 `notify-send` 适配器。执行代码与 Daemon 的状态所有权分离。 |
| `mcbtodo-ports` (`crates/ports`) | Agent、本地传输、进程控制、脚本隔离、工作流 Attempt 持久化、完成事件读取、桌面通知和 Provider 选择的业务契约。 |
| `mcbtodo-platform` (`crates/platform`) | 用户路径和配置、Linux Unix socket 客户端及 peer 身份验证。 |
| `mcbtodo-protocol` (`crates/protocol`) | 本机 Unix socket 的计时和工作流请求与响应，以及 Daemon 与单次 Worker 之间有界、带 attempt 身份的消息契约。 |
| `mcbtodo-storage-sqlite` (`crates/storage-sqlite`) | Daemon 使用工作流运行、调度、通知和专注会话的持久化接口。Vault 使用完成意图、提交、游标读取和快照验证接口。SQLite 连接由适配器持有。 |
| `mcbtodo-application` (`crates/application`) | 共享 Vault 业务命令、查询投影，以及工作流校验、模拟和 revision 发布用例。依赖 domain、Vault 和 workflow。 |
| `mcbtodo` (`apps/mcbtodo`) | CLI、Ratatui、MCP、人工提案审批和备份恢复。 |
| `mcbtodo-daemon` (`apps/mcbtodo-daemon`) | 单用户守护进程。管理计时、持久化调度、审批和工作流执行；为每个可执行节点启动并监督一个 Worker 子进程，独占运行队列及结果提交。 |
| `mcbtodo-worker` (`apps/mcbtodo-worker`) | 按单次 attempt 运行的 Linux Worker。通过受限协议接收固定输入，复用 Agent 与脚本执行适配器，不拥有队列、Vault 或运行数据库。 |
| `mcbtodo-xtask` (`xtask`) | 仓库检查、隔离及 systemd 冒烟测试、发行打包、开发用 AI harness 与 workflow 的选择和准备；仅供仓库开发，不发布到 crates.io。 |

crate 的完整命名规则见[命名规范](naming.md)。
所有文档的用途和变更同步规则见[文档索引](README.md)。

执行隔离实现位于 `mcbtodo-execution`。该 crate 提供 Agent 协议和脚本
沙箱；它不拥有运行队列、RunStore 或工作流结果提交。Daemon 已通过 Worker
协议启动并监督独立子进程；Worker 复用这些执行 API，Daemon 仍是唯一的状态
与结果权威。Daemon RunStore 会核验 attempt 身份并原子提交结果，但它不是独立
租约服务；Worker 外部副作用无法由数据库 epoch 单独 fencing。

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

CLI 在命令入口解析一次 `WorkspacePaths`，将同一实例传给 Vault、工作流、提案、备份及运行时命令处理器；工作流 revision 存储、完成证据校验和 IPC socket 因此都由同一用户 home 派生，不在下游重新发现 home。TUI 与 Daemon 也在启动边界创建 `WorkspacePaths`，并向其组件传递路径所有权。CLI、TUI 和 MCP 共用 application 层的 Vault 查询与业务命令。application 从同一套 `TargetAction` 约束投影可选业务目标；TUI 的提醒与时间块选择器消费当前已验证快照的投影，最终写入仍携带观察到的 revision 并由 Vault 在写锁下重验。CLI 的工作流校验、模拟、发布和 revision 读取，以及提案批准时对已审核快照的发布，也经 application 的工作流用例进入 `crates/workflow`。`doctor` 的 Vault 完整性检查与 `validate` 命令也共用 `VaultQueries::validate`；doctor 额外负责环境与运行目录诊断，不直接打开 Vault 快照。MCP 的 Vault 写入请求进入待人工检查的提案队列。Inbox 可由 MCP 查询并以 expected revision 提议分流到 Project；审批通过后 Vault 发布稳定 UUID 的 Task，再更新 Inbox 来源条目。两文件操作不伪装成原子事务：中断后重试会验证已发布 Task 的精确内容，发现冲突时失败关闭。MCP 也可提交冻结的工作流图与源码字节快照；TUI 审阅该快照后，批准只将其发布为内容寻址 revision，不授予脚本授权或触发运行。已发布工作流也能创建任务，Agent 影响的写入由运行时审批控制。时间和工作流请求经 IPC 交给守护进程。守护进程与 TUI 共用 Vault 层的去抖文件监听；外部变更通过完整 Vault 加载校验，错误时保留最后有效快照。提醒 occurrence 扫描消费该快照投影出的 owned 定义，不另行读取 Vault；配置变更时丢弃旧快照扫描结果。生产 Daemon 仅从一个 `WorkspacePaths` 实例派生 state、socket、计时数据库及 Runtime 路径，避免组件意外分属不同 home。显式 `--vault` 选择只作用于前台 CLI/TUI；后台 Daemon 的 watcher、提醒和 WorkUnit 完成事件触发器绑定默认 Vault，不为外部 Vault 提供独立后台服务。工作流 `task.create` 动作在 Daemon 事件循环内执行，使用同一个 `WorkspacePaths` 查询 Project revision 并写入 Task，不授予执行 Worker Vault 写入能力；临时 workspace 集成测试覆盖该路径。

**执行架构目标与现状：**冻结目标是单机 `Daemon + 按节点启动的独立 Worker`。Daemon 是队列、授权、调度和运行状态的唯一权威；Worker 每个节点尝试启动一个进程，只执行绑定不可变 workflow revision 的工作，不自行调度、写 Vault/运行数据库或成为第二个状态库。生产路径已接通 revision 字节信封、Ready/持久化 Started/Start 门、结果身份校验、Daemon 结果提交、取消消息和 Linux 进程组终止/回收。Linux 测试覆盖终止 helper 对真实父子进程的回收、生产 Daemon 取消运行中的 sandbox 脚本，以及 Started 后 SIGKILL Worker：运行进入 `outcome_unknown`、不提交新步骤、Worker 被回收，重开 RunStore 后不重放。生产 `spawn_worker_at` 有畸形 readiness 帧测试。错身份 Result 回归测试由 fixture 子进程通过 stdout pipe 发出协议帧，再经 `read_worker_messages` 解码并交给生产 `poll_worker` 状态路径；它验证进程组回收、`outcome_unknown`、不提交步骤和重启不重放。该 fixture 并非打包 Worker 二进制。发行 smoke 还会从提取目录启动真实发行 Daemon，并用仅存在于开发 `xtask` 中的协议替身完成 Prepare/Ready/Start 后发送畸形 Result；它验证运行变为 `outcome_unknown`、没有步骤 trace 且 Worker 被终止回收。这覆盖发行 Daemon 的已启动后协议故障路径，但替身不是发行 Worker，不能证明发行 Worker 自身会产生畸形输出。另一个归档验收验证归档内 packaged Worker 执行 `exit 23` 脚本后，RunStore 保存失败运行、一个步骤和含退出状态的 trace 原因；这仅覆盖普通脚本非零退出，不覆盖 Worker 自身畸形输出或多故障组合。Daemon 在启动 Worker 时传入自身 PID；Worker 启动早期通过 `mcbtodo-platform` 的 Linux API 安装父进程死亡信号，并检查父 PID 竞态。Sandbox smoke 在脚本副作用出现后强制终止 Daemon，检查当时采样到的 Worker 后代均退出，再验证重启后运行进入 `outcome_unknown` 且不重放。这只证明本机采样进程树的退出，不撤销已发生的外部副作用，也不证明 Agent 所连接外部服务的状态。协议、故障边界和未完成验收见[设计第 11.3 节](design.md#113-首发-worker-协议与故障边界)。M5 仍为 Partial：发行 Worker 输出故障的组合验收，以及外部副作用结果核对仍未全部覆盖。分布式 Worker 集群、远程 Worker 和团队执行仍延期。

Daemon 的 `RunStore` 已为当前执行路径持久化 attempt：领取时分配单调 epoch，并绑定 run、revision、node 和预期步骤；启动转换只允许一次；结果事务在同一事务内校验完整绑定、提交 attempt 状态和运行结果。Daemon 启动时会使未决 attempt 失效，并将活动运行置为 `outcome_unknown`。attempt 身份也经 IPC 传给 Worker，并在回包时由 Daemon 核验；RunStore 的提交事务继续保证单写入状态权威。这不是可续期或自动过期的 Worker 租约，也不能阻止已发生的外部副作用。当前 supervisor 对已知子进程退出、协议失败和取消执行 fail-closed 处理；生产取消路径已有 Linux 真实工作负载、进程树停止和 Worker 回收测试。Started 后 Worker SIGKILL 的生产 supervisor 测试确认运行转为 `outcome_unknown`、不提交步骤且重新打开 RunStore 不会重放。Daemon 死亡 smoke 现验证 Worker 后代退出及运行状态恢复，但不构成外部系统副作用撤销或结果核对的证据。错身份 Result 测试由 fixture 子进程向 stdout pipe 输出帧，经真实协议 reader 和 supervisor 状态路径验证。Worker 进程测试和 `xtask smoke` 还会直接向所选 binary directory 中的真实 Worker 发送畸形 Prepare 帧，确认它以非零状态退出且拒绝输入前没有输出 Ready。发行归档 sandbox smoke 另验证 packaged Worker 的脚本非零退出和失败 trace 持久化；Daemon 收到畸形 Worker readiness 的归档验收仍使用故障注入替身，打包 Worker 的恶意/畸形输出及组合故障仍未覆盖，不能据此声称任意 Worker 故障已恢复。

WorkUnit 完成时，`mcbtodo` CLI 会先核对每条工作流步骤引用的 revision 绑定和已持久化 trace；仅 `trace.status = "succeeded"` 可作为完成证据。失败、跳过、缺失或畸形 trace 在调用应用层写入命令前拒绝，因此不会改变 WorkUnit 状态。

CLI 的 Vault 写命令从已加载快照取得乐观并发控制 token 时使用 application 的 `document_revision_in_snapshot` 投影；写命令仍将该 token 传给 Vault，由 Vault 在 writer lock 下重验。该投影避免适配器直接访问 Vault revision API，也避免为读取 token 加载第二份快照。

焦点报告的 Project 存在性检查、WorkUnit 所属信息和估时现在由 `VaultQueries::focus_report_context` 从同一个已验证快照提供，避免 CLI 在连续查询间读取到不同版本的 Markdown。按日向守护进程读取计时记录、跨日聚合和 Markdown/CSV/JSON 呈现仍属于报告适配器。

## 适配器边界

适配器共用领域语义和应用用例，不要求每个入口提供完全相同的命令集合或交互流程：

| 场景 | 共享边界 | 入口差异 |
| --- | --- | --- |
| 项目、任务、WorkUnit、Inbox 和日历查询 | CLI 与 MCP 使用 `VaultQueries` 的拥有型投影；TUI 从其正在展示的已验证 Vault snapshot 调用同一组投影函数，避免重复读取。 | 各入口自行选择过滤参数、交互方式和输出格式。 |
| 创建、完成、依赖、日期与提醒等 Vault 修改 | 共享 `VaultCommands` 的验证、revision 约束和 Vault 发布语义。 | MCP 修改请求先进入提案队列并由用户审批；CLI/TUI 可在本地用户明确操作后直接调用命令。 |
| Graph Diff | application 负责候选 DAG、预览及发布前验证；Vault 在锁内再次验证并发布。 | MCP 可预览并提交提案；TUI 展示和审批；CLI 暴露相应命令。 |
| 工作流发布与执行授权 | application/workflow 负责类型化校验、内容寻址 revision；授权和执行由独立运行时策略控制。 | MCP 捕获精确快照并请求审批，审批只发布、不授权或执行；CLI/TUI 提供本地操作。 |
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

### SQLite 能力与迁移

Daemon 启动时要求 `WorkflowRepository` 保证同一操作内的多记录原子提交，
以及完成事件游标随对应运行和收据持久化。缺少任一保证时，启动错误会指出
后端与缺失能力。SQLite 是目前接入该门禁的后端。

| 能力 | 当前约定 |
| --- | --- |
| 事务与原子批次 | Agent 预算预留与 Attempt 领取共同提交。工作流结果与 Attempt、预算结算和轨迹共同提交。完成事件消费与游标共同提交。提醒和 outbox 共同写入。跨数据库原子批次暂无契约。 |
| 持久游标 | 工作流完成事件消费游标随 `workflow.sqlite3` 保留。桌面弹窗的水位只在 Daemon 进程内使用。 |
| 并发 | Daemon 单写者顺序调用。其他进程不可同时作为写入权威。读写并存受到 SQLite 锁限制，宿主可见存储错误。 |
| 快照 | 由现有备份流程在工作区锁保护下保存和校验。仓储本身没有面向同步的导出接口。 |
| Watch | 仓储未声明该能力。Vault 文件监听独立于其事务。 |

旧版 `workflow.sqlite3` 的 `user_version` 为 0，首次打开时在同一初始化与恢复
事务中登记为 1。沿用原表、状态、游标和路径。失败时该事务回滚，保留原库供
检查或从现有备份恢复。无法识别的版本会被拒绝。`focus.sqlite3` 保留原有
版本 1 与 `needs_review` 恢复规则。

### 平台验证范围

下表按能力记录验证层级。类型检查覆盖目标 Rust 代码，原生测试覆盖该平台的链接与所列操作，
运行时集成测试才可作为进程、权限和隔离的证据。`待验证` 表示存在相应实现或 CI 流程，
`待提供适配器` 表示能力尚在规划，`不支持` 表示现有实现拒绝该调用。
静态支持与运行时可用性分别判断。工具缺失、配置无效或健康检查失败均不能授予能力。

| 能力 | Linux AArch64 | Linux x86_64 | macOS | Windows |
| --- | --- | --- | --- | --- |
| Domain、Ports、Platform、Protocol | 本地工作区测试通过 | `cargo check --tests` 通过，有原生 CI 配置 | ARM64 `cargo check --tests` 通过 | x64 MSVC `cargo check --tests` 通过 |
| 前台应用与 Vault | 本地 CLI、默认 TUI 和 Vault 验证通过 | CI 配置目标 | ARM64 前台应用及测试通过 Rust 类型检查。已配置原生 CLI 与 Vault 门禁 | x64 MSVC 前台应用及测试通过 Rust 类型检查。已配置原生离线查询、诊断与拒绝路径门禁 |
| xtask 注册表与工作流准备 | 本地测试覆盖全部内置组合 | CI 配置目标 | ARM64 类型检查通过，已配置原生测试 | x64 MSVC 类型检查通过，已配置原生测试 |
| Daemon、Worker 与本机 IPC | 本地进程和真实 sandbox smoke | CI 配置目标 | 不支持 Linux Daemon、Worker 与本机 IPC。`doctor` 返回 `unsupported` | 不支持 Linux Daemon、Worker 与本机 IPC。`doctor` 返回 `unsupported` |
| SQLite 工作流事务及完成事件游标 | 回滚、重启、幂等测试通过 | CI 配置目标 | 待验证 | 待验证 |
| Bubblewrap 隔离与取消 | 真实沙箱验收 | CI 配置目标 | 不支持 Bubblewrap | 不支持 Bubblewrap |
| 桌面通知 | helper 与 outbox 集成测试 | CI 配置目标 | 待提供适配器 | 待提供适配器 |
| Codex app-server | 实测离线握手、中断、流式结果、生产 Workflow 和进程回收 | 适配器通过交叉检查，原生运行待验证 | 不支持此 Linux 适配器 | 不支持此 Linux 适配器 |
| 私有提案队列及工作区备份、恢复 | 队列、快照验证和恢复测试通过 | CI 配置目标 | Unix 实现通过 Rust 类型检查。运行及权限验收待完成 | 不支持。CLI 明确拒绝，MCP 工具发现只公布可用工具 |

当前工作树的验证结果以[架构验收评估](architecture-assessment.md)为准。
前台应用的交叉检查使用 `LIBSQLITE3_SYS_USE_PKG_CONFIG=1` 跳过 bundled SQLite
的目标 C 构建。macOS 和 Windows 的原生链接、运行和平台安全门禁仍待取得结果。
`notify-send` 的集成测试不覆盖实际桌面通知中心的显示效果。
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
| `~/.mcbtodo/vault/` | 默认 Vault。`inbox/` 保存待分类捕获；项目、任务、工作单元、时间块和证据引用以 Markdown 为准。 |
| `~/.mcbtodo/vault/.mcbtodo-events.sqlite3` | WorkUnit 完成事件及中断恢复所需的写入意图，跟随 Vault 备份。 |
| `~/.mcbtodo/state/focus.sqlite3` | 专注会话、最后保存的工时和恢复状态。 |
| `~/.mcbtodo/state/last-view` | 仅在 `home_view = "restore_last_view"` 时使用的最近一次正常 TUI 视图。缺失或值未知时从 Month 启动。 |
| `~/.mcbtodo/state/workflow.sqlite3` | 计划、工作流运行、轨迹、审批、脚本授权和按摘要存储的产物。 |
| `~/.mcbtodo/state/workflows/revisions/` | 不可变的工作流定义、脚本和声明的可执行文件快照。 |
| `~/.mcbtodo/state/proposals/` | 尚待审核或已处理的 Vault 提案。 |
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

由 Vault 重建 Inbox 列表、日历、大纲和任务图。CLI `vault capture-inbox` 无需 Project 即可捕获，`vault triage-inbox` 以 revision 条件将条目转换为 Task，保留来源条目及其原始内容。备份前停止守护进程，从默认 Vault 和 SQLite 快照生成私有目录备份。发布暂存目录前校验 Vault、事件日志、提案和文件完整性。恢复到新的家目录，默认 Vault 提案继续绑定默认 Vault，包含待审工作流的图和源码快照。默认 Vault 通过 canonical 路径识别：当 `~/.mcbtodo/vault` 是指向实际目录的符号链接时，默认路径与该目标相同；提案按两者的 canonical 路径是否精确匹配来绑定，备份将该目标作为 Vault 数据复制到实目录。备份会比较解析时记录的目录设备号/inode 与后续打开的目录文件描述符，若目录在准备期间被替换则失败关闭。外部 Vault 仍不会进入默认 Vault 备份，且备份拒绝 Vault 内部的符号链接。尚未提交到提案队列的外部工作流草稿仍由用户自行保存。运行 socket、进程锁、沙箱临时目录和缓存随运行重新创建。

守护进程重启时，未完成的计时会话保留最后一次检查点并进入 `needs_review`。正在执行的工作节点进入 `outcome_unknown`，等待用户核对可能已经发生的副作用；`mcbtodo workflow resolve <run-id> --outcome completed|failed|not-started --note <说明>` 会向独立审计表写入一次性人工观察，运行生命周期仍保持 `outcome_unknown`。核对不是机器验证，不补造步骤轨迹、不重放任务，也不触发 Dependency 调度；确认未启动时，操作者可另行创建新运行。运行摘要含核对结果、说明与记录时间。运行中的取消请求会尝试中止执行；若取消与活动工作竞争，无论执行端没有返回还是迟到返回结果，Daemon 都不提交该结果、不继续后续节点，而将运行置为 `outcome_unknown`，因为取消请求和进程停止都不能证明外部副作用已撤销。结果写入事务失败时，attempt、产物、轨迹及运行推进一并回滚；运行时随即将仍为 Running 的运行转为 `outcome_unknown`，重启后也不会重放。待定计划与等待人工决策的节点可继续。Dependency 调度只在运行真实进入成功终态的事务内写入下游队列，并按规则和上游运行去重。

完成事件调度在注册时记录 Vault 事件水位。守护进程有界读取后续事件，在同一个 SQLite 事务内入队并推进游标，按规则和事件去重。Vault 和守护进程分别持久化，中途停止时可从已提交的游标继续。

完成事件使用持久 UUID 表示事件身份，SQLite sequence 负责本地读取位置。
跨设备层还需要设备身份、可导出的事件信封、逻辑导入、去重及 revision
冲突规则。私有备份包含恢复所需的 SQLite 内容。跨设备同步会使用逻辑事实，
运行 socket、锁、恢复日志和缓存属于设备本地。

## 执行与部署

运行绑定发布摘要。脚本的执行许可同时绑定节点与摘要。`mcbtodo-execution` 的 Bubblewrap 沙箱在宿主 Linux 上隔离运行 Bash 或 Nushell，执行配置限制输入、输出、时间与可见资源。`process.exec` 允许节点使用一个图目录内的本机 ELF。发布时将字节写入快照，执行时将其只读挂载到 `/work/executable`。脚本与可执行文件合计上限为 2 MiB，其中脚本合计上限为 1 MiB。Agent 通过同 UID 的本机 Unix socket 接收类型化绑定输入。Agent 的网络和凭据由适配器进程管理。Daemon 通过独立 Worker 启动这些受限执行器并监督结果；仍未验收的进程故障边界见上文及[架构验收评估](architecture-assessment.md)。[Agent 适配器](agent-adapter.md)

GitHub Actions 为两种架构配置原生 Linux runner，执行 `cargo xtask check`、示例 Vault 校验和锁定依赖的发行构建。CI 安装 Bubblewrap 和 Nushell，以覆盖脚本沙箱及其受支持的解释器；从发行包解出应用和守护进程后运行 `cargo xtask smoke --sandbox`，并核对打包的 [systemd 用户服务](../deploy/systemd/README.md)。可选的 `cargo xtask smoke-systemd` 使用 Bubblewrap 和可访问的 systemd 用户管理器，JSON 校验由 `xtask` 完成。Sandbox smoke 覆盖隔离、Agent 决策和进程中断恢复；它仍是确定性 fixture，不替代真实桌面环境和长期使用验收。

CI 当前验证构建、包归档和 smoke，不负责创建 GitHub Release 或向 crates.io 发布。项目使用 Apache-2.0，完整许可证文本位于仓库根目录；第三方材料审查规则见[许可与第三方材料](licensing.md)。发布包仍需准确的 description 和 canonical repository 元数据，并须逐个检查归档内容；见[命名规范](naming.md)。

[设计文档](design.md) 给出完整目标与验收条件。单次提醒可从 TUI、CLI 或经审批的 MCP 提案创建；周期提醒可从 CLI 或 MCP 提案创建。TUI `/` Picker 搜索 Project、Task 和 WorkUnit 并聚焦到 Outline。TUI `:` 命令支持退出、重载、跳转今天/日期、视图切换、对象搜索、计时操作、提案审阅和帮助；workflow 命令复用既有守护进程查询。`keymap.toml` 可覆盖主视图常用导航与 `Space u` 视图选择键，表单和确认键保持固定；Graph 的 `x/a/d` 操作与 `v` 选择键不可被其他动作覆盖。首页配置可选择每次从 Month 启动，或从私有运行状态恢复最近一次正常退出的视图；TUI 将九个稳定视图名保存到 `state/last-view`。Graph 通过 `v/x` 选择源节点、焦点作为目标，使用 `a`/`d` 预览新增或移除的依赖边及阻塞变化，确认后以目标文档 revision 写入。Graph 投影按已验证 Vault 快照缓存，在成功安装新快照时失效；列表只为当前终端可见窗口构造行，避免重复重建 DAG 和为全图分配文本。该优化不构成大规模 Vault 的延迟或内存验收。Timeline 将同一 Vault 日历投影呈现为两周甘特视图，以 `h/l` 移动日期、`j/k` 选择计划项，并以不同标记显示活动期、任务 occurrence、事件、时间块和期限。TUI 日历行的内部焦点身份包含对象 UUID、日历类型和具体 occurrence；跨视图或 Vault reload 时优先恢复精确 occurrence，目标视图无此行时再回退至同一对象。现有测试覆盖九个 Space 视图目标、Agenda/Day 切换与 reload，以及三日重复全天事件在 Month/Week/Day/Agenda 四种视图之间的 16 种有向切换并保留 occurrence 和日期范围；完整 Month/Week/Day/Agenda/Outline 交互矩阵仍未整体验收，多日定时、时区边界、窄终端和真实交互会话仍需验证。日历支持全天与定时事件重复。定时实例使用模板时区的本地开始时刻锚定，按规则处理夏令时跳时与回拨，并保留实际经过时长。48×8 测试验证 Day 与 Agenda 在重复小时切换和重载时仍定位到同一实例与源文档。重复人工任务可从规则模板按有界窗口物化为独立 Markdown Task，使用稳定 UUID v5 身份并保留各实例状态。`calendar export-ical` 按最多 367 个含首尾日期的窗口输出 RFC 5545 `VEVENT`/`VTODO`；`calendar import-ical` 严格接受支持的非重复 VEVENT，并通过 Vault 原子批量接口一次发布。
