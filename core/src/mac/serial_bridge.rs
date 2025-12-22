//! Serial port bridge for connecting emulated SCC to host system
//!
//! This module provides a bridge between the emulated Z8530 SCC and the host
//! system, allowing external tools like Retro68's LaunchAPPL to communicate
//! with software running inside the emulated Mac.
//!
//! Supports three modes:
//! - PTY mode (Unix): Creates a pseudo-terminal that appears as a serial port
//! - TCP mode: Listens on a TCP port for connections
//! - LocalTalk mode: LocalTalk over UDP multicast for AppleTalk networking

use std::io::{self, Read, Write};
use std::path::PathBuf;

use log::*;
use serde::{Deserialize, Serialize};

use super::localtalk_bridge::LocalTalkStatus;

/// Configuration for a serial bridge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SerialBridgeConfig {
    /// Create a PTY (pseudo-terminal) - Unix only
    Pty,
    /// Listen on a TCP port
    Tcp(u16),
    /// LocalTalk over UDP multicast
    LocalTalk,
}

impl std::fmt::Display for SerialBridgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pty => write!(f, "PTY"),
            Self::Tcp(port) => write!(f, "TCP:{}", port),
            Self::LocalTalk => write!(f, "LocalTalk"),
        }
    }
}

/// Status of an active serial bridge
#[derive(Debug, Clone)]
pub enum SerialBridgeStatus {
    /// PTY bridge active, with path to slave device
    Pty(PathBuf),
    /// TCP bridge listening on port
    TcpListening(u16),
    /// TCP bridge with connected client
    TcpConnected(u16, String),
    /// LocalTalk bridge active
    LocalTalk(LocalTalkStatus),
}

impl std::fmt::Display for SerialBridgeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pty(path) => write!(f, "PTY: {}", path.display()),
            Self::TcpListening(port) => write!(f, "TCP:{} (listening)", port),
            Self::TcpConnected(port, addr) => write!(f, "TCP:{} ({})", port, addr),
            Self::LocalTalk(status) => write!(f, "{}", status),
        }
    }
}

/// Trait for serial bridge implementations
pub trait SerialBridgeBackend: Send {
    /// Read available data from the bridge (non-blocking)
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Write data to the bridge
    fn write(&mut self, data: &[u8]) -> io::Result<usize>;

    /// Get current status
    fn status(&self) -> SerialBridgeStatus;

    /// Poll for new connections (TCP only), returns true if state changed
    fn poll(&mut self) -> bool;
}

/// PTY-based serial bridge (Unix only)
#[cfg(unix)]
pub mod pty {
    use super::*;
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    use nix::libc;
    use nix::pty::{openpty, OpenptyResult};
    use nix::sys::termios::{cfmakeraw, tcsetattr, SetArg};

    pub struct PtyBridge {
        master: OwnedFd,
        slave_path: PathBuf,
        read_file: File,
    }

    impl PtyBridge {
        pub fn new() -> io::Result<Self> {
            // Open PTY pair
            let OpenptyResult { master, slave } = openpty(None, None).map_err(io::Error::other)?;

            // Get the slave path (must use master fd for ptsname)
            let slave_path = Self::get_slave_path(&master)?;

            // Configure the slave PTY for raw mode (8N1, no echo, no processing)
            let mut termios = nix::sys::termios::tcgetattr(&slave).map_err(io::Error::other)?;
            cfmakeraw(&mut termios);
            tcsetattr(&slave, SetArg::TCSANOW, &termios).map_err(io::Error::other)?;

            // Drop the slave fd - external tools will open it via the path
            drop(slave);

            // Set master to non-blocking
            let flags = fcntl(master.as_raw_fd(), FcntlArg::F_GETFL).map_err(io::Error::other)?;
            fcntl(
                master.as_raw_fd(),
                FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
            )
            .map_err(io::Error::other)?;

            // Create a File wrapper for reading (needs to be separate due to borrow rules)
            let read_file = unsafe { File::from_raw_fd(libc::dup(master.as_raw_fd())) };

            info!("PTY bridge created: {}", slave_path.display());

            Ok(Self {
                master,
                slave_path,
                read_file,
            })
        }

        fn get_slave_path(master: &OwnedFd) -> io::Result<PathBuf> {
            // Use ptsname to get the slave path (called on master fd)
            let slave_name = unsafe {
                let ptr = libc::ptsname(master.as_raw_fd());
                if ptr.is_null() {
                    return Err(io::Error::last_os_error());
                }
                std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
            };
            Ok(PathBuf::from(slave_name))
        }

        pub fn slave_path(&self) -> &PathBuf {
            &self.slave_path
        }
    }

    impl SerialBridgeBackend for PtyBridge {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.read_file.read(buf) {
                Ok(n) => Ok(n),
                // WouldBlock = no data available (non-blocking)
                // EIO (error 5) = no client connected to slave yet
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.raw_os_error() == Some(libc::EIO) =>
                {
                    Ok(0)
                }
                Err(e) => Err(e),
            }
        }

        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            // Write to master fd - EIO means no client connected, silently ignore
            match nix::unistd::write(&self.master, data) {
                Ok(n) => Ok(n),
                Err(nix::errno::Errno::EIO) => Ok(data.len()), // Discard if no client
                Err(e) => Err(io::Error::other(e)),
            }
        }

        fn status(&self) -> SerialBridgeStatus {
            SerialBridgeStatus::Pty(self.slave_path.clone())
        }

        fn poll(&mut self) -> bool {
            false // PTY doesn't need polling
        }
    }
}

/// TCP-based serial bridge
pub mod tcp {
    use super::*;
    use std::net::{SocketAddr, TcpListener, TcpStream};

    use socket2::{Domain, Socket, Type};

    pub struct TcpBridge {
        listener: TcpListener,
        client: Option<TcpStream>,
        client_addr: Option<String>,
        port: u16,
    }

    impl TcpBridge {
        pub fn new(port: u16) -> io::Result<Self> {
            // Use socket2 to create a TCP listener with a backlog of 1
            // This prevents multiple clients from queuing up in the backlog
            let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
            socket.set_reuse_address(true)?;
            let addr: SocketAddr = ([0, 0, 0, 0], port).into();
            socket.bind(&addr.into())?;
            socket.listen(1)?;
            socket.set_nonblocking(true)?;
            let listener: TcpListener = socket.into();

            info!("TCP serial bridge listening on port {}", port);

            Ok(Self {
                listener,
                client: None,
                client_addr: None,
                port,
            })
        }

        fn accept_connection(&mut self) -> bool {
            match self.listener.accept() {
                Ok((stream, addr)) => {
                    stream.set_nonblocking(true).ok();
                    let addr_str = addr.to_string();
                    info!("TCP serial bridge: client connected from {}", addr_str);
                    self.client = Some(stream);
                    self.client_addr = Some(addr_str);
                    true
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => false,
                Err(e) => {
                    warn!("TCP serial bridge accept error: {}", e);
                    false
                }
            }
        }
    }

    impl SerialBridgeBackend for TcpBridge {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(ref mut client) = self.client else {
                return Ok(0);
            };

            match client.read(buf) {
                Ok(0) => {
                    // Client disconnected
                    info!("TCP serial bridge: client disconnected");
                    self.client = None;
                    self.client_addr = None;
                    Ok(0)
                }
                Ok(n) => Ok(n),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
                Err(e) => {
                    warn!("TCP serial bridge read error: {}", e);
                    self.client = None;
                    self.client_addr = None;
                    Err(e)
                }
            }
        }

        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            let Some(ref mut client) = self.client else {
                return Ok(data.len()); // Silently discard if no client
            };

            match client.write(data) {
                Ok(n) => Ok(n),
                Err(e) => {
                    warn!("TCP serial bridge write error: {}", e);
                    self.client = None;
                    self.client_addr = None;
                    Err(e)
                }
            }
        }

        fn status(&self) -> SerialBridgeStatus {
            match &self.client_addr {
                Some(addr) => SerialBridgeStatus::TcpConnected(self.port, addr.clone()),
                None => SerialBridgeStatus::TcpListening(self.port),
            }
        }

        fn poll(&mut self) -> bool {
            if self.client.is_none() {
                self.accept_connection()
            } else {
                false
            }
        }
    }
}

/// Unified serial bridge that can use either PTY or TCP backend
pub struct SerialBridge {
    backend: Box<dyn SerialBridgeBackend>,
    rx_buffer: Vec<u8>,
}

impl SerialBridge {
    /// Create a new serial bridge with the given configuration
    /// Note: LocalTalk should use SccBridge::new() instead
    pub fn new(config: &SerialBridgeConfig) -> io::Result<Self> {
        let backend: Box<dyn SerialBridgeBackend> = match config {
            #[cfg(unix)]
            SerialBridgeConfig::Pty => Box::new(pty::PtyBridge::new()?),
            #[cfg(not(unix))]
            SerialBridgeConfig::Pty => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "PTY mode is only supported on Unix systems",
                ));
            }
            SerialBridgeConfig::Tcp(port) => Box::new(tcp::TcpBridge::new(*port)?),
            SerialBridgeConfig::LocalTalk => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "LocalTalk should use SccBridge::new() instead",
                ));
            }
        };

        Ok(Self {
            backend,
            rx_buffer: Vec::with_capacity(4096),
        })
    }

    /// Read data from the bridge into the internal buffer
    /// Returns the data that should be sent to the SCC RX queue
    pub fn read_to_scc(&mut self) -> Vec<u8> {
        let mut buf = [0u8; 1024];
        loop {
            match self.backend.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    self.rx_buffer.extend_from_slice(&buf[..n]);
                }
                Err(e) => {
                    warn!("Serial bridge read error: {}", e);
                    break;
                }
            }
        }

        std::mem::take(&mut self.rx_buffer)
    }

    /// Write data from the SCC TX queue to the bridge
    pub fn write_from_scc(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let mut offset = 0;
        while offset < data.len() {
            match self.backend.write(&data[offset..]) {
                Ok(n) => offset += n,
                Err(e) => {
                    warn!("Serial bridge write error: {}", e);
                    break;
                }
            }
        }
    }

    /// Get current bridge status
    pub fn status(&self) -> SerialBridgeStatus {
        self.backend.status()
    }

    /// Poll for state changes (e.g., new TCP connections)
    pub fn poll(&mut self) -> bool {
        self.backend.poll()
    }
}

/// Unified bridge that can be either serial (PTY/TCP) or LocalTalk
pub enum SccBridge {
    /// Serial bridge (PTY or TCP)
    Serial(SerialBridge),
    /// LocalTalk over UDP
    LocalTalk(super::localtalk_bridge::LocalTalkBridge),
}

impl SccBridge {
    /// Create a new bridge with the given configuration
    pub fn new(config: &SerialBridgeConfig) -> io::Result<Self> {
        match config {
            SerialBridgeConfig::LocalTalk => Ok(Self::LocalTalk(
                super::localtalk_bridge::LocalTalkBridge::new()?,
            )),
            _ => Ok(Self::Serial(SerialBridge::new(config)?)),
        }
    }

    /// Write data from the SCC TX queue to the bridge
    pub fn write_from_scc(&mut self, data: &[u8]) {
        match self {
            Self::Serial(bridge) => bridge.write_from_scc(data),
            Self::LocalTalk(bridge) => bridge.write_from_scc(data),
        }
    }

    /// Read data from the bridge to inject into SCC RX queue
    pub fn read_to_scc(&mut self) -> Vec<u8> {
        match self {
            Self::Serial(bridge) => bridge.read_to_scc(),
            Self::LocalTalk(bridge) => bridge.read_to_scc().unwrap_or_default(),
        }
    }

    /// Poll for state changes
    pub fn poll(&mut self) -> bool {
        match self {
            Self::Serial(bridge) => bridge.poll(),
            Self::LocalTalk(bridge) => bridge.poll(),
        }
    }

    /// Get current bridge status
    pub fn status(&self) -> SerialBridgeStatus {
        match self {
            Self::Serial(bridge) => bridge.status(),
            Self::LocalTalk(bridge) => SerialBridgeStatus::LocalTalk(bridge.status()),
        }
    }

    /// Check if this bridge is a LocalTalk bridge
    pub fn is_localtalk(&self) -> bool {
        matches!(self, Self::LocalTalk(_))
    }
}
