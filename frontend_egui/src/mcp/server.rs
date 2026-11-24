//! The socket each Snow with MCP on listens on
//!
//! One JSON request per line: {"id": 1, "name": "screenshot", "arguments": {}}
//! One JSON answer per line: {"id": 1, "result": <MCP CallToolResult>}

use super::hub::Hub;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

static FILES: Mutex<Option<(PathBuf, PathBuf)>> = Mutex::new(None);

/// /tmp/snow-<uid>: made if missing, and it must be ours and private
pub fn folder() -> anyhow::Result<PathBuf> {
    // SAFETY: getuid cannot fail
    let uid = unsafe { libc::getuid() };
    let dir = PathBuf::from(format!("/tmp/snow-{}", uid));
    if !dir.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    }
    let md = std::fs::symlink_metadata(&dir)?;
    if !md.is_dir() || md.uid() != uid {
        anyhow::bail!("{} is not a folder of this user", dir.display());
    }
    if md.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

pub fn start(hub: Arc<Hub>, file: Option<&str>) -> anyhow::Result<()> {
    let dir = folder()?;
    let pid = std::process::id();
    let sock = dir.join(format!("{}.sock", pid));
    let info = dir.join(format!("{}.json", pid));
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let workspace = file.map(|f| {
        let p = Path::new(f);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        }
    });
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let meta = json!({
        "name": super::vm_name(),
        "pid": pid,
        "socket": sock,
        "cwd": cwd,
        "file": workspace,
        "started": started,
        "version": snow_core::build_version(),
    });
    // Write the file whole, then rename, so a bridge never reads half of it
    let tmp = dir.join(format!("{}.json.tmp", pid));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&meta)?)?;
    std::fs::rename(&tmp, &info)?;
    *FILES.lock().unwrap() = Some((sock, info));

    // SAFETY: registers a plain function to run at exit
    unsafe {
        libc::atexit(at_exit);
    }

    thread::Builder::new()
        .name("mcp-server".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else {
                    continue;
                };
                let hub = Arc::clone(&hub);
                let _ = thread::Builder::new()
                    .name("mcp-conn".into())
                    .spawn(move || serve(&hub, conn));
            }
        })?;
    Ok(())
}

extern "C" fn at_exit() {
    cleanup();
}

pub fn cleanup() {
    if let Some((sock, info)) = FILES.lock().map(|mut f| f.take()).unwrap_or(None) {
        let _ = std::fs::remove_file(sock);
        let _ = std::fs::remove_file(info);
    }
}

fn serve(hub: &Arc<Hub>, conn: UnixStream) {
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    for line in BufReader::new(conn).lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let (id, answer, exit) = match serde_json::from_str::<Value>(&line) {
            Ok(req) => {
                let id = req.get("id").cloned().unwrap_or(Value::Null);
                let name = req.get("name").and_then(Value::as_str).unwrap_or("");
                let args = req.get("arguments").cloned().unwrap_or(Value::Null);
                let r = super::tools::call(hub, name, &args);
                (id, r.to_json(), r.exit)
            }
            Err(e) => (
                Value::Null,
                json!({"content": [{"type": "text", "text": format!("Bad request: {}", e)}], "isError": true}),
                false,
            ),
        };
        let mut msg = serde_json::to_vec(&json!({"id": id, "result": answer})).unwrap_or_default();
        msg.push(b'\n');
        let sent = out.write_all(&msg).and_then(|()| out.flush());
        if exit {
            super::exit_process();
        }
        if sent.is_err() {
            break;
        }
    }
}
