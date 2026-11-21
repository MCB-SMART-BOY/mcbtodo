# mcbtodo 架构

mcbtodo 当前面向 Linux AArch64 和 x86_64。Markdown Vault 保存用户的项目和计划。守护进程独立保存已确认的运行状态，终端客户端可随时退出。仓库 CI 在这两种 Linux 架构上构建和测试；其他操作系统尚未列为支持目标。

## Cargo workspace

| 组件 | 职责 |
| --- | --- |
| `mcbtodo-domain` (`crates/domain`) | UUID、Inbox entry、任务状态、时间语义、日历投影和 WorkUnit DAG。边从前置单元指向依赖它的单元。 |
| `mcbtodo-vault` (`crates/vault`) | Markdown 与 YAML front matter、Inbox/任务等引用校验、预览和带修订条件的写入。依赖 `mcbtodo-domain`。 |
| `mcbtodo-workflow` (`crates/workflow`) | 类型化图编译、发布快照、确定性轨迹重放和模拟。文件发布后由摘要标识。 |
| `mcbtodo-execution` (`crates/execution`) | Linux Agent socket 适配器和 Bubblewrap 脚本沙箱；执行代码与 Daemon 状态所有权分离，供 Daemon 与 Worker 进程复用。 |
| `mcbtodo-platform` (`crates/platform`) | 用户路径和配置，供应用和开发工具共用。 |
| `mcbtodo-protocol` (`crates/protocol`) | 本机 Unix socket 的计时和工作流请求与响应，以及 Daemon 与单次 Worker 之间有界、带 attempt 身份的消息契约。 |
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

`WorkspacePaths::initialize` 及每次通过平台 API 读取用户配置时，都会检查
`.config` 与 `mcbtodo` 配置目录是当前用户拥有的真实目录，且不允许组/其他用户写入；
`mcbtodo` 配置目录还必须对组/其他用户不可访问。配置文件以 no-follow
方式打开，限制为 1 MiB，并检查属主与禁止组/其他用户写入。keymap 与主配置
共用该读取边界；Agent policy 还有独立的内容、属主、权限和 socket 检查。
首次初始化的默认配置通过同目录临时文件与 no-replace 原子重命名发布，
避免并发初始化时将暂存文件与正式配置同时保留为硬链接；已存在的有效配置不被覆盖。
`.mcbtodo` 下私有应用目录也检查属主、目录类型和 Unix 权限。首页状态文件仅能
读写为 `state/` 的直接子项；读取采用 no-follow、有界打开并检查属主和权限，
写入通过同目录临时文件原子替换。这些检查不等于
对所有祖先目录竞态、硬链接、任意外部 Vault 或所有运行时文件读写提供完整隔离；
相关限制与后续验收项见[架构验收评估](architecture-assessment.md)。

CLI 在命令入口解析一次 `WorkspacePaths`，将同一实例传给 Vault、工作流、提案、备份及运行时命令处理器；工作流 revision 存储、完成证据校验和 IPC socket 因此都由同一用户 home 派生，不在下游重新发现 home。TUI 与 Daemon 也在启动边界创建 `WorkspacePaths`，并向其组件传递路径所有权。CLI、TUI 和 MCP 共用 application 层的 Vault 查询与业务命令。application 从同一套 `TargetAction` 约束投影可选业务目标；TUI 的提醒与时间块选择器消费当前已验证快照的投影，最终写入仍携带观察到的 revision 并由 Vault 在写锁下重验。CLI 的工作流校验、模拟、发布和 revision 读取，以及提案批准时对已审核快照的发布，也经 application 的工作流用例进入 `crates/workflow`。`doctor` 的 Vault 完整性检查与 `validate` 命令也共用 `VaultQueries::validate`；doctor 额外负责环境与运行目录诊断，不直接打开 Vault 快照。MCP 的 Vault 写入请求进入待人工检查的提案队列。Inbox 可由 MCP 查询并以 expected revision 提议分流到 Project；审批通过后 Vault 发布稳定 UUID 的 Task，再更新 Inbox 来源条目。两文件操作不伪装成原子事务：中断后重试会验证已发布 Task 的精确内容，发现冲突时失败关闭。MCP 也可提交冻结的工作流图与源码字节快照；TUI 审阅该快照后，批准只将其发布为内容寻址 revision，不授予脚本授权或触发运行。已发布工作流也能创建任务，Agent 影响的写入由运行时审批控制。时间和工作流请求经 IPC 交给守护进程。守护进程与 TUI 共用 Vault 层的去抖文件监听；外部变更通过完整 Vault 加载校验，错误时保留最后有效快照。提醒 occurrence 扫描消费该快照投影出的 owned 定义，不另行读取 Vault；配置变更时丢弃旧快照扫描结果。生产 Daemon 仅从一个 `WorkspacePaths` 实例派生 state、socket、计时数据库及 Runtime 路径，避免组件意外分属不同 home。显式 `--vault` 选择只作用于前台 CLI/TUI；后台 Daemon 的 watcher、提醒和 WorkUnit 完成事件触发器绑定默认 Vault，不为外部 Vault 提供独立后台服务。工作流 `task.create` 动作在 Daemon 事件循环内执行，使用同一个 `WorkspacePaths` 查询 Project revision 并写入 Task，不授予执行 Worker Vault 写入能力；临时 workspace 集成测试覆盖该路径。

**执行架构目标与现状：**冻结目标是单机 `Daemon + 按节点启动的独立 Worker`。Daemon 是队列、授权、调度和运行状态的唯一权威；Worker 每个节点尝试启动一个进程，只执行绑定不可变 workflow revision 的工作，不自行调度、写 Vault/运行数据库或成为第二个状态库。生产路径已接通 revision 字节信封、Ready/持久化 Started/Start 门、结果身份校验、Daemon 结果提交、取消消息和 Linux 进程组终止/回收。Linux 测试覆盖终止 helper 对真实父子进程的回收、生产 Daemon 取消运行中的 sandbox 脚本，以及 Started 后 SIGKILL Worker：运行进入 `outcome_unknown`、不提交新步骤、Worker 被回收，重开 RunStore 后不重放。生产 `spawn_worker_at` 有畸形 readiness 帧测试。错身份 Result 回归测试由 fixture 子进程通过 stdout pipe 发出协议帧，再经 `read_worker_messages` 解码并交给生产 `poll_worker` 状态路径；它验证进程组回收、`outcome_unknown`、不提交步骤和重启不重放。该 fixture 并非打包 Worker 二进制。新增的归档验收从发行包解压目录启动 Daemon，并验证归档内 packaged Worker 执行 `exit 23` 脚本后，RunStore 保存失败运行、一个步骤和含退出状态的 trace 原因；这仅覆盖普通脚本非零退出，不覆盖打包 Worker 的畸形协议帧或多故障组合。Daemon 在启动 Worker 时传入自身 PID；Worker 启动早期通过 `mcbtodo-platform` 的 Linux API 安装父进程死亡信号，并检查父 PID 竞态。Sandbox smoke 在脚本副作用出现后强制终止 Daemon，检查当时采样到的 Worker 后代均退出，再验证重启后运行进入 `outcome_unknown` 且不重放。这只证明本机采样进程树的退出，不撤销已发生的外部副作用，也不证明 Agent 所连接外部服务的状态。协议、故障边界和未完成验收见[设计第 11.3 节](design.md#113-首发-worker-协议与故障边界)。M5 仍为 Partial：打包归档上的协议故障联合验收，以及外部副作用结果核对仍未全部覆盖。分布式 Worker 集群、远程 Worker 和团队执行仍延期。

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

## 数据权威与恢复

| 路径 | 内容 |
| --- | --- |
| `~/.config/mcbtodo/config.toml` | 日历显示时区与首页策略。`--timezone` 可覆盖指定客户端的投影时区；`home_view` 默认为 `month`，也可设为 `restore_last_view`。 |
| `~/.config/mcbtodo/agents.toml` | 本机 Agent 适配器的能力、发布摘要与调用时限。 |
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

由 Vault 重建 Inbox 列表、日历、大纲和任务图。CLI `vault capture-inbox` 无需 Project 即可捕获，`vault triage-inbox` 以 revision 条件将条目转换为 Task，保留来源条目及其原始内容。备份前停止守护进程，从默认 Vault 和 SQLite 快照生成私有目录备份。发布暂存目录前校验 Vault、事件日志、提案和文件完整性。恢复到新的家目录，默认 Vault 提案继续绑定默认 Vault，包含待审工作流的图和源码快照。默认 Vault 通过 canonical 路径识别：当 `~/.mcbtodo/vault` 是指向实际目录的符号链接时，默认路径与该目标相同；提案按两者的 canonical 路径是否精确匹配来绑定，备份将该目标作为 Vault 数据复制到实目录。备份会比较解析时记录的目录设备号/inode 与后续打开的目录文件描述符，若目录在准备期间被替换则失败关闭。外部 Vault 仍不会进入默认 Vault 备份，且备份拒绝 Vault 内部的符号链接。尚未提交到提案队列的外部工作流草稿仍由用户自行保存。运行 socket、进程锁、沙箱临时目录和缓存随运行重新创建。

守护进程重启时，未完成的计时会话保留最后一次检查点并进入 `needs_review`。正在执行的工作节点进入 `outcome_unknown`，等待用户核对可能已经发生的副作用；`mcbtodo workflow resolve <run-id> --outcome completed|failed|not-started --note <说明>` 会向独立审计表写入一次性人工观察，运行生命周期仍保持 `outcome_unknown`。核对不是机器验证，不补造步骤轨迹、不重放任务，也不触发 Dependency 调度；确认未启动时，操作者可另行创建新运行。运行摘要含核对结果、说明与记录时间。运行中的取消请求会尝试中止执行；若取消与活动工作竞争，无论执行端没有返回还是迟到返回结果，Daemon 都不提交该结果、不继续后续节点，而将运行置为 `outcome_unknown`，因为取消请求和进程停止都不能证明外部副作用已撤销。结果写入事务失败时，attempt、产物、轨迹及运行推进一并回滚；运行时随即将仍为 Running 的运行转为 `outcome_unknown`，重启后也不会重放。待定计划与等待人工决策的节点可继续。Dependency 调度只在运行真实进入成功终态的事务内写入下游队列，并按规则和上游运行去重。

完成事件调度在注册时记录 Vault 事件水位。守护进程有界读取后续事件，在同一个 SQLite 事务内入队并推进游标，按规则和事件去重。Vault 和守护进程分别持久化，中途停止时可从已提交的游标继续。

## 执行与部署

运行绑定发布摘要。脚本的执行许可同时绑定节点与摘要。`mcbtodo-execution` 的 Bubblewrap 沙箱在宿主 Linux 上隔离运行 Bash 或 Nushell，执行配置限制输入、输出、时间与可见资源。`process.exec` 允许节点使用一个图目录内的本机 ELF。发布时将字节写入快照，执行时将其只读挂载到 `/work/executable`。脚本与可执行文件合计上限为 2 MiB，其中脚本合计上限为 1 MiB。Agent 通过同 UID 的本机 Unix socket 接收类型化绑定输入。Agent 的网络和凭据由适配器进程管理。Daemon 通过独立 Worker 启动这些受限执行器并监督结果；仍未验收的进程故障边界见上文及[架构验收评估](architecture-assessment.md)。[Agent 适配器](agent-adapter.md)

GitHub Actions 为两种架构配置原生 Linux runner，执行 `cargo xtask check`、示例 Vault 校验和锁定依赖的发行构建。CI 安装 Bubblewrap、Nushell 与 jq，从发行包解出应用和守护进程运行 `cargo xtask smoke --sandbox`，并核对打包的 [systemd 用户服务](../deploy/systemd/README.md)。Sandbox smoke 覆盖隔离、Agent 决策和进程中断恢复；它仍是确定性 fixture，不替代真实桌面环境和长期使用验收。

CI 当前验证构建、包归档和 smoke，不负责创建 GitHub Release 或向 crates.io 发布。项目使用 Apache-2.0，完整许可证文本位于仓库根目录；第三方材料审查规则见[许可与第三方材料](licensing.md)。发布包仍需准确的 description 和 canonical repository 元数据，并须逐个检查归档内容；见[命名规范](naming.md)。

[设计文档](design.md) 给出完整目标与验收条件。单次提醒可从 TUI、CLI 或经审批的 MCP 提案创建；周期提醒可从 CLI 或 MCP 提案创建。TUI `/` Picker 搜索 Project、Task 和 WorkUnit 并聚焦到 Outline。TUI `:` 命令支持退出、重载、跳转今天/日期、视图切换、对象搜索、计时操作、提案审阅和帮助；workflow 命令复用既有守护进程查询。`keymap.toml` 可覆盖主视图常用导航与 `Space u` 视图选择键，表单和确认键保持固定；Graph 的 `x/a/d` 操作与 `v` 选择键不可被其他动作覆盖。首页配置可选择每次从 Month 启动，或从私有运行状态恢复最近一次正常退出的视图；TUI 将九个稳定视图名保存到 `state/last-view`。Graph 通过 `v/x` 选择源节点、焦点作为目标，使用 `a`/`d` 预览新增或移除的依赖边及阻塞变化，确认后以目标文档 revision 写入。Timeline 将同一 Vault 日历投影呈现为两周甘特视图，以 `h/l` 移动日期、`j/k` 选择计划项，并以不同标记显示活动期、任务 occurrence、事件、时间块和期限。TUI 日历行的内部焦点身份包含对象 UUID、日历类型和具体 occurrence；跨视图或 Vault reload 时优先恢复精确 occurrence，目标视图无此行时再回退至同一对象。当前回归测试覆盖九个 Space 视图目标、Agenda 与 Day 切换以及 reload，但完整 Month/Week/Day/Agenda/Outline 交互矩阵仍未整体验收。日历日期区间重复已支持；重复人工任务可从规则模板按有界窗口物化为独立 Markdown Task，使用稳定 UUID v5 身份并保留各实例状态。`calendar export-ical` 按最多 367 个含首尾日期的窗口输出 RFC 5545 `VEVENT`/`VTODO`；`calendar import-ical` 严格接受支持的非重复 VEVENT，并通过 Vault 原子批量接口一次发布。
