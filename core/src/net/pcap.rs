//! Packet capture to pcap files

use super::EthernetPort;
use crate::emulator::comm::EthernetCaptureStatus;

use anyhow::Result;
use binrw::{BinWrite, binrw};

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Pcap file header structure
/// https://wiki.wireshark.org/Development/LibpcapFileFormat
/// https://www.winpcap.org/docs/docs_412/html/structpcap__file__header.html
#[binrw]
#[brw(little)]
struct PcapFileHeader {
    #[brw(magic = 0xa1b2c3d4u32)]
    _magic_number: (),
    #[brw(magic = 2u16)]
    _version_major: (),
    #[brw(magic = 4u16)]
    _version_minor: (),
    thiszone: i32,
    sigfigs: u32,
    snaplen: u32,
    network: u32,
}

/// Pcap packet header structure
/// https://wiki.wireshark.org/Development/LibpcapFileFormat
/// https://www.winpcap.org/docs/docs_412/html/structpcap__file__header.html
#[binrw]
#[brw(little)]
struct PcapPacketHeader {
    ts_sec: u32,
    ts_usec: u32,
    incl_len: u32,
    orig_len: u32,
}

/// Pcap capture state
pub(super) struct PcapCaptureState {
    filename: PathBuf,
    packet_count: Arc<AtomicUsize>,
    error: Arc<Mutex<Option<String>>>,
    thread_handle: Option<JoinHandle<()>>,
}

impl EthernetPort {
    pub(super) fn capture_packet(&self, packet: &[u8]) {
        if let Some(ref tx) = self.pcap_tx {
            let _ = tx.try_send((std::time::SystemTime::now(), packet.to_vec()));
        }
    }

    pub fn start_capture<P: AsRef<Path>>(&mut self, filename: P) -> Result<()> {
        use std::io::Write;
        use std::sync::Mutex;
        use std::sync::atomic::AtomicUsize;

        if self.pcap_capture.is_some() {
            anyhow::bail!("Capture already active");
        }

        let filename = filename.as_ref();

        // Create file and write global header
        let mut file = std::fs::File::create(filename)?;
        let header = PcapFileHeader {
            _magic_number: (),
            _version_major: (),
            _version_minor: (),
            thiszone: 0,
            sigfigs: 0,
            snaplen: 65535,
            network: 1, // Ethernet
        };
        header.write_le(&mut file)?;
        file.flush()?;

        // Create channel for packet passing
        let (tx, rx) = crossbeam_channel::unbounded::<(std::time::SystemTime, Vec<u8>)>();

        // Create shared state
        let packet_count = Arc::new(AtomicUsize::new(0));
        let error = Arc::new(Mutex::new(None));

        // Spawn background thread
        let thread_handle = Self::spawn_capture_thread(
            filename.to_path_buf(),
            rx,
            packet_count.clone(),
            error.clone(),
        );

        // Store state
        self.pcap_capture = Some(PcapCaptureState {
            filename: filename.to_path_buf(),
            packet_count,
            error,
            thread_handle: Some(thread_handle),
        });
        self.pcap_tx = Some(tx);

        log::info!("Started pcap capture to '{}'", filename.display());
        Ok(())
    }

    pub fn stop_capture(&mut self) -> Option<(std::path::PathBuf, usize)> {
        use std::sync::atomic::Ordering;

        if let Some(mut state) = self.pcap_capture.take() {
            // Drop channel sender to signal thread termination
            self.pcap_tx = None;

            // Join thread to ensure clean shutdown
            if let Some(handle) = state.thread_handle.take() {
                let _ = handle.join();
            }

            let count = state.packet_count.load(Ordering::Relaxed);
            log::info!(
                "Stopped pcap capture, wrote {} packets to '{}'",
                count,
                state.filename.display()
            );
            Some((state.filename, count))
        } else {
            None
        }
    }

    pub fn capture_status(&self) -> Option<EthernetCaptureStatus> {
        use std::sync::atomic::Ordering;

        self.pcap_capture
            .as_ref()
            .map(|state| EthernetCaptureStatus {
                active: true,
                filename: Some(state.filename.clone()),
                packet_count: state.packet_count.load(Ordering::Relaxed),
                error: state.error.lock().unwrap().clone(),
            })
    }

    fn spawn_capture_thread(
        filename: PathBuf,
        rx: crossbeam_channel::Receiver<(std::time::SystemTime, Vec<u8>)>,
        packet_count: Arc<AtomicUsize>,
        error: Arc<Mutex<Option<String>>>,
    ) -> JoinHandle<()> {
        use std::io::Write;
        use std::sync::atomic::Ordering;

        std::thread::spawn(move || {
            let mut file = match std::fs::OpenOptions::new().append(true).open(&filename) {
                Ok(f) => f,
                Err(e) => {
                    *error.lock().unwrap() = Some(format!("Failed to open file: {}", e));
                    return;
                }
            };

            while let Ok((timestamp, packet)) = rx.recv() {
                let duration = timestamp
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();

                let header = PcapPacketHeader {
                    ts_sec: duration.as_secs() as u32,
                    ts_usec: duration.subsec_micros(),
                    incl_len: packet.len() as u32,
                    orig_len: packet.len() as u32,
                };

                if let Err(e) = header.write_le(&mut file) {
                    *error.lock().unwrap() = Some(format!("Write error: {}", e));
                    break;
                }

                if let Err(e) = file.write_all(&packet) {
                    *error.lock().unwrap() = Some(format!("Write error: {}", e));
                    break;
                }

                packet_count.fetch_add(1, Ordering::Relaxed);
            }

            let _ = file.flush();
        })
    }
}
