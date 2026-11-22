use std::{path::Path, time::Duration};

use mcbtodo_platform::agent_policy::{AgentBackend, AgentPolicy, MAX_POLICY_BYTES};

const SOCKET: &str = "[providers]\nunix_socket = \"state/agent.sock\"\n";
const REVISION_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const REVISION_B: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn grant(capability: &str, revision: &str) -> String {
    format!("[[allow]]\ncapability = {capability:?}\nrevision = {revision:?}\n")
}

fn grant_with_provider(capability: &str, revision: &str, provider: &str) -> String {
    format!(
        "[[allow]]\ncapability = {capability:?}\nrevision = {revision:?}\nprovider = {provider:?}\n"
    )
}

#[test]
fn policy_defaults_to_no_grants_and_thirty_seconds() {
    let policy = AgentPolicy::parse(SOCKET.as_bytes()).unwrap();
    assert_eq!(policy.backend(), AgentBackend::UnixSocket);
    assert_eq!(policy.socket(), Path::new("state/agent.sock"));
    assert_eq!(policy.timeout(), Duration::from_secs(30));
    assert!(policy.route("triage", REVISION_A).is_none());
}

#[test]
fn policy_infers_a_single_codex_provider_and_keeps_the_mixed_default() {
    let codex = "[providers]\ncodex = 'state/model.sock'\n";
    let allowed = grant("triage", REVISION_A);
    let policy = AgentPolicy::parse(format!("{codex}{allowed}").as_bytes()).unwrap();
    let route = policy.route("triage", REVISION_A).unwrap();
    assert_eq!(policy.backend(), AgentBackend::Codex);
    assert_eq!(route.backend, AgentBackend::Codex);
    assert_eq!(route.socket, Path::new("state/model.sock"));
    assert!(policy.route("triage", REVISION_B).is_none());

    let mixed = format!("{codex}unix_socket = 'state/agent.sock'\n{allowed}");
    let policy = AgentPolicy::parse(mixed.as_bytes()).unwrap();
    assert_eq!(policy.backend(), AgentBackend::UnixSocket);
    assert_eq!(
        policy.route("triage", REVISION_A).unwrap().backend,
        AgentBackend::UnixSocket
    );
    let missing = format!("provider = 'unix_socket'\n{codex}{allowed}");
    assert!(AgentPolicy::parse(missing.as_bytes()).is_err());
}

#[test]
fn provider_selection_is_explicit_and_unknown_providers_fail_closed() {
    for (provider, expected) in [
        ("unix_socket", AgentBackend::UnixSocket),
        ("codex", AgentBackend::Codex),
    ] {
        let config = if provider == "codex" {
            format!(
                "provider = {provider:?}\n[providers]\nunix_socket = 'state/agent.sock'\n\
                 codex = 'state/model.sock'\n"
            )
        } else {
            format!("provider = {provider:?}\n{SOCKET}")
        };
        assert_eq!(
            AgentPolicy::parse(config.as_bytes()).unwrap().backend(),
            expected
        );
    }
    let invalid = format!("provider = 'other'\n{SOCKET}");
    assert!(AgentPolicy::parse(invalid.as_bytes()).is_err());
}

#[test]
fn authorization_matches_exact_pairs() {
    let text = format!(
        "provider = 'unix_socket'\ntimeout_seconds = 120\n{SOCKET}{}{}{}",
        grant("triage", REVISION_A),
        grant("triage-other_2", REVISION_A),
        grant("triage-other_2", REVISION_B),
    );
    let policy = AgentPolicy::parse(text.as_bytes()).unwrap();
    assert_eq!(policy.timeout(), Duration::from_secs(120));
    assert!(policy.route("triage", REVISION_A).is_some());
    assert!(policy.route("triage-other_2", REVISION_A).is_some());
    assert!(policy.route("triage-other_2", REVISION_B).is_some());
    assert!(policy.route("triage", REVISION_B).is_none());
    assert!(policy.route("TRIAGE", REVISION_A).is_none());
    assert!(policy.route("triage", &REVISION_A.to_uppercase()).is_none());
    assert!(policy.route("triage", &REVISION_A[..12]).is_none());
}

#[test]
fn the_same_capability_can_route_different_revisions_to_different_providers() {
    let text = format!(
        "provider = 'unix_socket'\n[providers]\nunix_socket = 'state/agent.sock'\n\
         codex = 'state/model.sock'\n{}{}",
        grant("triage", REVISION_A),
        grant_with_provider("triage", REVISION_B, "codex"),
    );
    let policy = AgentPolicy::parse(text.as_bytes()).unwrap();
    let local = policy.route("triage", REVISION_A).unwrap();
    let codex = policy.route("triage", REVISION_B).unwrap();
    assert_eq!(local.backend, AgentBackend::UnixSocket);
    assert_eq!(local.socket, Path::new("state/agent.sock"));
    assert_eq!(codex.backend, AgentBackend::Codex);
    assert_eq!(codex.socket, Path::new("state/model.sock"));
    assert!(policy.route("other", REVISION_B).is_none());
    assert!(policy.route("triage", &"f".repeat(64)).is_none());
}

#[test]
fn rejects_ambiguous_routes_and_unconfigured_providers() {
    let local = grant("triage", REVISION_A);
    for (text, expected) in [
        (
            format!("{SOCKET}{local}provider = 'codex'"),
            "Agent grant selects an unconfigured provider",
        ),
        (
            format!("{SOCKET}[providers]\nunix_socket = 'state/another.sock'\n"),
            "Invalid agents.toml configuration",
        ),
        (
            format!(
                "provider = 'codex'\n[providers]\nunix_socket = 'state/agent.sock'\n\
                 codex = 'state/model.sock'\n{local}{local}"
            ),
            "Duplicate Agent authorization entry",
        ),
        (
            format!(
                "provider = 'unix_socket'\n[providers]\nunix_socket = 'state/agent.sock'\n\
                 codex = '../outside.sock'\n{local}"
            ),
            "Agent socket must be a relative path beneath the app home",
        ),
    ] {
        let error = AgentPolicy::parse(text.as_bytes()).err().unwrap();
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn rejects_unknown_missing_duplicate_and_mistyped_fields() {
    for text in [
        "timeout_seconds = 10".to_owned(),
        "socket = \"state/agent.sock\"".to_owned(),
        format!("{SOCKET}unknown = \"sensitive-value\""),
        format!("{SOCKET}{SOCKET}"),
        format!("{SOCKET}allow = {{ capability = \"triage\" }}"),
        format!("{SOCKET}allow = [[\"triage\", \"{REVISION_A}\"]]"),
        format!("{SOCKET}[[allow]]\ncapability = \"triage\"\n"),
        format!("{SOCKET}[[allow]]\nrevision = \"{REVISION_A}\"\n"),
        format!(
            "{SOCKET}{}unknown = \"sensitive-value\"",
            grant("triage", REVISION_A)
        ),
        format!(
            "{SOCKET}{}revision = \"{REVISION_B}\"",
            grant("triage", REVISION_A)
        ),
    ] {
        let error = AgentPolicy::parse(text.as_bytes()).err().unwrap();
        assert_eq!(format!("{error:#}"), "Invalid agents.toml configuration");
    }
}

#[test]
fn rejects_bad_grants_and_repeated_pairs() {
    for capability in ["", "9triage", "triage.task", "triagé", &"x".repeat(97)] {
        let text = format!("{SOCKET}{}", grant(capability, REVISION_A));
        assert!(AgentPolicy::parse(text.as_bytes()).is_err(), "{capability}");
    }
    for revision in [
        &REVISION_A[..63],
        &REVISION_A.to_uppercase(),
        &"g".repeat(64),
    ] {
        let text = format!("{SOCKET}{}", grant("triage", revision));
        assert!(AgentPolicy::parse(text.as_bytes()).is_err(), "{revision}");
    }
    let entry = grant("triage", REVISION_A);
    let text = format!("{SOCKET}{entry}{entry}");
    let error = AgentPolicy::parse(text.as_bytes()).err().unwrap();
    assert_eq!(error.to_string(), "Duplicate Agent authorization entry");
}

#[test]
fn rejects_unsafe_socket_paths_and_invalid_timeouts() {
    for socket in [
        "",
        ".",
        "./state/agent.sock",
        "/tmp/agent.sock",
        "../agent.sock",
        "state/../agent.sock",
    ] {
        let text = format!("[providers]\nunix_socket = {socket:?}");
        assert!(AgentPolicy::parse(text.as_bytes()).is_err(), "{socket}");
    }
    assert!(
        AgentPolicy::parse(
            br#"[providers]
unix_socket = "state/agent\u0000.sock""#
        )
        .is_err()
    );

    for timeout in ["-1", "0", "121", "1.5", "\"30\""] {
        let text = format!("provider = 'unix_socket'\ntimeout_seconds = {timeout}\n{SOCKET}");
        assert!(AgentPolicy::parse(text.as_bytes()).is_err(), "{timeout}");
    }
    let text = format!("provider = 'unix_socket'\ntimeout_seconds = 1\n{SOCKET}");
    assert_eq!(
        AgentPolicy::parse(text.as_bytes()).unwrap().timeout(),
        Duration::from_secs(1)
    );
}

#[test]
fn checks_byte_limit_and_utf8() {
    let mut bytes = SOCKET.as_bytes().to_vec();
    bytes.resize(MAX_POLICY_BYTES, b' ');
    assert!(AgentPolicy::parse(&bytes).is_ok());
    bytes.push(b' ');
    let error = AgentPolicy::parse(&bytes).err().unwrap();
    assert_eq!(error.to_string(), "agents.toml exceeds the size limit");
    assert!(AgentPolicy::parse(b"[providers]\nunix_socket = \"\xff\"").is_err());
}

#[test]
fn an_output_budget_is_optional_and_accepts_positive_byte_counts() {
    assert_eq!(
        AgentPolicy::parse(SOCKET.as_bytes())
            .unwrap()
            .max_output_bytes(),
        None
    );
    for value in [2, 4096] {
        let text = format!("max_output_bytes = {value}\n{SOCKET}");
        assert_eq!(
            AgentPolicy::parse(text.as_bytes())
                .unwrap()
                .max_output_bytes(),
            Some(value)
        );
    }
    for value in ["-1", "0", "1", "1.5", "\"4096\""] {
        let text = format!("max_output_bytes = {value}\n{SOCKET}");
        assert!(AgentPolicy::parse(text.as_bytes()).is_err(), "{value}");
    }
}
