//! The thread that does every access to a slow disc, such as a physical drive,
//! so the emulator never waits for it.
//!
//! The thread does one job at a time, as a drive would, and takes no new job
//! until the current one is over. A job can be cancelled; the thread stays
//! busy with it until the access actually ends, and its result is dropped.

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender, TryRecvError};

use crate::mac::scsi::cdrom::{CdromError, MediaCheck, MediaId, RawSector};

/// Access to a disc that can be shared with the reader thread
pub trait CdromReader: Send + Sync {
    /// Reads user data, see `CdromBackend::read_bytes`
    fn read_at(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError>;

    /// Reads a raw CD Digital Audio sector, see `CdromBackend::read_cdda_sector`
    fn read_cdda_sector(&self, sector: u32) -> Result<RawSector, CdromError>;

    /// Finds out whether the disc in the drive is still the one `known`
    /// identifies, and describes the new one if not. Only reports: the
    /// backend takes the result in, with `CdromBackend::apply_media_check`.
    fn check_media(&self, _known: MediaId) -> Result<MediaCheck, CdromError> {
        Ok(MediaCheck::Unchanged)
    }

    /// Cancels the access `thread` is blocked in on this reader, if it can.
    /// The access then ends with an error sooner than it would have, or the
    /// call does nothing. The thread may also have just finished, or not yet
    /// started, the access.
    fn cancel(&self, _thread: &JoinHandle<()>) {}
}

/// A job for the reader thread
#[derive(Debug)]
pub(super) enum Job {
    Read { offset: usize, length: usize },
    ReadCdda { sector: u32 },
    CheckMedia { known: MediaId },
}

/// The result of a job
pub(super) enum JobOutput {
    Read(Vec<u8>),
    ReadCdda(Box<RawSector>),
    CheckMedia(MediaCheck),
}

/// The state of the current job
#[derive(Clone, Copy, PartialEq, Eq)]
enum Current {
    Running,
    Cancelled,
}

/// The reader thread of a disc. Dropping this cancels the current job, and
/// lets the thread end once the access is over, without waiting for it.
pub(super) struct DiscThread {
    jobs: Sender<Job>,
    results: Receiver<Result<JobOutput, CdromError>>,
    current: Option<Current>,
    reader: Arc<dyn CdromReader>,
    /// Kept for cancelling: holding it keeps the thread's OS handle open
    thread: JoinHandle<()>,
}

impl DiscThread {
    pub(super) fn spawn(reader: Arc<dyn CdromReader>) -> std::io::Result<Self> {
        let (jobs, job_queue) = crossbeam_channel::bounded(1);
        let (result_sender, results) = crossbeam_channel::bounded(1);
        let thread_reader = reader.clone();
        let thread = thread::Builder::new()
            .name("CD-ROM reader".to_string())
            .spawn(move || Self::run(&*thread_reader, &job_queue, &result_sender))?;
        Ok(Self {
            jobs,
            results,
            current: None,
            reader,
            thread,
        })
    }

    fn run(
        reader: &dyn CdromReader,
        jobs: &Receiver<Job>,
        results: &Sender<Result<JobOutput, CdromError>>,
    ) {
        for job in jobs {
            let result = match job {
                Job::Read { offset, length } => reader.read_at(offset, length).map(JobOutput::Read),
                Job::ReadCdda { sector } => reader
                    .read_cdda_sector(sector)
                    .map(|s| JobOutput::ReadCdda(Box::new(s))),
                Job::CheckMedia { known } => reader.check_media(known).map(JobOutput::CheckMedia),
            };
            if results.send(result).is_err() {
                // The DiscThread is gone
                return;
            }
        }
    }

    /// Whether the thread is on a job, cancelled or not, and takes no other
    pub(super) fn is_busy(&self) -> bool {
        self.current.is_some()
    }

    /// Starts a job. Returns it back if the thread is busy.
    pub(super) fn send(&mut self, job: Job) -> Result<(), Job> {
        if self.is_busy() {
            return Err(job);
        }
        // A send fails when the thread is gone, which poll() then reports
        let _ = self.jobs.send(job);
        self.current = Some(Current::Running);
        Ok(())
    }

    /// The result of the current job, once it is over. A cancelled job gives
    /// none, but the thread is free again afterwards all the same.
    pub(super) fn poll(&mut self) -> Option<Result<JobOutput, CdromError>> {
        let current = self.current?;
        let result = match self.results.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            // The thread is gone without answering: it panicked
            Err(TryRecvError::Disconnected) => Err(CdromError::BackendDead),
        };
        self.current = None;
        (current == Current::Running).then_some(result)
    }

    /// Cancels the current job, if any. The thread stays busy until the
    /// access is over.
    pub(super) fn cancel(&mut self) {
        if self.current == Some(Current::Running) {
            self.current = Some(Current::Cancelled);
            // No other job can have started since this one, so this cancels
            // this one, if anything
            self.reader.cancel(&self.thread);
        }
    }
}

impl Drop for DiscThread {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// A reader whose reads wait until the test opens the gate, or until
    /// they are cancelled
    #[derive(Default)]
    struct GatedReader {
        open: AtomicBool,
        cancelled: AtomicBool,
        /// Reads started
        reads: AtomicUsize,
        /// cancel() calls
        cancels: AtomicUsize,
    }

    impl CdromReader for GatedReader {
        fn read_at(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError> {
            self.reads.fetch_add(1, Ordering::AcqRel);
            loop {
                if self.cancelled.swap(false, Ordering::AcqRel) {
                    return Err(CdromError::BackendDead);
                }
                if self.open.load(Ordering::Acquire) {
                    return Ok(vec![offset as u8; length]);
                }
                thread::sleep(Duration::from_millis(1));
            }
        }

        fn read_cdda_sector(&self, _sector: u32) -> Result<RawSector, CdromError> {
            unreachable!()
        }

        fn cancel(&self, thread: &JoinHandle<()>) {
            assert_eq!(thread.thread().name(), Some("CD-ROM reader"));
            self.cancels.fetch_add(1, Ordering::AcqRel);
            self.cancelled.store(true, Ordering::Release);
        }
    }

    /// Polls until the current job is over
    fn wait(thread: &mut DiscThread) -> Option<Result<JobOutput, CdromError>> {
        let start = std::time::Instant::now();
        while thread.is_busy() {
            assert!(start.elapsed() < Duration::from_secs(5), "job never over");
            if let Some(result) = thread.poll() {
                return Some(result);
            }
            thread::sleep(Duration::from_millis(1));
        }
        None
    }

    fn read(offset: usize) -> Job {
        Job::Read { offset, length: 16 }
    }

    #[test]
    fn one_job_at_a_time() {
        let reader = Arc::new(GatedReader::default());
        let mut thread = DiscThread::spawn(reader.clone()).unwrap();
        assert!(!thread.is_busy());

        assert!(thread.send(read(1)).is_ok());
        assert!(thread.is_busy());
        assert!(matches!(
            thread.send(read(2)),
            Err(Job::Read { offset: 2, .. })
        ));
        assert!(thread.poll().is_none());

        reader.open.store(true, Ordering::Release);
        match wait(&mut thread) {
            Some(Ok(JobOutput::Read(d))) => assert_eq!(d, [1; 16]),
            _ => panic!("expected the data"),
        }
        assert!(!thread.is_busy());
        assert_eq!(reader.reads.load(Ordering::Acquire), 1);
    }

    #[test]
    fn cancel_drops_the_job() {
        let reader = Arc::new(GatedReader::default());
        let mut thread = DiscThread::spawn(reader.clone()).unwrap();

        thread.send(read(1)).unwrap();
        thread.cancel();
        assert_eq!(reader.cancels.load(Ordering::Acquire), 1);
        // Busy until the access is over, then free without a result
        assert!(thread.send(read(2)).is_err());
        assert!(wait(&mut thread).is_none());
        assert!(!thread.is_busy());

        // The next job runs
        reader.open.store(true, Ordering::Release);
        thread.send(read(3)).unwrap();
        match wait(&mut thread) {
            Some(Ok(JobOutput::Read(d))) => assert_eq!(d, [3; 16]),
            _ => panic!("expected the data"),
        }
    }

    #[test]
    fn cancel_without_a_job_does_nothing() {
        let reader = Arc::new(GatedReader::default());
        let mut thread = DiscThread::spawn(reader.clone()).unwrap();
        thread.cancel();

        reader.open.store(true, Ordering::Release);
        thread.send(read(1)).unwrap();
        assert!(matches!(wait(&mut thread), Some(Ok(_))));
        // Nor after the job is over
        thread.cancel();
        assert_eq!(reader.cancels.load(Ordering::Acquire), 0);
    }
}
