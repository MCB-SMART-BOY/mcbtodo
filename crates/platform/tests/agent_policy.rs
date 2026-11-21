use std::{path::Path, time::Duration};

use mcbtodo_platform::agent_policy::{AgentPolicy, MAX_POLICY_BYTES};

const SOCKET: &str = "socket = \"state/agent.sock\"\n";
const REVISION_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const REVISION_B: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn grant(capability: &str, revision: &str) -> String {
    format!("[[allow]]\ncapability = {capability:?}\nrevision = {revision:?}\n")
}

#[test]
fn policy_defaults_to_no_grants_and_thirty_seconds() {
    let policy = AgentPolicy::parse(SOCKET.as_bytes()).unwrap();
    assert_eq!(policy.socket(), Path::new("state/agent.sock"));
    assert_eq!(policy.timeout(), Duration::from_secs(30));
    assert!(!policy.allows("triage", REVISION_A));
}

#[test]
fn authorization_matches_exact_pairs() {
    let text = format!(
        "{SOCKET}timeout_seconds = 120\n{}{}{}",
        grant("triage", REVISION_A),
        grant("triage-other_2", REVISION_A),
        grant("triage-other_2", REVISION_B),
    );
    let policy = AgentPolicy::parse(text.as_bytes()).unwrap();
    assert_eq!(policy.timeout(), Duration::from_secs(120));
    assert!(policy.allows("triage", REVISION_A));
    assert!(policy.allows("triage-other_2", REVISION_A));
    assert!(policy.allows("triage-other_2", REVISION_B));
    assert!(!policy.allows("triage", REVISION_B));
    assert!(!policy.allows("TRIAGE", REVISION_A));
    assert!(!policy.allows("triage", &REVISION_A.to_uppercase()));
    assert!(!policy.allows("triage", &REVISION_A[..12]));
}

#[test]
fn rejects_unknown_missing_duplicate_and_mistyped_fields() {
    for text in [
        "timeout_seconds = 10".to_owned(),
        "socket = 42".to_owned(),
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
        let text = format!("socket = {socket:?}");
        assert!(AgentPolicy::parse(text.as_bytes()).is_err(), "{socket}");
    }
    assert!(AgentPolicy::parse(br#"socket = "state/agent\u0000.sock""#).is_err());

    for timeout in ["-1", "0", "121", "1.5", "\"30\""] {
        let text = format!("{SOCKET}timeout_seconds = {timeout}");
        assert!(AgentPolicy::parse(text.as_bytes()).is_err(), "{timeout}");
    }
    let text = format!("{SOCKET}timeout_seconds = 1");
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
    assert!(AgentPolicy::parse(b"socket = \"\xff\"").is_err());
}
