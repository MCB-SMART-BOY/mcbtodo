# Workflow Agent adapter

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
socket = "state/agent.sock"
timeout_seconds = 30

[[allow]]
capability = "analyze_test_failure"
revision = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
```

Add one `[[allow]]` entry per capability and revision pair. The daemon reads
the current file before each call. Removing an entry revokes future calls.
Publishing changed graph or script bytes yields a different hash, so grant that
revision separately. Calls fail when the file is missing or the pair is absent.
Use a user-owned regular file, preferably mode `0600`, in user-owned
configuration directories protected from group and other writes.

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
On cancellation or timeout the daemon closes the connection. An adapter should
stop work when it observes the disconnect. If the adapter cannot produce a
response, it can close the connection to fail the node. A denied call,
connection failure, timeout, or invalid reply produces a workflow node failure.

## Per-run limits

The daemon allows up to four Agent attempts and 60 seconds of cumulative
Agent attempt time per workflow run. These fixed limits apply across all Agent
nodes in the run. Only one workflow execution attempt runs at a time. The
Daemon starts and supervises a separate local `mcbtodo-worker` process for the
attempt; the Worker executes the adapter but does not own scheduling or durable
run state. Packaged-binary failure combinations and external side-effect
reconciliation remain unverified; see
[the architecture assessment](architecture-assessment.md).
Each adapter call uses the earlier of its configured timeout and the remaining
run deadline.

The daemon records an attempt before dispatch, so authorization failures and
adapter errors also count. It measures elapsed attempt time from dispatch
through result delivery from the Worker. A completed step and the return of any
unused time are recorded in the same transaction. A run whose Worker exits
before the result is durably recorded keeps the full reservation and enters
`outcome_unknown`. The daemon never replays that attempt.

If a node has exhausted its run's call count or wall time, the daemon fails the
run before contacting the adapter. If a call reaches the cumulative time limit,
the daemon disconnects and fails the run. A run resumed from an older database
with an already executed Agent step and no accounting record is denied further
Agent calls because the earlier elapsed time is unknown.

## Shared policy validation

The daemon, backup, and restore use the same `agents.toml` parser in
`mcbtodo-platform`. The policy must be UTF-8 TOML, at most 64 KiB, with a
required `socket` and optional `timeout_seconds` and `allow` fields. Each
`allow` entry is a table with `capability` and `revision` fields. Unknown fields
and duplicate grant pairs invalidate the whole policy. An omitted `allow`
grants no access.

Capability names are 1–96 ASCII characters. They start with a letter and can
then contain letters, digits, hyphens, and underscores. Revisions are exactly
64 lowercase hexadecimal digits. A grant must match both strings, including
case. The socket path must be nonempty and relative to the app home, with no
parent traversal or NUL bytes.

Backup and restore validate the policy before copying it and preserve its exact
bytes. Neither operation needs a running adapter. The daemon validates the
current policy before each Agent call and checks the live socket path and peer
identity when opening a connection.
