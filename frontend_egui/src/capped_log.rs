//! Bound on the memory of the egui log window
//!
//! egui_logger keeps every record until its window is shown, which trims it,
//! so a flood of log messages with the window closed takes memory without
//! limit. This wrapper counts what it holds and clears it past a cap.

use std::fmt::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use log::{Log, Metadata, Record};

/// Bytes of records the log window may hold
const CAP: usize = 1 << 30;

/// Bytes egui_logger takes for a record, beyond its message and target
const RECORD_OVERHEAD: usize = 128;

/// Counts the bytes of formatted text, without keeping it
struct ByteCount(usize);

impl Write for ByteCount {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

/// Wraps the egui logger, clearing it once it holds about [`CAP`] bytes. The
/// count doesn't follow the trimming done by the window, so it may clear
/// earlier than needed.
pub struct CappedLog {
    inner: Box<dyn Log>,
    bytes: AtomicUsize,
}

impl CappedLog {
    pub fn new(inner: Box<dyn Log>) -> Self {
        Self {
            inner,
            bytes: AtomicUsize::new(0),
        }
    }
}

impl Log for CappedLog {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.inner.enabled(record.metadata()) {
            return;
        }
        let mut count = ByteCount(record.target().len() + RECORD_OVERHEAD);
        let _ = write!(count, "{}", record.args());
        if self.bytes.fetch_add(count.0, Ordering::Relaxed) + count.0 > CAP {
            self.bytes.store(count.0, Ordering::Relaxed);
            egui_logger::clear_logs();
            self.inner.log(
                &Record::builder()
                    .level(log::Level::Warn)
                    .target(module_path!())
                    .args(format_args!("Log cleared after holding 1 GB"))
                    .build(),
            );
        }
        self.inner.log(record);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}
