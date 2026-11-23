# Workflow Agent adapter

The reusable business-facing contract is
`mcbtodo-ports::AgentProvider`. Linux Workers use the Unix socket adapter in
`mcbtodo-execution` by default. The policy can select the Codex app-server
adapter or a one-shot stdio Agent plugin for each authorized call. After
grant validation, the Worker constructs the selected adapter. The adapters
check availability at launch or connection time. Failure terminates the call.

On supported Linux targets, a workflow Agent node can call a trusted adapter listening on a
Unix stream socket. Each call requires a grant for the node’s exact capability
and the exact publication SHA-256. The adapter receives the node’s bound inputs
and returns its declared outputs. Configure any model credentials in the
adapter process.

## Authorization

Create `~/.config/mcbtodo/agents.toml` alongside `config.toml`. This example
authorizes `analyze_test_failure` for one publication. Replace the example hash
with the full lowercase hash printed by `mcbtodo workflow publish`.

```toml
timeout_seconds = 30

[providers]
unix_socket = "state/agent.sock"

[[allow]]
capability = "analyze_test_failure"
revision = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
provider = "unix_socket"
```

Add one `[[allow]]` entry per capability and revision pair. The Worker reads
the current file before each call. Removing an entry revokes future calls.
Publishing changed graph or script bytes yields a different hash, so grant that
revision separately. Calls fail when the file is missing or the pair is absent.
Use a user-owned regular file, preferably mode `0600`, in user-owned
configuration directories protected from group and other writes. The host
checks the grant and caps the deadline before calling `AgentProvider`.

Workspace backups include the user-owned policy file. After a restore, start
your adapter separately to recreate its socket.

The socket path is relative to `~/.mcbtodo`. Keep `~/.mcbtodo` private, and keep
the socket and its parent directories user-owned. Its parent directories must
be protected from group and other writes. The daemon rejects symlinks along the
socket path and checks that the socket peer runs under its own user ID before
sending the request.

## Socket protocol

Each connection carries one request and one response. Each frame starts with
a four-byte unsigned big-endian length followed by that many bytes of UTF-8
JSON. The length must be between 1 and 16,384 bytes inclusive. The request
contains exactly these fields.

```json
{
  "schema": "mcbtodo.agent/v1",
  "capability": "analyze_test_failure",
  "inputs": {"summary": "The integration test timed out"}
}
```

The adapter replies with `schema` and `outputs`. Each output name must appear
exactly once, and the keys and types must match the compiled node’s declared
outputs. The supported values are JSON booleans, signed 64-bit integers, and
strings. The daemon rejects extra response fields and extra or missing output
ports.

```json
{
  "schema": "mcbtodo.agent/v1",
  "outputs": {"title": "Investigate the integration test timeout"}
}
```

The socket exposes a single request and reply operation. There are no fields
for tool calls, file access, or daemon commands. Give the adapter only the
access it needs and bind only inputs that may be sent to it. If the adapter
uses an external model, it controls what it sends to that service.

The timeout covers connect, request write, and response read under a single
deadline. `timeout_seconds` defaults to 30 and accepts values from 1 to 120.
On cancellation or timeout the Worker closes the connection. An adapter should
stop work when it observes the disconnect. If the adapter cannot produce a
response, it can close the connection. A denied call or connection failure before
dispatch produces a workflow node failure. Once sending begins, a disconnect,
timeout, invalid response or output shape can leave the run in
`outcome_unknown`. The daemon fences that Attempt and does not replay it.
When the Worker can report a dispatch with no trustworthy result, it sends
`agent_outcome_unknown` with the Attempt identity and the selected adapter's
host-defined `provider_id`, including after it has received a Cancel frame.
If the Daemon accepts the report, it records `outcome_unknown` and logs
`run_id`, `attempt_id`, and `provider_id`. Cancelling still fences the Attempt
in the Daemon and prevents any output commit. Daemon-detected failures such as
a Worker crash can leave only a process or IPC diagnostic.

## Per-run limits

The daemon allows up to four Agent attempts and 60 seconds of cumulative
Agent attempt time per workflow run. These fixed limits apply across all Agent
nodes in the run. Workflow attempts share one Worker slot by default. The
Linux Daemon can be configured with up to four slots. The
Daemon starts and supervises a separate local `mcbtodo-worker` process for the
attempt; the Worker executes the adapter but does not own scheduling or durable
run state. Packaged-binary failure combinations and external side-effect
reconciliation remain unverified; see
[the architecture assessment](architecture-assessment.md).
Each adapter call uses the earlier of its configured timeout and the remaining
run deadline.

Set the optional `max_output_bytes` in `agents.toml` above `[providers]` to cap
the normalized result of each Agent call. For example, `max_output_bytes = 4096`
allows up to 4,096 UTF-8 bytes of compact JSON for the `outputs` object. The
minimum is 2 bytes for `{}`. Omit the setting to use the adapter's existing
limits alone. The Worker applies this host policy after decoding the result,
so protocol framing and streaming keep their own limits. An over-budget
result records `outcome_unknown` without a committed step, budget refund,
or automatic replay.

The daemon records an attempt before dispatch, so authorization failures and
adapter errors also count. It measures elapsed attempt time from dispatch
through result delivery from the Worker. A completed step and the return of any
unused time are recorded in the same transaction. A run whose Worker exits
before the result is durably recorded keeps the full reservation and enters
`outcome_unknown`. The daemon never replays that attempt.

If a node has exhausted its run's call count or wall time, the daemon fails the
run before contacting the adapter. If an active Worker reaches the cumulative
time limit, the daemon initiates cancellation and records `outcome_unknown`.
A run resumed from an older database
with an already executed Agent step and no accounting record is denied further
Agent calls because the earlier elapsed time is unknown.

## Shared policy validation

The Worker, doctor, backup, and restore use the same `agents.toml` parser in
`mcbtodo-platform`. The policy must be UTF-8 TOML and at most 64 KiB. The
`[providers]` table configures `unix_socket`, `codex`, and `stdio_plugin`
endpoints. If it
contains a single endpoint, grants without a `provider` use that endpoint.
With multiple endpoints, the default is `unix_socket` unless a top-level
`provider` chooses another configured backend. Set `provider` explicitly
when configuring multiple endpoints without `unix_socket`.
The top-level `socket` field is not part of the current schema and is rejected.
Each `allow` entry is a table with `capability`,
`revision`, and an optional `provider` field. Unknown fields and duplicate
grant pairs invalidate the whole policy. An omitted `allow` grants no access.

Capability names are 1–96 ASCII characters. They start with a letter and can
then contain letters, digits, hyphens, and underscores. Revisions are exactly
64 lowercase hexadecimal digits. A grant must match both strings, including
case. The endpoint must be nonempty and relative to the app home, with no
parent traversal or NUL bytes. Socket providers use a socket path.
The stdio provider uses an executable path.

Backup and restore validate the policy before copying it and preserve its exact
bytes. Neither operation needs a running adapter. The Worker validates the
current policy before each Agent call. A selected provider is never replaced by
another provider after a dispatch failure. The Unix adapter checks the live
socket path and peer identity when opening a connection.

Invalid policy files fail closed and are reported without contacting a
provider. The TUI preferences page edits `config.toml` and its Keyboard page
edits `keymap.toml`. Configure execution authorization and budgets in
`agents.toml` and Codex provider parameters in `codex.toml`.

## Codex app-server

Use a trusted Linux Codex executable with compatible app-server behavior
and a running same-user model service on a Unix stream socket under
`~/.mcbtodo`. The Worker needs Bubblewrap and Bash for this adapter. Add both
endpoints to `agents.toml` when the same workspace uses Unix and Codex
providers. Retain the exact `[[allow]]` grants. The example below adds that
provider to the same grant shown earlier.

```toml
timeout_seconds = 30

[providers]
unix_socket = "state/agent.sock"
codex = "state/codex-model.sock"

[[allow]]
capability = "analyze_test_failure"
revision = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
provider = "codex"
```

Use Settings → `F3` Agents → `F4` Codex on Linux to edit
`~/.config/mcbtodo/codex.toml`, or create it as a protected user-owned file.
The executable must be an absolute path to the installed binary. Its
canonical path must have root- or current-user-owned directories protected
from group and other writes. The same checks apply to the installed Worker.
Installations under shared writable ancestors such as `/tmp` are refused.

```toml
executable = "/home/user/.local/bin/codex"
capability = "analyze_test_failure"
instructions = "Summarize the supplied failure as a short actionable title."

[outputs]
title = "string"
```

This configuration serves one capability and one output schema. Output
types are `string`, `integer` and `boolean`. The Daemon checks the outputs
against the published Workflow before durable submission. The protected
provider file is read each attempt. Workspace backups include `agents.toml`.
Provision `codex.toml`, the executable and the model service separately
after restoring a workspace.

The Worker runs the ephemeral app-server under Bubblewrap. The sandbox has a
private PID and network namespace, no mounted Vault, no inherited user
configuration or credentials, and a bounded writable scratch filesystem.
Before initiating a turn, it requires the expected handshake and the
capabilities used by the adapter. Codex receives a single loopback route. A
supervised relay forwards a connection to the same-user model service, which
receives the bound model inputs and owns HTTP handling, upstream credentials,
and its own egress policy. The service speaks HTTP for the Responses endpoint
over the Unix stream. Forwarded bytes are capped at 256 KiB per request
direction and 2 MiB per response direction, with one connection per invocation.
Use only a service trusted to receive those inputs. Managed Codex distribution,
a built-in credential-bearing model service, and live upstream acceptance
are pending.

`mcbtodo-agent-codex` implements `mcbtodo-ports::AgentProvider` over the external app-server
protocol. The caller supplies a trusted isolated launcher, instructions, and
the expected output schema. Each invocation uses a fresh thread. The adapter
bounds each request frame to 4 KiB, each response frame to 64 KiB, total
consumed response bytes to 1 MiB per session, and accumulated assistant text
to 16 KiB. It accepts up to 4,096 stream messages while retaining a 32-message
limit for each control exchange. It correlates stream notifications to the
selected turn and item and returns typed workflow scalars. The Codex and Unix
socket adapters share the output decoder, which rejects duplicate names and
non-scalar values. The Daemon validates names and types against the published
step before committing. Before dispatch,
the host verifies support for the exact capability and applies the existing
capability and revision grant. The v1 Unix adapter delegates capability
handling to the configured peer because the socket protocol has no discovery
exchange. Before accepting success, the host rechecks deadline and
cancellation. The Daemon validates output ports and authorizes the final commit.

On Linux, `mcbtodo doctor --json` reports the `agent_policy` and `agent_codex`
checks and an `agent_providers` array. The policy check reports `skipped` for
an absent policy, `warning` for an unreadable or invalid policy, and `ok`
with the configured backends when the policy validates. Each array entry
reports the backend, whether it is the policy default, and the count of
exact capability/revision grants routed to it. The default applies to
grants that omit a provider. Each invocation still requires an exact grant.

The Codex installation result reports `preflight_passed` with the configured
capability when the protected `codex.toml` and executable pass the same checks
used by the Worker. Installation failures produce `unavailable` and a
diagnostic. Unix socket installation is `not_checked`. Provider health
stays `unchecked` in this passive report. Protocol compatibility and model
connectivity require a runtime exchange.

The Agents page (`F3` in Settings) shows configured routes and grant counts.
With multiple configured providers, select one and press `Enter` to save it
as the default. The focused row previews the number of grants that follow
the default. On Linux, press `F6` to edit the selected provider endpoint.
Adding or removing providers or revision grants requires editing
`agents.toml`. A changed save rewrites TOML formatting and comments.
Press `F5` to reload after an intervening edit.

`F4` edits Codex parameters through the same settings publisher. The editor
and Worker share Codex field validation. The Worker checks executable trust
at launch. `doctor` runs the local executable diagnosis on request.

### Protocol probe

Run `cargo xtask codex-probe --codex /absolute/path/to/codex` with the actual
trusted Linux executable you plan to use. The probe uses `/usr/bin/bwrap`
with a private scratch directory, no host network, and no inherited environment
or mounted Vault.

The probe uses the [official app-server stdio protocol](https://developers.openai.com/codex/app-server).
It correlates the `initialize` response, sends `initialized`, and requires
`thread/start` to confirm `ephemeral`, `approvalPolicy: "never"`, and a read-only
sandbox with no tool network access. It starts a text-only turn and interrupts
it. Both the `turn/interrupt` response and the matching `turn/completed`
notification with status `interrupted` must arrive within the probe deadline.
The Worker sets the platform and scratch home through its Linux launch
environment. The protocol client accepts a successful, correlated
`initialize` reply and then checks the thread options it depends on.

The second probe invokes the real provider against a single-use Responses
fixture on the isolated namespace's loopback. It checks mixed Chinese and
English inputs, the structured output schema, single-character model deltas,
and the final matching assistant message. Cancellation of an acknowledged structured turn
attempts a correlated interrupt with at most 150 ms of the existing deadline.
Cancellation, timeout, and lost results after dispatch remain uncertain.
The Worker receives this disposition and the adapter ID directly from Agent
execution. The internal Worker protocol v2 carries both in an Attempt-bound
result. Install the Daemon and Worker from the same build. This change requires
no configuration or database migration. Provider error text for uncertain
calls stays within the execution layer.

### Offline Workflow acceptance

Build the CLI, Daemon, and Worker and pass `--bin-dir target/debug` to
`cargo xtask codex-probe`. For a release archive, extract it under a protected
directory such as the workspace's `target/` and pass the extracted `usr/bin`.
The command selects Codex in the temporary workspace's policy and serves a
deterministic Responses fixture on a Unix socket. It exercises the production
CLI, Daemon and Worker. Revision authorization, provider configuration, and
local sandbox preparation complete before the Worker connects to the model
socket.
An executable that exits before answering `initialize` fails without model
traffic. The granted run records the streamed title and awaits approval
before creating a report.

The probe drops a dispatched connection, stalls one past the deadline,
terminates app-server with SIGKILL, cancels a dispatched Workflow, and
terminates the Daemon while another call is in progress. Cancellation closes
the model connection and prevents late delivery. The Daemon-death scenario
observes the sampled Worker process tree exit and the model connection close.
After restart all five run IDs retain `outcome_unknown` and zero steps.
Disconnect and app-server crash also retain the Agent diagnostic.
Resubmitting the runs produces no new request. This probe ran on Linux
AArch64 with synthetic model responses.

## Stdio Agent plugin

Set the endpoint relative to `~/.mcbtodo/`, then grant a capability to the
published revision that will use it.

```toml
[providers]
stdio_plugin = "bin/my-agent"

[[allow]]
capability = "analyze_test_failure"
revision = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
provider = "stdio_plugin"
```

Install a self-contained native executable in a protected location owned by
your user or root. Its canonical path and ancestors must be protected from
group and other writes. Linux AArch64 and x86-64 Workers use Bash and
Bubblewrap to run it with no host network or Vault mount, no inherited
environment, and bounded scratch space. The sandbox provides only the
launcher's system libraries. A statically linked executable avoids depending
on those libraries. Provision the binary separately after restoring a backup.

`mcbtodo-plugin` provides Rust helpers for the v1 wire format. A process
handles one request. Each message is a JSON object prefixed by a four-byte
unsigned big-endian length. Payloads are 1–16,384 bytes. The exchange is:

```text
Host    {"type":"hello","version":1}
Plugin  {"type":"ready","version":1,"capabilities":["analyze_test_failure"]}
Host    {"type":"execute","id":1,"capability":"analyze_test_failure","inputs":{"text":"Test timed out"}}
Plugin  {"type":"result","id":1,"outputs":{"title":"Investigate the test timeout"}}
```

The host checks the advertised version and capability before sending
`execute`. Revision authorization stays in the host. Use stdout exclusively
for protocol frames. The Worker discards stderr. The plugin must exit
successfully within the call deadline after sending its result. The Worker
terminates the sandbox on failure, deadline, or cancellation. Failure after
the first bytes of `execute` have been written can produce `outcome_unknown`.
The Daemon validates the outputs against the published workflow and controls
the final commit. The protocol currently supports one result and
process-based cancellation. The production adapter runs on Linux only.
