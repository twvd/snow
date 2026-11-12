//! The thread that does every access to a slow disc, such as a physical drive,
//! so the emulator never waits for it.
//!
//! The thread takes requests in order, one at a time, as a drive would. Each
//! request has its own `Reply`: dropping it abandons the request, which the
//! thread then skips, or cancels if already started.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender, TryRecvError};

use crate::mac::scsi::cdrom::{CdromError, MediaCheck, MediaId};

/// Access to a disc that can be shared with the reader thread
pub trait CdromReader: Send + Sync {
    /// Reads user data, see `CdromBackend::read_bytes`
    fn read_at(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError>;

    /// Finds out whether the disc in the drive is still the one `known`
    /// identifies, and describes the new one if not. Only reports: the
    /// backend takes the result in, with `CdromBackend::apply_media_check`.
    fn check_media(&self, _known: MediaId) -> Result<MediaCheck, CdromError> {
        Ok(MediaCheck::Unchanged)
    }

    /// Cancels the access `thread` is blocked in on this reader, if it can,
    /// for a request that was abandoned. The access then ends with an error
    /// sooner than it would have, or the call does nothing. The thread may
    /// also have just finished, or never have started, an access.
    fn cancel(&self, _thread: &JoinHandle<()>) {}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestState {
    /// Waiting in the queue
    Queued,
    /// The reader thread is on it
    Running,
    /// The reader thread is done with it
    Done,
    /// The reply was dropped
    Abandoned,
}

/// Where a request stands, shared by the `Reply` and the reader thread
struct Request(Mutex<RequestState>);

impl Request {
    fn lock(&self) -> MutexGuard<'_, RequestState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The reader thread takes the request up. False if it was abandoned.
    fn start(&self) -> bool {
        let mut state = self.lock();
        if *state == RequestState::Abandoned {
            return false;
        }
        *state = RequestState::Running;
        true
    }

    /// The reader thread is done with the request
    fn finish(&self) {
        let mut state = self.lock();
        if *state == RequestState::Running {
            *state = RequestState::Done;
        }
    }
}

/// What the reader thread is asked to do, and where to answer
enum Job {
    Read {
        offset: usize,
        length: usize,
        reply: Sender<Result<Vec<u8>, CdromError>>,
    },
    CheckMedia {
        known: MediaId,
        reply: Sender<Result<MediaCheck, CdromError>>,
    },
}

/// The thread and what it reads, kept for cancelling an access. Holding the
/// `JoinHandle` keeps the thread's OS handle open.
struct ReaderThread {
    thread: JoinHandle<()>,
    reader: Arc<dyn CdromReader>,
}

impl ReaderThread {
    fn cancel(&self) {
        self.reader.cancel(&self.thread);
    }
}

/// The answer to a request, once the thread gets to it. Dropping this
/// abandons the request.
pub(super) struct Reply<T> {
    answer: Receiver<Result<T, CdromError>>,
    request: Arc<Request>,
    thread: Arc<ReaderThread>,
}

impl<T> Reply<T> {
    /// Returns None while the request is waiting or running
    pub(super) fn poll(&self) -> Option<Result<T, CdromError>> {
        match self.answer.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            // The thread is gone without answering: it panicked
            Err(TryRecvError::Disconnected) => Some(Err(CdromError::BackendDead)),
        }
    }
}

impl<T> Drop for Reply<T> {
    fn drop(&mut self) {
        let mut state = self.request.lock();
        if *state == RequestState::Running {
            // The thread can't finish this request and start the next one
            // while the lock is held, so this only cancels this request.
            self.thread.cancel();
        }
        *state = RequestState::Abandoned;
    }
}

/// The reader thread of a disc. Dropping this lets the thread end once it is
/// done with the request it is on, without waiting for it.
pub(super) struct DiscThread {
    requests: Sender<(Job, Arc<Request>)>,
    thread: Arc<ReaderThread>,
}

impl DiscThread {
    pub(super) fn spawn(reader: Arc<dyn CdromReader>) -> std::io::Result<Self> {
        let (requests, queue) = crossbeam_channel::unbounded();
        let thread_reader = reader.clone();
        let thread = thread::Builder::new()
            .name("CD-ROM reader".to_string())
            .spawn(move || Self::run(&*thread_reader, &queue))?;
        Ok(Self {
            requests,
            thread: Arc::new(ReaderThread { thread, reader }),
        })
    }

    fn run(reader: &dyn CdromReader, queue: &Receiver<(Job, Arc<Request>)>) {
        for (job, request) in queue {
            if !request.start() {
                continue;
            }
            // A send fails when the request was abandoned meanwhile
            match job {
                Job::Read {
                    offset,
                    length,
                    reply,
                } => {
                    let result = reader.read_at(offset, length);
                    request.finish();
                    let _ = reply.send(result);
                }
                Job::CheckMedia { known, reply } => {
                    let result = reader.check_media(known);
                    request.finish();
                    let _ = reply.send(result);
                }
            }
        }
    }

    fn queue<T>(&self, job: impl FnOnce(Sender<Result<T, CdromError>>) -> Job) -> Reply<T> {
        let (reply, answer) = crossbeam_channel::bounded(1);
        let request = Arc::new(Request(Mutex::new(RequestState::Queued)));
        // A send fails when the thread is gone, which then shows in the reply
        let _ = self.requests.send((job(reply), request.clone()));
        Reply {
            answer,
            request,
            thread: self.thread.clone(),
        }
    }

    /// Queues a read of user data
    pub(super) fn read(&self, offset: usize, length: usize) -> Reply<Vec<u8>> {
        self.queue(|reply| Job::Read {
            offset,
            length,
            reply,
        })
    }

    /// Queues a check of the disc in the drive, against the one `known`
    /// identifies
    pub(super) fn check_media(&self, known: MediaId) -> Reply<MediaCheck> {
        self.queue(|reply| Job::CheckMedia { known, reply })
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

        fn cancel(&self, thread: &JoinHandle<()>) {
            assert_eq!(thread.thread().name(), Some("CD-ROM reader"));
            self.cancels.fetch_add(1, Ordering::AcqRel);
            self.cancelled.store(true, Ordering::Release);
        }
    }

    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !cond() {
            assert!(start.elapsed() < Duration::from_secs(5), "{}", what);
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn abandoning_a_running_read_cancels_it() {
        let reader = Arc::new(GatedReader::default());
        let thread = DiscThread::spawn(reader.clone()).unwrap();

        let running = thread.read(1, 16);
        wait_for("the read never started", || {
            reader.reads.load(Ordering::Acquire) == 1
        });
        let queued = thread.read(2, 16);

        // A request still in the queue is skipped without a cancel
        drop(queued);
        assert_eq!(reader.cancels.load(Ordering::Acquire), 0);

        drop(running);
        assert_eq!(reader.cancels.load(Ordering::Acquire), 1);

        // The thread goes on with the next request
        reader.open.store(true, Ordering::Release);
        let next = thread.read(3, 16);
        wait_for("the next read never came", || next.poll().is_some());
        assert_eq!(reader.reads.load(Ordering::Acquire), 2);
    }

    #[test]
    fn dropping_a_finished_reply_cancels_nothing() {
        let reader = Arc::new(GatedReader::default());
        reader.open.store(true, Ordering::Release);
        let thread = DiscThread::spawn(reader.clone()).unwrap();

        let reply = thread.read(1, 16);
        let mut answer = None;
        wait_for("the read never came", || {
            answer = reply.poll();
            answer.is_some()
        });
        drop(reply);
        assert_eq!(reader.cancels.load(Ordering::Acquire), 0);
    }
}
