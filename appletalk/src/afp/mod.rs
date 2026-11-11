//! AppleTalk Filing Protocol (AFP) 1.1 to 2.2, server side
//!
//! See Inside AppleTalk, 2nd edition, chapter 13, and the AFP 2.1
//! specification.
//!
//! Serves one volume, a host directory, to guests: the server information
//! block (served as the ASP GetStatus status block), login with guest
//! authentication, file and directory parameters, enumeration, forks and
//! a desktop database. Read-only by default. Host file names are converted
//! between UTF-8 and MacRoman.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::dtdb::{DT_REF_NUM, DeskBatch, DeskDb, scan_volume};
use crate::ids::{IdTable, ROOT_DIR_ID};
use crate::meta::Meta;
use crate::wire::wire_log;

pub use crate::dtdb::ScanStatus;

mod desktop;
mod fork;
mod params;
mod path;
mod tree;

use self::{params::*, path::*};

/// AFP commands
const CMD_BYTE_RANGE_LOCK: u8 = 1;
const CMD_CLOSE_VOL: u8 = 2;
const CMD_CLOSE_FORK: u8 = 4;
const CMD_ENUMERATE: u8 = 9;
const CMD_FLUSH: u8 = 10;
const CMD_FLUSH_FORK: u8 = 11;
const CMD_GET_FORK_PARMS: u8 = 14;
const CMD_GET_SRVR_PARMS: u8 = 16;
const CMD_GET_VOL_PARMS: u8 = 17;
const CMD_LOGIN: u8 = 18;
const CMD_LOGOUT: u8 = 20;
const CMD_OPEN_VOL: u8 = 24;
const CMD_OPEN_FORK: u8 = 26;
const CMD_READ: u8 = 27;
const CMD_GET_FILE_DIR_PARMS: u8 = 34;
const CMD_OPEN_DT: u8 = 48;
const CMD_CLOSE_DT: u8 = 49;
const CMD_GET_ICON: u8 = 51;
const CMD_GET_ICON_INFO: u8 = 52;
const CMD_ADD_APPL: u8 = 53;
const CMD_RMV_APPL: u8 = 54;
const CMD_GET_APPL: u8 = 55;
const CMD_ADD_CMT: u8 = 56;
const CMD_RMV_CMT: u8 = 57;
const CMD_GET_CMT: u8 = 58;
const CMD_ADD_ICON: u8 = 192;
const CMD_CREATE_DIR: u8 = 6;
const CMD_CREATE_FILE: u8 = 7;
const CMD_DELETE: u8 = 8;
const CMD_MOVE: u8 = 23;
const CMD_RENAME: u8 = 28;
const CMD_SET_DIR_PARMS: u8 = 29;
const CMD_SET_FILE_PARMS: u8 = 30;
const CMD_SET_FORK_PARMS: u8 = 31;
const CMD_SET_VOL_PARMS: u8 = 32;
const CMD_WRITE: u8 = 33;
const CMD_SET_FILE_DIR_PARMS: u8 = 35;

/// AFP error codes (afpdocs reference.md "Result Codes")
const ERR_ACCESS_DENIED: i32 = -5000;
const ERR_BAD_UAM: i32 = -5002;
const ERR_BAD_VERS: i32 = -5003;
const ERR_BITMAP: i32 = -5004;
const ERR_EOF: i32 = -5009;
const ERR_OBJECT_NOT_FOUND: i32 = -5018;
const ERR_ITEM_NOT_FOUND: i32 = -5012;
const ERR_PARAM: i32 = -5019;
const ERR_LOCK: i32 = -5013;
const ERR_NO_MORE_LOCKS: i32 = -5015;
const ERR_RANGE_NOT_LOCKED: i32 = -5020;
const ERR_RANGE_OVERLAP: i32 = -5021;
const ERR_CALL_NOT_SUPPORTED: i32 = -5024;
const ERR_FILE_BUSY: i32 = -5010;
const ERR_OBJECT_LOCKED: i32 = -5032;
const ERR_OBJECT_TYPE: i32 = -5025;
const ERR_TOO_MANY_FILES_OPEN: i32 = -5026;
const ERR_DIR_NOT_FOUND: i32 = -5029;
const ERR_DIR_NOT_EMPTY: i32 = -5007;
const ERR_DISK_FULL: i32 = -5008;
const ERR_CANT_MOVE: i32 = -5005;
const ERR_OBJECT_EXISTS: i32 = -5017;
const ERR_VOL_LOCKED: i32 = -5031;

/// The identifier of the single volume
const VOLUME_ID: u16 = 1;
/// The identifier of the root directory's parent (the HFS convention)
const ROOT_PARENT_DIR_ID: u32 = 1;

/// The AFP protocol versions the server speaks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfpVersion {
    /// AFP 1.1: no reserved byte after the function in commands, a
    /// FileFlag byte in Enumerate, File/DirFlag values 0/1
    V11,
    /// AFP 2.0
    V20,
    /// AFP 2.1: handled on the same wire as 2.0
    V21,
    /// AFP 2.2 (without TCP): same wire plus the extended volume
    /// bitmap bits
    V22,
}

impl AfpVersion {
    pub fn from_string(s: &str) -> Option<Self> {
        match s {
            "AFPVersion 1.1" => Some(Self::V11),
            "AFPVersion 2.0" | "AFP2.0" => Some(Self::V20),
            "AFPVersion 2.1" | "AFP2.1" => Some(Self::V21),
            "AFP2.2" => Some(Self::V22),
            _ => None,
        }
    }

    pub fn string(&self) -> &'static str {
        match self {
            Self::V11 => "AFPVersion 1.1",
            Self::V20 => "AFPVersion 2.0",
            Self::V21 => "AFPVersion 2.1",
            Self::V22 => "AFP2.2",
        }
    }
}

/// The highest AFP version the server advertises and accepts; the
/// advertised list is every version up to the maximum, highest first
/// (clients pick the highest version they know)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MaxVersion {
    /// Advertise and accept AFP up to 1.1
    #[serde(alias = "V11Only")]
    Max11,
    /// Advertise and accept AFP up to 2.0
    #[serde(alias = "V20Only", alias = "Both20First", alias = "Both")]
    Max20,
    /// Advertise and accept AFP up to 2.1
    #[serde(alias = "Both21First", alias = "V21Only")]
    Max21,
    /// Advertise and accept AFP up to 2.2 (without TCP; the default:
    /// some Mac OS versions only request color icons over 2.2)
    #[default]
    Max22,
}

impl MaxVersion {
    /// The versions to advertise, highest first
    pub fn advertised(&self) -> &'static [AfpVersion] {
        match self {
            Self::Max11 => &[AfpVersion::V11],
            Self::Max20 => &[AfpVersion::V20, AfpVersion::V11],
            Self::Max21 => &[AfpVersion::V21, AfpVersion::V20, AfpVersion::V11],
            Self::Max22 => &[
                AfpVersion::V22,
                AfpVersion::V21,
                AfpVersion::V20,
                AfpVersion::V11,
            ],
        }
    }

    /// Whether a login with the given version is accepted
    pub fn accepts(&self, version: AfpVersion) -> bool {
        self.advertised().contains(&version)
    }
}

impl std::fmt::Display for MaxVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Max11 => write!(f, "1.1"),
            Self::Max20 => write!(f, "2.0"),
            Self::Max21 => write!(f, "2.1"),
            Self::Max22 => write!(f, "2.2"),
        }
    }
}

/// How hidden (dot-prefixed) host entries appear on the wire
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum HiddenFiles {
    /// Not addressable at all
    #[default]
    Hide,
    /// Addressable, with the Finder's invisible attribute set
    Invisible,
    /// Fully visible
    Show,
}

impl std::fmt::Display for HiddenFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hide => write!(f, "Hide"),
            Self::Invisible => write!(f, "Show as invisible"),
            Self::Show => write!(f, "Show"),
        }
    }
}

/// The advanced AFP server settings
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AfpSettings {
    /// The maximum directory depth of the desktop scan (a bind-mount
    /// loop cannot go deeper than this)
    pub scan_depth: u32,
    /// The maximum number of directory entries the scan visits (a
    /// time and memory bound; the depth limit is the loop defense)
    pub scan_entries_cap: u32,
    /// How hidden (dot-prefixed) host entries appear
    pub hidden_files: HiddenFiles,
    /// Whether the desktop scan follows symlinks that stay inside the
    /// share root (symlinks outside the root are never followed)
    pub follow_symlinks: bool,
    /// Whether the AppleDouble metadata containers are read (the ._
    /// sidecars and the .AppleDouble directories)
    pub probe_apple_double: bool,
    /// Whether the wire-level dumps print (a diagnostic; off by
    /// default, toggled from the advanced settings)
    #[serde(default)]
    pub wire_debug: bool,
    /// Whether the volume is read-only (writes return afpVolLocked
    /// and the volume advertises the read-only attribute)
    #[serde(default = "default_true")]
    pub read_only: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AfpSettings {
    fn default() -> Self {
        Self {
            scan_depth: 16,
            scan_entries_cap: 20_000_000,
            hidden_files: HiddenFiles::Hide,
            follow_symlinks: false,
            probe_apple_double: true,
            wire_debug: false,
            read_only: true,
        }
    }
}

/// The AFP error for a failed host operation, so a full disk shows as
/// one on the Mac rather than as a parameter error
fn io_err(e: &std::io::Error) -> i32 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => ERR_OBJECT_NOT_FOUND,
        PermissionDenied => ERR_ACCESS_DENIED,
        ReadOnlyFilesystem => ERR_VOL_LOCKED,
        StorageFull | QuotaExceeded => ERR_DISK_FULL,
        _ => ERR_PARAM,
    }
}

/// What a command answers: its reply data, or an AFP result code
type Reply = Result<Vec<u8>, i32>;

/// An open fork
struct Fork {
    file: File,
    /// Length of the fork
    len: u64,
    /// Offset of the fork data inside the host file (nonzero for
    /// AppleSingle containers and resource forks)
    offset: u64,
    /// The file's wire name and metadata, for FPGetForkParms
    name: String,
    meta: Meta,
    /// The Directory ID of the file's parent directory
    parent: u32,
    /// Current marker position (after the last FPRead)
    marker: u64,
    /// The host path of the file (for the write backends)
    path: PathBuf,
    /// The file's ID in the volume's identity table
    file_id: u32,
    /// Whether the fork was opened for reading
    read: bool,
    /// Whether the fork was opened for writing
    write: bool,
    /// Whether FPWrite data actually landed in this fork
    written: bool,
    /// Whether the client set the modification date while this fork
    /// was open (close must not stamp it with "now" then)
    date_set_by_client: bool,
    /// The host file the handle is open on: the file itself for a data
    /// fork, the metadata container for a resource fork or an AppleSingle
    /// file (an empty resource fork starts on the data file and moves into
    /// its container after the first write)
    container_path: PathBuf,
    /// The session that opened the fork (its close takes the fork)
    session: u8,
    /// Whether this is the resource fork (writes go through the
    /// metadata backend; the data fork writes through the file)
    rsrc: bool,
    /// The byte ranges locked through this fork (FPByteRangeLock), as
    /// start and end offsets; they go with the fork when it closes
    locks: Vec<(u64, u64)>,
}

/// AFP protocol module: the server
pub struct Afp {
    /// Canonicalized host directory exposed as the volume
    root: PathBuf,
    /// Volume name
    volume_name: String,
    /// The highest AFP version to advertise and accept
    max_version: MaxVersion,
    /// Open forks by reference number
    forks: HashMap<u16, Fork>,
    next_fork_ref: u16,
    /// The server-side volume modification stamp (every write command
    /// bumps it)
    vol_modified: u32,
    /// The response capacity of the request being served (the ATP
    /// requester's offered buffers in bytes): FPRead caps its read at
    /// it. MAX_RESPONSE until the ASP layer records the real value,
    /// so a direct command() call answers like a full window.
    req_capacity: usize,
    /// The volume's ID table: one ID per file and directory, keyed
    /// by the object's identity (see the ids module)
    ids: IdTable,
    /// The AFP version negotiated at login, per session
    versions: HashMap<u8, AfpVersion>,
    /// The advanced server settings
    settings: AfpSettings,
    /// The desktop database contents gathered so far: every APPL file
    /// whose parameters the server has served, with its bundle icons
    desktop: Option<DeskDb>,
    /// The background desktop scan's progress batches
    scan_rx: Option<std::sync::mpsc::Receiver<DeskBatch>>,
    /// The background scan thread's abort flag
    scan_abort: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Whether the background scan has been started (it runs once)
    scan_started: bool,
    /// Progress reports to the UI, if one is connected
    scan_status_tx: Option<crossbeam_channel::Sender<ScanStatus>>,
    /// The listing Enumerate is paging through
    enum_cache: Option<EnumCache>,
}

impl Drop for Afp {
    fn drop(&mut self) {
        // Stop the background scan thread when the server goes away
        if let Some(abort) = &self.scan_abort {
            abort.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Afp {
    /// Create a server sharing the given host directory as a volume, with
    /// the given volume name (a missing or empty one is "Snow") and
    /// maximum AFP version
    pub fn new_with(
        root: PathBuf,
        volume_name: Option<String>,
        max_version: MaxVersion,
        settings: AfpSettings,
    ) -> Self {
        let root = root.canonicalize().unwrap_or(root);
        Self {
            root,
            volume_name: volume_name
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Snow".to_string()),
            max_version,
            forks: HashMap::new(),
            next_fork_ref: 1,
            vol_modified: 0,
            req_capacity: crate::atp::MAX_RESPONSE,
            ids: IdTable::new(),
            versions: HashMap::new(),
            settings,
            desktop: None,
            scan_rx: None,
            scan_abort: None,
            scan_started: false,
            scan_status_tx: None,
            enum_cache: None,
        }
    }

    /// Create a server exposing the given host directory as a volume
    pub fn with_root(root: PathBuf) -> Self {
        Self::new_with(root, None, MaxVersion::default(), AfpSettings::default())
    }

    /// The volume's name, which is also the server's
    pub fn volume_name(&self) -> &str {
        &self.volume_name
    }

    /// The server information block, served as the ASP GetStatus block.
    /// The offsets point at the Machine Type, the AFP version list, the
    /// UAM list and the (absent) volume icon.
    pub fn server_info(&self) -> Vec<u8> {
        let mut block = Vec::new();
        // Offsets are filled in below, the fields follow the header
        block.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        block.extend_from_slice(&0u16.to_be_bytes()); // flags
        block.extend_from_slice(&pstring(&self.volume_name));

        let machine_type = block.len() as u16;
        block.extend_from_slice(&pstring("Macintosh"));

        let versions = block.len() as u16;
        let advertised = self.max_version.advertised();
        block.push(advertised.len() as u8);
        for version in advertised {
            block.extend_from_slice(&pstring(version.string()));
        }

        let uams = block.len() as u16;
        block.push(1);
        block.extend_from_slice(&pstring("No User Authent"));
        // "Cleartxt passwrd" is accepted at login (as a guest, the
        // server has no real authentication) but not advertised, so
        // the Chooser does not offer "Registered User"

        // Offsets: machine type, AFP versions, UAMs, volume icon (none)
        block[0..2].copy_from_slice(&machine_type.to_be_bytes());
        block[2..4].copy_from_slice(&versions.to_be_bytes());
        block[4..6].copy_from_slice(&uams.to_be_bytes());
        block
    }

    /// Handle an AFP command, returning the 4-byte command result and
    /// the reply data
    pub fn command(&mut self, session: u8, data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let version = self
            .versions
            .get(&session)
            .copied()
            .unwrap_or(AfpVersion::V20);
        // A 1.1 request is just the function byte for several commands
        if data.is_empty() {
            return (err(ERR_PARAM), Vec::new());
        }
        let cmd = data[0];
        // Merge any findings the background desktop scan has reported
        self.poll_scan();
        let reply = match cmd {
            // FPLogin and FPOpenFork have no reserved byte after the
            // function in either version
            CMD_LOGIN => self.cmd_login(session, data.get(1..).unwrap_or(&[])),
            CMD_OPEN_FORK => self.cmd_open_fork(version, session, data.get(1..).unwrap_or(&[])),
            // FPEnumerate in 1.1 carries a FileFlag byte where 2.0 has
            // the reserved byte
            CMD_ENUMERATE if version == AfpVersion::V11 => {
                let file_flag = data.get(1).copied();
                self.cmd_enumerate(version, data.get(2..).unwrap_or(&[]), file_flag)
            }
            CMD_ENUMERATE => self.cmd_enumerate(version, data.get(2..).unwrap_or(&[]), None),
            // FPGetSrvrParms and FPLogout carry no SRefNum: the
            // function byte stands alone in every AFP version (both
            // the 1.1 and the 2.1 clients send a bare byte)
            CMD_GET_SRVR_PARMS | CMD_LOGOUT => {
                let params = data.get(1..).unwrap_or(&[]);
                if cmd == CMD_GET_SRVR_PARMS {
                    self.cmd_get_srvr_parms(params)
                } else {
                    // Logout closes everything the session owns
                    self.close_session(session);
                    Ok(Vec::new())
                }
            }
            _ => {
                // Every other command has the reserved byte after the
                // function in both versions; a short command is a
                // parameter error, never a panic
                let params = data.get(2..).unwrap_or(&[]);
                match cmd {
                    CMD_OPEN_VOL => self.cmd_open_vol(version, params),
                    CMD_GET_VOL_PARMS => self.cmd_get_vol_parms(version, params),
                    CMD_CLOSE_VOL => Ok(Vec::new()),
                    CMD_GET_FILE_DIR_PARMS => self.cmd_get_file_dir_parms(version, params),
                    CMD_SET_FILE_DIR_PARMS => {
                        self.cmd_set_params(version, SetCall::FileDir, params)
                    }
                    CMD_SET_FILE_PARMS => self.cmd_set_params(version, SetCall::File, params),
                    CMD_SET_DIR_PARMS => self.cmd_set_params(version, SetCall::Dir, params),
                    // FPCreateFile's header byte is the CreateFlag
                    // (bit 7 = hard create); FPCreateDir's is the
                    // plain reserved byte
                    CMD_CREATE_FILE => {
                        let flag = data.get(1).copied().unwrap_or(0);
                        self.cmd_create(false, flag, params)
                    }
                    CMD_CREATE_DIR => self.cmd_create(true, 0, params),
                    CMD_DELETE => self.cmd_delete(params),
                    CMD_RENAME => self.cmd_rename(params),
                    CMD_MOVE => self.cmd_move_and_rename(params),
                    CMD_FLUSH | CMD_FLUSH_FORK => self.cmd_flush(cmd, params),
                    // FPRead's EOF comes with the data read before it
                    CMD_READ => match self.cmd_read(params) {
                        Ok((data, eof)) => return (if eof { err(ERR_EOF) } else { ok() }, data),
                        Err(code) => Err(code),
                    },
                    CMD_WRITE => {
                        // FPWrite's header byte is the Start/EndFlag
                        // (bit 7 = the offset is relative to the end
                        // of the fork)
                        let flag = data.get(1).copied().unwrap_or(0);
                        self.cmd_write(flag, params)
                    }
                    CMD_GET_FORK_PARMS => self.cmd_get_fork_parms(params),
                    CMD_BYTE_RANGE_LOCK => {
                        // FPByteRangeLock's header byte holds the
                        // Start/EndFlag (bit 7) and the UnlockFlag (bit 0)
                        let flags = data.get(1).copied().unwrap_or(0);
                        self.cmd_byte_range_lock(flags, params)
                    }
                    CMD_SET_FORK_PARMS => self.cmd_set_fork_params(params),
                    CMD_SET_VOL_PARMS => self.cmd_set_vol_params(params),
                    CMD_CLOSE_FORK => self.cmd_close_fork(params),
                    CMD_OPEN_DT => self.cmd_open_dt(params),
                    CMD_CLOSE_DT => self.cmd_close_dt(params),
                    CMD_GET_ICON => self.cmd_get_icon(params),
                    CMD_GET_ICON_INFO => self.cmd_get_icon_info(params),
                    CMD_GET_APPL => self.cmd_get_appl(params),
                    // The desktop database is synthesized server-side:
                    // the client's updates are acknowledged but not
                    // stored (its comments and icons are not persisted)
                    CMD_ADD_APPL | CMD_RMV_APPL | CMD_ADD_CMT | CMD_RMV_CMT | CMD_ADD_ICON => {
                        if self.settings.read_only {
                            Err(ERR_VOL_LOCKED)
                        } else {
                            Ok(Vec::new())
                        }
                    }
                    // Comments are not stored, so there never is one
                    CMD_GET_CMT => Err(ERR_ITEM_NOT_FOUND),
                    _ => {
                        wire_log!("AFP: dropped unsupported command {}", cmd);
                        Err(ERR_CALL_NOT_SUPPORTED)
                    }
                }
            }
        };
        match reply {
            Ok(data) => (ok(), data),
            Err(code) => (err(code), Vec::new()),
        }
    }

    /// FPLogin: accept an advertised AFP version with a guest UAM and
    /// record the negotiated version for the session
    fn cmd_login(&mut self, session: u8, params: &[u8]) -> Reply {
        let mut r = Reader::new(params);
        let version = r.pstring()?;
        let uam = r.pstring()?;
        let Some(version) = AfpVersion::from_string(&version) else {
            wire_log!("AFP: login with unknown version '{}'", version);
            return Err(ERR_BAD_VERS);
        };
        if !self.max_version.accepts(version) {
            wire_log!(
                "AFP: login with unadvertised version '{}'",
                version.string()
            );
            return Err(ERR_BAD_VERS);
        }
        let uam_ok = ["No User Authent", "Cleartxt passwrd"].contains(&uam.as_str());
        if !uam_ok {
            wire_log!("AFP: login with unsupported UAM '{}'", uam);
            return Err(ERR_BAD_UAM);
        }
        wire_log!(
            "AFP: guest login (version '{}', UAM '{}')",
            version.string(),
            uam
        );
        self.versions.insert(session, version);
        Ok(Vec::new())
    }

    /// FPGetSrvrParms: the server time and the volume list
    fn cmd_get_srvr_parms(&self, _params: &[u8]) -> Reply {
        let mut reply = Vec::new();
        reply.extend_from_slice(&afp_now().to_be_bytes()); // server time
        reply.push(1); // number of volumes
        reply.push(0); // flags: no password, no Apple II config info
        reply.extend_from_slice(&pstring(&self.volume_name));
        Ok(reply)
    }

    /// Bump the volume modification stamp: every write command calls
    /// this, so the client's 10-second volume poll sees server-side
    /// changes anywhere in the tree (a directory's mtime only moves
    /// when its own entries change, and a host-side change deep in the
    /// tree does not show). The kept Enumerate listing is dropped too:
    /// a file system with coarse times may give the folder the same
    /// modification time after the change.
    fn touch_volume(&mut self) {
        self.vol_modified = afp_now();
        self.enum_cache = None;
    }

    /// Close everything a session owns: its open forks (their handles
    /// die with it) and its negotiated AFP version. FPLogout, CloseSess
    /// and a guest restart all land here.
    pub(crate) fn close_session(&mut self, session: u8) {
        self.forks.retain(|_, fork| fork.session != session);
        self.versions.remove(&session);
    }

    /// Record the requester's response capacity for the command being
    /// served: FPRead caps its read at it, so a reply never exceeds
    /// the buffers the requester offered (the ATP window is 8
    /// packets, MAX_RESPONSE bytes)
    pub(crate) fn set_request_capacity(&mut self, buffers: u8) {
        self.req_capacity = usize::from(buffers) * crate::atp::MAX_RESPONSE_DATA;
    }

    /// Connect the UI's progress reporting channel
    pub fn set_scan_status_tx(&mut self, tx: crossbeam_channel::Sender<ScanStatus>) {
        self.scan_status_tx = Some(tx);
    }

    /// Start the background desktop scan: the volume is walked on a
    /// separate thread and its findings are merged as they arrive, so
    /// the AFP thread never blocks on a large share
    pub fn start_scan(&mut self) {
        if self.scan_started {
            return;
        }
        self.scan_started = true;
        let root = self.root.clone();
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<DeskBatch>();
        self.scan_abort = Some(abort.clone());
        self.scan_rx = Some(rx);
        let status_tx = self.scan_status_tx.clone();
        let settings = self.settings;
        wire_log!("AFP: desktop scan started on '{}'", root.display());
        std::thread::spawn(move || scan_volume(root, &tx, &abort, status_tx.as_ref(), settings));
    }

    /// Merge whatever the background desktop scan has reported so far
    fn poll_scan(&mut self) {
        let Some(rx) = self.scan_rx.take() else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(batch) => {
                    for group in batch.groups {
                        let dir_id = self.dir_id(&group.parent);
                        let db = self.desktop.get_or_insert_with(DeskDb::new);
                        db.merge_group(group, dir_id);
                    }
                    let (appls, icons) = self
                        .desktop
                        .as_ref()
                        .map(|db| (db.application_count(), db.icon_count()))
                        .unwrap_or((0, 0));
                    wire_log!(
                        "AFP: desktop scan progress: {} applications and {} icons",
                        appls,
                        icons
                    );
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.scan_rx = Some(rx);
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.scan_abort = None;
                    let (appls, icons) = self
                        .desktop
                        .as_ref()
                        .map(|db| (db.application_count(), db.icon_count()))
                        .unwrap_or((0, 0));
                    wire_log!(
                        "AFP: desktop scan finished: {} applications and {} icons",
                        appls,
                        icons
                    );
                    return;
                }
            }
        }
    }
}

/// The 4-byte command result for "no error"
fn ok() -> Vec<u8> {
    0i32.to_be_bytes().to_vec()
}

/// The 4-byte command result for an error
fn err(code: i32) -> Vec<u8> {
    code.to_be_bytes().to_vec()
}

/// Convert a host file name (UTF-8) to a MacRoman Pascal string
fn pstring(name: &str) -> Vec<u8> {
    let (bytes, _, _) = encoding_rs::MACINTOSH.encode(name);
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(bytes.len() as u8);
    out.extend_from_slice(&bytes);
    out
}

/// A request's parameters, read field by field in the order of the ch13
/// request diagrams: a parse pointer that checks the end. A field past the
/// end of the request is a parameter error.
struct Reader<'a> {
    data: &'a [u8],
    /// How many bytes were read
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    /// The next `n` bytes
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], i32> {
        if self.data.len() < n {
            return Err(ERR_PARAM);
        }
        let (field, rest) = self.data.split_at(n);
        self.data = rest;
        self.offset += n;
        Ok(field)
    }

    /// Skip `n` bytes (a reserved byte, a pad)
    fn skip(&mut self, n: usize) -> Result<(), i32> {
        self.bytes(n).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8, i32> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, i32> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, i32> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    /// A Pascal string: a length byte and that many MacRoman characters
    fn pstring(&mut self) -> Result<String, i32> {
        let len = usize::from(self.u8()?);
        let (s, _, _) = encoding_rs::MACINTOSH.decode(self.bytes(len)?);
        Ok(s.into_owned())
    }

    /// A path name: its path type byte, then a Pascal string. The type is
    /// not looked at: long and short names are the same here.
    fn path(&mut self) -> Result<String, i32> {
        self.skip(1)?;
        self.pstring()
    }

    /// Skip to an even offset from the start of the request: the values
    /// that follow a path name start there (the wire convention). The
    /// reader starts where the caller cut the request, 2 bytes in for
    /// most commands, so offsets keep their parity. FPLogin and
    /// FPOpenFork start 1 byte in, and neither aligns.
    fn align(&mut self) {
        if self.offset % 2 == 1 && !self.data.is_empty() {
            let _ = self.skip(1);
        }
    }

    /// What is left of the request
    fn rest(&self) -> &'a [u8] {
        self.data
    }
}

/// A host file's mtime as an AFP date (signed seconds since
/// 2000-01-01); 0 when the file cannot be stat'ed
fn host_mtime_date(path: &Path) -> u32 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map_or(0, crate::meta::afp_date)
}

/// The current time as signed seconds since 2000-01-01 (the AFP
/// date-time epoch)
fn afp_now() -> u32 {
    crate::meta::afp_date(std::time::SystemTime::now())
}

#[cfg(test)]
mod tests;
