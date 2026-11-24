//! MCP (Model Context Protocol) server, so AI agents can drive the emulator
//!
//! Off by default; `--mcp` or `SNOW_MCP=1` turns it on. Each Snow with MCP on
//! opens a Unix socket in `/tmp/snow-<uid>/` (a folder only that user can
//! open) and writes `<pid>.json` next to it, saying what it is. The agent
//! starts `snowemu --mcp-bridge`, a stdio MCP server that finds the running
//! Snows through those files and passes each tool call to the one it names.
//! See docs/src/manual/mcp.md.

// The socket server and the bridge are Unix only; elsewhere most of this is unused
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

mod debug;
pub mod hub;
mod image;
mod keys;
mod tools;

#[cfg(unix)]
mod bridge;
#[cfg(unix)]
mod server;

use hub::Hub;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, OnceLock};

static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
static NAME: OnceLock<String> = OnceLock::new();

const TOOLS_JSON: &str = include_str!("tools.json");
const INSTRUCTIONS: &str = include_str!("instructions.txt");

/// Whether the environment asks for MCP
pub fn enabled_by_env() -> bool {
    std::env::var("SNOW_MCP").is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
}

/// The hub, if MCP is on
pub fn hub() -> Option<Arc<Hub>> {
    HUB.get().cloned()
}

/// The name agents use for this emulator
pub fn vm_name() -> String {
    NAME.get().cloned().unwrap_or_else(|| "snow".into())
}

/// Picks the name: SNOW_MCP_NAME, else the workspace or ROM file's name, else
/// the working folder's name
fn choose_name(file: Option<&str>) -> String {
    if let Ok(n) = std::env::var("SNOW_MCP_NAME")
        && !n.is_empty()
    {
        return n;
    }
    if let Some(stem) = file
        .and_then(|f| Path::new(f).file_stem())
        .map(|s| s.to_string_lossy().into_owned())
    {
        return stem;
    }
    std::env::current_dir()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "snow".into())
}

/// Turns MCP on: makes the hub and opens the socket
#[cfg(unix)]
pub fn start(file: Option<&str>) -> anyhow::Result<()> {
    let _ = NAME.set(choose_name(file));
    let hub = Arc::new(Hub::new());
    server::start(Arc::clone(&hub), file)?;
    let _ = HUB.set(hub);
    log::info!("MCP server on, VM name '{}'", vm_name());
    Ok(())
}

/// Turns MCP on: not available on this host
#[cfg(not(unix))]
pub fn start(_file: Option<&str>) -> anyhow::Result<()> {
    anyhow::bail!("The MCP server needs a Unix host (macOS or Linux)")
}

/// Removes the socket and its file. Safe to call more than once.
pub fn shutdown() {
    #[cfg(unix)]
    server::cleanup();
}

/// Ends Snow after an agent asked it to quit
pub fn exit_process() -> ! {
    shutdown();
    std::process::exit(0)
}

/// Runs `snowemu --mcp-bridge`: the stdio MCP server for agents
pub fn run_bridge() -> ! {
    #[cfg(unix)]
    {
        bridge::run();
        std::process::exit(0)
    }
    #[cfg(not(unix))]
    {
        eprintln!("The MCP bridge needs a Unix host (macOS or Linux)");
        std::process::exit(1)
    }
}

/// The tools' descriptions for tools/list, with the shared parts filled in
/// and the vm argument added to each
pub fn tool_list() -> Value {
    let rect = json!({
        "type": "object",
        "description": "A rectangle in screen pixels",
        "properties": {
            "x": {"type": "integer"}, "y": {"type": "integer"},
            "w": {"type": "integer"}, "h": {"type": "integer"}
        },
        "required": ["x", "y", "w", "h"]
    });
    let then = json!({
        "type": "object",
        "description": "After the action: wait for the screen (until change, stable, change-then-stable or none), then return a screenshot (screenshot: false for none)",
        "properties": {
            "until": {"type": "string", "enum": ["change", "stable", "change-then-stable", "none"]},
            "stable-ms": {"type": "integer"},
            "timeout-ms": {"type": "integer"},
            "region": rect,
            "ignore": {"type": "array", "items": rect},
            "min-pixels": {"type": "integer"},
            "screenshot": {"type": "boolean"},
            "scale": {"type": "number"}
        }
    });
    fn fill(v: &mut Value, rect: &Value, then: &Value) {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(r)) = m.get("$ref") {
                    let def = match r.as_str() {
                        "#/$defs/rect" => rect,
                        "#/$defs/then" => then,
                        _ => return,
                    };
                    let desc = m.get("description").cloned();
                    *v = def.clone();
                    if let (Some(d), Value::Object(o)) = (desc, v) {
                        o.insert("description".into(), d);
                    }
                    return;
                }
                for x in m.values_mut() {
                    fill(x, rect, then);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| fill(x, rect, then)),
            _ => (),
        }
    }
    let mut tools: Value = serde_json::from_str(TOOLS_JSON).expect("tools.json");
    fill(&mut tools, &rect, &then);
    for t in tools.as_array_mut().unwrap() {
        if t["name"] == "vm-list" {
            continue;
        }
        t["inputSchema"]["properties"]["vm"] = json!({
            "type": "string",
            "description": "The VM's name or PID from vm-list; may be left out while only one runs"
        });
    }
    tools
}

pub fn instructions() -> &'static str {
    INSTRUCTIONS
}
