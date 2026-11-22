use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, TcpListener},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use mcbtodo_agent_codex::{CodexProvider, TurnAction, TurnResult, run_turn};
use mcbtodo_ports::{AgentExecutionContext, AgentProvider, AgentRequest, Scalar};
use rustix::fs::{OFlags, fcntl_setfl};
use serde_json::{Value, json};

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
pub(crate) const FIXTURE_PROMPT: &str =
    "Return the JSON output requested for mcbtodo-stream-fixture.";
pub(crate) const FIXTURE_CAPABILITY: &str = "codex-fixture";
pub(crate) const FIXTURE_SAMPLE: &str = "混合 text";
pub(crate) const FIXTURE_TITLE: &str = "已验证跨平台计划 with a finely chunked streamed reply";

pub(crate) fn run(codex: &Path, bin_dir: Option<&Path>) -> Result<()> {
    ensure!(codex.is_absolute(), "--codex requires an absolute path");
    let codex = codex
        .canonicalize()
        .context("resolve the Codex executable")?;
    ensure!(codex.is_file(), "Codex must be a regular executable");
    let scratch = tempfile::tempdir().context("create Codex probe workspace")?;
    fs::create_dir(scratch.path().join(".codex"))?;

    probe_interruption(&codex, scratch.path()).context("verify offline Codex interruption")?;
    probe_provider(&codex, scratch.path()).context("verify offline Codex streamed completion")?;
    println!(
        "Codex executable {} passed offline initialize, thread/start, turn/interrupt, and streamed fixture completion",
        codex.display()
    );
    if let Some(bin_dir) = bin_dir {
        crate::smoke::exercise_codex_workflow(bin_dir, &codex)?;
        println!(
            "Production Codex Workflow passed provider configuration, protocol validation, revision grants, approval, output budget, disconnect, timeout, crash, cancellation, and daemon-death recovery"
        );
    }
    Ok(())
}

fn fixture_provider<'a>(codex: &'a Path, scratch: &'a Path) -> impl AgentProvider + 'a {
    CodexProvider::new(
        FIXTURE_CAPABILITY.into(),
        FIXTURE_PROMPT.into(),
        json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
            "additionalProperties": false
        }),
        move |_: AgentExecutionContext<'_>| {
            let mut command = sandbox(codex, scratch);
            command.arg("--ro-bind").arg(env::current_exe()?).args([
                "/bin/xtask",
                "--ro-bind",
                "/lib",
                "/lib",
                "--ro-bind-try",
                "/lib64",
                "/lib64",
                // The fixture binary is dynamically linked. Usr-merged
                // distributions may search /usr/lib by default.
                "--setenv",
                "LD_LIBRARY_PATH",
                "/lib:/lib64",
                "--remount-ro",
                "/",
                "--",
                "/bin/xtask",
                "codex-fixture",
            ]);
            Ok(command)
        },
    )
}

fn probe_provider(codex: &Path, scratch: &Path) -> Result<()> {
    let provider = fixture_provider(codex, scratch);
    ensure!(
        provider
            .metadata()
            .capabilities
            .contains(FIXTURE_CAPABILITY),
        "Codex provider does not advertise the fixture capability"
    );
    let result = provider.execute(
        AgentRequest {
            capability: FIXTURE_CAPABILITY.into(),
            revision: "0".repeat(64),
            inputs: [("sample".into(), Scalar::String(FIXTURE_SAMPLE.into()))].into(),
        },
        AgentExecutionContext {
            deadline: Instant::now() + PROBE_TIMEOUT,
            cancellation: &AtomicBool::new(false),
        },
    )?;
    ensure!(
        result.outputs == [("title".into(), Scalar::String(FIXTURE_TITLE.into()))].into(),
        "Codex fixture returned unexpected text"
    );
    Ok(())
}

fn probe_interruption(codex: &Path, scratch: &Path) -> Result<()> {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut server = Running(
        command(codex, scratch)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("start offline Codex app-server")?,
    );
    let mut stdin = server.0.stdin.take().context("open Codex stdin")?;
    let stdout = server.0.stdout.take().context("open Codex stdout")?;
    fcntl_setfl(&stdin, OFlags::NONBLOCK)?;
    fcntl_setfl(&stdout, OFlags::NONBLOCK)?;

    let result = run_turn(
        &mut BufReader::new(stdout),
        &mut stdin,
        AgentExecutionContext {
            deadline,
            cancellation: &AtomicBool::new(false),
        },
        "Return the literal text mcbtodo-probe. Do not use tools.",
        TurnAction::Interrupt,
    )?;
    ensure!(
        result == TurnResult::Interrupted,
        "Codex did not confirm interruption"
    );
    drop(stdin);
    server.finish(deadline)
}

fn command(codex: &Path, scratch: &Path) -> Command {
    let mut cmd = sandbox(codex, scratch);
    cmd.args(["--remount-ro", "/", "--", "/bin/codex"]);
    cmd
}

fn sandbox(codex: &Path, scratch: &Path) -> Command {
    let mut cmd = Command::new("/usr/bin/bwrap");
    cmd.env_clear()
        .current_dir("/")
        .stderr(Stdio::null())
        .args([
            "--unshare-all",
            "--unshare-user",
            "--disable-userns",
            "--cap-drop",
            "ALL",
            "--die-with-parent",
            "--new-session",
            "--ro-bind",
        ])
        .arg(codex)
        .args(["/bin/codex", "--bind"])
        .arg(scratch)
        .args([
            "/scratch",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--chdir",
            "/scratch",
            "--clearenv",
            "--setenv",
            "HOME",
            "/scratch",
            "--setenv",
            "CODEX_HOME",
            "/scratch/.codex",
            "--setenv",
            "PATH",
            "/bin",
        ]);
    cmd
}

/// Runs only inside the probe namespace. The model is a single-use HTTP
/// fixture on that namespace's loopback. It never receives host credentials.
pub(crate) fn run_fixture() -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut codex = Running(
        Command::new("/bin/codex")
            .current_dir("/scratch")
            .env_clear()
            .env("HOME", "/scratch")
            .env("CODEX_HOME", "/scratch/.codex")
            .env("PATH", "/bin")
            .arg("-c")
            .arg("model_provider=\"mcbtodo_fixture\"")
            .arg("-c")
            .arg(format!(
                "model_providers.mcbtodo_fixture={{name=\"mcbtodo-fixture\",base_url=\"http://127.0.0.1:{port}/v1\",wire_api=\"responses\",supports_websockets=false,request_max_retries=0,stream_max_retries=0}}"
            ))
            .args(["app-server", "--listen", "stdio://"])
            .spawn()
            .context("start Codex against the loopback fixture")?,
    );
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ensure!(
                    codex.0.try_wait()?.is_none(),
                    "Codex exited before its model request"
                );
                pause(deadline)?;
            }
            Err(error) => return Err(error.into()),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    read_model_request(&mut stream)?;
    write_model_reply(&mut stream)?;
    drop(stream);
    codex.finish(deadline)
}

pub(crate) fn read_model_request(stream: impl Read) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    (&mut reader).take(256).read_line(&mut line)?;
    ensure!(
        line == "POST /v1/responses HTTP/1.1\r\n",
        "unexpected model request"
    );
    let mut content_length = None;
    let mut end_of_headers = false;
    for _ in 0..32 {
        line.clear();
        (&mut reader).take(8192).read_line(&mut line)?;
        ensure!(line.ends_with("\r\n"), "incomplete fixture HTTP header");
        if line == "\r\n" {
            end_of_headers = true;
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            ensure!(content_length.is_none(), "duplicate fixture content-length");
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = content_length.context("fixture request lacks content-length")?;
    ensure!(
        end_of_headers && length <= 256 * 1024,
        "invalid fixture HTTP request"
    );
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let request: Value = serde_json::from_slice(&body).context("decode model fixture request")?;
    ensure!(
        request["stream"] == true
            && request["text"]["format"]["schema"]["properties"]["title"]["type"] == "string"
            && request["input"]
                .as_array()
                .is_some_and(|messages| messages.iter().any(|message| {
                    message["role"] == "user"
                        && message["content"].as_array().is_some_and(|items| {
                            items.iter().any(|item| {
                                item["text"]
                                    .as_str()
                                    .and_then(|text| {
                                        text.strip_prefix(FIXTURE_PROMPT)?
                                            .strip_prefix("\n\nInputs (JSON): ")
                                    })
                                    .and_then(|inputs| serde_json::from_str::<Value>(inputs).ok())
                                    == Some(json!({"sample": FIXTURE_SAMPLE}))
                            })
                        })
                })),
        "Codex changed the fixture prompt, streaming flag or output schema"
    );
    Ok(())
}

pub(crate) fn write_model_reply(mut stream: impl Write) -> Result<()> {
    let message = |text| {
        json!({
            "type": "message", "id": "fixture-item", "role": "assistant",
            "content": [{"type": "output_text", "text": text}]
        })
    };
    let output = json!({"title": FIXTURE_TITLE}).to_string();
    let body = [
        json!({"type": "response.created", "response": {"id": "fixture-response"}}),
        json!({"type": "response.output_item.added", "item": message("")}),
    ]
    .into_iter()
    .chain(
        output
            .chars()
            .map(|ch| json!({"type": "response.output_text.delta", "delta": ch.to_string()})),
    )
    .chain([
        json!({"type": "response.output_item.done", "item": message(&output)}),
        json!({"type": "response.completed", "response": {"id": "fixture-response"}}),
    ])
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}

struct Running(Child);

impl Running {
    fn finish(&mut self, deadline: Instant) -> Result<()> {
        let status = self.wait_status(deadline)?;
        ensure!(
            status.success(),
            "offline Codex process exited with {status}"
        );
        Ok(())
    }

    fn wait_status(&mut self, deadline: Instant) -> Result<ExitStatus> {
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            pause(deadline)?;
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn pause(deadline: Instant) -> Result<()> {
    let left = deadline.saturating_duration_since(Instant::now());
    ensure!(!left.is_zero(), "offline Codex probe timed out");
    thread::sleep(POLL_INTERVAL.min(left));
    Ok(())
}
