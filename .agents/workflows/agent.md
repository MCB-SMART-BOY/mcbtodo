# Agent Provider Workflow

Use this workflow when changing Agent invocation, policy, transport, sessions,
provider discovery, supervision, Codex integration, or execution authorization.
The host owns policy and business consistency. Providers supply capability and
execution services within an explicitly bounded request.

## Layer Responsibilities

Keep these responsibilities separate and testable.

- `AgentProvider` exposes business-level Agent invocation.
- `AgentPolicy` authorizes capabilities, paths, timeouts, budgets, side effects,
  and result writing.
- `AgentTransport` implements stdio, Unix socket, Windows named pipe, or TCP and
  returns verifiable peer or process identity where the transport can provide it.
- `AgentSession` correlates requests, streams notifications, cancels work, and
  observes connection termination.
- `AgentPolicy` resolves the selected provider for each authorized capability
  and revision. The Worker constructs that adapter. Actual launch or
  connection determines availability.
- `AgentSupervisor` starts, monitors, reaps, and optionally restarts provider
  processes according to host policy.

Do not move peer credential validation, revision authorization, approval,
budget reservation, idempotency, cancellation semantics, or final result commit
into a third-party provider. Do not let a provider write the Vault or workflow
state directly.

## Request Contract

Every provider request must have an identity and a bounded execution context.
Where applicable include request ID, idempotency key, priority, deadline,
cancellation, required capabilities, workspace root, filesystem and network
policy, output limit, wall-clock limit, execution budget, and side-effect policy.
Clearly record whether each restriction is enforced by the host, guaranteed by
the provider, observed only, or unsupported.

Timeout, cancellation, transport loss, provider crash, and malformed output can
occur after an external side effect. Preserve `outcome_unknown` and reject
stale, duplicate, unauthorized, or revision-mismatched results.
Pass typed `Scalar` inputs and outputs through `AgentProvider`. Decode outputs
from the wire once with the shared duplicate-key check. At commit, validate
those outputs against the published workflow.
Apply `agents.toml`'s optional `max_output_bytes` in the shared Worker result
admission path. Count the UTF-8 bytes of the compact JSON output object after
normalization. Keep adapter wire and streaming limits in place. Exercise an
exact fit and one-byte overflow with non-ASCII output. Overflow leaves the
Attempt uncertain without a committed step, refund, or replay.
When a Worker observes an uncertain Agent dispatch, send the fixed
`agent_outcome_unknown` status bound to the Attempt identity with the selected
adapter's host-defined `provider_id`. Verify that the Daemon records the Agent
diagnostic without a step, an artifact, or a budget refund. Keep Provider error
text out of that status. Ship Daemon and Worker together when the internal
Worker protocol changes.
Test cancellation before Start and after the Agent reads the full request.
After dispatch, the Worker disconnects and can emit the fixed uncertainty
status even after receiving Cancel. The Daemon's cancellation fence remains
authoritative for the durable outcome.

## Local Adapter Migration

Before adding a new abstraction, locate existing Unix socket, peer credential,
policy file, timeout, cancellation, request/response limit, capability, and
revision checks. Move only the transport-specific code into the transport or
adapter. Keep the existing safety checks and verify the production caller uses
the port. Missing optional tools must affect only their capability. Missing
required isolation must fail closed according to policy.

Share the Linux host-policy reader between Worker authorization and `doctor`.
Read through opened configuration directories, reject symlinks and unsafe
ownership or write permissions, and cap the read before TOML parsing. Report
missing policy separately from unusable policy. Share one passive discovery
summary between `doctor` and the Agents settings page. Derive defaults and
grant counts from the policy. Save default-route changes through the settings
publisher and preserve explicit grant providers, revisions, endpoints and
limits. When the policy configures Codex,
reuse the Worker configuration and executable trust checks. Treat failure
as a warning for the base workspace. Verify that discovery creates no
runtime files and never launches Codex or connects to the model socket.
The execution path enforces authorization and reports launch or connection
failures. Passive discovery reports unchecked runtime health.

## Codex Adapter

`mcbtodo-agent-codex` must use the external `codex app-server` process boundary
and its documented protocol. Do not make application code depend on Codex
internal Rust crates. The adapter must verify the executable, app-server
availability, initialize handshake, protocol capabilities,
approval and sandbox mapping, request/notification correlation, streaming,
cancellation, response limits, process exit, and result normalization.

Record the Codex versions exercised by real probes. Check compatibility
through the required protocol exchanges and state unsupported behavior
explicitly. System Codex discovery is the first mode. Managed fixed-version
distribution is a later release capability.

A controlled real integration test is required before claiming the adapter is
verified. Tests must cover initialize, capability negotiation, timeout,
cancellation, malformed and oversized responses, provider crash, stale result,
duplicate request, denied network or filesystem access, and unknown side effects.

## Verification

Run focused Agent and execution tests, then:

```sh
cargo fmt --all -- --check
git diff --check
cargo xtask codex-probe --codex /absolute/path/to/codex --bin-dir target/debug
cargo xtask smoke --bin-dir target/debug --sandbox
cargo xtask check
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate
```

If a real Codex binary, credential, sandbox, or platform is unavailable, mark the
specific test `Blocked` or `Unsupported` and do not convert protocol fixtures
into real integration evidence.
