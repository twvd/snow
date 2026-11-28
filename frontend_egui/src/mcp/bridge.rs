//! `snowemu --mcp-bridge`: the stdio MCP server an agent starts
//!
//! It speaks MCP (JSON-RPC 2.0, one message per line) on stdin and stdout,
//! finds the running Snows through /tmp/snow-<uid>/*.json, answers vm-list
//! and vm-wait itself and passes the other tool calls to the Snow named by
//! their vm argument. It connects to a Snow only for the length of a call.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Vm {
    name: String,
    pid: u32,
    socket: PathBuf,
    meta: Value,
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The running Snows; removes the files of ones that ended without cleaning up
fn list() -> Vec<Vm> {
    let Ok(dir) = super::server::folder() else {
        return vec![];
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut vms = vec![];
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(meta) = std::fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        let pid = meta["pid"].as_u64().unwrap_or(0) as u32;
        let socket = PathBuf::from(meta["socket"].as_str().unwrap_or(""));
        if pid == 0 || !alive(pid) {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(&socket);
            continue;
        }
        vms.push(Vm {
            name: meta["name"].as_str().unwrap_or("snow").to_string(),
            pid,
            socket,
            meta,
        });
    }
    vms.sort_by_key(|v| v.pid);
    vms
}

fn find(vm: Option<&str>) -> Result<Vm, String> {
    let vms = list();
    let Some(want) = vm.filter(|v| !v.is_empty()) else {
        return match vms.len() {
            0 => Err(
                "No Snow with MCP is running. Start one with: snowemu --mcp <workspace.snoww> (or SNOW_MCP=1)"
                    .into(),
            ),
            1 => Ok(vms[0].clone()),
            _ => Err(format!(
                "Several Snows run; name one with vm: {}",
                vms.iter()
                    .map(|v| format!("{} (pid {})", v.name, v.pid))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        };
    };
    if let Ok(pid) = want.parse::<u32>()
        && let Some(v) = vms.iter().find(|v| v.pid == pid)
    {
        return Ok(v.clone());
    }
    let named: Vec<&Vm> = vms
        .iter()
        .filter(|v| v.name.eq_ignore_ascii_case(want))
        .collect();
    match named.len() {
        1 => Ok(named[0].clone()),
        0 => Err(format!(
            "No Snow named '{}' runs. Running: {}",
            want,
            if vms.is_empty() {
                "none".to_string()
            } else {
                vms.iter()
                    .map(|v| format!("{} (pid {})", v.name, v.pid))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )),
        _ => Err(format!(
            "Several Snows are named '{}'; use the PID from vm-list",
            want
        )),
    }
}

fn text_result(text: impl Into<String>, error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}], "isError": error})
}

fn forward(vm: &Vm, name: &str, args: &Value) -> Result<Value, String> {
    let conn = UnixStream::connect(&vm.socket)
        .map_err(|e| format!("Can't reach {} (pid {}): {}", vm.name, vm.pid, e))?;
    let mut w = conn.try_clone().map_err(|e| e.to_string())?;
    let mut msg = serde_json::to_vec(&json!({"id": 1, "name": name, "arguments": args}))
        .map_err(|e| e.to_string())?;
    msg.push(b'\n');
    w.write_all(&msg).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(conn)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.is_empty() {
        // Snow ends after vm-power quit, sometimes before the answer is read
        if name == "vm-power" && args["action"] == "quit" {
            return Ok(text_result("Snow has quit", false));
        }
        return Err(format!(
            "{} (pid {}) closed the connection",
            vm.name, vm.pid
        ));
    }
    let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    Ok(v["result"].clone())
}

fn vm_list() -> Value {
    let vms: Vec<Value> = list()
        .into_iter()
        .map(|v| {
            json!({
                "name": v.name,
                "pid": v.pid,
                "folder": v.meta["cwd"],
                "file": v.meta["file"],
                "version": v.meta["version"],
            })
        })
        .collect();
    if vms.is_empty() {
        return text_result(
            "No Snow with MCP is running. Start one with: snowemu --mcp <workspace.snoww> (or SNOW_MCP=1)",
            false,
        );
    }
    text_result(
        serde_json::to_string_pretty(&vms).unwrap_or_default(),
        false,
    )
}

fn vm_wait(args: &Value) -> Value {
    let up = args["state"].as_str().unwrap_or("up") != "gone";
    let timeout = Duration::from_millis(
        args["timeout-ms"]
            .as_u64()
            .unwrap_or(60000)
            .min(25 * 60 * 1000),
    );
    let vm = args["vm"].as_str();
    let deadline = Instant::now() + timeout;
    loop {
        let found = match vm {
            Some(_) => find(vm).ok(),
            None => list().into_iter().next(),
        };
        match (up, found) {
            (true, Some(v)) => {
                return text_result(format!("{} is up (pid {})", v.name, v.pid), false);
            }
            (false, None) => return text_result("Gone", false),
            _ => (),
        }
        if Instant::now() >= deadline {
            return text_result(
                format!(
                    "Not met: still {} after {} ms",
                    if up { "not up" } else { "running" },
                    timeout.as_millis()
                ),
                false,
            );
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn call(name: &str, args: &Value) -> Value {
    match name {
        "vm-list" => vm_list(),
        "vm-wait" => vm_wait(args),
        _ => {
            let mut args = args.clone();
            let vm = args
                .as_object_mut()
                .and_then(|m| m.remove("vm"))
                .and_then(|v| match v {
                    Value::String(s) => Some(s),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });
            match find(vm.as_deref()).and_then(|v| forward(&v, name, &args)) {
                Ok(r) => r,
                Err(e) => text_result(e, true),
            }
        }
    }
}

fn send(out: &Mutex<std::io::Stdout>, msg: &Value) {
    let mut o = out.lock().unwrap();
    let _ = serde_json::to_writer(&mut *o, msg);
    let _ = o.write_all(b"\n");
    let _ = o.flush();
}

pub fn run() {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            send(
                &out,
                &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}),
            );
            continue;
        };
        // Notifications have no id and get no answer
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg["method"].as_str().unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => {
                let version = params["protocolVersion"]
                    .as_str()
                    .unwrap_or("2025-06-18")
                    .to_string();
                send(
                    &out,
                    &json!({"jsonrpc": "2.0", "id": id, "result": {
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "snow", "version": snow_core::build_version()},
                        "instructions": super::instructions(),
                    }}),
                );
            }
            "ping" => send(&out, &json!({"jsonrpc": "2.0", "id": id, "result": {}})),
            "tools/list" => send(
                &out,
                &json!({"jsonrpc": "2.0", "id": id, "result": {"tools": super::tool_list()}}),
            ),
            "tools/call" => {
                let out = Arc::clone(&out);
                // Each call on its own thread, so a long wait holds up nothing else
                thread::spawn(move || {
                    let name = params["name"].as_str().unwrap_or("");
                    let args = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let r = call(name, &args);
                    send(&out, &json!({"jsonrpc": "2.0", "id": id, "result": r}));
                });
            }
            _ => send(
                &out,
                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Unknown method {}", method)}}),
            ),
        }
    }
    // stdin closed: let running calls finish their answers
    thread::sleep(Duration::from_millis(100));
}
