//! A thread that does all the I/O of a physical CD-ROM drive
//!
//! A physical drive can take seconds to answer while its disc spins up, so its
//! I/O happens on a thread of its own. A CD-ROM backend for a physical drive
//! owns a `DriveThread` and sends it requests, which the thread serves one at a
//! time, in order.
//!
//! This module is platform-independent. A platform provides the drive itself
//! by implementing `PhysicalCdromDrive`.

use std::thread;

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, TryRecvError};

use crate::mac::scsi::cdrom::{CdromError, DiscInfo, MediaCheck, RawSector};

/// The blocking operations of a physical CD-ROM drive. Only called on the
/// drive thread.
pub trait PhysicalCdromDrive: Send {
    /// What the drive needs to be opened, such as its path
    type Config: Send + 'static;

    /// Opens the drive and loads the disc information
    fn open(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    /// Checks whether a disc is still in the drive, and reloads the disc
    /// information if it changed. See `CdromBackend::check_media`.
    fn check_media(&mut self) -> Result<(), CdromError>;

    /// The information of the disc in the drive, None if there is no disc
    fn disc_info(&self) -> Option<&DiscInfo>;

    /// Reads user data. See `CdromBackend::read_bytes`.
    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError>;

    /// Reads a raw CD Digital Audio sector. See `CdromBackend::read_cdda_sector`.
    fn read_cdda_sector(&self, sector: u32) -> Result<RawSector, CdromError>;
}

/// A request to the drive thread, with the channel to send its reply on
enum Request {
    CheckMedia {
        reply: Sender<Result<MediaCheck, CdromError>>,
    },
    ReadBytes {
        offset: usize,
        length: usize,
        reply: Sender<Result<Vec<u8>, CdromError>>,
    },
    ReadCdda {
        sector: u32,
        reply: Sender<Result<RawSector, CdromError>>,
    },
}

/// Serves requests until the `DriveThread` is dropped
fn run(drive: &mut impl PhysicalCdromDrive, requests: &Receiver<Request>) {
    // A failed send means that the requester has stopped waiting, so the
    // reply is dropped.
    for request in requests {
        match request {
            Request::CheckMedia { reply } => {
                let result = drive.check_media();
                let _ = reply.send(Ok(MediaCheck {
                    result,
                    disc_info: drive.disc_info().cloned(),
                }));
            }
            Request::ReadBytes {
                offset,
                length,
                reply,
            } => {
                let _ = reply.send(drive.read_bytes(offset, length));
            }
            Request::ReadCdda { sector, reply } => {
                let _ = reply.send(drive.read_cdda_sector(sector));
            }
        }
    }
}

/// The reply to a request the drive thread is working on
///
/// Dropping this abandons the request: the thread still does the I/O, and its
/// reply is thrown away.
pub struct Reply<T>(Receiver<Result<T, CdromError>>);

impl<T> Reply<T> {
    /// The reply if it has come, None if the thread is still working on the
    /// request. Never blocks.
    pub fn poll(&self) -> Option<Result<T, CdromError>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            // The thread is gone
            Err(TryRecvError::Disconnected) => Some(Err(CdromError::BackendDead)),
        }
    }

    /// Waits for the reply
    pub fn wait(self) -> Result<T, CdromError> {
        self.0.recv().map_err(|_| CdromError::BackendDead)?
    }

    /// A reply the test sends itself
    #[cfg(test)]
    pub(crate) fn channel() -> (Sender<Result<T, CdromError>>, Self) {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        (sender, Self(receiver))
    }
}

/// The thread of a physical CD-ROM drive, as seen from the emulator
///
/// Each method sends a request to the thread and waits for its reply, unless
/// it returns a `Reply` to poll. Dropping
/// this lets the thread end once its current I/O is over, without waiting for
/// it.
pub struct DriveThread {
    /// Requests to the drive thread
    requests: Sender<Request>,

    /// The disc information, as of the last media check
    disc_info: Option<DiscInfo>,
}

impl DriveThread {
    /// Starts the drive thread, which opens a drive of type `D`, and waits for
    /// the drive to be open.
    pub fn spawn<D: PhysicalCdromDrive + 'static>(config: D::Config) -> Result<Self> {
        let (requests, request_queue) = crossbeam_channel::unbounded();
        let (opened_sender, opened) = crossbeam_channel::bounded(1);

        thread::Builder::new()
            .name("CD-ROM drive".to_string())
            .spawn(move || {
                let mut drive = match D::open(config) {
                    Ok(drive) => drive,
                    Err(e) => {
                        let _ = opened_sender.send(Err(e));
                        return;
                    }
                };
                let _ = opened_sender.send(Ok(drive.disc_info().cloned()));
                run(&mut drive, &request_queue);
            })?;

        let disc_info = opened
            .recv()
            .map_err(|_| anyhow!("The CD-ROM drive thread ended while opening the drive"))??;

        Ok(Self {
            requests,
            disc_info,
        })
    }

    /// Sends a request. The thread being gone means that the drive is
    /// unusable.
    fn send(&self, request: Request) -> Result<(), CdromError> {
        self.requests
            .send(request)
            .map_err(|_| CdromError::BackendDead)
    }

    /// See `PhysicalCdromDrive::check_media`. Updates the disc information.
    pub fn check_media(&mut self) -> Result<(), CdromError> {
        let check = self.start_check_media().wait()?;
        self.apply_media_check(check)
    }

    /// Starts a media check, without waiting for it. Its outcome goes to
    /// `apply_media_check`.
    pub fn start_check_media(&self) -> Reply<MediaCheck> {
        let (reply, reply_receiver) = crossbeam_channel::bounded(1);
        // If the thread is gone, the reply's sender is dropped with the
        // request, and the reply reports a dead drive
        let _ = self.send(Request::CheckMedia { reply });
        Reply(reply_receiver)
    }

    /// Takes in the outcome of a media check: updates the disc information,
    /// and returns the result of the check
    pub fn apply_media_check(&mut self, check: MediaCheck) -> Result<(), CdromError> {
        self.disc_info = check.disc_info;
        check.result
    }

    /// The disc information, as of the last media check
    pub fn disc_info(&self) -> Option<&DiscInfo> {
        self.disc_info.as_ref()
    }

    /// See `PhysicalCdromDrive::read_bytes`
    pub fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError> {
        self.start_read_bytes(offset, length).wait()
    }

    /// Starts reading user data, without waiting for it. See
    /// `PhysicalCdromDrive::read_bytes`.
    pub fn start_read_bytes(&self, offset: usize, length: usize) -> Reply<Vec<u8>> {
        let (reply, reply_receiver) = crossbeam_channel::bounded(1);
        // If the thread is gone, the reply's sender is dropped with the
        // request, and the reply reports a dead drive
        let _ = self.send(Request::ReadBytes {
            offset,
            length,
            reply,
        });
        Reply(reply_receiver)
    }

    /// See `PhysicalCdromDrive::read_cdda_sector`
    pub fn read_cdda_sector(&self, sector: u32) -> Result<RawSector, CdromError> {
        self.start_read_cdda_sector(sector).wait()
    }

    /// Starts reading a raw CD Digital Audio sector, without waiting for it.
    /// See `PhysicalCdromDrive::read_cdda_sector`.
    pub fn start_read_cdda_sector(&self, sector: u32) -> Reply<RawSector> {
        let (reply, reply_receiver) = crossbeam_channel::bounded(1);
        // If the thread is gone, the reply's sender is dropped with the
        // request, and the reply reports a dead drive
        let _ = self.send(Request::ReadCdda { sector, reply });
        Reply(reply_receiver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::scsi::cdrom::{LBA_START_SECTOR, QSub, RAW_SECTOR_LEN, SessionInfo, TrackInfo};
    use crate::mac::scsi::{ASC_MEDIUM_NOT_PRESENT, CC_KEY_NOT_READY};
    use std::sync::{Arc, Mutex};

    /// What the fake drive holds, shared with the test
    #[derive(Default)]
    struct FakeState {
        /// The disc in the drive: its contents
        disc: Option<Vec<u8>>,
        /// Fail to open the drive
        fail_open: bool,
        /// The drive was dropped, the thread has ended
        dropped: bool,
        /// Panic on the next access, as if the thread died
        panic: bool,
        /// Reads wait for a message on this before they go ahead
        read_gate: Option<Receiver<()>>,
    }

    struct FakeDrive {
        state: Arc<Mutex<FakeState>>,
        disc_info: Option<DiscInfo>,
    }

    fn disc_info_for(disc: &[u8]) -> DiscInfo {
        DiscInfo {
            capacity: disc.len(),
            sessions: vec![SessionInfo {
                number: 1,
                disc_type: 0,
                start: 0,
                leadout: LBA_START_SECTOR + 100,
            }],
            tracks: vec![TrackInfo {
                tno: 1,
                session: 1,
                control: 4,
                sector: LBA_START_SECTOR,
            }],
        }
    }

    impl FakeDrive {
        fn check_panic(&self) {
            if self.state.lock().unwrap().panic {
                panic!("fake drive thread dies");
            }
        }
    }

    impl Drop for FakeDrive {
        fn drop(&mut self) {
            if let Ok(mut state) = self.state.lock() {
                state.dropped = true;
            }
        }
    }

    impl PhysicalCdromDrive for FakeDrive {
        type Config = Arc<Mutex<FakeState>>;

        fn open(state: Self::Config) -> Result<Self> {
            let guard = state.lock().unwrap();
            if guard.fail_open {
                return Err(anyhow!("no drive"));
            }
            let disc_info = guard.disc.as_deref().map(disc_info_for);
            drop(guard);
            Ok(Self { state, disc_info })
        }

        fn check_media(&mut self) -> Result<(), CdromError> {
            self.check_panic();
            let disc = self.state.lock().unwrap().disc.clone();
            self.disc_info = disc.as_deref().map(disc_info_for);
            if self.disc_info.is_some() {
                Ok(())
            } else {
                Err(CdromError::CheckCondition(
                    CC_KEY_NOT_READY,
                    ASC_MEDIUM_NOT_PRESENT,
                ))
            }
        }

        fn disc_info(&self) -> Option<&DiscInfo> {
            self.disc_info.as_ref()
        }

        fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError> {
            let gate = self.state.lock().unwrap().read_gate.clone();
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            self.check_panic();
            self.state
                .lock()
                .unwrap()
                .disc
                .as_ref()
                .map(|disc| disc[offset..offset + length].to_vec())
                .ok_or(CdromError::CheckCondition(
                    CC_KEY_NOT_READY,
                    ASC_MEDIUM_NOT_PRESENT,
                ))
        }

        fn read_cdda_sector(&self, sector: u32) -> Result<RawSector, CdromError> {
            self.check_panic();
            Ok(RawSector {
                data: [sector as u8; RAW_SECTOR_LEN],
                qsub: QSub::default(),
            })
        }
    }

    /// The state of a fake drive holding a disc of 4 KB
    fn drive_with_disc() -> Arc<Mutex<FakeState>> {
        Arc::new(Mutex::new(FakeState {
            disc: Some((0..4096).map(|i| i as u8).collect()),
            ..Default::default()
        }))
    }

    /// A drive thread for a fake drive holding a disc of 4 KB
    fn drive_thread() -> (DriveThread, Arc<Mutex<FakeState>>) {
        let state = drive_with_disc();
        let thread = DriveThread::spawn::<FakeDrive>(state.clone()).unwrap();
        (thread, state)
    }

    /// Waits for the drive thread to drop its drive
    fn wait_for_drop(state: &Mutex<FakeState>) {
        for _ in 0..1000 {
            if state.lock().unwrap().dropped {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the drive thread did not end");
    }

    #[test]
    fn open_reports_the_disc() {
        let (thread, _) = drive_thread();
        let info = thread.disc_info().unwrap();
        assert_eq!(info.capacity, 4096);
        assert_eq!(info.tracks.len(), 1);
        assert_eq!(info.sessions.len(), 1);
    }

    #[test]
    fn open_error_is_returned() {
        let state = drive_with_disc();
        state.lock().unwrap().fail_open = true;
        let result = DriveThread::spawn::<FakeDrive>(state);
        assert_eq!(result.err().unwrap().to_string(), "no drive");
    }

    #[test]
    fn reads_go_through_the_thread() {
        let (thread, _) = drive_thread();
        let Ok(data) = thread.read_bytes(0x100, 4) else {
            panic!("read failed");
        };
        assert_eq!(data, [0, 1, 2, 3]);

        let Ok(sector) = thread.read_cdda_sector(7) else {
            panic!("CDDA read failed");
        };
        assert!(sector.data.iter().all(|b| *b == 7));
    }

    #[test]
    fn media_check_updates_the_disc_info() {
        let (mut thread, state) = drive_thread();

        state.lock().unwrap().disc = None;
        assert!(matches!(
            thread.check_media(),
            Err(CdromError::CheckCondition(
                CC_KEY_NOT_READY,
                ASC_MEDIUM_NOT_PRESENT
            ))
        ));
        assert!(thread.disc_info().is_none());

        state.lock().unwrap().disc = Some(vec![0; 2048]);
        assert!(thread.check_media().is_ok());
        assert_eq!(thread.disc_info().unwrap().capacity, 2048);
    }

    #[test]
    fn dead_thread_is_a_dead_drive() {
        let (mut thread, state) = drive_thread();
        state.lock().unwrap().panic = true;
        assert!(matches!(
            thread.read_bytes(0, 1),
            Err(CdromError::BackendDead)
        ));
        assert!(matches!(thread.check_media(), Err(CdromError::BackendDead)));
    }

    #[test]
    fn dropping_the_drive_thread_ends_the_thread() {
        let (thread, state) = drive_thread();
        drop(thread);
        wait_for_drop(&state);
    }

    /// Polls a reply until it comes
    fn poll_until_done<T>(reply: &Reply<T>) -> Result<T, CdromError> {
        for _ in 0..1000 {
            if let Some(result) = reply.poll() {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the reply did not come");
    }

    #[test]
    fn started_read_completes_in_the_background() {
        let (thread, state) = drive_thread();
        let (gate, gate_receiver) = crossbeam_channel::unbounded();
        state.lock().unwrap().read_gate = Some(gate_receiver);

        let reply = thread.start_read_bytes(0x100, 4);
        // The drive is stuck in the read
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(reply.poll().is_none());

        gate.send(()).unwrap();
        let Ok(data) = poll_until_done(&reply) else {
            panic!("read failed");
        };
        assert_eq!(data, [0, 1, 2, 3]);
    }

    #[test]
    fn abandoned_read_leaves_the_next_one_alone() {
        let (thread, state) = drive_thread();
        let (gate, gate_receiver) = crossbeam_channel::unbounded();
        state.lock().unwrap().read_gate = Some(gate_receiver);

        drop(thread.start_read_bytes(0, 4));
        let reply = thread.start_read_bytes(0x100, 4);
        gate.send(()).unwrap();
        gate.send(()).unwrap();
        let Ok(data) = poll_until_done(&reply) else {
            panic!("read failed");
        };
        assert_eq!(data, [0, 1, 2, 3]);
    }

    #[test]
    fn started_read_on_a_dead_thread_is_a_dead_drive() {
        let (thread, state) = drive_thread();
        state.lock().unwrap().panic = true;
        let reply = thread.start_read_bytes(0, 1);
        assert!(matches!(
            poll_until_done(&reply),
            Err(CdromError::BackendDead)
        ));

        // The thread is gone (or about to be): a new read fails too
        let reply = thread.start_read_bytes(0, 1);
        assert!(matches!(
            poll_until_done(&reply),
            Err(CdromError::BackendDead)
        ));
    }

    #[test]
    fn started_media_check_updates_the_disc_info() {
        let (mut thread, state) = drive_thread();

        state.lock().unwrap().disc = None;
        let reply = thread.start_check_media();
        let Ok(check) = poll_until_done(&reply) else {
            panic!("media check failed");
        };
        assert!(matches!(
            thread.apply_media_check(check),
            Err(CdromError::CheckCondition(
                CC_KEY_NOT_READY,
                ASC_MEDIUM_NOT_PRESENT
            ))
        ));
        assert!(thread.disc_info().is_none());

        state.lock().unwrap().disc = Some(vec![0; 2048]);
        let Ok(check) = thread.start_check_media().wait() else {
            panic!("media check failed");
        };
        assert!(thread.apply_media_check(check).is_ok());
        assert_eq!(thread.disc_info().unwrap().capacity, 2048);
    }

    #[test]
    fn started_cdda_read_completes_in_the_background() {
        let (thread, _) = drive_thread();
        let reply = thread.start_read_cdda_sector(9);
        let Ok(sector) = poll_until_done(&reply) else {
            panic!("CDDA read failed");
        };
        assert!(sector.data.iter().all(|b| *b == 9));
    }
}
