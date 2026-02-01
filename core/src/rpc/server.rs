//! RPC server implementation
//!
//! Handles Unix domain socket (and optionally TCP) connections for JSON-RPC 2.0 requests.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{env, fs, process};

use crossbeam_channel::{Receiver, Sender};
use log::*;

use super::handlers::RpcHandler;
use super::types::{RpcRequest, RpcResponse};

/// RPC server configuration
#[derive(Debug, Clone)]
pub struct RpcConfig {
    /// Enable Unix socket server
    pub unix_socket: bool,
    /// Custom Unix socket path (if None, uses default location)
    pub unix_socket_path: Option<PathBuf>,
    /// Enable TCP server
    pub tcp: bool,
    /// TCP port to listen on
    pub tcp_port: u16,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            unix_socket: true,
            unix_socket_path: None,
            tcp: false,
            tcp_port: 0,
        }
    }
}

impl RpcConfig {
    /// Returns the Unix socket path to use
    pub fn socket_path(&self) -> PathBuf {
        if let Some(ref path) = self.unix_socket_path {
            return path.clone();
        }

        let pid = process::id();
        let filename = format!("snow-{}.sock", pid);

        // Try XDG_RUNTIME_DIR first, fall back to /tmp
        if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join(&filename)
        } else {
            PathBuf::from("/tmp").join(&filename)
        }
    }
}

/// Message sent from the RPC server to the handler
pub enum RpcMessage {
    /// A request that needs to be handled
    Request {
        request: RpcRequest,
        response_tx: Sender<RpcResponse>,
    },
    /// Server is shutting down
    Shutdown,
}

/// RPC server that listens for connections and dispatches requests
pub struct RpcServer {
    config: RpcConfig,
    running: Arc<AtomicBool>,
    unix_thread: Option<JoinHandle<()>>,
    tcp_thread: Option<JoinHandle<()>>,
    request_tx: Sender<RpcMessage>,
    request_rx: Receiver<RpcMessage>,
}

impl RpcServer {
    /// Creates a new RPC server with the given configuration
    pub fn new(config: RpcConfig) -> Self {
        let (request_tx, request_rx) = crossbeam_channel::unbounded();
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            unix_thread: None,
            tcp_thread: None,
            request_tx,
            request_rx,
        }
    }

    /// Returns the request receiver channel
    pub fn request_receiver(&self) -> Receiver<RpcMessage> {
        self.request_rx.clone()
    }

    /// Starts the RPC server
    pub fn start(&mut self) -> anyhow::Result<()> {
        self.running.store(true, Ordering::SeqCst);

        if self.config.unix_socket {
            let socket_path = self.config.socket_path();

            // Remove existing socket file if it exists
            if socket_path.exists() {
                fs::remove_file(&socket_path)?;
            }

            let listener = UnixListener::bind(&socket_path)?;
            listener.set_nonblocking(true)?;

            info!(
                "RPC server listening on Unix socket: {}",
                socket_path.display()
            );
            eprintln!(
                "RPC server listening on Unix socket: {}",
                socket_path.display()
            );

            let running = self.running.clone();
            let request_tx = self.request_tx.clone();

            self.unix_thread = Some(thread::spawn(move || {
                Self::unix_server_loop(listener, running, request_tx, socket_path);
            }));
        }

        if self.config.tcp && self.config.tcp_port > 0 {
            let addr = format!("127.0.0.1:{}", self.config.tcp_port);
            let listener = TcpListener::bind(&addr)?;
            listener.set_nonblocking(true)?;

            info!("RPC server listening on TCP: {}", addr);
            eprintln!("RPC server listening on TCP: {}", addr);

            let running = self.running.clone();
            let request_tx = self.request_tx.clone();

            self.tcp_thread = Some(thread::spawn(move || {
                Self::tcp_server_loop(listener, running, request_tx);
            }));
        }

        Ok(())
    }

    /// Stops the RPC server
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);

        // Send shutdown message to wake up any waiting handlers
        let _ = self.request_tx.send(RpcMessage::Shutdown);

        if let Some(thread) = self.unix_thread.take() {
            let _ = thread.join();
        }

        if let Some(thread) = self.tcp_thread.take() {
            let _ = thread.join();
        }

        // Clean up Unix socket file
        if self.config.unix_socket {
            let socket_path = self.config.socket_path();
            if socket_path.exists() {
                let _ = fs::remove_file(&socket_path);
            }
        }
    }

    /// Returns the socket path if Unix socket is enabled
    pub fn socket_path(&self) -> Option<PathBuf> {
        if self.config.unix_socket {
            Some(self.config.socket_path())
        } else {
            None
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn unix_server_loop(
        listener: UnixListener,
        running: Arc<AtomicBool>,
        request_tx: Sender<RpcMessage>,
        socket_path: PathBuf,
    ) {
        while running.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let tx = request_tx.clone();
                    thread::spawn(move || {
                        if let Err(e) = Self::handle_unix_connection(stream, tx) {
                            debug!("Unix connection error: {}", e);
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    error!("Unix socket accept error: {}", e);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }

        // Clean up socket file on exit
        let _ = fs::remove_file(&socket_path);
    }

    #[allow(clippy::needless_pass_by_value)]
    fn tcp_server_loop(
        listener: TcpListener,
        running: Arc<AtomicBool>,
        request_tx: Sender<RpcMessage>,
    ) {
        while running.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, addr)) => {
                    debug!("TCP connection from {}", addr);
                    let tx = request_tx.clone();
                    thread::spawn(move || {
                        if let Err(e) = Self::handle_tcp_connection(stream, tx) {
                            debug!("TCP connection error: {}", e);
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    error!("TCP accept error: {}", e);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn handle_unix_connection(
        stream: UnixStream,
        request_tx: Sender<RpcMessage>,
    ) -> anyhow::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;

        Self::handle_connection(&mut reader, &mut writer, request_tx)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn handle_tcp_connection(
        stream: TcpStream,
        request_tx: Sender<RpcMessage>,
    ) -> anyhow::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;

        Self::handle_connection(&mut reader, &mut writer, request_tx)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn handle_connection<R: BufRead, W: Write>(
        reader: &mut R,
        writer: &mut W,
        request_tx: Sender<RpcMessage>,
    ) -> anyhow::Result<()> {
        let mut line = String::new();
        while reader.read_line(&mut line)? > 0 {
            let line_trimmed = line.trim();
            if line_trimmed.is_empty() {
                line.clear();
                continue;
            }

            let response = match serde_json::from_str::<RpcRequest>(line_trimmed) {
                Ok(request) => {
                    // Validate JSON-RPC version
                    if request.jsonrpc != "2.0" {
                        RpcResponse::invalid_request(request.id)
                    } else {
                        // Send request to handler and wait for response
                        let (response_tx, response_rx) = crossbeam_channel::bounded(1);
                        request_tx.send(RpcMessage::Request {
                            request,
                            response_tx,
                        })?;

                        // Wait for response with timeout
                        match response_rx.recv_timeout(Duration::from_secs(30)) {
                            Ok(response) => response,
                            Err(_) => RpcResponse::internal_error(None, "Handler timeout"),
                        }
                    }
                }
                Err(e) => {
                    debug!("Failed to parse RPC request: {}", e);
                    RpcResponse::parse_error()
                }
            };

            // Write response
            let response_json = serde_json::to_string(&response)?;
            writeln!(writer, "{}", response_json)?;
            writer.flush()?;

            line.clear();
        }

        Ok(())
    }
}

impl Drop for RpcServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Helper struct for the handler side that processes RPC requests
#[allow(dead_code)]
pub struct RpcRequestProcessor {
    request_rx: Receiver<RpcMessage>,
}

#[allow(dead_code)]
impl RpcRequestProcessor {
    pub fn new(request_rx: Receiver<RpcMessage>) -> Self {
        Self { request_rx }
    }

    /// Tries to receive and process a pending RPC request without blocking.
    /// Returns true if a request was processed.
    pub fn try_process<H: RpcHandler>(&self, handler: &mut H) -> bool {
        match self.request_rx.try_recv() {
            Ok(RpcMessage::Request {
                request,
                response_tx,
            }) => {
                let response = handler.handle_request(&request);
                let _ = response_tx.send(response);
                true
            }
            Ok(RpcMessage::Shutdown) => false,
            Err(_) => false,
        }
    }

    /// Processes all pending RPC requests without blocking.
    pub fn process_all<H: RpcHandler>(&self, handler: &mut H) {
        while self.try_process(handler) {}
    }
}
