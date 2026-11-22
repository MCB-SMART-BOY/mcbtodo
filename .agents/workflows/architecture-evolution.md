# MCBTODO Architecture Evolution

执行现有 Rust workspace 的渐进演进。维护本地可日用体验，把外部能力放在可替换适配器边界之后，逐步建立跨平台与跨设备的数据基础。日常施工遵循 [architecture](architecture.md)、[develop](develop.md) 和 [documentation](documentation.md)。本文件定义专项门禁。交付状态和实测证据记录在 [架构验收评估](../../docs/architecture-assessment.md)。

## 1. 审计与约束

- 修改前读取仓库指令、设计和当前实现。检查 Git 状态与 diff、Cargo workspace 和依赖树、配置、示例 Vault、已有测试、CI、`cargo xtask check`、运行平台和工具。识别用户已有改动和运行中的工作。
- 追踪 domain、Vault、application、前台应用、Daemon 与 Worker 的真实责任。核对 Markdown 读写、快照、revision、冲突、SQLite schema/事务/迁移、Attempt 调度与恢复。核对 Agent Unix socket、peer credentials、policy、授权、timeout、cancel、消息大小及结果提交。
- 定位 Bubblewrap、Bash、Nushell、jq、notify-send 的调用点。检查进程启动、退出、回收、TUI 状态/键位/布局/主题/渲染、配置发现、运行时路径、日志和密钥。分别记录 Linux AArch64、x86_64、macOS、Windows 的证据。
- 区分已验证、已实现未验证、部分实现、仅设计、未实现、能力降级和不支持。记录文档与代码出入。先给出差距、落地文件、接口迁移和测试方案，再从单个可运行切片开始。
- 应用依赖 application，application 可组合 Vault，Vault 依赖 domain。业务规则留在拥有层，平台和工具细节留在适配器。保留 Daemon/Worker 分离、Markdown 权威、快照与 revision、任务/日历/Focus/Timer/Workflow/Completion Event 语义。复用已存在的等价边界。
- 保留用户已有改动、数据、示例 Vault 和测试。避免数据库清空、重置工作树、历史重写及未经授权的 push。不同阶段保持可独立审查和验证。遵守 Apache-2.0、`mcbtodo-` package 命名与 `cargo xtask` 约定。

## 2. 必须保持的不变量

- Vault 快照绑定明确 revision。过期写入被拒绝。读取、编写和恢复仍遵循现有锁、冲突与原子发布语义。Markdown 继续是用户可编辑主数据的权威。
- 日历验收从序列化 Markdown 进入 Vault 和 application 投影。日期截止日保持日历日期，定时截止日保持 UTC 瞬间且按显示时区归属日期。DST 重复小时保留显式偏移，不存在的模板端点在加载阶段拒绝。固定时刻的日历重复以模板时区作为锚点，保持实际经过时长。验收后续实例的 DST 跳时、回拨、跨时区跨午夜重叠、源 Markdown 身份及每规则展开上限。
- Workflow Attempt 以 Claim、Start、Commit 的合法顺序转换。Claim 不能重复取得外部执行权。追踪确切 run、node、revision、epoch 和预期结果。提交结果与下游推进须满足当前事务保证。必要事务能力缺失时拒绝启用后端或执行工作流。
- Workflow 批准绑定用户实际看到的等待节点及审阅快照。审阅内容刷新后，重复提交旧凭据或省略凭据都必须拒绝。CLI 检查输出和 TUI 弹窗从同一份 run summary 获取内容和凭据。实际审批与重新检查由调用者明确触发。
- 超时、取消、崩溃、通信中断和结果事务失败都可能发生在副作用之后。保留 `outcome_unknown`。恢复不重放，不伪造成功或未执行证据，不触发仅限成功的下游任务。仅在确定尚未执行或真实幂等的情况下重试。
- Agent 预算预留、运行状态转换与 Attempt 领取通过 `WorkflowRunStore` 在同一个 SQLite 写事务提交。验证过期领取回滚、预算不变及无 Provider 调用。脚本适配器区分可证明尚未释放的启动故障与释放后丢失结果。验证真实 Worker 返回的启动失败 trace、无工件和重启后的持久状态。
- Provider 能力声明仅供选择。Approval、Policy、Budget、幂等、revision 授权、取消与结果写入由宿主控制。第三方 sandbox 是附加保障。执行所需隔离不可用时默认拒绝。`AllowUnsandboxed`、`AllowSideEffectFreeOnly` 或 `RequireUserConfirmation` 只有在明确策略与验证支持时才可使用。
- Worker 从执行层接收明确的完成、取消或结果不明状态。派发结果不明时，Worker 上报绑定 Attempt 的固定状态和所选宿主适配器 ID。Daemon 保存 `outcome_unknown` 并保留已占用预算。运行 Worker 进程用例，检查不确定结果不携带 Provider 错误原文或工件。
- `cargo run` 默认可启动 TUI。可选工具缺失只影响相应能力。配置语义、CLI/TUI 操作和现有数据保持兼容。必要的不兼容变化须附恢复与迁移路径。

## 3. Phase 1 — Stable Ports

使用 `mcbtodo-ports` 定义现有调用者真正需要的业务契约。优先检查 `AgentProvider`、`LocalTransport`、`ProcessLauncher`、`Sandbox`、`WorkflowRunStore`、`CompletionEventStore`、`NotificationSink`。接口小而具体，明确错误、同步或异步、生命周期与并发，至少以一个真实适配器通过契约测试。为业务调用者提供 mock 或内存替身。业务接口不暴露 SQLite、Unix socket、Bubblewrap 或 Codex 内部类型。

按调用链评估 `StorageRepository`、`ProcessController`、`Clock`、`Scheduler`、`FileSystem`、`FileWatcher`、`FileLock`、`AtomicFileWriter`、`ServiceSupervisor`、`WorkspaceLayout`、`CredentialProvider`、`SecretStore`、`AuthSession`。没有真实消费者时推迟。Ratatui、Serde、Chrono、标准库等保留为编译时依赖。Rust trait、Cargo crate 和运行时插件分别解决不同问题。

验收包括现有入口及业务语义、真实调用者使用端口、适配器与替身测试、端口独立于工具实现。完成单一切片后继续同阶段的下一项。

## 4. Phase 2 — Existing Adapter Migration

按现有边界迁移 SQLite、Unix socket、Bubblewrap、Bash/Nushell、notify-send。必要时使用 `mcbtodo-storage-sqlite`、`mcbtodo-transport-local`、`mcbtodo-sandbox-bwrap`、`mcbtodo-script-runner`、`mcbtodo-notification-desktop`，小模块可归入已有 crate。先验证真实适配器再稳定抽象。维持 Linux 特性、Daemon/Worker 协议和 Vault 恢复。默认 TUI 不依赖 Bubblewrap、Nushell、jq。

存储按 `FocusStore`、`TimerStore`、`WorkflowRunStore`、`CompletionEventStore`、`ProposalStore`、`AgentBudgetStore`、`NotificationStore` 等业务职责切分，依据事务范围合并相关操作。核心不接受任意 SQL、连接、statement、row 或适配器事务。RunStore 负责原子领取、Start fencing、结果提交与未知结果标记。SQLite adapter 保留既有 schema 及状态。定义可检查的 schema version、失败恢复和迁移测试，避免重建用户数据。Memory、Postgres 和远端实现基于真实需求。

每个后端说明 transactions、atomic batch、durable cursor、snapshot、concurrent read、watch 中的可保证能力。所需能力缺失时明确诊断。存储测试覆盖原子 commit、rollback、游标续读、重启恢复、幂等插入、并发读写、快照导出和验证。沙箱测试覆盖 workspace 隔离、只读挂载、输出上限、超时、进程树清理、网络策略和不支持能力时的拒绝。

### Provider 路由和 ResourceScheduler

从实际公共调度需求提取 Provider 路由和 ResourceScheduler。当前
`AgentPolicy` 已服务 Worker 的 Agent provider 选择，为每个授权的
capability/revision 解析唯一路由。Worker 构造被选中的适配器，
由适配器在启动或连接时检查执行条件。
宿主继续管理 Policy、revision 授权、预算、deadline、cancellation、幂等和最终结果提交。Agent、
Sandbox、Notification 可有不同生命周期与选择路径。具有本地一致性职责的存储
单独管理。避免用同一泛型接口包裹全部基础设施。

Provider 路由：Implemented / Verified。真实 Unix socket 和 Codex
适配器通过 Worker 使用该选择路径，策略测试覆盖显式路由、重复授权、
未配置 Provider；Codex 适配器在启动前检查配置绑定的 capability。
Daemon 的 Workflow 调度已协调 Script 与 Agent：默认一个 Worker 执行槽，
CLI 可显式配置 1 到 4 个槽。所有槽共享持久领取、Start 栅栏、预算和终态
提交边界，每个槽独立监督、取消和结果接纳。Daemon 直接按空闲槽状态计算
队列唤醒时间，并在读取下一项运行前判断空位。忙碌时保持待执行状态，
不会领取 Attempt 或预留 Agent 预算。当前能力是本地有界并发。通用调度端口
应在出现实际的优先级、跨资源配额或其他资源选择需求时提取。

- 元数据记录 `provider_id`、`provider_version`、`protocol_version`、capabilities、supported platforms、required permissions、resource limits、health。区分静态平台支持、运行时可用性与宿主授权。
- 调度请求记录 request ID、idempotency key、priority、deadline、cancellation token、所需能力、workspace root、网络/文件系统策略和预算。预算包括 wall-clock、最大输出、并发额度、CPU 或执行额度、网络、读写范围、副作用和适用的 token/费用。区分宿主强制、Provider 保证、仅检测和不支持的约束。
- 检查能力、平台、策略与预算后选择 Provider、建立 Attempt、管理取消和 deadline、归一化结果和处理失败。宿主保留结果授权及不确定结果判断。
- Daemon 在持久化 Started 后且发送 Start 前记录计时起点，用于监督 Agent 预算及脚本 Worker 期限。Start 传输使用握手、控制帧与执行截止时间中最早的一项。额度包含 Start 发送时间，脚本另有两秒沙箱清理和上报窗口。逾时后拒收结果并发送 Cancel，再给 Worker 独立的两秒退出窗口，随后回收进程组。验收应涵盖短预算限制 Start 传输、卡住的 Worker 释放执行槽、排队工作继续和重启后不重放。
- Daemon 将当前监督阶段的截止时间纳入事件循环的唤醒计算。分别测试启动、执行、发送 Cancel 后的宽限期，以及回收后读取终态期间执行期限先到的情况。
- `Prepare` 写入、`Ready` 等待和 `Start` 写入共享同一绝对握手期限。后台握手只处理 Prepare 和 Ready。Daemon 循环保有进程控制权并独占 Started 与 Start 的发布。Worker 未读取 stdin 时也要按期限回收，随后推进队列。启动期取消以同一事务作废未 Started 的 Attempt 并保存 Cancelled。持久化 Started 后的 `Start` 发送错误保留 `outcome_unknown`。由监督循环发送有界 `Cancel`。部分写入后禁止重发帧。
- Worker 编解码器直接校验类型化消息，保持 v1 帧格式、Attempt 字段约束和大小上限。超限消息在写入帧头前返回错误。Daemon 在收到结果时仍要确认它绑定当前 Attempt。

### 跨平台适配

逐项对 Linux、macOS、Windows 记录构建、启动、基础流程、平台适配测试和安全保证。矩阵使用 Adapter × Platform × Capability，包括事务、取消和隔离。静态支持以 Supported / Unsupported 表达。运行时状态以 Available / Unavailable / Degraded 表达，分别处理工具缺失、版本不兼容、健康检查失败和终端限制。

适配范围包括 workspace 路径、文件锁、watch、原子替换、进程启动/终止/进程树/父进程退出、Unix socket/Windows named pipe、桌面通知、服务管理、sandbox 和终端能力。平台替代品达到所需隔离契约后才可启用原本要求隔离的执行。

原生 macOS CI 覆盖 CLI 集成和 Vault 校验。原生 Windows CI 覆盖离线查询、
Vault 校验、`doctor` 的 `unsupported` 状态以及备份、恢复和提案队列的拒绝路径。
两项原生作业均检查三个应用入口及测试。共享协议 DTO 的 `mcbtodo-ports`
依赖适用于所有平台，Linux 传输依赖继续按目标启用。交叉 Rust 类型检查、
原生 C/SQLite 链接和执行结果分别登记。
`cargo xtask agent validate` 与 `agent prepare --offline` 也纳入 macOS 和
Windows 门禁。Linux 专用烟测、打包、systemd 和沙箱示例任务在其他平台
须返回明确错误，并避免创建输出。`doctor --json` 须保留不可用能力的诊断项。

## 5. Phase 3 — Codex Agent Adapter

梳理业务 `AgentProvider`、宿主 `AgentPolicy`、`AgentTransport`、`AgentSession`、`AgentRegistry`、`AgentSupervisor`。传输提供经验证的身份，宿主授权。Session 负责关联消息、流、取消、连接退出，Supervisor 负责启动、回收、崩溃和重启。按实际使用评估 stdio、Unix socket、Windows named pipe 和 TCP。业务调用描述能力、上下文、工具/路径权限、预算、期限及结果写入范围。

对照官方文档，使用 `codex app-server` 外部协议实现 `mcbtodo-agent-codex`。保留进程边界。用户配置 System Codex 的可执行文件。通过实际握手、请求和响应验证运行时需要的协议能力，运行时不绑定具体 Codex 版本或 schema 摘要。Managed Codex 固定版本分发属于后续交付。

`initialize` 验证响应关联和成功状态。Linux Worker 在进程启动时设置隔离环境和 scratch home。测试覆盖缺少自报平台字段的成功握手，以及在创建线程前拒绝失败握手。

验收覆盖 `initialize` 握手、真实的兼容性与 capability 检查、approval/sandbox 策略映射、请求与通知、流式输出、取消、进程异常、响应/上下文上限、结果归一化和宿主最终授权。记录真实探针覆盖的 Codex 版本、已验证的协议交互与不受支持的机制。至少运行一个受控真实集成测试。Agent 路径不能直接写 Vault 或提交 Workflow 状态。

契约测试还须覆盖 malformed/oversized response、timeout、cancel、provider crash、stale result、duplicate request、拒绝网络与文件系统访问。验证取消与超时后的未知副作用、过期结果拒绝、提交幂等和宿主授权。

已构建三个二进制时，使用 `cargo xtask codex-probe --codex /absolute/path/to/codex --bin-dir target/debug` 验证真实 Codex 的离线 Workflow 路径。门禁通过 `provider = "codex"` 启动生产 Worker 的隔离 app-server，使用同用户 Unix 模型服务模拟上游。验证无效配置、revision 授权、审批、细粒度流式分块、模型断连、超时、app-server 崩溃、Workflow 取消后拒收迟到结果和 Daemon SIGKILL 后关闭模型连接及回收进程。失去结果的运行保持 `outcome_unknown`，重启与同 ID 重提均不重放。在线上游、凭据与其他平台分别验收。

## 6. Phase 4 — TUI Visual Architecture

将已验证 Vault/Application Snapshot 投影为可纯内存测试的 ScreenModel，再交给 Ratatui renderer。Presenter 负责业务展示计算和状态文案，renderer 负责绘制。按需要拆分 `theme`、`metrics`、`components`、`status`、`screen_model`。共享展示语义与终端坐标及 Ratatui 样式各有边界。

统一背景、surface、active surface、border、focus border、primary/secondary/muted text、accent、success、warning、error、selection、disabled token。依次优化标题与上下文、模式及焦点、内容层级与留白、弹窗、空/加载/错误态、footer、窄终端、中文和 Unicode、无色和高对比度，以及视觉快照测试。Footer 只放当前模式最相关的 3–5 个动作，完整按键进入帮助或命令面板。保留日历 occurrence、导航、键位、模式和操作语义。

验收须覆盖常规、窄屏和小高度、中文与英文混排、Unicode 宽字符、长标题、选中焦点、弹窗和错误、无色与高对比度。断言文字、边界、截断、遮盖和对齐。与存储迁移和 IPC 改造分开验证。

主布局在 24 列且同时存在操作反馈与 Vault 警告时，验证 4、3、2 行高度均保留一行焦点内容。
高度依次缩小时，先移除标题，再移除操作反馈，最后移除操作提示。

Workflow 展示切片通过 `WorkflowScreenModel` 投影运行列表、详情、
时区、轨迹标题和操作提示。`cargo test --locked -p mcbtodo --bin mcbtodo tui::`
覆盖内存渲染、现有键位与审批弹窗。检查 30×6 高对比度下的中英文对齐、
选中焦点、边框和页脚，并验证零步骤运行不会显示轨迹快捷键。

Timeline 展示切片验证跨午夜标记、日期导航后的文档焦点和 30×6 无色终端下的
中文对齐、截断、选中态、边框和页脚。紧凑布局沿用两周范围和既有键位。

Proposal 视图以 28×6 内存缓冲区验收空队列、待审及只读详情的中英混排与页脚。
Proposal 详情和 Workflow trace 复用可折行及水平平移的审阅视口。在 24×5
验收中英混排续行、最后一行、确认或返回，以及既有水平平移和步骤切换。
详情从 24×5 放宽至 70×5 后应保留当前源文本行。取消换行后移动到另一源行，
再恢复换行并缩窄窗口，目标源行仍应可见。
同一条长源行的可见字素在连续缩放后应保持可见。覆盖长行完全展开、
正文完全可见及缩回单显示行。End 应将定位更新到文末。
Graph 视图利用安装快照的缓存版本投影可见节点和编辑预览。以 30×6 验收选择态，
以 28×6 验证依赖变更审阅可滚动阅读完整信息。Graph 与 Workflow 共用审阅组件。
帮助覆盖层复用滚动视口，在 28×6 验证可读到最后一项快捷键，并能退出返回原视图。

快捷键展示也必须从同一个 `Keymap` 投影。主视图页脚、视图菜单、帮助覆盖层和 Workflow 页脚不得继续硬编码可重映射的键位。固定的领域操作键只有在输入协议本身不支持重映射时才保留为固定文案，并通过测试说明原因。
主视图通用页脚按终端显示列宽选择完整的提示组。用中文重映射按键验收
25、30 和 80 列的显示。主操作菜单、视图菜单和创建菜单共用滚动展示模型。
在 24×5 无色缓冲区和 28×6 伪终端检查 Home、End、Escape 及原有命令键。

Week 在 42×18 同时显示每日计数和焦点预览。在 24×5 无色缓冲区检查所选日期行
的小时与预览、中文列宽、右边框，并用 Enter 打开预览的同一条 occurrence。

日历视图复用有界查询，保留与窗口相交的重复事件的原始日期范围和源文件。
验证跨月延续、每条规则的展开上限、超限后继续导航，以及外部编辑后安装新的
Vault 快照。快照比较依据源文件路径和 revision。

截止日期、时间块与文档搜索弹窗由 ScreenModel 投影视图内容和操作提示。
在 28×5 验证匹配项、当前输入和 Esc 的位置。在 28×4 无色终端验证时间块
输入焦点、校验反馈和页脚。继续验证原有 revision 与取消路径。

## 7. Phase 5 — Process Plugin Protocol

至少两个真实 Provider 或 Backend 通过同类端口，并证明有跨进程扩展需求后，才建立 `mcbtodo-plugin-protocol`。首期采用 stdio framed JSON。Rust 动态库不作为公共插件机制。

协议规定 framing、message ID、响应关联、版本、消息上限、错误、取消与终止。基础消息包括 hello、capabilities、health、prepare、execute、cancel、result、diagnostic、shutdown。Manifest 校验 provider ID、版本、可执行目标、capabilities、required permissions 与配置。宿主决定授权。生命周期覆盖发现、版本选择、启动超时、健康检查、崩溃恢复、重启、熔断、卸载、日志保留和配置验证。以真实 Provider 和契约测试验证错误诊断、权限及协议协商。

## 8. 跨设备数据准备

区分 Markdown Vault、SQLite 本地派生和运行状态、可导出逻辑 Event Log、运行文件。socket、锁、journal recovery、cache、temporary files 不参与逻辑同步。备份可以保存 SQLite 快照与恢复材料。

定义可跨设备去重的 `event_id`、`origin_device_id`、`event_type`、`entity_id`、payload、`created_at`、`observed_revision`、`idempotency_key`。本机 sequence 只作为游标。时间戳不能独立决定事件身份、排序或冲突胜者。同步协议使用逻辑导出、幂等导入、合并规则、revision 冲突、设备身份与恢复，禁止用数据库文件复制代替。完成协议和验证前只记录数据模型进度。

设备来源切片按 [sync](sync.md) 验收。主程序在首次完成写入时创建私有安装 UUID。
待提交日志记录当时的来源，分页导出保留该值。旧记录来源保持缺失。恢复保留历史事件来源，
新写入使用恢复目标 home 的身份。跨设备认证、导入与冲突合并单独验收。

完成事件的 `vault completion-events --after` 分页导出沿用本机游标，
保留原 UUID 与写入前的 Markdown 摘要。设备来源需要可验证的设备标识。
验证原有无参数 JSONL 格式、排他游标、事件标识和 revision 保真。

## 9. 诊断、配置和认证

- 诊断涵盖 provider ID、operation、request ID、retryable、user action、platform、capability、cause。TUI 说明简短原因，日志保留关键信息。权限拒绝与暂时故障区分。
- 关联 trace ID、run ID、attempt ID、provider ID、adapter version、protocol version。记录阶段、状态、身份、错误、耗时及必要资源指标。默认不记录完整 prompt、密钥、OAuth secret、敏感正文及无必要个人信息。敏感调试日志需显式授权。
- 评估 CredentialProvider、SecretStore、AuthSession，区分安全密钥存储与普通配置。凭据与 Agent 执行逻辑分离。
- 区分产品配置、Provider 配置、平台能力、用户 Policy、运行状态与 cache。明确发现、默认、版本、覆盖、校验、迁移和热重载边界，保持现有配置兼容。

## 10. 持续验收与报告

循环执行审计、识别不变量、最小设计、实现、测试、diff 复核、门禁核对。优先保证正确性、安全与现有行为，再依照端口、适配器、调度、Codex、TUI、跨平台验证、进程插件和跨设备同步推进。独立切片可并行，仍须通过依赖门禁。

- 维护架构综述、适配器契约、平台能力矩阵、Agent 集成、存储与同步、TUI 设计系统、迁移与兼容性。现状写入证据文档。目标与已验证状态分开。
- 执行 `cargo xtask agent validate`、`cargo xtask check`、`cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate`。运行针对性单元、集成、契约与 smoke。进程路径变更时构建当前版本的三个二进制后运行 `cargo xtask smoke --bin-dir target/debug --sandbox`。默认 TUI 使用隔离的临时 home 和空的工具搜索路径验证启动和退出。记录环境原因与未执行检查。最终复核 `git diff --check`、Git 状态与用户改动。
- Linux Worker 即使主动退出，也须在回收进程前清理同组成员并保留原退出状态。测试仍须验证有界结果收尾、取消和未知结果恢复。实现契约见 [ports](ports.md)。
- 阶段完成需要实际端口、真实适配器和相应契约测试。存储需满足事务、幂等、重启恢复和既有数据兼容。Agent 需验证能力选择、宿主权限、超时/取消/崩溃和过期结果。平台和 TUI 必须按各自测试门禁验收。任一未过硬性门禁的阶段继续标记部分实现。

阶段报告按 A–G 给出实际结果。

A. **Completed Work** 列出落地的功能与架构变化。

B. **Changed Files** 列出新增、修改、移动文件和职责。

C. **Invariants Preserved** 说明 Vault、Daemon、Worker、Workflow、Agent Policy 与数据一致性证据。

D. **Validation** 写明运行过的命令与结果。

E. **Capability Status** 使用 Implemented、Verified、Partially Implemented、Planned、Blocked、Unsupported，记录降级和验证范围。

F. **Remaining Risks** 记录平台差异、事务保证、安全降级、Agent/配置兼容、数据迁移、性能和维护负担。

G. **Next Milestone** 记录目标、前置、实施任务和验收标准。
