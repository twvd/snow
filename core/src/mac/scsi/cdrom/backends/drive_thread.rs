//! A thread that does all the I/O of a physical CD-ROM drive
//!
//! A physical drive can take seconds to answer while its disc spins up, so its
//! I/O happens on a thread of its own, opening the drive included. A CD-ROM
//! backend for a physical drive owns a `DriveThread` and sends it requests,
//! which the thread serves one at a time, in order.
//!
//! This module is platform-independent. A platform provides the drive itself
//! by implementing `PhysicalCdromDrive`, and gets its `CdromBackend` from
//! `PhysicalDriveCdromBackend`.

use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, TryRecvError};

use crate::mac::scsi::cdrom::{
    CdromBackend, CdromError, DiscInfo, MediaCheck, RawSector, SessionInfo, Started, TrackInfo,
};
use crate::mac::scsi::{ASC_LOGICAL_UNIT_BECOMING_READY, CC_KEY_NOT_READY};

/// The blocking operations of a physical CD-ROM drive, which a platform
/// implements
///
/// Every method is only called on the drive thread, and may block for as long
/// as the drive takes. Sector numbers are absolute: LBA 0 is sector 150
/// (00:02:00).
///
/// Errors follow what the target reports to the guest:
/// - no disc in the drive: `CheckCondition(CC_KEY_NOT_READY, ASC_MEDIUM_NOT_PRESENT)`;
/// - the device is gone, for instance unplugged: `BackendDead`, which ejects
///   the drive;
/// - anything unexpected: `Other`.
pub trait PhysicalCdromDrive: Send {
    /// What the drive needs to be opened, such as its path
    type Config: Send + 'static;

    /// Opens the drive and loads the information of the disc in it. An error
    /// is reported to the user as the load failing.
    fn open(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    /// Checks whether a disc is still in the drive. If the disc changed since
    /// the last check (a drive usually counts media changes), reloads the disc
    /// information, which is None for an empty drive. Returns `Ok(())` when a
    /// disc is in. See `CdromBackend::check_media`.
    fn check_media(&mut self) -> Result<(), CdromError>;

    /// The information of the disc in the drive, None if there is no disc.
    /// Sessions and tracks use absolute sector numbers. `capacity` is the user
    /// data in bytes, 2048 per sector up to the lead-out, as
    /// `CdromBackend::byte_len`.
    fn disc_info(&self) -> Option<&DiscInfo>;

    /// Reads `length` bytes of user data at `offset`, counting 2048 bytes per
    /// sector from LBA 0. Reading an audio sector fails with
    /// `CheckCondition(CC_KEY_ILLEGAL_REQUEST, ASC_ILLEGAL_MODE_FOR_THIS_TRACK)`.
    /// See `CdromBackend::read_bytes`.
    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, CdromError>;

    /// Reads the raw CD Digital Audio sector at the absolute `sector`: 2352
    /// bytes of 16-bit little-endian stereo samples, and the sector's Q
    /// subchannel. A drive that can't read subchannel data can build one from
    /// the TOC with `QSub::new_mode1`. See `CdromBackend::read_cdda_sector`.
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
// TODO: we might want to cancel abandoned requests: skip one the thread
// hasn't started yet, and possibly cancel the I/O of the one in progress, so
// that a slow drive doesn't hold up the requests after it.
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
/// Each `start_*` method sends a request to the thread and returns its
/// `Reply` without waiting. Dropping this lets the thread end once it has
/// served the requests already sent, without waiting for it.
pub struct DriveThread {
    /// Requests to the drive thread
    requests: Sender<Request>,

    /// The disc information, as of the last media check
    disc_info: Option<DiscInfo>,

    /// How opening the drive went
    open: OpenState,
}

/// How opening the drive went
enum OpenState {
    /// The thread is opening the drive
    Opening(Reply<Option<DiscInfo>>),
    /// Opening the drive (loading it) failed with this error, which hasn't
    /// been taken yet
    Failed(anyhow::Error),
    /// The drive is open, or opening failed and the error was taken
    Done,
}

impl DriveThread {
    /// Starts the drive thread, which opens a drive of type `D` in the
    /// background. Until the drive is open, media checks report that the
    /// drive is becoming ready.
    pub fn spawn<D: PhysicalCdromDrive + 'static>(config: D::Config) -> Result<Self> {
        let (requests, request_queue) = crossbeam_channel::unbounded();
        let (opened_sender, opened) = crossbeam_channel::bounded(1);

        thread::Builder::new()
            .name("CD-ROM drive".to_string())
            .spawn(move || {
                let mut drive = match D::open(config) {
                    Ok(drive) => drive,
                    Err(e) => {
                        let _ = opened_sender.send(Err(CdromError::Other(e)));
                        return;
                    }
                };
                let _ = opened_sender.send(Ok(drive.disc_info().cloned()));
                run(&mut drive, &request_queue);
            })?;

        Ok(Self {
            requests,
            disc_info: None,
            open: OpenState::Opening(Reply(opened)),
        })
    }

    /// Whether the thread is still opening the drive, as of the last check
    pub fn is_opening(&self) -> bool {
        matches!(self.open, OpenState::Opening(_))
    }

    /// Checks whether the thread has opened the drive. Never blocks.
    fn poll_open(&mut self) {
        let OpenState::Opening(reply) = &self.open else {
            return;
        };
        let Some(result) = reply.poll() else {
            return;
        };
        self.open = match result {
            Ok(disc_info) => {
                self.disc_info = disc_info;
                OpenState::Done
            }
            Err(CdromError::Other(e)) => OpenState::Failed(e),
            Err(_) => OpenState::Failed(anyhow!(
                "The CD-ROM drive thread ended while opening the drive"
            )),
        };
    }

    /// The error loading the drive (opening it) failed with, once. After it,
    /// the drive is dead. Never blocks.
    pub fn take_load_error(&mut self) -> Option<anyhow::Error> {
        self.poll_open();
        match std::mem::replace(&mut self.open, OpenState::Done) {
            OpenState::Failed(e) => Some(e),
            other => {
                self.open = other;
                None
            }
        }
    }

    /// Sends a request. The thread being gone means that the drive is
    /// unusable.
    fn send(&self, request: Request) -> Result<(), CdromError> {
        self.requests
            .send(request)
            .map_err(|_| CdromError::BackendDead)
    }

    /// Starts a media check, without waiting for it. Its outcome goes to
    /// `apply_media_check`. While the drive is still opening, the check
    /// reports at once that the drive is becoming ready.
    pub fn start_check_media(&mut self) -> Reply<MediaCheck> {
        let (reply, reply_receiver) = crossbeam_channel::bounded(1);
        self.poll_open();
        if self.is_opening() {
            // A drive answers this way while it spins up
            let _ = reply.send(Ok(MediaCheck {
                result: Err(CdromError::CheckCondition(
                    CC_KEY_NOT_READY,
                    ASC_LOGICAL_UNIT_BECOMING_READY,
                )),
                disc_info: None,
            }));
            return Reply(reply_receiver);
        }
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

/// A CD-ROM backend for a physical drive, whose I/O happens on a `DriveThread`
pub struct PhysicalDriveCdromBackend {
    path: PathBuf,
    thread: DriveThread,
}

impl PhysicalDriveCdromBackend {
    /// Starts the drive thread for a drive of type `D`, opened with `config`.
    /// `path` is the drive's path, as reported by `image_path`.
    pub fn new<D: PhysicalCdromDrive + 'static>(path: &Path, config: D::Config) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            thread: DriveThread::spawn::<D>(config)?,
        })
    }
}

impl CdromBackend for PhysicalDriveCdromBackend {
    fn check_media(&mut self) -> Started<MediaCheck> {
        Started::Pending(self.thread.start_check_media())
    }

    fn apply_media_check(&mut self, check: MediaCheck) -> Result<(), CdromError> {
        self.thread.apply_media_check(check)
    }

    fn is_loading(&self) -> bool {
        self.thread.is_opening()
    }

    fn take_load_error(&mut self) -> Option<anyhow::Error> {
        self.thread.take_load_error()
    }

    fn byte_len(&self) -> usize {
        self.thread
            .disc_info()
            .map(|info| info.capacity)
            .unwrap_or(0)
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Started<Vec<u8>> {
        Started::Pending(self.thread.start_read_bytes(offset, length))
    }

    fn image_path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn sessions(&self) -> Option<&[SessionInfo]> {
        Some(&self.thread.disc_info()?.sessions)
    }

    fn tracks(&self) -> Option<&[TrackInfo]> {
        Some(&self.thread.disc_info()?.tracks)
    }

    fn read_cdda_sector(&self, sector: u32) -> Started<RawSector> {
        Started::Pending(self.thread.start_read_cdda_sector(sector))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::scsi::cdrom::{LBA_START_SECTOR, QSub, RAW_SECTOR_LEN};
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
        /// Opening waits for a message on this before it goes ahead
        open_gate: Option<Receiver<()>>,
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
            let gate = state.lock().unwrap().open_gate.clone();
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
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

    /// A drive thread for a fake drive holding a disc of 4 KB, once the drive
    /// is open
    fn drive_thread() -> (DriveThread, Arc<Mutex<FakeState>>) {
        let state = drive_with_disc();
        let mut thread = DriveThread::spawn::<FakeDrive>(state.clone()).unwrap();
        if let Some(e) = wait_for_open(&mut thread) {
            panic!("opening failed: {:#}", e);
        }
        (thread, state)
    }

    /// Waits for the drive thread to open the drive. Returns the error if
    /// opening failed.
    fn wait_for_open(thread: &mut DriveThread) -> Option<anyhow::Error> {
        for _ in 0..1000 {
            let error = thread.take_load_error();
            if error.is_some() || !thread.is_opening() {
                return error;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the drive did not open");
    }

    /// A media check, waited for and applied
    fn check_media(thread: &mut DriveThread) -> Result<(), CdromError> {
        let check = thread.start_check_media().wait()?;
        thread.apply_media_check(check)
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
    fn load_error_is_reported_once() {
        let state = drive_with_disc();
        state.lock().unwrap().fail_open = true;
        let mut thread = DriveThread::spawn::<FakeDrive>(state).unwrap();
        let error = wait_for_open(&mut thread).expect("opening should fail");
        assert_eq!(error.to_string(), "no drive");
        assert!(thread.take_load_error().is_none());

        // The drive is dead
        assert!(matches!(
            thread.start_read_bytes(0, 1).wait(),
            Err(CdromError::BackendDead)
        ));
        assert!(matches!(
            check_media(&mut thread),
            Err(CdromError::BackendDead)
        ));
    }

    #[test]
    fn media_check_while_opening_reports_becoming_ready() {
        let state = drive_with_disc();
        let (gate, gate_receiver) = crossbeam_channel::unbounded();
        state.lock().unwrap().open_gate = Some(gate_receiver);

        // spawn returns while the drive is opening
        let mut thread = DriveThread::spawn::<FakeDrive>(state).unwrap();
        assert!(thread.is_opening());
        assert!(thread.disc_info().is_none());

        // Answered at once
        let Some(Ok(check)) = thread.start_check_media().poll() else {
            panic!("the media check should be answered at once");
        };
        assert!(matches!(
            thread.apply_media_check(check),
            Err(CdromError::CheckCondition(
                CC_KEY_NOT_READY,
                ASC_LOGICAL_UNIT_BECOMING_READY
            ))
        ));

        gate.send(()).unwrap();
        assert!(wait_for_open(&mut thread).is_none());
        assert_eq!(thread.disc_info().unwrap().capacity, 4096);
        assert!(check_media(&mut thread).is_ok());
    }

    #[test]
    fn reads_go_through_the_thread() {
        let (thread, _) = drive_thread();
        let Ok(data) = thread.start_read_bytes(0x100, 4).wait() else {
            panic!("read failed");
        };
        assert_eq!(data, [0, 1, 2, 3]);

        let Ok(sector) = thread.start_read_cdda_sector(7).wait() else {
            panic!("CDDA read failed");
        };
        assert!(sector.data.iter().all(|b| *b == 7));
    }

    #[test]
    fn media_check_updates_the_disc_info() {
        let (mut thread, state) = drive_thread();

        state.lock().unwrap().disc = None;
        assert!(matches!(
            check_media(&mut thread),
            Err(CdromError::CheckCondition(
                CC_KEY_NOT_READY,
                ASC_MEDIUM_NOT_PRESENT
            ))
        ));
        assert!(thread.disc_info().is_none());

        state.lock().unwrap().disc = Some(vec![0; 2048]);
        assert!(check_media(&mut thread).is_ok());
        assert_eq!(thread.disc_info().unwrap().capacity, 2048);
    }

    #[test]
    fn dead_thread_is_a_dead_drive() {
        let (mut thread, state) = drive_thread();
        state.lock().unwrap().panic = true;
        assert!(matches!(
            thread.start_read_bytes(0, 1).wait(),
            Err(CdromError::BackendDead)
        ));
        assert!(matches!(
            check_media(&mut thread),
            Err(CdromError::BackendDead)
        ));
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

    #[test]
    fn backend_is_loading_while_the_drive_opens() {
        let state = drive_with_disc();
        let (gate, gate_receiver) = crossbeam_channel::unbounded();
        state.lock().unwrap().open_gate = Some(gate_receiver);
        let mut backend =
            PhysicalDriveCdromBackend::new::<FakeDrive>(Path::new("fake"), state).unwrap();
        assert!(backend.is_loading());

        gate.send(()).unwrap();
        assert!(wait_for_open(&mut backend.thread).is_none());
        assert!(!backend.is_loading());
    }

    #[test]
    fn physical_drive_backend_goes_through_the_thread() {
        let mut backend =
            PhysicalDriveCdromBackend::new::<FakeDrive>(Path::new("fake"), drive_with_disc())
                .unwrap();
        assert_eq!(backend.image_path(), Some(Path::new("fake")));

        // Check the media until the drive is open
        let mut result = None;
        for _ in 0..1000 {
            let Started::Pending(reply) = backend.check_media() else {
                panic!("the media check should go to the drive thread");
            };
            let Ok(check) = reply.wait() else {
                panic!("the drive thread is gone");
            };
            match backend.apply_media_check(check) {
                Err(CdromError::CheckCondition(_, ASC_LOGICAL_UNIT_BECOMING_READY)) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                other => {
                    result = Some(other);
                    break;
                }
            }
        }
        assert!(matches!(result, Some(Ok(()))));
        assert_eq!(backend.byte_len(), 4096);
        assert_eq!(backend.tracks().unwrap().len(), 1);
        assert_eq!(backend.sessions().unwrap().len(), 1);

        let Started::Pending(reply) = backend.read_bytes(0x100, 4) else {
            panic!("the read should go to the drive thread");
        };
        let Ok(data) = reply.wait() else {
            panic!("read failed");
        };
        assert_eq!(data, [0, 1, 2, 3]);
    }
}
