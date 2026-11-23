# Rust AI-Native Productivity Workspace

> 产品设计与技术架构规范 v2.0（全新整合版）  
> 日期：2026-11-20  
> 状态：产品目标与设计约束。本文中的“应当”“建议”“目标”不代表功能已经实现；现状以 `docs/architecture.md`、`docs/architecture-assessment.md`、`README.md` 和源码为准。  
> 项目名称：`mcbtodo`。

---

## 0. 阅读指南与决策摘要

本文同时保留产品目标和架构决策。除明确标为“当前实现”的段落外，交互示意、候选方案、示例 schema、验收要求和里程碑都属于目标或设计建议，不应视为现有 CLI/TUI/API 契约。Cargo package、二进制及源码目录的实际命名见[命名规范](naming.md)；当前 workspace 结构及架构差距见[架构说明](architecture.md)和[架构验收评估](architecture-assessment.md)。

这不是“用 Rust 再做一款滴答清单”，而是 **Local-first、Markdown-first、Terminal-first、Helix-inspired、Graph-based、AI-native 的个人工作管理与自动化运行时**。

**不变约束**：

1. Linux ARM64（尤其 Omarchy/Arch 环境）是首批目标；x86_64 同步支持。不依赖 Electron 或桌面图形栈。此为目标平台范围，不代表两种架构或桌面环境均已完成现场验收。
2. 目标是默认打开**日历首页**，以月历为主；周历、日程、Outline、Graph、Timeline、Workflow、Focus、Reports 都是同一业务模型的投影。当前各视图和首页偏好支持程度见架构验收评估。
3. TUI 模仿 Helix 的模态交互和 selection-first 哲学，包括 Space Mode 可视化菜单、模糊 Picker、`:` 命令、选择后操作、可自定义键位与底部状态栏。
4. TUI 是临时客户端，Daemon 静默常驻；关闭终端不影响提醒、计时和可授权的计划执行。
5. 项目、任务、原子工作单元和说明文档以 Markdown/YAML 为用户可编辑的事实；执行队列、计时、审批、审计等事务性状态以 SQLite 为权威。**“Markdown-first”不等于“只用 Markdown”。**
6. 同时存在 Task Graph（工作依赖）与 Workflow Graph（可执行逻辑），不混为一种图；执行证据 Evidence 将两者关联。
7. 目标是由共享 Command/Query API 承载相同业务场景的规则；TUI/CLI/MCP 尽量共用，但不要求命令集合或交互步骤完全一致。MCP 写操作必须经过人工审批，TUI 可复用已加载快照，CLI 可保留格式解析等传输职责。当前共享边界和验收缺口详见架构与架构验收评估。AI 不能通过任意 Shell 或 SQL 越过权限与校验。
8. 脚本、定时计划和 AI 修改采用版本固定、权限声明、独立授权、执行隔离、结果留痕、失败恢复。
9. 单用户、单机、单活动 Vault、单写入 Daemon 加独立本机执行 Worker 为默认首发架构边界；Daemon 可显式配置有限的本机 Worker 并发。分布式 Worker 集群、多设备同步和团队协作延后。
10. 从第一迭代起写集成测试、属性测试和崩溃恢复测试，UI 不先于核心语义。

**最重要的对象边界**：

- `Project`：长期目标；可拥有活动日期区间、任务和里程碑。
- `Task`：可交付结果；可含若干可执行工作单元。
- `WorkUnit`：可计时、可验收的最小管理单元，不等于瞬时事件。
- `CalendarEvent`：日历活动；可能全天、跨日，也可能精确到分钟。
- `TimeBlock`：打算投入某项工作的时间安排。
- `TimeSession`：实际工作会话（已发生的时间记录）。
- `Deadline`：应当完成的期限；不是时间占用。
- `ScheduleTrigger`：启动自动化的规则；不是人类活动。
- `DomainEvent`：诸如“任务完成”“计时暂停”的系统事实；不是日历活动。
- `WorkflowDefinition`：可执行流程图的源定义；`WorkflowRun`：一次具体运行；`Evidence`：运行产生的可核查结果。

---

## 1. 目标用户、价值与产品边界

### 1.1 核心使用情境

**A. 个人规划**：今天要做什么？本周哪些日期有活动？哪些是长期项目而不是占满每一天的会议？某件事截止到哪天？

**B. 深度工作**：某工作单元预计 2 小时，今天预留 09:00–11:00，实际工作 94 分钟；可在退出 TUI 后继续记录计时状态，并生成偏差报告。

**C. 技术项目**：把“实现 RISC-V 页表管理”拆成阅读规范、设计、映射、地址转换、测试；用 DAG 表示依赖，推算就绪工作单元。

**D. 自动化**：每天 23:00 运行经批准的 Nushell 测试脚本；失败时保存日志和测试证据、请求 AI 分析、产生待审批修复提案。

**E. AI 协同**：AI 根据授权上下文规划任务、创建工作单元、修改依赖、生成流程图、提出脚本和定时安排，但不能偷偷运行新脚本或删除历史记录。

**F. 文本编辑**：用户在 Helix 中修改 Markdown/YAML/脚本，Daemon 检测并验证；修改无效时保留文件、报告诊断，不破坏已运行的已发布版本。

### 1.2 必须具备的首批能力

Inbox、项目、任务、工作单元、日期/时间区间、截止日期、提醒、时间块、计时与统计、依赖图、工作流定义与模拟、有限脚本运行、持久化调度、TUI、CLI、MCP、备份与诊断。

### 1.3 明确延后

团队多人编辑、移动端原生 App、复杂双向日历同步、CRDT 实时协同、插件市场、任意分布式执行、多节点循环编排平台、内置大语言模型推理框架。架构可以保留接口，但不能让它们挡住首版交付。

---

## 2. 核心领域对象与不变量

### 2.1 关系

```text
Workspace / Vault
  ├─ Inbox entries
  ├─ Project ── Task ── WorkUnit
  │             │           ├── WorkUnitDependency (DAG)
  │             │           ├── AcceptanceRule
  │             │           ├── Planned TimeBlock(s)
  │             │           ├── Actual TimeSession(s)
  │             │           └── Evidence reference(s)
  │             └── Milestone / Deadline
  ├─ CalendarEvent / Recurrence
  ├─ WorkflowDefinition ── WorkflowRevision ── WorkflowRun
  │                              ├── Script snapshot
  │                              ├── NodeRun / Artifact
  │                              └── Approval / ScheduleTrigger
  └─ Audit / Policy / Configuration
```

`Task` 的包含树表达“属于哪个交付物”；`WorkUnitDependency` 的 DAG 表达“谁必须先完成”；`WorkflowGraph` 表达“哪些机器步骤、分支与等待应当被执行”。这些结构不互相替代。

### 2.2 关键字段

- `id`：稳定、不依赖路径的 ULID/UUID；删除后的 ID 不复用。
- `title`、`description`、`tags`、`priority`、`status`、`revision`。
- `estimate`：预计工作量，不意味着已经占用日历时间。
- `acceptance`：可核验的验收条件；自动化证据要按规则关联。
- `dependencies`：稳定 WorkUnit ID 列表。
- `temporal`：不同时间语义明确分离，详见第 3 节。
- `origin` / `actor` / `causation_id`：支持审计、Agent 调用和事件回环检测。

### 2.3 不可违反的不变量

1. 同一 Vault 内业务对象 ID 唯一；路径重命名不改变对象身份。
2. WorkUnit 依赖图必须无环；不允许引用不存在的前置工作单元。
3. “被阻塞”是根据未满足依赖计算出的就绪属性，不应成为与人工 `status` 冲突的第二份真相。
4. TimeSession 不得因重复 `pause/stop` 请求而重复累计。
5. 每个 WorkUnit 可以有多个 TimeBlock 和 TimeSession；不能以一个 `start_at/end_at` 同时表示计划和事实。
6. 任务完成并不由脚本退出码自动推出；必须满足验收规则或明确由人确认。
7. 已发布 WorkflowRevision 及其脚本内容不可原地更改。
8. 修改需校验版本、权限与业务约束；过期 AI 提案不得静默覆盖新数据。
9. 已确认执行的工作流有可追踪运行记录；不能把不确定的外部副作用误标为成功或可安全重试。
10. 单个客户端退出不能停止 Daemon 的持续工作。

---

## 3. 时间语义：日期精度、时间跨度与业务含义

### 3.1 三个相互独立的维度

| 维度 | 选项 | 解释 |
|---|---|---|
| 精度（Precision） | Date / DateTime | 只知道日期，或精确到时刻 |
| 跨度（Span） | 一个日期、多个日期、数分钟、数月 | 时间持续多久 |
| 含义（Semantics） | ActivePeriod、Event、TimeBlock、Deadline、Trigger、Session | 它在业务上代表什么 |

**重要**：跨几个月也可以是日期精度；跨三天的活动也可以精确到小时。不要用“长/短”推断“日期/时刻”。

### 3.2 基础时间类型

- `DatePoint`：严格 `YYYY-MM-DD`，只接受真实日历日期且年份为
  `0001–9999`；不能在内部偷偷解释为当天 `00:00`。CLI 和 MCP 共用领域
  日期解析规则，不接受省略前导零或超出该年份范围的日期。
- `DateSpan`：日历日期范围，面向用户的 `start`、`through` 都包含边界；内部规范化成 `[start, end_exclusive)`，便于区间算法。
- `ZonedDateTime`：当地时间 + IANA 时区 + 可解析 UTC 瞬时；保存原始时区意图。涉及时区歧义时必须明确策略。
- `DateTimeSpan`：两个真实时刻定义的区间，内部左闭右开；可跨日/跨月。
- `Deadline`：`Date` 或 `DateTime`，语义是期限，不是 busy 占位。
- 重复规则与发生实体分离，按有界查询窗口展开。日历规则锚定事件模板的当地开始日期。定时事件保持模板时区的本地开始时刻及实际经过时长，锚定实例保留显式偏移，后续实例按声明的 DST gap/fold 策略生成。任务规则保存在 Task 模板，发生日期通过显式物化成为独立 Task Markdown 实例。实例 UUID 由模板 UUID 与发生日期稳定派生，实例状态与模板分开持久化。
- `Duration`：工作量或实际耗时；不与日历日期/时间戳混用。

建议研究 RFC 5545（iCalendar）中的 DATE、DATE-TIME、RRULE 和非包含式 DTEND 语义，导入/导出时正确转换。

### 3.3 业务类型与忙闲规则

| 对象 | 可以是日期区间吗 | 可以是精确时间吗 | 默认 Busy 吗 |
|---|---|---|---|
| Project ActivePeriod | 是 | 允许但通常不必要 | 否 |
| CalendarEvent | 是（全天或多日） | 是 | 可配置，通常是 |
| Task ActivePeriod | 是 | 是 | 否 |
| TimeBlock | 不建议用“全项目跨度”替代 | 是，核心表示 | 是，可手动改 |
| Deadline | 日期点 | 时间点 | 否 |
| TimeSession | 否 | 是，实际发生 | 仅用于历史统计 |
| ScheduleTrigger | 日期 / Cron 规则 | 精确时间 / 间隔 | 否 |

`Project` 持续三个月仅表示项目活跃，并不阻塞三个月的所有时间；`TimeBlock` 才表示计划安排；自动执行脚本也不应默认把用户日历标成忙碌。

### 3.4 具体边界

- **全天事件**：日期精度、全天 Busy；不构造虚假的 `00:00–23:59:59`。
- **跨日活动**：可在月历中连续显示，在周/日视图顶部 all-day lane 展示。
- **截止日期**：在相应日期显示旗帜或标记，不能自动转化为忙碌时间块。
- **重复任务**：表示经常需要人做的工作；可生成任务实例。
- **重复触发器**：表示经常自动执行某工作流；产生运行实例，不生成虚假的人工任务。
- **夏令时**：当地时间不存在或重复时，使用显式可配置策略（如跳过、下一个有效时刻、选择先/后一个）；不靠字符串猜测。
- **系统睡眠与断电**：不能将机器离线时间无条件算作人工工时。

### 3.5 配置示例

长期项目的活动期及截止日：

```yaml
active_period:
  kind: date_span
  start: 2026-11-20
  through: 2027-03-01
deadline:
  kind: date
  value: 2027-03-01
```

单日活动：

```yaml
when:
  kind: date_span
  start: 2026-11-25
  through: 2026-11-25
availability: busy
```

跨时刻的活动：

```yaml
when:
  kind: date_time_span
  start: "2026-11-25T14:00:00+09:00"
  end: "2026-11-25T16:00:00+09:00"
  timezone: Asia/Tokyo
availability: busy
```

---

## 4. 日历中心设计：同一数据、多种投影视图

### 4.1 首页策略

**目标**：第一次打开 `mcbtodo` 进入 `Home / Month`；再次进入可以按配置选择“总是月历”或“恢复上次视图”，默认总是月历。当前 `config.toml` 支持 `home_view = "month" | "restore_last_view"`；恢复策略将上次正常退出时的九种稳定 TUI 视图名保存在私有 `.mcbtodo/state/last-view` 中，缺失或未知值回退 Month。此实现已有配置与平台状态文件测试，但完整 Month/Week/Day/Agenda/Outline 模式矩阵和真实交互仍未整体验收。TUI 日历行焦点身份区分对象 UUID 与具体 occurrence，并在已覆盖的 Agenda/Day 切换、Vault reload，以及从 Week continuation hour 打开多日定时事件的测试中恢复精确 occurrence；多日全天事件也覆盖了从非首日经 Agenda 切换至 Day 并 reload 的场景。源文档编辑仍定位到业务对象 UUID。日历按用户选定的显示时区进行投影，与调度器的触发时区分离。

新增回归覆盖重复日历事件经 Agenda、Month、Week、Day、Outline 往返切换后，返回 Agenda 能恢复精确 occurrence；Outline 不展示日历行时仅保留源对象 UUID。该用例补足了单条路径的证据，不代表所有视图组合、多日类型或交互式桌面验收完成。

Month、Week、Day、Agenda 四种日历视图之间的 16 种有向切换现有键盘交互回归：使用三日重复全天事件验证日期范围与 occurrence 焦点保持，并在回到 Agenda 后核对精确实例。此覆盖不代表多日定时事件、跨时区边界、窄终端渲染及真实桌面使用已完成验收。

### 4.2 视图矩阵

| 视图 | 重点 | 对长期日期区间 | 对精确时段 | 对 Deadline / Trigger |
|---|---|---|---|---|
| Month | 全月事务与跨度 | 连续跨格色条 | 日期角标 / 汇总 | 截止旗帜 / 自动化图标 |
| Week | 全日+小时 | 顶部多日区 | 小时网格 | 独立栏或角标 |
| Day | 当天日程 | 顶部“进行中”区 | 时间轴 | 截止区 / 自动执行区 |
| Agenda | 按时间排序列表 | 区分正在持续 | 明确起止时刻 | 清晰标注语义 |
| Outline | 项目、任务层级 | 显示活动期 | 当前/计划块 | 任务属性 |
| Graph | 工作依赖 | 可在详情中查看 | 时间预算与状态 | 关联自动化 |
| Timeline | 甘特/长期规划 | 主体横条 | 可折叠为工作块 | 里程碑/期限 |
| Workflow | 自动化图与实例 | 显示规则有效期 | Scheduled / Executed | 调度/运行状态 |
| Focus | 当前执行 | 当前项目 | 实际工时 | 下次提醒 |
| Reports | 历史统计 | 项目周期 | TimeSession 汇总 | 漏执行统计 |

### 4.3 日历布局规则

- 月历采用 `7 × 5/6` 的真实日历格；周从周一开始可配置。
- 多日事件显示连贯条带，跨周分行续接，视觉上有同一对象标识；狭窄终端自动压缩并显示 `+N`。
- Project ActivePeriod 是低优先级、默认可折叠的背景/顶部条，不应覆盖当日真实事件。
- Day/Week 把 all-day lane 和 timed lane 分开；前者有全天/多日事件，后者只放明确带时刻的日程。
- Deadline、ScheduleTrigger、完成记录、实际工时使用不同图例，不强迫用户凭颜色辨别；同时用符号/文案。
- “选中日期”和“选中任务”是不同焦点类型；跨视图保留日期锚点及可定位对象 ID。日历行内部同时保留 occurrence 身份，不能仅以对象 ID 区分重复实例；进入编辑器时再将 occurrence 映射回其业务对象。
- 冲突检测只针对明确设置为占用的事件和 TimeBlock；仅有项目活动期或截止日不算时间冲突。

### 4.4 首页交互示意（非真实应用）

```text
┌─ PROD / HOME / MONTH ─────────────────────────────────────────────────────────┐
│ 2026 November                ◉ LOCAL / DAEMON    Focus: 01:24    NORMAL      │
├───────────────────────────────────────────────────────────────────────────────┤
│ Mon        Tue        Wed        Thu        Fri        Sat        Sun         │
│ 16         17         18         19         20▣       21         22          │
│ ...        ...        ...        ...        ...        ...        ...         │
│                                   └──────── Project: Rust OS (11/20–03/01) ─│
│                                  ● 09:00 Page mapping                        │
│                                  ⚡ 23:00 Nightly test                       │
│ 23         24         25         26         27         28         29          │
│                        █ All-day activity                                     │
│                                               ⚑ Submission deadline          │
├─ FRI 20 / AGENDA ────────────────────────────────────────────────────────────┤
│  ▰ Rust OS: active project (date span, not busy time)                          │
│  ○ 09:00–11:00  Implement page mapping          estimated 120m                │
│  ⚡ 23:00       Nightly automated tests          scheduled / no busy block    │
├───────────────────────────────────────────────────────────────────────────────┤
│ NORMAL  Month  selected: 2026-11-20      hjkl move  v select  Space u views   │
└───────────────────────────────────────────────────────────────────────────────┘
```

---

## 5. Helix 风格 TUI：输入与反馈系统

### 5.1 模式状态机

以下是目标交互模型，不是对当前 TUI 所有模式均已实现的声明。现有键位、命令及支持范围以 `docs/architecture.md` 和 CLI 帮助为准。

- `Normal`：默认；导航、选择目标、触发非破坏性动作。
- `Select`：扩展多选、日期范围选择、节点集合；选择之后再操作。
- `Insert`：小字段快速编辑（标题、标签、搜索框）；复杂文档交给 `$EDITOR`。
- `Space`：层次化命令菜单；屏幕实时提示下一级按键与命令解释。
- `Picker`：全局模糊搜索项目、任务、工作流、命令、最近访问项。
- `Command`：`:` 进入类似 Helix 的命令行，具有补全、历史和参数错误提示。
- `Confirm`：批准高影响操作、发布脚本、批量删除时使用的专用对话模式，不接受偶然误触。

Space、Picker、Confirm 是覆盖模式，退出后还原原来的 Normal/Select 和焦点。

### 5.2 设计原则

- **Selection-first**：命令作用于当前选择集合；默认单选也视为一个选择集合。
- **Visible discoverability**：Space Menu 像 which-key 一样显示命令与快捷键；禁用命令显示原因。
- **Consistency**：`hjkl` 保持方向感；`Esc` 返回上层；`Space` 是命令入口；`:` 是命令模式。
- **Immediate feedback**：底栏始终显示模式、视图、当前对象、选择数量、操作结果。
- **Undo and preview**：可逆业务操作生成可追溯撤销；删除、脚本执行、外部 API 副作用不可假定全部可 Undo。
- **Editable keymap**：默认 Helix-like，允许 TOML 重映射；解决终端和 tmux 键位冲突。
- **Avoid reimplementing Helix**：Markdown/YAML/脚本直接交给 `$EDITOR`，应用处理验证和诊断定位。

### 5.3 首批默认键位（待 UX 测试后冻结）

| 键位 | 作用 | 注释 |
|---|---|---|
| `h/j/k/l` | 在当前视图按空间或语义方向导航 | 月历 h/l 一天、j/k 一周；Outline j/k 行、h/l 折叠/展开 |
| `v` | 切换扩展选择模式 | 日期区间/多节点/多任务 |
| `x` | 选择当前行或节点 | 类似 Helix select-line 的对象对应 |
| `i` | 快速编辑字段 | 仅小文本与简单属性 |
| `e` | `$EDITOR` 打开源文件 | WorkUnit Markdown 或脚本 |
| `o/O` | 新建同级或前置对象 | 上下文决定具体对象类型，预览归属 |
| `/` | 当前视图过滤搜索 | `n/N` 下一项/上一项 |
| `:` | Command 模式 | 带补全和日志 |
| `u/U` | 撤销/重做已支持的业务修改 | 不承诺撤销任意外部副作用 |
| `Space` | 打开全局/上下文命令树 | 自动可视化提示 |
| `Space u m` | Month 首页 | 不丢失日期锚点 |
| `Space u w` | Week | 同上 |
| `Space u d` | Day | 同上 |
| `Space u a` | Agenda | 同上 |
| `Space u l` | Task Outline | 同上 |
| `Space u g` | Task Graph | 定位当前 WorkUnit |
| `Space u t` | Timeline | 长期项目规划 |
| `Space u f` | Workflow | 自动化图 |
| `Space g ...` | 依赖图编辑 | 加边、删边、自动布局、验证 |
| `Space w ...` | 工作流操作 | 节点、脚本、发布、模拟 |
| `Space t ...` | 时间操作 | 开始计时、暂停、设 TimeBlock |
| `Space a ...` | AI 操作 | 提案、审核、影响分析 |
| `Space x ...` | 执行与历史 | 运行、追踪、取消、日志 |
| `Space y/p/P/R` | 系统剪贴板相关 | 与内部寄存器行为分离 |
| `Space ?` | 当前上下文帮助 | 列出来源与是否被自定义 |
| `Esc` | 后退一层 / 退出模式 | 多层可预测返回 |

以上是本软件自己的建议映射，不声称与 Helix 每个快捷键完全相同。特别是 Graph 和 Calendar 具有与文本编辑不同的空间语义。

### 5.4 对象多选与操作语义

- **Month**：`v` 将起始日期设为 anchor，导航扩展为 `[anchor, cursor]` 日期集合；可一键创建 DateSpan、TimeBlock 提案或批量查看。
- **Outline**：`v` 后按 `j/k` 增减任务选择；批量设置标签、优先级、归档。
- **Graph**：可以选中多个节点；边操作需要明确源端、目标端和关系类型；不能以“选择多个节点”隐式构造完全连接图。
- **Workflow**：选中节点后可连边、复制、设置权限，修改显示 Graph Diff 和静态检查结果。
- **Timeline**：选择跨度是日期范围还是具体时段，由当前精度工具明确提示；不允许静默丢失时间信息。

### 5.5 自适应布局

- 宽屏：左侧 Explorer、中央 Calendar/Outline/Graph、右侧 Inspector、底部状态栏与 Space Menu。
- 中屏：中央主视图 + 可弹出的 Inspector。
- 窄终端（例如 80×24）：单主视图 + modal picker / details，不强塞三栏。
- 图节点过多：Graph 支持 zoom level（信息密度层级）、折叠分组、局部子图和拓扑 Outline；不要求全部节点一屏可见。
- ANSI/Unicode 兼容模式；色彩不可作为唯一传达状态的渠道。
- 鼠标可选，不应成为执行核心动作的唯一方式。

### 5.6 命令树结构建议

```text
Space
├── f   Files / Sources
├── s   Search / Picker
├── c   Commands
├── u   UI / Views
│   ├── m Month ├── w Week ├── d Day ├── a Agenda
│   ├── l Outline ├── g Task Graph ├── t Timeline └── f Workflow
├── g   Task Graph operations
├── w   Workflow definition / script / validation
├── t   Time / focus / schedule
├── x   Execution / history / logs
├── a   AI proposals / approvals
├── y/p/P/R   System clipboard actions
└── ?   Contextual help
```

---

## 6. 项目、任务与原子工作单元

### 6.1 任务与工作单元

`Project` 代表长期目标；`Task` 表达具体成果；`WorkUnit` 表达可独立行动且可验收的最小管理单元。将“阅读规范”“编写实现”“跑测试”拆为不同 WorkUnit，而“开始”“暂停”“完成”是 DomainEvent，不是 WorkUnit。

每个 WorkUnit 最好具有：目标、验收标准、预计工作量、可选的计划时间块、可选截止日期、依赖、状态、证据引用。

### 6.2 状态模型

`TaskStatus = Todo | InProgress | WaitingReview | Completed | Cancelled | Archived`（具体枚举需规范化）。

`WorkUnitStatus = Todo | InProgress | WaitingReview | Completed | Cancelled`；依赖阻塞 `Blocked` 作为派生 `readiness` 属性，不与持久化 Status 混同。

计时器另有状态机 `Idle | Running | Paused | NeedsReview`，不可混入 TaskStatus。

### 6.3 优先级与计划

任务优先级与日历时间分开。高优先级不自动意味着“今天执行”；调度建议应考虑期限、依赖、可用时间和用户的手动安排。

项目可以关联 Milestone（里程碑）与 Roadmap。关键路径分析应标记它使用的工时估计、并行资源假设以及日期约束，不能宣称必然的实际完成日期。

### 6.4 验收与证据

- `AcceptanceRule`：可人工或机器校验的条件。
- `Evidence`：测试报告、GitHub CI 链接、文件哈希、审核记录、截图等。
- `TaskWorkflowBinding`：连接 WorkUnit 与某个 WorkflowDefinition/Run。
- 脚本成功仅生成 Evidence；只有规则满足或人工确认后，WorkUnit 才变为 Completed。

---

## 7. Markdown-first 工作空间与文件格式

### 7.1 Vault 结构

下面的目标布局用于说明领域关系。实际 schema、目录与命令见 `README.md` 和 `examples/vault/`。CLI 创建的 Task 文件使用 UUID 命名，WorkUnit 示例位于 `units/`。可执行工作流源快照由提案和发布流程管理。

```text
~/.mcbtodo/vault/
├── inbox/
│   └── <entry-uuid>.md
├── projects/
│   └── rust-os/
│       ├── project.md
│       ├── tasks/
│       │   └── <task-uuid>.md
│       ├── units/
│       │   └── <work-unit-file>.md
├── calendar/
│   └── <calendar-entry>.md
```

当前 Inbox capture 与 triage、Project/Task/WorkUnit 等对象的 schema 和行为，以 README、Vault 实现及 fixture 为准。Inbox triage 从标题、正文与标签生成 Task，Task UUID 由 Inbox UUID 稳定派生；Vault 先发布 Task，再更新保留的 Inbox 来源记录。此流程不宣称跨文件原子性：中断后重试必须精确验证已存在 Task，冲突时失败关闭，不覆盖外部编辑。

Linux 默认 Vault 的 TUI 通过本地 IPC 读取 Inbox 和 Project 列表，并将选择时的 Inbox revision 交给服务校验。服务在 Vault 写锁内执行分流，锁忙时报告冲突。如果只有 Task 已发布，服务会明确报告 Inbox 分流尚未完成。

SQLite 和运行状态位于 `~/.mcbtodo/` 下；完成事件数据库是 Vault 内的 `.mcbtodo-events.sqlite3`。备份需要同时保留配置、Vault 与权威运行状态，具体备份清单以当前实现文档为准。

### 7.2 Project 示例 `project.md`

```markdown
---
schema: mcbtodo.project/v1
id: 10000000-0000-4000-8000-000000000001
status: active
active_period:
  kind: date_span
  start: 2026-11-20
  through: 2027-03-01
deadline:
  kind: date
  value: 2027-03-01
tags: [rust, operating-systems]
---

# Rust OS

## Outcome
在 RISC-V 环境运行自研内核，并在目标系统中推进编译能力。

## Milestones
- [ ] 页表与地址空间
- [ ] 进程与调度
- [ ] 文件系统
```

### 7.3 WorkUnit 示例 `units/mapping.md`

```markdown
---
schema: mcbtodo.work-unit/v1
id: 30000000-0000-4000-8000-000000000002
project_id: 10000000-0000-4000-8000-000000000001
parent_task_id: 20000000-0000-4000-8000-000000000001
status: in_progress
priority: high
estimated_minutes: 120
deadline:
  kind: date
  value: 2026-11-25
depends_on: [30000000-0000-4000-8000-000000000001]
tags: [riscv, paging]
---

# 实现 Sv39 页表映射

## Goal
实现三级页表映射和必要的 PTE 标志逻辑。

## Acceptance Criteria
- [ ] 正确映射虚拟页
- [ ] PTE 标志位验证通过
- [ ] 对应单元测试通过

## Notes
研究记录可以直接在 Helix 中编辑。
```

### 7.4 文件编辑原则

- Front Matter 是结构化字段；Body 是自由 Markdown。不要全量重新序列化正文。
- 解析器应保留未知字段、注释、空白、代码块；不认识的必需语义字段必须报错。
- 直接修改文件与通过 API 修改都受到相同的有效性检查；但文件外部编辑无法天然被 API 事务控制。
- 文件 watcher 去抖、内容哈希和合法性检查；解析失败时保持最后一个有效运行快照，并提示用户。
- Git 版本跟踪可以保留，但不能当作多设备并发事务引擎。

---

## 8. 存储分工、事务和恢复

本节描述目标权威性划分与一致性约束，不表示每一类建议表、Artifact Store 或写入协议都已存在。当前实现使用 Markdown Vault、Vault 完成事件 SQLite，以及 focus/workflow 等运行时 SQLite；备份、恢复、快照校验和当前表用途见 `docs/architecture.md` 与源码。

### 8.1 权威性划分

| 类型 | 权威来源 | 说明 |
|---|---|---|
| 项目、任务、WorkUnit、日历声明 | Markdown + YAML | 用户编辑 |
| WorkflowDefinition | `workflow.md` + `graph.yaml` | 用户编辑；图结构唯一来源为 graph.yaml |
| 脚本源码 | `.nu`/`.sh`/`.py` 文件 | 用户编辑 |
| 已发布 WorkflowRevision | 不可变内容快照 | 运行依据，不能依赖可能改变的路径 |
| TimeSession | SQLite | 持久化实际工作记录 |
| WorkflowRun/NodeRun/Queue | SQLite | 权威运行状态 |
| Approval/Authorization | SQLite | 权威授权记录 |
| Audit/DomainEvent | SQLite 持久化事件 + 可导出 JSONL | 具备查询与恢复语义 |
| 搜索/Graph 投影/统计缓存 | SQLite 派生表 | 可以重建 |
| 大型日志/报告/测试结果 | Artifact Store | 以内容哈希引用、按策略保留 |

建议 SQLite WAL + 可靠备份；对关键事务考虑 `synchronous=FULL`，明确不同设置的性能与断电持久性权衡。备份要使用 SQLite 一致性备份机制，不直接只复制可能处于 WAL 事务中的单个主库文件。

### 8.2 一致性与写入协调

- Daemon 是**唯一业务写入协调者**：TUI、CLI、MCP 不直接修改索引或运行时 DB。
- API 修改使用 `expected_revision` 和 `idempotency_key`；在权威记录中保存操作审计。
- 人类可直接编辑文件：通过内容哈希比较、来源识别与三方合并/冲突报告对齐。
- 单文件安全写入：同文件系统暂存、文件内容同步、原子替换、必要的目录同步，具体能力因平台文件系统而异。
- 多 Markdown 文件不是 ACID 事务：采用 Prepare/Validate/Journal/Commit/Recover；在发布给客户端前保证一致性快照。外部文件查看器仍可能短暂看到中间文件状态，不能对外承诺物理多文件原子性。
- 首发可以限制一次自动批量修改的文件数与冲突场景；不允许无日志的大规模覆盖。
- 禁止对指向 Vault 外部的非授权符号链接进行写入；校验路径规范化和目录穿越。

### 8.3 Schema 与备份

- 每类定义文件含 `schema`，首发只接受现行格式。
- SQLite 保存权威运行表，派生索引可重新建立。首发直接创建当前结构。
- 备份必须包含 Vault 定义、不可变执行快照、Runtime SQLite、必要 Artifacts 与密钥引用配置（敏感凭据本体单独处理）。
- 恢复流程具备 `mcbtodo doctor`、备份验证、导出审计清单、冲突恢复报告。

---

## 9. Task Graph 与 Graph 编辑器

### 9.1 DAG 语义

WorkUnit 之间的依赖边 `A -> B` 表示 B 的启用需满足 A 所要求的前置条件。模型支持 `depends_on`、拓扑排序、Ready 集合、阻塞原因、关键路径的有条件估计。

`Ready` = 未完成且必要前置工作已满足、没有被明确暂停/取消、没有其他业务限制；不能仅由状态字段直接推断。

### 9.2 Graph UI

- 选择焦点节点、拓扑跳转、方向跳转、Pan、折叠子图、局部详情、边类型图例。
- `v` 多选节点，先选源后选目标建立依赖；“建立边”必须让用户看到完整源/目标集合和预览。
- AI 修改图时显示 **Graph Diff**：增加、删除、改边、影响哪些就绪节点、是否触发重新计划。
- 导出 Mermaid / Graphviz DOT 仅是视图，不能维护第二份“真图”。
- 大图可通过 Outline+Graph 联动定位，不强求所有节点一屏可见。

### 9.3 时间线与依赖图分离

Task Graph 描述工作前后顺序，Timeline 描述计划日期与持续期。关键路径算法得出的估计最短工期不自动写入真实 TimeBlock；必须经明确的调度规划步骤。

---

## 10. Workflow Graph：可编译执行的流程图

本节的节点集合、语法和示例是目标设计。当前工作流定义、可编译节点、调度/执行能力及限制以 `examples/workflow/`、`crates/workflow/`、守护进程实现和 `docs/architecture.md` 为准；示例 YAML 不是承诺可直接编译的现行 schema。

### 10.1 与 Task Graph 的关系

Task Graph = “工作要完成什么”；Workflow Graph = “机器/人工步骤如何被触发、判断、执行、等待和结束”。通过 Binding 和 Evidence 连接。流程图不单纯是 UI 美化，其控制流和数据流均可执行。

### 10.2 节点集合

**目标节点集合（非当前能力清单）**：

1. `Action`：调用系统内建类型化命令（如创建待办、保存报告）。
2. `Script`：经授权的 Nushell/Bash/Python（首版至少 Nushell）外部脚本。
3. `Condition`：根据结构化输入选择分支，不执行任意 Shell。
4. `Approval`：挂起工作流，等待人类批准/拒绝。
5. `Agent`：调用 AI 完成有结构化输入、输出及预算的有限任务。

**后续扩展**：Wait、Notification（初期可用 Action）、Subworkflow、Loop（有界）、丰富的并行 Join 策略、外部事件监听。

Trigger 是工作流启动定义，不占据普通运行节点的控制流。

### 10.3 控制流与数据流

- Control Edge 说明哪个节点满足条件后可执行。
- Data Binding 说明目标输入字段来自哪个已完成节点的输出或 Artifact。
- 节点的 Input/Output 使用受限制的、版本化 JSON Schema；不允许未声明的任意路径表达式。
- `Condition` 仅允许从可检查的标量和类型化对象取值。
- 普通成功边要求上游 `Succeeded`；分支未选择的节点被 `Skipped`，Join 按被激活分支确定等待对象。
- 上游失败默认阻止成功后继，错误边显式处理；不允许含糊“失败也算成功”。
- DAG 自动识别可并行节点，但必须限制全局和工作流并发度。
- 第一版不实现任意循环；需要重试时用受限 RetryPolicy，不以自环表达。

### 10.4 编译过程

`Parse -> Resolve -> Typecheck -> Validate DAG -> Capability Check -> Snapshot/Publish -> Execution Plan`

- 语法、节点 ID、边与端口合法性。
- 类型、必填参数、条件分支覆盖、无法到达节点诊断。
- 脚本及 Schema 文件存在且内容可读。
- 权限清单与实际授权相分离；没有授权不能发布为自动可执行工作流。
- 发布生成不可变 Revision，包含图定义、脚本字节/不可变引用、Schema、所需能力声明与哈希。
- 正在运行的 WorkflowRun 始终绑定创建时的 Revision，不热切换到新草稿。

### 10.5 工作流定义示例

目录：

```text
workflows/nightly-check/
  workflow.md      # 文字说明和概览
  graph.yaml       # 唯一可执行图定义
  scripts/test.nu  # 具体脚本
  schemas/test-result.schema.json
  fixtures/passed.json
  fixtures/failed.json
```

`graph.yaml`（拟定的本项目 v1 语法，并非现行外部标准）：

```yaml
schema: mcbtodo.workflow/v1
id: nightly-check
name: Rust Nightly Check

triggers:
  - id: nightly
    kind: cron
    expression: "0 23 * * *"   # 五字段 cron：每天 23:00
    timezone: Asia/Tokyo
    missed: run_once

execution:
  overlap: skip
  max_concurrency: 1
  timeout_seconds: 1200

nodes:
  - id: test
    kind: script
    runner: nushell
    source: scripts/test.nu
    output_schema: schemas/test-result.schema.json
    timeout_seconds: 600

  - id: judge
    kind: condition
    expression: "nodes.test.output.passed == true"

  - id: report
    kind: action
    action: report.create

  - id: triage
    kind: agent
    capability: analyze_test_failure
    output_schema: schemas/repair-plan.schema.json

  - id: approve
    kind: approval

  - id: create_followup
    kind: action
    action: task.create

edges:
  - {from: test, to: judge}
  - {from: judge, to: report, when: "true"}
  - {from: judge, to: triage, when: "false"}
  - {from: triage, to: approve}
  - {from: approve, to: create_followup, when: approved}
```

这里 `report.create` 等动作仍需具体定义输入映射，示例表达架构而非完整可编译语法。开发前必须将 `graph.yaml` 的 JSON Schema、节点端口、错误分支和 `expression` 子语言固定下来。

---

## 11. Workflow Runtime 的执行语义

### 11.1 三种版本/生命周期

- `WorkflowDefinition`：磁盘草稿，可编辑。
- `PublishedRevision`：不可变的已校验内容版本。
- `WorkflowRun`：由某个 Revision 产生的一次具体运行，其内部包含 `NodeRun` 与 `Attempt`。

### 11.2 状态机

`WorkflowRun`: Pending / Running / Waiting / Succeeded / Failed / Cancelled / OutcomeUnknown。

`NodeRun`: Pending / Ready / Claimed / Running / Waiting / Succeeded / Failed / Skipped / Cancelled / OutcomeUnknown。

`Attempt` 是持久化执行记录的设计概念，包含次数、执行者/进程标识、超时、开始/结束时间、退出状态、结果引用和异常原因。首发目标要求独立本机 Worker 由 Daemon 监督；其协议、回收语义与结果 fencing 必须先定义并通过验收，不能以线程或受限子进程替代该进程边界。

持久化 WAIT 不能占用永久阻塞的 Tokio Task；Approvals 用持久化等待状态和事件恢复。

### 11.3 首发 Worker 协议与故障边界

首发采用 **Daemon 按执行节点启动一个受监督的本机 Worker 进程**，每个进程只执行一个节点尝试并退出。它不是常驻队列消费者，不拥有调度器、Vault 或运行数据库；同一时间最多运行一个尝试。Daemon 是唯一 supervisor 和业务状态权威。Worker 与受限脚本/Agent 子进程是不同边界：Worker 提供独立于 Daemon 线程的故障隔离，子进程隔离具体执行环境。

Daemon 仅在持久化领取成功后启动 Worker。每个领取产生单调递增的 `attempt_epoch`，并绑定以下不可变执行信封：

- `run_id`、`workflow_revision` 内容摘要、节点 ID、预期步骤数和 `attempt_epoch`；
- 已编译节点输入及其有界大小；Daemon 必须先从已发布 revision 取得并校验脚本和可执行文件的精确字节，再将有界字节放入信封。信封不得接收任意主机路径，Worker 不得自行按路径读取可变源码；
- 该节点的能力集合、授权决定及其策略摘要；不得将未使用的凭据写入信封、日志或运行数据库；
- 截止时间、协议版本和唯一尝试 ID。

Worker 仅通过受限本机 IPC 接收信封并返回一个有界的类型化结果。它必须先发送 `Ready`；Daemon 将该尝试身份和 epoch 持久化后才回 `Start`，Worker 在收到 `Start` 前不得启动任何可能产生副作用的操作。结果必须原样绑定并回显尝试 ID、run/revision/node/epoch 和预期步骤数；Daemon 在同一个结果事务中再次验证这些字段及当前运行代次，并原子提交步骤轨迹、产物引用和运行状态。缺字段、超限、revision 不符、epoch 过期或运行已取消的结果均拒绝，不能写轨迹、产物或触发下游节点。Worker 不直接写 `workflow.sqlite3`，也不能自行推进队列。需要修改 Vault 的内建 Action 由 Daemon 在重新验证 revision 后执行；脚本和 Agent 的外部副作用则由 Worker 管理的受限执行器承担。

协议在 `mcbtodo-protocol::worker` 中定义为版本化有界 JSON 帧、attempt 身份、Prepare/Ready/Start/Cancel 消息，以及身份绑定的脚本/Agent 结果。Daemon 生产路径已从发布的 revision 读取脚本/可执行文件字节，创建并监督 `mcbtodo-worker` 子进程；收到匹配的 Ready 后先持久化 Started，再发送 Start。结果在进程成功退出后由 Daemon 校验并提交，启动前失败会原子失败运行并使 attempt 失效。Linux 进程组终止 helper 有真实父子进程覆盖，并且非 `ESRCH` 的进程组终止错误会向上传递。Daemon 将自身 PID 传给 Worker；Worker 在协议处理和执行前通过 `mcbtodo-platform` 安装父进程死亡信号，并复核父 PID，以覆盖信号安装前 Daemon 已退出的竞态。Linux 端到端回归测试通过生产 Daemon 取消正在运行的 sandbox 脚本，验证运行进入 `outcome_unknown`、不提交新步骤、Worker 被回收且已观察到的后代不再运行；sandbox smoke 在副作用出现后强制终止 Daemon，验证采样到的 Worker 后代退出、重启后运行进入 `outcome_unknown` 且不重放。另有畸形 readiness 帧的生产进程监督测试，以及错身份 Result 回归测试：fixture 子进程将合法编码的错身份帧写入 stdout pipe，再由生产 `read_worker_messages` 解码并交给 Started 后的 `poll_worker` 路径，验证失败关闭、进程回收和 no-replay。fixture 并非打包 Worker 二进制。Linux 发行归档 smoke 已从解压目录启动 Daemon，并验证归档内 Worker 执行 `exit 23` 脚本后，将单个失败步骤和包含退出状态的 trace 原因持久化；这不覆盖打包 Worker 的畸形协议帧或多故障组合。进程退出不能撤销已发生的外部副作用，也不能证明外部 Agent 服务的状态。M5 仍需完成打包归档上的协议故障组合与外部副作用结果核对；结果提交失败另有事务回滚和重启不重放测试。不得将 Daemon RunStore 内的 epoch 校验描述为独立租约服务或分布式 fencing。

取消时 Daemon 先持久化取消状态，再向该尝试发送取消请求；超时后终止 Worker 及其整个进程组，并等待回收。只有确认进程树停止后才可清除当前尝试。已开始执行但没有可验证结果、Daemon 与 Worker 通信中断、Worker 异常退出，或结果事务未提交时，一律转入 `outcome_unknown`；不自动重放该节点，也不调度依赖节点。Worker 仅在已持久化领取且可证明尚未开始副作用时退出，才可按明确状态安全释放领取。

内部 Worker 协议 v2 的 `agent_outcome_unknown` 结果携带 Attempt 身份和所选宿主适配器的 `provider_id`。Daemon 接纳报告后持久化不确定原因，并记录 `run_id`、`attempt_id`、`provider_id`。这条结果帧不传输 Provider 错误原文和工件。若由 Daemon 先判定超时、取消或崩溃，以对应的进程监督诊断为准。Daemon 和 Worker 使用同一构建的版本，用户配置和 SQLite 数据无须迁移。

Daemon 重启时不接管或重放旧 Worker 的任务：事务中所有活动尝试失效并成为 `outcome_unknown`。每次进程启动使用新的 epoch/attempt ID；即使旧进程返回迟到结果，也无法通过 fencing。Worker 单独崩溃由 Daemon 观察退出状态并执行同一恢复规则。生产取消路径已有真实 workload 的进程树终止与回收测试；Worker SIGKILL 和 Daemon 强制终止后的状态恢复也有覆盖。Daemon 死亡 smoke 同时核验其当时可见的 Worker 后代退出；这不代表已经发生的外部副作用被撤销或得到核对。错身份 Result 已由 fixture 子进程写入 stdout pipe，并通过生产协议 reader 和 `outcome_unknown`/no-replay 路径测试；发行归档 smoke 另覆盖 packaged Worker 执行非零退出脚本并持久化失败结果，但打包 Worker 的协议故障及故障组合仍须验收，并明确各外部集成的幂等、补偿或人工确认边界。OS 进程监督、私有 IPC、进程树回收和上述状态转换都必须有故障注入测试；租约超时或父进程失联本身不能证明外部副作用已停止。

### 11.4 副作用与幂等性

- 单 Daemon 持有唯一队列与运行状态；独立本机 Worker 的领取/执行身份必须可撤销，迟到结果不得覆盖更新的运行代次。
- Worker 领取与结果提交需在事务边界内检查唯一任务身份和代次（fencing token）；租约过期本身不证明旧进程或其子进程已停止。
- 过期执行必须先被确认隔离或转人工处理，不得与新 Worker 并发产生不可控副作用。
- 外部副作用不保证 Exactly-once；能使用 Idempotency Key 的调用应使用，并支持补偿/人工确认。
- 不确定结果进入 `OutcomeUnknown`，不默认无限重试。
- Cancel 既要变更持久化状态，也要尝试取消进程树；对已经发生的外部副作用不能宣称撤销。

### 11.5 数据产物

运行日志、测试结果、图差异、AI 结构化输出保存在 Artifact Store，使用内容哈希、类型、大小和关联 ID 进行引用。日志有大小上限、敏感信息控制和保留政策。

---

## 12. Script Engine：编写、调试、版本与授权

### 12.1 编辑体验

- 在 Workflow Graph 中选中 Script Node，`e` 调用 `$EDITOR` 打开实际脚本。
- 在 TUI 中设置 Runner、输入 Schema、输出 Schema、超时、权限声明。
- 在 Source View 可查看脚本、对应 workflow.md、graph.yaml 和测试 Fixture。
- AI 也可调用 `script.propose_edit` 生成补丁，用户查看文本 Diff 与权限 Diff 后决定发布。

### 12.2 最小脚本执行协议

- 输入：经 Schema 验证的 JSON，放在受控输入文件或 stdin。
- 输出：结构化 JSON 文件，Schema 验证后作为业务输出。
- stdout / stderr：仅作为日志，不能随意当作结构化结果。
- 退出码：表示进程技术状态；测试业务是否通过应明确输出 `passed`。
- 子进程具备明确 working directory、最小环境、超时、输出上限、资源配额。

示例 Nushell 逻辑：

```nu
let result = (do { cargo test --workspace --offline --quiet } | complete)
{
  passed: ($result.exit_code == 0)
  exit_code: $result.exit_code
} | to json | save -f $env.PROD_OUTPUT_JSON
```

这是 Runner Contract 的演示脚本，具体 Shell 版本与环境变量接口应由实现测试确定。

### 12.3 权限模型

权限声明不是授权。每一次无人值守运行依据：

`Published Script Hash + Capability Set + Resource Scope + Approval Lifetime`

能力至少包含：可读/可写文件路径、网络权限、允许的可执行程序、密钥访问、业务 API 范围、最大 CPU/内存/时间/日志量。

- `Safe`：仅类型化内建 Action 与受限表达式。
- `Sandboxed`：已授权隔离脚本。
- `Trusted`：用户明确选择的受信任本机脚本，不作为默认或隐式回退。

Linux 优先研究 Bubblewrap + namespace + cgroup/进程限制；不能假定 Bubblewrap 本身就实现完整策略。沙箱不支持时，未授权脚本默认拒绝自动执行。

### 12.4 调试能力

- `workflow validate`：静态解析/类型/权限校验。
- `workflow simulate`：使用 Fixture 模拟节点输出，无外部副作用。
- `workflow run`：真实执行，明确显示影响。
- `workflow trace`：每一步的输入、输出、状态与原因。
- `workflow replay`：按已有历史重建/查看，不重新触发真实副作用。
- 后续增强断点、逐步运行、Graph Diff、失败节点重试及局部复算。

---

## 13. Scheduler：周期计划与持久化执行

### 13.1 触发类型

- Manual：用户在 TUI/CLI 显式运行。
- Once：指定日期或具体瞬时运行。
- Cron：在显式时区周期触发。
- Interval：按明确间隔触发（说明固定速率/完成后延迟语义）。
- Event：某任务完成、CI 回调、文件变化等符合条件时运行。
- Dependency：上一个工作流成功后运行。

人工重复任务和机器自动化 ScheduleTrigger 应共享基础日历算法但保持业务类型区分。

### 13.2 容错策略

- `missed`: skip / run_once / replay_limited，默认 `run_once`，且记录漏执行统计。
- `overlap`: skip / queue / parallel（受并发上限约束），默认 skip。
- `retry`: only safe / never / configured with max_attempts and backoff。
- `timezone`: IANA 时区，不能只保存当前 UTC 偏移。
- `dst_resolution`: 明确描述缺失/重复本地时间处理。
- `on_suspend/resume`：区分机器睡眠造成的时间跳跃与用户工作会话。
- `dedupe`: stable key based on schedule_id + occurrence_identity + workflow_revision/policy。

### 13.3 调度架构

首发执行路径：

目标执行路径：

`Trigger Calculator -> Durable Queue (SQLite) -> Daemon Coordinator -> Supervised Local Worker -> Restricted Child Process -> Persist Result -> Activate Successors`

当前生产实现由 Daemon 协调器启动独立 `mcbtodo-worker` 子进程，再由 Worker 启动受限执行子进程；其进程接线已存在，但故障注入、进程树回收和归档验收尚不完整。

首发拓扑及 Worker 信封、fencing、取消与恢复契约见[第 11.3 节](#113-首发-worker-协议与故障边界)。受限脚本子进程仍运行在 Worker 管理之下，不能将它本身描述为 Worker。分布式 Worker 集群、远程 Worker 和跨设备执行是未来可选项，不属于首发验收范围。

- Scheduler 只有一个权威；systemd 管理 Daemon 生命周期，可以辅助启动，不独立重复执行同一工作流。
- 断电关机时无本地执行能力；开机恢复时按 missed policy 处理。
- systemd `--user` 在默认环境与登录会话相关；需要登出后继续运行时可由用户明确配置 linger。
- 不能保证硬实时触发；延迟、离线和积压需要可见诊断。

---

## 14. AI-native：统一接口、提案、授权与防回环

### 14.1 AI 一等公民，但不接管可靠性

AI 负责规划、分解、文本分析和生成建议；Rust Core 负责身份/权限、类型检查、图验证、事务提交、调度和审计。即使不连接任何模型，TUI/CLI 仍可正常工作。

### 14.2 API 分类

| 领域 | 典型命令 / 查询 |
|---|---|
| Workspace | workspace.inspect, settings.get/update |
| Project | project.create/list/update/archive |
| Task | task.create/update/complete, task.search |
| WorkUnit | unit.create/split/merge/update, unit.ready |
| TaskGraph | graph.list/link/unlink/validate/diff |
| Calendar | calendar.query, event.create/update, block.assign/move |
| Time | timer.start/pause/resume/stop, session.list/correct |
| Workflow | workflow.create/validate/publish/run/cancel/inspect |
| Script | script.inspect/propose_edit/validate/approve |
| Schedule | schedule.create/update/pause/preview_occurrences |
| Evidence | evidence.attach/list, acceptance.evaluate |
| Proposal | plan.preview/apply, approvals.list/approve/reject |
| Execution | run.status/logs/trace/retry_if_safe |
| Reports | report.day/week/project/export |

TUI 的临时焦点、光标与动画是 UI 行为；如果 AI 要“把界面切换到某项目”，可提供独立的 Session Control Adapter，但它不应该绕过业务 API。

### 14.3 提案式写入流程

`Read authorized context -> Generate structured Proposal -> Validate -> Dry preview / Graph Diff -> Approve if needed -> Apply with expected_revision -> Journal / Broadcast`

- 大批量操作先预览并检测冲突。
- 计划应用要求幂等键和版本预期。
- 高影响操作必须明确授权：批量删除、修改运行脚本、扩大权限、自动化发布、外部服务写入。
- AI 只能取得当次会话/项目允许的工具权限，不能自己在 Prompt 中宣布获得权限。

### 14.4 Agent Policy

作用域：项目 ID、对象类型、可见文件、工具集合、允许的外部服务。

预算：调用次数、运行时长、并发数、费用/Token 限额（若外部模型能报告）。

当前守护进程对单次工作流运行限制为最多 4 次 Agent 尝试和累计 60 秒，并通过
SQLite 持久化预算。守护进程全局最多协调一个外部工作节点。适配器调用使用其
配置超时与运行剩余预算中较短的一项。详细记账与重启行为见 [Agent 适配器](agent-adapter.md#per-run-limits)。

终止：任务完成、预算耗尽、人工取消、达到错误上限、检测循环。

事件元数据包含 `origin`、`actor_id`、`causation_id`、`correlation_id`、`depth`。设定最大级联深度，防止“AI 修改任务→触发自动化→再次修改任务”的无限回环。

### 14.5 信任边界

Markdown、外部网页、GitHub Issue、测试日志、AI 输出均可包含不可信内容。内容文本不能覆盖系统权限策略；模型的结构化返回也必须校验。MCP Adapter 只是入口，不是授予任意本地权限的捷径。

---

## 15. 后台 Daemon、IPC、CLI 与生命周期

### 15.1 进程划分

- `mcbtodo-daemon`：常驻 Daemon，持有业务协调、文件监听、运行时 DB、计时与调度。
- `mcbtodo`：按需运行的 TUI/CLI，可有 `mcbtodo mcp` 子命令作为独立 stdio MCP Adapter。

执行边界：首发 Daemon 是队列、调度、审批和运行状态的唯一权威；Daemon 在自身事件循环中运行会写 Vault 的内建 Action，并为脚本/Agent 尝试启动独立 Worker，由 Worker 以受限子进程/沙箱执行脚本。Worker 不拥有业务状态，也不能绕过 Daemon 的授权和审计边界。不同进程本身不等于沙箱。

首发执行架构冻结为单用户、单机、单活动 Vault、单写入 Daemon 加按节点启动的独立本机 Worker。Daemon 保有队列、授权和运行状态的唯一权威；Worker 只执行固定 revision 的单次尝试，通过私有协议接收受限信封并返回带 `attempt_epoch` 的结果。协议与故障语义见[第 11.3 节](#113-首发-worker-协议与故障边界)。分布式 Worker 集群、远程 Worker 和团队执行延期。当前生产路径已接通独立 Worker 启动和监督，Linux 端到端测试已覆盖真实工作负载取消后的进程树回收；错身份 Result 的编码帧由 fixture 子进程写入 pipe，并通过生产 decoder/reader 与 supervisor 状态路径测试；打包 Worker 子进程故障帧和故障组合验收仍是 M5 缺口。当前 Daemon IPC 通过 `mcbtodo-protocol`；单仓库 Cargo Workspace 与 `cargo xtask` 是工程范式。

### 15.2 IPC

当前使用位于 `~/.mcbtodo/state/mcbtodo-daemon.sock` 的本机 Unix socket 和 `mcbtodo-protocol` 中的类型化消息。以下列表是持续适用的协议设计要求；快照/增量事件订阅等未由现有协议证明的能力仍属目标：

- `protocol_version`, `request_id`, `idempotency_key`, `expected_revision`。
- 明确错误类型：Conflict、NotFound、PermissionDenied、InvalidGraph、InvalidTemporal、Unavailable、Internal 等。
- 限制 Socket 文件访问权限，并在 Linux 验证对端身份；不只靠“路径难猜”作为安全机制。
- 支持 Snapshot + Incremental Event stream；客户端重连后按 revision 获取最新状态。
- 业务 API 不能依赖 TUI 当前打开或活跃。

### 15.3 systemd

Daemon 以用户身份运行，通过服务日志提供诊断信息。发行归档和 `mcbtodo-daemon install-service` 共用一份 [systemd 用户服务模板](../apps/mcbtodo-daemon/assets/mcbtodo-daemon.service)。Cargo 安装后，`install-service` 将当前可执行文件的路径写入用户单元，保留已有用户单元。服务由用户通过 systemd 启用。[部署指南](../deploy/systemd/README.md)

### 15.4 CLI 示例

```sh
mcbtodo                # 默认打开月历 TUI
mcbtodo calendar       # 查询今天的日历
mcbtodo timer start
mcbtodo timer pause
mcbtodo graph          # 导出任务依赖图
mcbtodo workflow validate examples/workflow/graph.yaml
mcbtodo workflow simulate examples/workflow/graph.yaml --fixture examples/workflow/fixtures/failed.json
```

以上仅为命令形态示意，不构成完整或精确的当前 CLI 语法。实际可用命令以 README 和 `mcbtodo --help` 为准；设计要求中的能力是否已实现，见架构验收评估。

---

## 16. 软件架构与技术候选

### 16.1 当前 Cargo workspace 与目标边界

```text
apps/
├── mcbtodo/                 # mcbtodo：CLI、TUI 启动入口、MCP、提案、备份/诊断
├── mcbtodo-daemon/          # mcbtodo-daemon：IPC 服务、计时、调度和执行协调
└── mcbtodo-worker/          # mcbtodo-worker：执行已授权的单个节点 Attempt

crates/
├── agent-codex/           # Codex app-server adapter
├── application/           # shared business queries, commands, workflow use cases
├── client/                # typed client shared by CLI and TUI
├── domain/                # domain types, time semantics, projections, graph
├── execution/             # Worker adapters for Agent, sandbox, scripts and notifications
├── platform/              # WorkspacePaths, config, Agent policy
├── plugin/                # framed stdio Agent plugin protocol
├── ports/                 # business-facing adapter contracts
├── protocol/              # daemon IPC types and framing
├── storage-sqlite/        # daemon workflow, focus and notification persistence
├── tui/                   # terminal UI, input dispatch and presentation
├── vault/                 # Markdown persistence, validation, watcher, event journal
└── workflow/              # workflow definition, compiler, publication, scheduler logic

xtask/                     # mcbtodo-xtask package; cargo xtask check/smoke/package/agent
```

以上是当前源码树的角色概览，不展开 Rust 模块。workspace 还包含根 `Cargo.toml`/`Cargo.lock`、`.cargo/config.toml`、`.agents/`、`docs/`、`examples/`、`deploy/` 与 `.github/`。`.agents/` 保存项目 AI harness/workflow 注册表及来源文档；`cargo xtask agent` 可按注册表准备上下文并校验缓存。workspace package 名与 binary 名见[命名规范](naming.md)。

依赖方向以领域职责为界：应用入口依赖共享 application 用例及较低层库；`application` 仍未覆盖所有适配器读取与编排，不能描述为已完成的唯一业务入口。不要为设计图中的每个逻辑模块额外拆 crate；只有形成稳定边界后再拆分。实际 crate 均以 `mcbtodo-` 为 package 前缀，目录名保持短角色名。

### 16.2 技术建议（允许替换）

| 职责 | 候选技术 |
|---|---|
| 终端 UI | Ratatui + Crossterm |
| Tokio daemon 与本地通信 | Tokio |
| 命令行 | clap |
| Markdown 解析 | comrak / pulldown-cmark |
| YAML/Front Matter | 可保留注释格式的编辑器 + 严格类型校验 |
| 数据序列化 | serde |
| SQLite | rusqlite + SQLite FTS5 + WAL |
| 文件变化 | notify |
| 任务图 | petgraph |
| 时间与时区 | time / chrono + IANA timezone support（库组合再验证） |
| 结构化输入输出 | JSON Schema / schemars |
| 本机 MCP | rmcp |
| 网络集成 | reqwest |
| Script Runner | 原生进程 + Nushell/Bash；隔离可研究 Bubblewrap |
| 日志与观测 | tracing、结构化事件 |
| 内容版本哈希 | blake3 / SHA-256（按用途区分） |
| Linux 服务 | systemd --user |

选型以 ARM64 原生编译、可靠维护、正确性与可测试性为原则。对于日期/时区和 YAML Round-trip，优先验证库的实际能力而非单纯列依赖名称。

---

## 17. 配置、数据路径与跨平台

```text
~/.config/mcbtodo/config.toml  # 用户偏好
~/.config/mcbtodo/agents.toml  # 本机 Agent 授权策略
~/.config/mcbtodo/keymap.toml  # TUI 主视图导航与视图切换覆盖
~/.mcbtodo/vault/              # Markdown 与 YAML 定义
~/.mcbtodo/vault/.mcbtodo-events.sqlite3  # Vault 完成事件及恢复日志
~/.mcbtodo/state/focus.sqlite3             # 专注会话、工时和恢复状态
~/.mcbtodo/state/workflow.sqlite3          # 工作流计划、运行、审批、轨迹及产物
~/.mcbtodo/state/workflows/revisions/      # 不可变工作流定义和执行快照
~/.mcbtodo/state/proposals/                # 提案队列记录
~/.mcbtodo/cache/agents/                   # xtask agent 的生成上下文与校验后下载缓存
```

路径相对于当前用户主目录确定。用户配置统一位于 `~/.config/mcbtodo/`；Vault、权威运行状态、发布快照及可重建缓存位于 `~/.mcbtodo/`。运行时 socket、进程锁和临时沙箱目录属于运行状态，不属于用户配置。备份覆盖配置、Vault 与权威运行状态，不备份可重建缓存、socket、锁和临时目录。Artifact 内容保存在工作流 SQLite 数据库中，不单独维护文件目录。

根目录、配置文件、默认 Vault、提案目录、socket 和缓存路径由 `mcbtodo-platform::WorkspacePaths` 集中解析；表中的 SQLite 文件名和 revision 子目录是当前存储布局。上表不穷举锁及临时目录等运行期路径，详细路径以 `crates/platform/src/lib.rs`、备份实现和 `docs/architecture.md` 为准。发布快照位于 `state/workflows/revisions/`，不是 Artifact 文件目录；workflow Artifact 存在 `workflow.sqlite3`。

默认 Vault 的逻辑身份由其 canonical 目录确定。若 `~/.mcbtodo/vault` 是指向真实目录的符号链接，默认 Vault 提案绑定和备份均以解析后的目标为准；该链接目标会以普通目录内容恢复，不保留主机特定链接。备份拒绝 Vault 内容树内的符号链接，且不会因此将显式选择的其他 Vault 纳入默认备份。

设置项包括：显示时区、每周首日、默认首页行为、终端主题、ASCII fallback、键位、工作时间、提醒安静时间、工时恢复策略、自动化预算、归档与历史保留策略。

---

## 18. 通知、专注与个人工作效率

- 任务提醒与定时执行通知由 Daemon 产生，TUI 不必保持打开。
- 提醒 occurrence 入库与桌面投递任务在同一 SQLite 事务中写入；通知 Inbox 的 `created_at` 表示入库时间，不表示桌面送达时间。
- Linux 图形会话下可使用桌面通知；无图形会话时仍保留可查询的提醒 Inbox。桌面 helper 失败最多重试五次并采用有界指数退避，Daemon 重启会重新排队未确认项。
- 外部桌面通知无法与本地数据库形成原子事务；helper 已产生副作用而成功状态尚未落盘时崩溃，恢复可能重复投递。提醒只回看最近 24 小时的 occurrence，更久的停机缺口不自动补偿。
- 工作计时与番茄钟独立于 UI repaint，状态持久化。
- 支持多次工作会话，计划与实际偏差、每日/每周/项目聚合、可取消或暂停的专注模式。
- 休眠/崩溃恢复默认标记 NeedsReview；拒绝静默把离线时间累加为有效工时。
- 统计指标至少分清：投入时间、完成的工作单元、计划与实际偏差、自动化执行成功/失败/未知次数、延期与阻塞原因。
- 报告可输出 Markdown/CSV/JSON，历史报告是派生视图，不写回任务权威字段。

---

## 19. 安全、隐私与供应链

### 19.1 默认最小权限

- Daemon 与 TUI 以普通用户运行；脚本默认没有网络、完整 HOME 访问、密钥读取、Daemon Socket 控制权限。
- 路径白名单经 realpath / symlink 安全校验；工作目录和输出目录显式配置。
- 不把 OAuth token 和模型 API key 直接拼接到 stdout、日志或普通 Markdown；优先系统密钥服务或受限权限存储。
- 所有可自动执行脚本记录来源、代码版本、权限授权及最后审批者。

### 19.2 生命周期安全

- AI 写出脚本后默认只是草稿；用户/预授权规则明确批准脚本版本之后才可自动执行。
- 增加权限或修改脚本字节，先前授权默认失效。
- Script Runner 独立进程；沙箱不可用时对未经信任脚本 fail closed。
- 对外调用尽量使用具备幂等性和可追踪性的方法。
- MCP 只开放业务能力，不提供万能 SQL/Shell 工具。
- 审计记录与用户日常文本分开，避免被普通 Markdown 编辑无痕改写。

### 19.3 保护外部数据源

网页、GitHub Issue、日志、Markdown 正文都可能包含提示注入文本。Agent 仅把它们当任务资料，不作为系统权限指令；进入命令执行前仍需独立策略检查。

---

## 20. 集成、导入导出与同步策略

**首发**：本机 Markdown、可导出 CSV/JSON、Mermaid/DOT、有限窗口 iCalendar 导出（日期区间、精确时段与截止语义）、GitHub Issue 关联（可作为后续里程碑）。iCalendar 导入需要另行定义重复规则、TZID/VTimeZone、UID 去重和冲突审批语义，不允许静默部分导入。

**之后**：GitLab/Jira、CalDAV、WebDAV、同步服务、移动端或 GUI 客户端。Git 适合版本追踪和备份，但不自动解决正在运行的 WorkflowRun、TimeSession 和多个设备并发编辑问题。

第一阶段严格规定**一个 Vault 只有一个活动写入 Daemon**。多机共享 Vault 时采用明确的停止-备份-同步-验证-启动流程；没有冲突解决协议前，不开放两机同时写入。

---

## 21. 测试体系与可验收的不变量

### 21.1 分层测试

1. Domain property tests：ID 唯一、状态合法、依赖无环、重复命令不重复计时。
2. Temporal edge cases：跨月、闰年、日期端点、时区、DST、长期跨度渲染、重复任务与触发器区别。
3. Markdown round-trip：未知字段、注释、代码块、无效 YAML、外部编辑冲突、文件重命名。
4. Compiler tests：非法端口、类型错误、无效分支、环、缺失脚本、权限不足。
5. Runtime failpoints：入队、领取、脚本运行、结果持久化每个阶段注入崩溃。
6. Scheduler/Worker tests：关机漏执行、重复发生时间、重叠策略、队列积压、Daemon/Worker 分别重启、取消回收、过期执行隔离、迟到结果 fencing，以及副作用不确定时的 `outcome_unknown`。
7. Security tests：越权路径、未授权网络、权限扩大、Agent 提示注入、脚本版本替换。
8. IPC/CLI/MCP parity：多个入口若暴露同一业务场景，应保持领域规则、
   revision 行为与可观察结果一致；入口的命令集合、交互流程和安全审批
   边界不要求相同。代表性覆盖见
   [`architecture-assessment.md`](architecture-assessment.md#representative-adapter-coverage)。
9. TUI interaction tests：模式转换、选择集合、Space 菜单、窄终端、Graph Diff、键位冲突。
10. Linux ARM64 smoke tests：目标 Omarchy/Arch 环境原生编译、systemd、进程隔离、通知降级。

### 21.2 首批硬性验收指标

| 指标 | 通过条件 |
|---|---|
| 核心模型 | 不合法 DAG 不得写入有效业务状态 |
| 时间建模 | 日期区间、精确时段、截止日、项目活动期可并存，投影不相互混淆 |
| 文档存储 | 外部 Helix 修改可校验，格式错误不覆盖原内容 |
| 运行可靠性 | 重启后已确认任务、工时和审批状态不丢失；不确定运行显式标记，人工核对须单独审计且不得伪造机器证据、自动重放或触发成功依赖 |
| AI 可操作性 | 主要业务操作能通过类型化 API 完成；无未经授权的脚本执行 |
| TUI | 首页月历，键位切换视图，Space 可视菜单、Selection-first 真正可用 |
| 安全 | 不可信脚本不得默认继承用户完整权限；权限变化需要重新授权 |
| 可观测性 | 任何触发都能追踪到运行结果、失败、跳过或明确未知状态 |
| 平台 | 原生 ARM64，不需要运行 x86_64 容器或仿真层 |

### 21.3 建议性能预算（设计目标，须压测验证）

在普通个人工作区、正常磁盘与终端条件下，目标是“无感常驻、终端快速反馈”，而不是提前宣称真实基准数字。可以将可量化指标列为待验证门槛，例如：1000 WorkUnit 的查询与搜索可交互、100 节点 DAG 可导航、Daemon 空闲时无持续忙循环、运行日志有强制体积上限。具体数值需要以目标 ARM64 硬件压测结果冻结。

---

## 22. 里程碑与当前交付状态

以下状态基于 `docs/architecture-assessment.md` 的验收基线（2026-11-22）。阶段沿用产品规划，状态反映当前证据；“部分”表示有实质实现，但尚未满足该阶段全部目标。不能把测试通过等同于完整日用验收。

| 阶段 | 状态 | 当前已实现的主要部分 | 仍未完成的目标/验收 |
|---|---|---|---|
| M0 — 核心领域与时间规范 | 部分 | UUID 业务身份、时间类型与投影、重复规则、WorkUnit DAG。跨层测试验证日期与定时截止日期、跨显示时区的归属日、定时重复事件的 DST 跳时和回拨 | 定时重复事件的完整交互视图矩阵与桌面会话待验收。 |
| M1 — Markdown Vault + CLI | 部分 | Vault 加载、校验、原子写入、监听；CLI 与 TUI/MCP 共用 application 查询投影和业务命令，TUI 复用当前 snapshot | 尚无按实际 Vault 规模验证的查询性能基线；不以通用 daemon 查询/索引服务作为首发要求。 |
| M2 — Daemon + IPC + Time Engine | 部分 | Unix socket、计时持久化/恢复、提醒持久 outbox 与有界重试、完成事件分页、Vault 变更校验、systemd smoke | 桌面送达为崩溃窗口下的至少一次语义；24 小时以外 occurrence 不补偿，仍需更完整的跨进程、长时间运行和平台验收。 |
| M3 — Calendar-first + TUI | 部分 | 日历投影、Inbox triage、Graph/Timeline 等视图及若干创建/编辑流程；首页策略支持 Month/恢复上次视图并有配置和状态存储测试；日历行焦点包含 UUID、类型和 occurrence；Agenda/Day 切换、reload，以及 Month/Week/Day/Agenda 四种日历视图间 16 种有向切换均有针对性回归测试 | 完整 Month/Week/Day/Agenda/Outline 交互矩阵尚未整体验收；多日定时条目、时区边界、窄终端与真实交互会话仍需验证。 |
| M4 — Task Graph + Workflow Compiler | 部分 | DAG 校验、Graph Diff、类型化 workflow 编译、发布、模拟 | 大型图渲染与规模行为未充分验证；更广的图交互仍属目标。 |
| M5 — Durable Scheduler + Runner | 部分 | 持久化调度、授权、沙箱执行、恢复和产物；Daemon 已启动并监督独立 Worker，使用发布 revision 字节、Ready/Started/Start 门、attempt 身份校验及 Daemon 单事务结果提交；Linux 覆盖真实进程组终止，以及生产 Daemon 取消 sandbox 脚本后 `OutcomeUnknown`、不提交步骤、Worker 回收和后代停止；畸形 readiness 子进程回包与错身份 Result 经真实协议 decoder/reader 到 supervisor 的测试均已覆盖，后者由 fixture 子进程经 pipe 输出帧并验证 `OutcomeUnknown` 和重启不重放；发行归档 smoke 验证 packaged Worker 的非零脚本退出和失败结果持久化 | 仍缺打包 Worker 协议故障及组合故障验收。RunStore epoch 是单 Daemon 状态 fencing，不是独立 Worker 租约，也不能阻止失联进程的外部副作用。人工观察不等价于工作流机器验证或成功状态。 |
| M6 — MCP / AI 操作 | 部分 | 类型化 MCP tools、提案、revision 校验、人工审批、Agent policy/预算；MCP、CLI、TUI 对各自暴露的共享场景使用 application 规则，并在架构评估中维护代表性场景到适配器及测试的矩阵 | 现有矩阵不是完整命令清单；扩展适配器的共享场景时需补充对应 owner-layer/adapter 测试证据，并继续验证 revision 与权限行为。入口功能集合不要求相同。 |
| M7 — 生产力完善与发布 | 部分 | 备份/恢复、诊断、报告、Linux 打包；CI 覆盖 AArch64 与 x86_64 构建/测试；ARM64 本机解包产物 smoke 和 systemd 恢复 smoke | 尚未证明干净系统安装、目标桌面交互体验及持续日用可靠性；x86_64 的发行包现场安装和等价 smoke 验收仍需证据。 |
| M8 — 选择性增强 | 按设计延期 | 无首发要求 | GitHub/GitLab、CalDAV/WebDAV、移动端、插件及跨设备同步仍延期。 |

这些状态不是对后续工作的削减。产品目标、十二步端到端场景及其安全不变量仍然有效；具体证据、日期与缺口以架构验收评估为准。

---

## 23. 一个贯穿全系统的端到端验收案例

以“完成 Rust OS 页表管理模块”为场景。自动化验收使用 `tests/fixtures/vault/` 中的固定输入。

1. 在 `projects/rust-os/project.md` 建立长期项目，活动期 2026-11-20 至 2027-03-01，截止日是 2027-03-01；日历只显示长期条带，不占用整天。
2. AI 通过 MCP 读取项目，提出四个 WorkUnit：阅读 Sv39、实现映射、地址转换、测试；分别给出预计时间和验收标准。
3. Rust Core 校验 DAG，TUI 在 Graph Diff 中显示两个可并行分支；用户批准后 Markdown 文件更新。
4. 用户在月历选中 11/20，创建 09:00–11:00 的 TimeBlock，关联“实现映射”；日历出现精确安排。
5. 09:00 手动或通过明确的设置启动 Focus Session，关闭 TUI 后 Daemon 持续保存计时状态。暂停后生成实际时长。
6. 为测试 WorkUnit 绑定 `nightly-check` 工作流。用户在 Helix 编写 `test.nu` 并审核脚本权限。
7. 设置每天 23:00 的 Cron 触发，注明 `Asia/Tokyo`，漏执行 `run_once`；Workflow Compiler 固定脚本快照。
8. 到期后 Scheduler 将 Run 入队，Worker 隔离运行测试，产出 TestResult 和日志 Artifact。
9. 如果测试失败，Condition 走失败分支，Agent 在授权预算内分析日志并提出修复方案；未经审批不能直接修改源码。
10. 用户再次打开 TUI，在日历看到当天计划，在 Graph 看到等待验收节点，在 Workflow 查看失败轨迹，在 AI 面板批准或拒绝提案。
11. 用户确认完成条件后，WorkUnit 完成；系统保存 Evidence 引用、领域事件与实际工时，生成每日 Markdown 报告。
12. 模拟 Daemon 于“外部脚本已执行、结果尚未持久化”时崩溃，恢复后进入 OutcomeUnknown 或安全核对流程，不能产生未经授权的重复副作用。

如果上述十二步都能可靠完成，这个系统就具备与普通待办工具显著不同的技术价值。

---

## 24. 决策记录：已经确定、尚须验证、不可混淆

### 产品设计已定（不等于代码已实现）

- 月历为默认首页；其他视图统一可切换。
- 日期精度和 DateTime 精度独立于“长期/短期”；ActivePeriod、Busy Event、TimeBlock、Deadline、Trigger、Session 语义分离。
- Helix-inspired Selection-first + Space Mode 是产品一级设计约束。
- Markdown-first，不强求运行日志也是 Markdown。
- 统一 Command API，MCP/TUI/CLI 共用；AI 不绕开业务事务与权限。
- Task Graph 与 Workflow Graph 各司其职；Evidence 连接二者。
- 发布脚本版本不可变；定时执行有持久化队列与审计。
- 单用户/单机/单活动 Vault 是首发范围。

### 尚须原型验证

- Ratatui 在 ARM64/Wayland 所用终端与 tmux 中的键位、剪贴板和终端回显行为。
- 月历多日色条在 80×24 的最小可用布局、Unicode 宽度与 CJK 标题截断。
- YAML 格式保真编辑能力及 Markdown Front Matter 的最小修改策略。
- 实际 Linux ARM64 沙箱能力、用户 namespace 配置与平台限制。
- Cron DST 规则、系统休眠事件，以及手工时间调整情况下的稳定语义。
- 大型 Graph 的终端布局和渲染性能。

### 不可混淆

- **WorkUnit ≠ DomainEvent**：工作单元是待执行的工作，事件是已发生事实。
- **CalendarEvent ≠ ScheduleTrigger**：参加活动和自动执行脚本不是同一种东西。
- **ActivePeriod ≠ Busy TimeBlock**：长期项目不是全年占据日历。
- **WorkflowDefinition ≠ WorkflowRun**：用户修改定义不改变正在运行的实例。
- **Script success ≠ Task accepted**：通过 Evidence/Acceptance 才能验收。
- **Markdown-first ≠ SQLite disposable**：运行时权威 SQLite 不可随意删除。
- **“一次触发去重” ≠ 外部副作用恰好执行一次**：不能虚构 Exactly-once 保障。
- **AI can operate ≠ AI can self-authorize**：权限始终由 Rust Policy Engine 约束。

---

## 25. 参考标准与设计依据

- Helix Keymap 与 Modal/Select 模型：https://docs.helix-editor.com/keymap.html
- Helix 使用说明：https://docs.helix-editor.com/master/usage.html
- Ratatui 官方文档：https://ratatui.rs/
- MCP Rust SDK：https://rust.sdk.modelcontextprotocol.io/
- SQLite WAL 与 PRAGMA：https://www.sqlite.org/wal.html 、https://www.sqlite.org/pragma.html
- RFC 5545 iCalendar、DATE/DATE-TIME 与重复规则：https://www.rfc-editor.org/rfc/rfc5545

> **当前实现状态**：已交付范围、验收证据与下一步架构缺口见[架构验收评估](architecture-assessment.md)；本设计中的端到端案例和验收条件仍是目标，不表示整条链路已全部实现。
