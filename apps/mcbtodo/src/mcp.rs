//! Local MCP stdio adapter for the configured Vault.

mod tools;

use std::{
    io::{self, BufRead, Read as _, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use chrono_tz::Tz;
use mcbtodo_platform::WorkspacePaths;
use serde_json::{Map, Value, json};

const VERSION: &str = "2026-07-28";
const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
const MAX_LINE_BYTES: usize = 64 * 1024;
const MIN_TOOL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) fn serve(vault_path: &Path, timezone: Tz, paths: &WorkspacePaths) -> Result<()> {
    let root = vault_path
        .canonicalize()
        .context("opening the configured MCP Vault")?;
    ensure!(
        root.is_dir(),
        "the configured MCP Vault must be a directory"
    );
    serve_io(
        &root,
        timezone,
        paths,
        io::stdin().lock(),
        io::stdout().lock(),
    )
}

fn serve_io(
    root: &Path,
    timezone: Tz,
    paths: &WorkspacePaths,
    mut input: impl BufRead,
    mut output: impl Write,
) -> Result<()> {
    let mut server = Server {
        root,
        timezone,
        paths,
        last_tool_call: None,
    };
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut input)
            .take((MAX_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        ensure!(
            read <= MAX_LINE_BYTES,
            "MCP message exceeds the stdio limit"
        );
        if let Some(response) = server.handle(&line) {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

struct Server<'a> {
    root: &'a Path,
    timezone: Tz,
    paths: &'a WorkspacePaths,
    last_tool_call: Option<Instant>,
}

impl Server<'_> {
    fn handle(&mut self, line: &[u8]) -> Option<Value> {
        let message = match serde_json::from_slice::<Value>(line) {
            Ok(message) => message,
            Err(_) => return Some(rpc_error(&Value::Null, -32700, "Parse error")),
        };
        let id = message.get("id");
        let valid_id = id.is_none_or(|id| id.is_string() || id.is_i64() || id.is_u64());
        let method = message["method"].as_str();
        if !valid_id
            || method.is_none()
            || message["jsonrpc"] != "2.0"
            || message.get("result").is_some()
            || message.get("error").is_some()
        {
            let response_id = id.filter(|_| valid_id).unwrap_or(&Value::Null);
            return Some(rpc_error(response_id, -32600, "Invalid request"));
        }
        let method = method.expect("validated method");
        let params = &message["params"];
        let id = id?;
        if message
            .get("params")
            .is_some_and(|params| !params.is_object())
        {
            return Some(rpc_error(id, -32602, "Expected an object for params"));
        }
        Some(self.request(id, method, params))
    }

    fn request(&mut self, id: &Value, method: &str, params: &Value) -> Value {
        let meta = &params["_meta"];
        let (Some(version), true) = (
            meta[PROTOCOL_VERSION].as_str(),
            meta[CLIENT_CAPABILITIES].is_object(),
        ) else {
            return rpc_error(
                id,
                -32602,
                &format!("Expected MCP {VERSION} request metadata"),
            );
        };
        if version != VERSION {
            return json!({"jsonrpc": "2.0", "id": id, "error": {
                "code": -32022, "message": "Unsupported protocol version",
                "data": {"supported": [VERSION], "requested": version}
            }});
        }

        let mut result = match method {
            "server/discover" => json!({
                "supportedVersions": [VERSION],
                "capabilities": {"tools": {}},
            }),
            "ping" => json!({}),
            "tools/list" => {
                if params.get("cursor").is_some() {
                    return rpc_error(id, -32602, "Invalid tools/list cursor");
                }
                json!({"tools": tools::list()})
            }
            "tools/call" => {
                let Some(name) = params["name"].as_str() else {
                    return rpc_error(id, -32602, "Expected a tool name");
                };
                let arguments = match params.get("arguments") {
                    Some(Value::Object(arguments)) => arguments.clone(),
                    None => Map::new(),
                    _ => return rpc_error(id, -32602, "Expected an object for tool arguments"),
                };
                let Some(tool) = tools::Tool::named(name) else {
                    return rpc_error(id, -32602, "Unknown tool");
                };
                if let Some(last) = self.last_tool_call {
                    thread::sleep(MIN_TOOL_INTERVAL.saturating_sub(last.elapsed()));
                }
                self.last_tool_call = Some(Instant::now());
                match tool.run(self.root, self.timezone, self.paths, &arguments) {
                    Ok(data) => json!({
                        "content": [{"type": "text", "text": data.to_string()}],
                        "structuredContent": data
                    }),
                    Err(message) => json!({
                        "content": [{"type": "text", "text": message}], "isError": true
                    }),
                }
            }
            _ => return rpc_error(id, -32601, "Method not found"),
        };
        result["resultType"] = json!("complete");
        result["_meta"] = json!({SERVER_INFO: server_info()});
        rpc_result(id, result)
    }
}

fn server_info() -> Value {
    json!({"name": "mcbtodo", "version": env!("CARGO_PKG_VERSION")})
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests;
