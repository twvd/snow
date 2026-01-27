//! Snow RPC Command Line Interface
//!
//! A portable CLI for controlling Snow emulator via RPC.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "snow-rpc")]
#[command(about = "Snow RPC Command Line Interface", long_about = None)]
#[command(after_help = r#"EXAMPLES:
    snow-rpc status                          Get emulator status
    snow-rpc run                             Resume emulation
    snow-rpc stop                            Pause emulation
    snow-rpc reset                           Reset emulator
    snow-rpc speed Uncapped                  Set speed mode
    snow-rpc type "Hello"                    Type text
    snow-rpc combo command q                 Press Cmd+Q
    snow-rpc combo command shift s           Press Cmd+Shift+S
    snow-rpc key return down                 Press Return key
    snow-rpc screenshot /tmp/screen.png      Save screenshot
    snow-rpc floppy-insert 0 /path/disk.img  Insert floppy
    snow-rpc floppy-eject 0                  Eject floppy
    snow-rpc serial-enable A pty             Enable PTY on channel A
    snow-rpc serial-enable B tcp --port 2000 Enable TCP on channel B
    snow-rpc serial-enable B localtalk       Enable LocalTalk on channel B
    snow-rpc serial-disable A                Disable channel A
    snow-rpc fullscreen                      Enter fullscreen mode
    snow-rpc windowed                        Exit fullscreen mode
    snow-rpc toggle-fullscreen               Toggle fullscreen mode
"#)]
struct Cli {
    /// RPC socket path (Unix) or host:port (TCP)
    /// Auto-detected on Unix if not specified
    #[arg(short, long, global = true)]
    socket: Option<String>,

    /// Use TCP connection instead of Unix socket
    #[arg(short, long, global = true)]
    tcp: bool,

    /// Output in JSON format
    #[arg(short, long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Get emulator status
    Status,

    /// Resume emulator
    Run,

    /// Pause emulator
    Stop,

    /// Reset emulator
    Reset,

    /// Get or set speed mode
    Speed {
        /// Speed mode: Accurate, Uncapped, or Video
        mode: Option<String>,
    },

    /// Take a screenshot
    Screenshot {
        /// Path to save screenshot (prints base64 to stdout if not specified)
        path: Option<PathBuf>,

        /// Output format: png or raw_rgba
        #[arg(short, long, default_value = "png")]
        format: String,
    },

    /// Type text
    Type {
        /// Text to type
        text: String,

        /// Delay between keys in milliseconds
        #[arg(short, long)]
        delay: Option<u64>,
    },

    /// Press a key combination
    Combo {
        /// Keys to press (e.g., command q)
        keys: Vec<String>,

        /// Delay between keys in milliseconds
        #[arg(short, long)]
        delay: Option<u64>,
    },

    /// Press or release a single key
    Key {
        /// Key name or scancode
        key: String,

        /// Key state: down or up
        state: String,
    },

    /// Release all pressed keys
    ReleaseKeys,

    /// Move mouse
    MouseMove {
        /// X coordinate or delta
        x: i32,

        /// Y coordinate or delta
        y: i32,

        /// Use absolute coordinates
        #[arg(short, long)]
        absolute: bool,
    },

    /// Click mouse button
    MouseClick {
        /// X coordinate (optional)
        x: Option<u16>,

        /// Y coordinate (optional)
        y: Option<u16>,
    },

    /// Press or release mouse button
    MouseButton {
        /// Button state: down or up
        state: String,
    },

    /// Insert floppy disk
    FloppyInsert {
        /// Drive number (0-2)
        drive: usize,

        /// Path to floppy image
        path: PathBuf,

        /// Write protect the disk
        #[arg(short, long)]
        write_protect: bool,
    },

    /// Eject floppy disk
    FloppyEject {
        /// Drive number (0-2)
        drive: usize,
    },

    /// Insert CD-ROM image
    CdromInsert {
        /// SCSI ID (0-6)
        id: usize,

        /// Path to CD-ROM image
        path: PathBuf,
    },

    /// Eject CD-ROM
    CdromEject {
        /// SCSI ID (0-6)
        id: usize,
    },

    /// Attach SCSI hard drive
    ScsiAttachHdd {
        /// SCSI ID (0-6)
        id: usize,

        /// Path to HDD image
        path: PathBuf,
    },

    /// Attach SCSI CD-ROM drive
    ScsiAttachCdrom {
        /// SCSI ID (0-6)
        id: usize,
    },

    /// Detach SCSI device
    ScsiDetach {
        /// SCSI ID (0-6)
        id: usize,
    },

    /// Set shared directory
    SharedDir {
        /// Path to shared directory (or "none" to clear)
        path: String,
    },

    /// Enable serial port bridge
    SerialEnable {
        /// Channel: A or B
        channel: String,

        /// Mode: pty, tcp, or localtalk
        mode: String,

        /// Port number (required for tcp mode)
        #[arg(short, long)]
        port: Option<u16>,
    },

    /// Disable serial port bridge
    SerialDisable {
        /// Channel: A or B
        channel: String,
    },

    /// Enter fullscreen mode
    Fullscreen,

    /// Exit fullscreen mode
    Windowed,

    /// Toggle fullscreen mode
    ToggleFullscreen,

    /// Send raw RPC method
    Raw {
        /// RPC method name
        method: String,

        /// JSON parameters
        params: Option<String>,
    },
}

#[derive(Serialize)]
struct RpcRequest {
    jsonrpc: &'static str,
    method: String,
    params: Value,
    id: u32,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: String,
}

/// Find Snow RPC socket on Unix systems
#[cfg(unix)]
fn find_socket() -> Option<String> {
    use std::os::unix::fs::FileTypeExt;

    // Check XDG_RUNTIME_DIR first, then /tmp
    let dirs = [
        std::env::var("XDG_RUNTIME_DIR").ok(),
        Some("/tmp".to_string()),
    ];

    for dir in dirs.into_iter().flatten() {
        let dir_path = std::path::Path::new(&dir);
        if let Ok(entries) = std::fs::read_dir(dir_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && name.starts_with("snow-")
                    && name.ends_with(".sock")
                    && let Ok(meta) = entry.metadata()
                    && meta.file_type().is_socket()
                {
                    return Some(path.to_string_lossy().to_string());
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn find_socket() -> Option<String> {
    // Windows doesn't have Unix sockets, return None to force TCP
    None
}

/// Connect to the RPC server and send a request
fn rpc_call(socket: &str, tcp: bool, method: &str, params: Value) -> Result<Value> {
    let request = RpcRequest {
        jsonrpc: "2.0",
        method: method.to_string(),
        params,
        id: 1,
    };

    let request_json = serde_json::to_string(&request)?;
    let response_json = if tcp || cfg!(windows) {
        send_tcp(socket, &request_json)?
    } else {
        #[cfg(unix)]
        {
            send_unix(socket, &request_json)?
        }
        #[cfg(not(unix))]
        {
            send_tcp(socket, &request_json)?
        }
    };

    let response: RpcResponse =
        serde_json::from_str(&response_json).context("Failed to parse RPC response")?;

    if let Some(error) = response.error {
        bail!("RPC error: {}", error.message);
    }

    response
        .result
        .ok_or_else(|| anyhow!("No result in response"))
}

#[cfg(unix)]
fn send_unix(socket_path: &str, request: &str) -> Result<String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("Failed to connect to socket: {}", socket_path))?;

    stream.write_all(request.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response)?;

    Ok(response)
}

fn send_tcp(addr: &str, request: &str) -> Result<String> {
    use std::net::TcpStream;

    let mut stream =
        TcpStream::connect(addr).with_context(|| format!("Failed to connect to: {}", addr))?;

    stream.write_all(request.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response)?;

    Ok(response)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Determine socket/address
    let socket = if let Some(s) = cli.socket {
        s
    } else if cli.tcp {
        "127.0.0.1:9100".to_string()
    } else {
        find_socket().ok_or_else(|| {
            anyhow!(
                "Could not find Snow RPC socket. Is Snow running with --rpc?\n\
                 Specify socket path with -s or use -t for TCP."
            )
        })?
    };

    match cli.command {
        Commands::Status => {
            let result = rpc_call(&socket, cli.tcp, "status.get", Value::Null)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                // Status line
                let status = if result["running"].as_bool() == Some(true) {
                    "Running"
                } else {
                    "Stopped"
                };
                println!("Status: {}", status);

                // Model with CPU and RAM
                let model = result["model"].as_str().unwrap_or("Unknown");
                let cpu_type = result["cpu_type"].as_str().unwrap_or("Unknown");
                let ram_mb = result["ram_mb"].as_u64().unwrap_or(0);
                println!("Model: {} ({}, {} MB RAM)", model, cpu_type, ram_mb);

                // Screen info
                if let Some(screen) = result.get("screen") {
                    let width = screen["width"].as_u64().unwrap_or(0);
                    let height = screen["height"].as_u64().unwrap_or(0);
                    let color_str = if screen["color"].as_bool() == Some(true) {
                        "color"
                    } else {
                        "B&W"
                    };
                    println!("Screen: {}x{} {}", width, height, color_str);
                }

                // Features
                let mut features = Vec::new();
                if result["has_adb"].as_bool() == Some(true) {
                    features.push("ADB");
                }
                if result["has_scsi"].as_bool() == Some(true) {
                    features.push("SCSI");
                }
                if result["hd_floppy"].as_bool() == Some(true) {
                    features.push("HD Floppy");
                }
                if !features.is_empty() {
                    println!("Features: {}", features.join(", "));
                }

                // Speed
                let speed = result["speed"].as_str().unwrap_or("Unknown");
                let effective = result["effective_speed"].as_f64().unwrap_or(1.0);
                println!("Speed: {} ({:.1}x)", speed, effective);

                // Cycles
                let cycles = result["cycles"].as_u64().unwrap_or(0);
                println!("Cycles: {}", cycles);

                // Shared dir
                if let Some(dir) = result.get("shared_dir").and_then(|v| v.as_str()) {
                    println!("Shared Dir: {}", dir);
                }

                // Floppies
                if let Some(floppies) = result.get("floppy").and_then(|v| v.as_array()) {
                    for f in floppies {
                        if f["present"].as_bool() == Some(true)
                            && f["ejected"].as_bool() == Some(false)
                        {
                            let status = if f["writing"].as_bool() == Some(true) {
                                "writing"
                            } else if f["motor"].as_bool() == Some(true) {
                                "motor on"
                            } else {
                                "idle"
                            };
                            println!("Floppy {}: {} [{}]", f["drive"], f["image_title"], status);
                        }
                    }
                }

                // SCSI devices
                if let Some(scsi) = result.get("scsi").and_then(|v| v.as_array()) {
                    for s in scsi {
                        if let Some(target_type) = s.get("target_type").and_then(|v| v.as_str()) {
                            let img = s
                                .get("image")
                                .and_then(|v| v.as_str())
                                .unwrap_or("no media");
                            println!("SCSI {}: {} - {}", s["id"], target_type, img);
                        }
                    }
                }

                // Serial ports
                if let Some(serial) = result.get("serial").and_then(|v| v.as_array()) {
                    for s in serial {
                        if s["enabled"].as_bool() == Some(true)
                            && let Some(status) = s.get("status").and_then(|v| v.as_str())
                        {
                            println!("Serial {}: {}", s["channel"], status);
                        }
                    }
                }
            }
        }

        Commands::Run => {
            let result = rpc_call(&socket, cli.tcp, "emulator.run", Value::Null)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Emulator running");
            }
        }

        Commands::Stop => {
            let result = rpc_call(&socket, cli.tcp, "emulator.stop", Value::Null)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Emulator stopped");
            }
        }

        Commands::Reset => {
            let result = rpc_call(&socket, cli.tcp, "emulator.reset", Value::Null)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Emulator reset");
            }
        }

        Commands::Speed { mode } => {
            if let Some(mode) = mode {
                let result = rpc_call(
                    &socket,
                    cli.tcp,
                    "speed.set",
                    serde_json::json!({ "mode": mode }),
                )?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("Speed changed from {} to {}", result["previous"], mode);
                }
            } else {
                let result = rpc_call(&socket, cli.tcp, "speed.get", Value::Null)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("Speed: {}", result["mode"]);
                }
            }
        }

        Commands::Screenshot { path, format } => {
            if let Some(path) = path {
                let result = rpc_call(
                    &socket,
                    cli.tcp,
                    "screenshot.save",
                    serde_json::json!({ "path": path }),
                )?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("Screenshot saved to {}", result["path"]);
                }
            } else {
                let result = rpc_call(
                    &socket,
                    cli.tcp,
                    "screenshot.get",
                    serde_json::json!({ "format": format }),
                )?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    // Print base64 data
                    println!("{}", result["data"]);
                }
            }
        }

        Commands::Type { text, delay } => {
            let mut params = serde_json::json!({ "text": text });
            if let Some(d) = delay {
                params["delay_ms"] = d.into();
            }
            let result = rpc_call(&socket, cli.tcp, "keyboard.type", params)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Typed: {}", text);
            }
        }

        Commands::Combo { keys, delay } => {
            let mut params = serde_json::json!({ "keys": keys });
            if let Some(d) = delay {
                params["delay_ms"] = d.into();
            }
            let result = rpc_call(&socket, cli.tcp, "keyboard.combo", params)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Pressed: {}", keys.join("+"));
            }
        }

        Commands::Key { key, state } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "keyboard.key",
                serde_json::json!({ "key": key, "state": state }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Key {} {}", key, state);
            }
        }

        Commands::ReleaseKeys => {
            let result = rpc_call(&socket, cli.tcp, "keyboard.release_all", Value::Null)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("All keys released");
            }
        }

        Commands::MouseMove { x, y, absolute } => {
            let result = if absolute {
                rpc_call(
                    &socket,
                    cli.tcp,
                    "mouse.set_position",
                    serde_json::json!({ "x": x, "y": y }),
                )?
            } else {
                rpc_call(
                    &socket,
                    cli.tcp,
                    "mouse.move",
                    serde_json::json!({ "dx": x, "dy": y }),
                )?
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if absolute {
                println!("Mouse moved to ({}, {})", x, y);
            } else {
                println!("Mouse moved by ({}, {})", x, y);
            }
        }

        Commands::MouseClick { x, y } => {
            let params = match (x, y) {
                (Some(x), Some(y)) => serde_json::json!({ "x": x, "y": y }),
                _ => Value::Null,
            };
            let result = rpc_call(&socket, cli.tcp, "mouse.click", params)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Mouse clicked");
            }
        }

        Commands::MouseButton { state } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "mouse.button",
                serde_json::json!({ "state": state }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Mouse button {}", state);
            }
        }

        Commands::FloppyInsert {
            drive,
            path,
            write_protect,
        } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "floppy.insert",
                serde_json::json!({
                    "drive": drive,
                    "path": path,
                    "write_protect": write_protect
                }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Floppy inserted in drive {}: {}", drive, path.display());
            }
        }

        Commands::FloppyEject { drive } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "floppy.eject",
                serde_json::json!({ "drive": drive }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Floppy ejected from drive {}", drive);
            }
        }

        Commands::CdromInsert { id, path } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "cdrom.insert",
                serde_json::json!({ "id": id, "path": path }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("CD-ROM inserted in SCSI {}: {}", id, path.display());
            }
        }

        Commands::CdromEject { id } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "cdrom.eject",
                serde_json::json!({ "id": id }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("CD-ROM ejected from SCSI {}", id);
            }
        }

        Commands::ScsiAttachHdd { id, path } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "scsi.attach_hdd",
                serde_json::json!({ "id": id, "path": path }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("SCSI HDD attached at ID {}: {}", id, path.display());
            }
        }

        Commands::ScsiAttachCdrom { id } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "scsi.attach_cdrom",
                serde_json::json!({ "id": id }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("SCSI CD-ROM drive attached at ID {}", id);
            }
        }

        Commands::ScsiDetach { id } => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "scsi.detach",
                serde_json::json!({ "id": id }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("SCSI device detached from ID {}", id);
            }
        }

        Commands::SharedDir { path } => {
            let path_value = if path == "none" {
                Value::Null
            } else {
                Value::String(path.clone())
            };
            let result = rpc_call(
                &socket,
                cli.tcp,
                "config.set_shared_dir",
                serde_json::json!({ "path": path_value }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if path == "none" {
                println!("Shared directory cleared");
            } else {
                println!("Shared directory set to: {}", path);
            }
        }

        Commands::SerialEnable {
            channel,
            mode,
            port,
        } => {
            let channel_upper = channel.to_uppercase();
            if channel_upper != "A" && channel_upper != "B" {
                bail!("Channel must be A or B");
            }
            let mode_lower = mode.to_lowercase();
            if mode_lower != "pty" && mode_lower != "tcp" && mode_lower != "localtalk" {
                bail!("Mode must be pty, tcp, or localtalk");
            }
            if mode_lower == "tcp" && port.is_none() {
                bail!("TCP mode requires --port");
            }
            let mut params = serde_json::json!({
                "channel": channel_upper,
                "mode": mode_lower
            });
            if let Some(p) = port {
                params["port"] = p.into();
            }
            let result = rpc_call(&socket, cli.tcp, "config.serial.enable", params)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if result["success"].as_bool() == Some(true) {
                if let Some(status) = result.get("status") {
                    let mode = status
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let details = match mode {
                        "pty" => status
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        "tcp" => {
                            let port = status.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
                            if let Some(conn) = status.get("connected").and_then(|v| v.as_str()) {
                                format!("port {} (connected: {})", port, conn)
                            } else {
                                format!("port {} (listening)", port)
                            }
                        }
                        "localtalk" => status
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        _ => "".to_string(),
                    };
                    println!(
                        "Serial {} enabled: {} {}",
                        channel_upper,
                        mode.to_uppercase(),
                        details
                    );
                } else {
                    println!("Serial {} enabled", channel_upper);
                }
            } else {
                let err = result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                bail!("Failed to enable serial: {}", err);
            }
        }

        Commands::SerialDisable { channel } => {
            let channel_upper = channel.to_uppercase();
            if channel_upper != "A" && channel_upper != "B" {
                bail!("Channel must be A or B");
            }
            let result = rpc_call(
                &socket,
                cli.tcp,
                "config.serial.disable",
                serde_json::json!({ "channel": channel_upper }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Serial {} disabled", channel_upper);
            }
        }

        Commands::Fullscreen => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "window.set_fullscreen",
                serde_json::json!({ "fullscreen": true }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Entered fullscreen");
            }
        }

        Commands::Windowed => {
            let result = rpc_call(
                &socket,
                cli.tcp,
                "window.set_fullscreen",
                serde_json::json!({ "fullscreen": false }),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Exited fullscreen");
            }
        }

        Commands::ToggleFullscreen => {
            let result = rpc_call(&socket, cli.tcp, "window.toggle_fullscreen", Value::Null)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if result["fullscreen"].as_bool() == Some(true) {
                println!("Entered fullscreen");
            } else {
                println!("Exited fullscreen");
            }
        }

        Commands::Raw { method, params } => {
            let params = if let Some(p) = params {
                serde_json::from_str(&p).context("Invalid JSON params")?
            } else {
                Value::Null
            };
            let result = rpc_call(&socket, cli.tcp, &method, params)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    Ok(())
}
