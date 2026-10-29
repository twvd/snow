//! AppleTalk Filing Protocol (AFP) 2.0, server side
//!
//! See Inside AppleTalk, 2nd edition, chapter 13.
//!
//! Implements the read-only core of the AFP 2.0 server: the server
//! information block (served as the ASP GetStatus status block), login
//! with guest authentication, the volume list, opening and closing the
//! volume, file and directory parameters, directory enumeration and
//! reading forks. The volume is a read-only view of a host directory,
//! with host file names converted between UTF-8 and MacRoman.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::dtdb::{DT_REF_NUM, DeskBatch, DeskDb, scan_volume};
use crate::meta::Meta;

pub use crate::dtdb::ScanStatus;

/// AFP commands
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
const CMD_SET_FORK_PARMS: u8 = 31;
const CMD_SET_VOL_PARMS: u8 = 32;
const CMD_WRITE: u8 = 33;
const CMD_SET_FILE_DIR_PARMS: u8 = 35;
const CMD_CREATE_ID: u8 = 39;
const CMD_DELETE_ID: u8 = 40;
const CMD_RESOLVE_ID: u8 = 41;

/// AFP error codes (afpdocs reference.md "Result Codes")
const ERR_ACCESS_DENIED: i32 = -5000;
const ERR_BAD_UAM: i32 = -5002;
const ERR_BAD_VERS: i32 = -5003;
const ERR_BITMAP: i32 = -5004;
const ERR_EOF: i32 = -5009;
const ERR_OBJECT_NOT_FOUND: i32 = -5018;
const ERR_PARAM: i32 = -5019;
const ERR_CALL_NOT_SUPPORTED: i32 = -5024;
const ERR_FILE_BUSY: i32 = -5010;
const ERR_ID_NOT_FOUND: i32 = -5034;
const ERR_OBJECT_LOCKED: i32 = -5032;
const ERR_OBJECT_TYPE: i32 = -5025;
/// Not returned by any command yet; part of the reference table
#[allow(dead_code)]
const ERR_DIR_NOT_FOUND: i32 = -5029;
const ERR_DIR_NOT_EMPTY: i32 = -5007;
const ERR_CANT_MOVE: i32 = -5005;
/// A file ID already exists for the file (kept for the error
/// reference table; the identity-table design never reports it)
#[allow(dead_code)]
const ERR_ID_EXISTS: i32 = -5035;
const ERR_BAD_ID: i32 = -5039;
const ERR_OBJECT_EXISTS: i32 = -5017;
const ERR_VOL_LOCKED: i32 = -5031;

/// The identifier of the single volume
const VOLUME_ID: u16 = 1;
/// The identifier of the volume's root directory
use crate::ids::{IdTable, ROOT_DIR_ID};
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
    /// Whether AppleSingle files are read for their metadata
    pub probe_apple_single: bool,
    /// Whether the wire-level dumps print (a diagnostic; off by
    /// default, toggled from the advanced settings)
    #[serde(default)]
    pub wire_debug: bool,
    /// Whether the volume is read-only (writes return afpVolLocked
    /// and the volume advertises the read-only attribute)
    #[serde(default = "default_true")]
    pub read_only: bool,
    /// Where new metadata is written (the read side probes every
    /// format regardless)
    #[serde(default)]
    pub write_backend: crate::meta::Backend,
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
            probe_apple_single: false,
            wire_debug: false,
            read_only: true,
            write_backend: crate::meta::Backend::AppleDouble,
        }
    }
}

/// The File/DirFlag value marking a directory in GetFileDirParms and
/// Enumerate replies: bit 7 (0x80) in every AFP version. The book's
/// reply diagrams draw the flag as the high bit of the byte, and the
/// "0 = file, 1 = directory" text gives the bit value; CAP's
/// FDP_DIRFLG is 0x80 too. A 1.1 client tests bit 7, so the value 1
/// would make every subdirectory look like a file.
fn dir_flag() -> u8 {
    0x80
}

/// Whether two paths are the same object (device and inode where the
/// platform exposes them; a canonical-path comparison elsewhere). The
/// rename path must compare identities, not paths: on a
/// case-insensitive host the destination spelling resolves to the
/// source itself, while on a case-sensitive host two spellings are
/// two files.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return ma.dev() == mb.dev() && ma.ino() == mb.ino();
        }
        false
    }
    #[cfg(not(unix))]
    {
        if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
            return ca == cb;
        }
        false
    }
}

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
    /// Whether the fork was opened for writing
    write: bool,
    /// Whether FPWrite data actually landed in this fork
    written: bool,
    /// Whether the client set the modification date while this fork
    /// was open (close must not stamp it with "now" then)
    date_set_by_client: bool,
    /// The file the handle was opened on (an empty resource fork
    /// starts on the data file and moves into its container after the
    /// first write)
    file_path: PathBuf,
    /// The session that opened the fork (its close takes the fork)
    session: u8,
    /// Whether this is the resource fork (writes go through the
    /// metadata backend; the data fork writes through the file)
    rsrc: bool,
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
    /// Mangled wire name → original host name (the per-volume mangling
    /// table)
    mangles: HashMap<String, String>,
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
    /// Create a server exposing the given host directory (the default
    /// is the SNOW_AFP_ROOT environment variable) as a volume
    pub fn new() -> Self {
        Self::new_with(None, None, MaxVersion::default(), AfpSettings::default())
    }

    /// Create a server with the given share root, volume name and
    /// maximum AFP version; a missing root falls back to the
    /// SNOW_AFP_ROOT environment variable and /var/tmp/afp-share, a
    /// missing name to "Snow"
    pub fn new_with(
        root: Option<PathBuf>,
        volume_name: Option<String>,
        max_version: MaxVersion,
        settings: AfpSettings,
    ) -> Self {
        let root = root
            .or_else(|| std::env::var("SNOW_AFP_ROOT").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/var/tmp/afp-share"));
        let root = root.canonicalize().unwrap_or(root);
        // A writable volume needs the classic AppleShare convention
        // directories: the guest's client requires them to complete a
        // writable mount (they hold the network trash and the
        // Finder's temporary items) and treats the volume as broken
        // without them. They are invisible, like the real ones: the
        // attributes bit 0 (Invisible) plus the Finder-info fdFlags
        // invisible bit (0x4000), which is what the Finder looks at.
        if !settings.read_only {
            for name in ["Network Trash Folder", "Temporary Items", "VM Storage"] {
                let path = root.join(name);
                if !path.exists() {
                    let _ = std::fs::create_dir(&path);
                }
                let _ = settings.write_backend.set_attrs(&path, 0x0001);
                // The Finder's invisible bit joins the folder's
                // existing DInfo (its window position survives
                // restarts)
                let mut info = crate::meta::open(&path).finder_info;
                let flags = u16::from_be_bytes([info[8], info[9]]) | 0x4000;
                info[8..10].copy_from_slice(&flags.to_be_bytes());
                let _ = settings.write_backend.set_finder_info(&path, &info);
            }
        }
        Self {
            root,
            volume_name: volume_name.unwrap_or_else(|| "Snow".to_string()),
            max_version,
            forks: HashMap::new(),
            next_fork_ref: 1,
            vol_modified: 0,
            req_capacity: crate::atp::MAX_RESPONSE,
            mangles: HashMap::new(),
            ids: IdTable::new(),
            versions: HashMap::new(),
            settings,
            desktop: None,
            scan_rx: None,
            scan_abort: None,
            scan_started: false,
            scan_status_tx: None,
        }
    }

    /// Create a server exposing the given host directory as a volume
    pub fn with_root(root: PathBuf) -> Self {
        Self::new_with(
            Some(root),
            None,
            MaxVersion::default(),
            AfpSettings::default(),
        )
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
        block.push(2);
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
        match cmd {
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
                    (ok(), Vec::new())
                }
            }
            _ => {
                // Every other command has the reserved byte after the
                // function in both versions; a short command is a
                // parameter error, never a panic
                let params = data.get(2..).unwrap_or(&[]);
                match cmd {
                    CMD_GET_SRVR_PARMS => self.cmd_get_srvr_parms(params),
                    CMD_OPEN_VOL => self.cmd_open_vol(version, params),
                    CMD_GET_VOL_PARMS => self.cmd_get_vol_parms(version, params),
                    CMD_CLOSE_VOL => (ok(), Vec::new()),
                    CMD_LOGOUT => (ok(), Vec::new()),
                    CMD_GET_FILE_DIR_PARMS => self.cmd_get_file_dir_parms(version, params),
                    CMD_SET_FILE_DIR_PARMS => self.cmd_set_file_dir_params(params),
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
                    CMD_READ => self.cmd_read(params),
                    CMD_WRITE => {
                        // FPWrite's header byte is the Start/EndFlag
                        // (bit 7 = the offset is relative to the end
                        // of the fork)
                        let flag = data.get(1).copied().unwrap_or(0);
                        self.cmd_write(flag, params)
                    }
                    CMD_GET_FORK_PARMS => self.cmd_get_fork_parms(params),
                    CMD_SET_FORK_PARMS => self.cmd_set_fork_params(params),
                    CMD_SET_VOL_PARMS => self.cmd_set_vol_params(params),
                    // The file-ID calls are AFP 2.1 and up
                    CMD_CREATE_ID | CMD_DELETE_ID | CMD_RESOLVE_ID
                        if !matches!(version, AfpVersion::V21 | AfpVersion::V22) =>
                    {
                        (err(ERR_CALL_NOT_SUPPORTED), Vec::new())
                    }
                    CMD_CREATE_ID => self.cmd_create_id(params),
                    CMD_DELETE_ID => self.cmd_delete_id(params),
                    CMD_RESOLVE_ID => self.cmd_resolve_id(params),
                    CMD_CLOSE_FORK => {
                        if let Some(fork_ref) = get_u16(params)
                            && let Some(fork) = self.forks.remove(&fork_ref)
                            && fork.written
                            && !fork.date_set_by_client
                        {
                            // ch13: the modification date is set when a
                            // written fork is closed — unless the client
                            // set it itself during the session (the
                            // Finder copies dates with SetFileDirParms;
                            // a per-fork flag records that)
                            let meta = self.meta_open(&fork.path);
                            if let Ok(rewritten) = self.settings.write_backend.set_dates(
                                &fork.path,
                                meta.create_date,
                                afp_now(),
                                meta.backup_date,
                            ) {
                                // An AppleSingle rewrite replaced the
                                // file: the other forks' handles and
                                // the ID must follow
                                self.refresh_forks(&fork.path, rewritten);
                            }
                        }
                        (ok(), Vec::new())
                    }
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
                            (err(ERR_VOL_LOCKED), Vec::new())
                        } else {
                            (ok(), Vec::new())
                        }
                    }
                    CMD_GET_CMT => (err(ERR_CALL_NOT_SUPPORTED), Vec::new()),
                    _ => {
                        log::debug!("AFP: dropped unsupported command {}", cmd);
                        (err(ERR_CALL_NOT_SUPPORTED), Vec::new())
                    }
                }
            }
        }
    }

    /// FPLogin: accept an advertised AFP version with a guest UAM and
    /// record the negotiated version for the session
    fn cmd_login(&mut self, session: u8, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some((version, rest)) = get_pstring(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some((uam, _)) = get_pstring(rest) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(version) = AfpVersion::from_string(&version) else {
            log::debug!("AFP: login with unknown version '{}'", version);
            return (err(ERR_BAD_VERS), Vec::new());
        };
        if !self.max_version.accepts(version) {
            log::debug!(
                "AFP: login with unadvertised version '{}'",
                version.string()
            );
            return (err(ERR_BAD_VERS), Vec::new());
        }
        let uam_ok = ["No User Authent", "Cleartxt passwrd"].contains(&uam.as_str());
        if !uam_ok {
            log::debug!("AFP: login with unsupported UAM '{}'", uam);
            return (err(ERR_BAD_UAM), Vec::new());
        }
        log::debug!(
            "AFP: guest login (version '{}', UAM '{}')",
            version.string(),
            uam
        );
        self.versions.insert(session, version);
        (ok(), Vec::new())
    }

    /// FPGetSrvrParms: the server time and the volume list
    fn cmd_get_srvr_parms(&self, _params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut reply = Vec::new();
        reply.extend_from_slice(&afp_now().to_be_bytes()); // server time
        reply.push(1); // number of volumes
        reply.push(0); // flags: no password, no Apple II config info
        reply.extend_from_slice(&pstring(&self.volume_name));
        (ok(), reply)
    }

    /// FPOpenVol: make the volume available, returning the volume ID
    fn cmd_open_vol(&self, version: AfpVersion, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(bitmap) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some((name, _)) = params.get(2..).and_then(get_pstring) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if name != self.volume_name {
            log::debug!("AFP: open of unknown volume '{}'", name);
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        (
            ok(),
            vol_params(
                bitmap,
                &self.volume_name,
                &self.root,
                self.settings.read_only,
                matches!(version, AfpVersion::V21 | AfpVersion::V22),
                self.vol_modified,
            ),
        )
    }

    /// FPGetVolParms: the volume parameters
    fn cmd_get_vol_parms(&self, version: AfpVersion, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(bitmap) = params.get(2..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        (
            ok(),
            vol_params(
                bitmap,
                &self.volume_name,
                &self.root,
                self.settings.read_only,
                matches!(version, AfpVersion::V21 | AfpVersion::V22),
                self.vol_modified,
            ),
        )
    }

    /// Resolve a wire path against a base directory. The AFP pathname
    /// grammar (ch13): the path is relative to the base; a single
    /// leading NUL is ignored; a run of K consecutive NUL separators
    /// (K >= 2) ascends K-1 levels, stopping at the volume root; `.`
    /// and `..` as names stay refused. Every component maps through
    /// the mangling table and the `/` → `:` bijection, and — like the
    /// case-insensitive Mac file system — a case variant of a host
    /// name resolves to the real entry.
    fn resolve_from(&self, base: &Path, path: &str) -> Option<PathBuf> {
        let mut base = base.to_path_buf();
        // The AFP pathname grammar (ch13): the path is relative to the
        // base; a single leading NUL is ignored; a run of K consecutive
        // NUL separators (K >= 2) ascends K-1 levels, stopping at the
        // volume root; `.` and `..` as names stay refused. Mac names
        // cannot contain the separator, so every NUL is grammar.
        let components: Vec<&str> = path.split('\0').collect();
        let mut i = 0;
        while i < components.len() {
            let component = components[i];
            if component.is_empty() {
                // A run of empty components: K-1 ascends, except the
                // single leading separator, which is ignored
                let mut run = 1;
                while components.get(i + run) == Some(&"") {
                    run += 1;
                }
                if !(i == 0 && run == 1) {
                    for _ in 0..run - 1 {
                        // Above the root is ObjectNotFound
                        if base == self.root {
                            return None;
                        }
                        base.pop();
                    }
                }
                i += run;
                continue;
            }
            let host = self
                .mangles
                .get(component)
                .cloned()
                .unwrap_or_else(|| crate::name::to_host(component));
            if host == "." || host == ".." {
                // Parent traversal is never addressable on the wire
                return None;
            }
            if host.starts_with('.') && self.settings.hidden_files == HiddenFiles::Hide {
                // Hidden entries are not addressable unless the server
                // shows them
                return None;
            }
            if crate::name::is_meta_name(&host) {
                // The server's metadata store is never addressable
                return None;
            }
            let exact = base.join(&host);
            base = if exact.symlink_metadata().is_ok() {
                exact
            } else {
                // The Mac file system is case-insensitive while the
                // host may not be: a case variant of the name resolves
                // to the real entry (refusing to resolve into the
                // server's own metadata store under a case variant)
                match self.dir_entry(&base, &host) {
                    Some(found) => {
                        let fname = found
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default();
                        if crate::name::is_meta_name(fname)
                            || (fname.starts_with('.')
                                && self.settings.hidden_files == HiddenFiles::Hide)
                        {
                            return None;
                        }
                        found
                    }
                    None => exact,
                }
            };
            i += 1;
        }
        let canonical = base.canonicalize().ok()?;
        let inside = canonical.strip_prefix(&self.root).ok()?;
        // Hardened: no symlink may lead into a hidden entry
        if self.settings.hidden_files == HiddenFiles::Hide
            && inside
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            return None;
        }
        Some(canonical)
    }

    /// Resolve a wire path against a Directory ID: `dir_base` then the
    /// pathname grammar, with the reserved parent ID 1 whose only
    /// child is the volume itself, addressed by its name as the first
    /// element
    fn resolve_path(&self, dir_id: u32, path: &str) -> Option<PathBuf> {
        if dir_id == ROOT_PARENT_DIR_ID {
            // [volume name]\0... — anything else is ObjectNotFound
            let path = path.strip_prefix('\0').unwrap_or(path);
            let rest = path.strip_prefix(self.volume_name.as_str())?;
            if rest.is_empty() {
                return Some(self.root.clone());
            }
            let rest = rest.strip_prefix('\0')?;
            return self.resolve_from(&self.root, rest);
        }
        let base = self.dir_base(dir_id)?;
        self.resolve_from(&base, path)
    }

    /// The fixed Directory ID of a host directory (the root itself is
    /// the reserved ID 2): the identity table's ID, minted on first
    /// contact; 0 marks a vanished directory
    fn dir_id(&mut self, abs: &Path) -> u32 {
        if abs == self.root {
            return ROOT_DIR_ID;
        }
        // The ID comes from the volume's identity table (minted on
        // first contact); 0 marks a vanished directory
        self.ids.id_of(abs).unwrap_or(0)
    }

    /// The entry of `dir` whose name matches `host` case-insensitively
    /// (Unicode lower-case folding, as the Mac file system folds); None
    /// when no entry matches. Called only when the exact name misses,
    /// so the read_dir cost is paid on misses alone.
    fn dir_entry(&self, dir: &Path, host: &str) -> Option<PathBuf> {
        // NFC first: the server serves composed names, and a host may
        // store decomposed ones (macOS does); a byte-exact file system
        // then needs the normalization before the case fold
        let wanted: String = host.nfc().collect::<String>().to_lowercase();
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name();
            if name
                .to_string_lossy()
                .nfc()
                .collect::<String>()
                .to_lowercase()
                == wanted
            {
                return Some(dir.join(name));
            }
        }
        None
    }

    /// The first case-folded entry of `dir` matching `host` that is
    /// NOT `source` itself: a rename's own source is not a conflict
    /// (the case-only rename must stay allowed even where the host
    /// holds several spellings)
    fn case_variant_other(&self, dir: &Path, host: &str, source: &Path) -> Option<PathBuf> {
        let wanted = host.nfc().collect::<String>().to_lowercase();
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name();
            if name
                .to_string_lossy()
                .nfc()
                .collect::<String>()
                .to_lowercase()
                != wanted
            {
                continue;
            }
            let found = dir.join(name);
            if !same_file(&found, source) {
                return Some(found);
            }
        }
        None
    }

    /// The attribute bits of a file's open state: DAlreadyOpen when
    /// a data fork is open, RAlreadyOpen when the resource fork is
    fn open_attrs(&self, path: &Path) -> u16 {
        let mut attrs = 0;
        for fork in self.forks.values() {
            if fork.path == path {
                if fork.rsrc {
                    attrs |= 0x0010;
                } else {
                    attrs |= 0x0008;
                }
            }
        }
        attrs
    }

    /// The host directory a Directory ID refers to (the root is the
    /// fixed ID 2), only while the recorded path still holds the
    /// recorded directory
    fn dir_base(&self, dir_id: u32) -> Option<PathBuf> {
        if dir_id == ROOT_DIR_ID {
            return Some(self.root.clone());
        }
        self.ids.verified_path(dir_id).map(|p| p.to_path_buf())
    }

    /// The parent Directory ID of a host path: the root's parent is
    /// the reserved ID 1 (the HFS convention, where the parent walk
    /// terminates), a directory's parent is the ID of its parent
    /// directory otherwise
    fn parent_id_of(&mut self, abs: &Path) -> u32 {
        if abs == self.root {
            return ROOT_PARENT_DIR_ID;
        }
        match abs.parent() {
            Some(parent) if parent != abs => self.dir_id(parent),
            _ => ROOT_DIR_ID,
        }
    }

    /// The metadata of a host file, honoring the server's probe
    /// settings
    fn meta_open(&self, path: &Path) -> Meta {
        crate::meta::open_with(
            path,
            &crate::meta::ProbeFlags {
                apple_double: self.settings.probe_apple_double
                    || self.settings.write_backend == crate::meta::Backend::AppleDouble,
                apple_single: self.settings.probe_apple_single
                    || self.settings.write_backend == crate::meta::Backend::AppleSingle,
            },
        )
    }

    /// FPGetFileDirParms: parameters of a file or directory
    fn cmd_get_file_dir_parms(
        &mut self,
        _version: AfpVersion,
        params: &[u8],
    ) -> (Vec<u8>, Vec<u8>) {
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(dir_id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(file_bitmap) = params.get(6..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(dir_bitmap) = params.get(8..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some((path, _)) = params.get(11..).and_then(get_pstring) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if file_bitmap & !FILE_BITMAP_ALLOWED != 0 || dir_bitmap & !DIR_BITMAP_ALLOWED != 0 {
            return (err(ERR_BITMAP), Vec::new());
        }
        log::debug!(
            "AFP: GetFileDirParms dir {} path '{}' file bm {:04x} dir bm {:04x}",
            dir_id,
            path,
            file_bitmap,
            dir_bitmap
        );

        // Resolve relative to the given directory
        let Some(target) = self.resolve_path(dir_id, &path) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        let Ok(fs_meta) = std::fs::metadata(&target) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        let mut meta = self.meta_open(&target);
        meta.attrs |= self.open_attrs(&target);
        let name = if target == self.root {
            self.volume_name.clone()
        } else {
            self.mac_name_of(&target)
        };
        let parent_id = self.parent_id_of(&target);
        // The desktop database is fed by the files the client asks
        // about: register an application and its bundle icons now, so
        // the desktop calls that follow find them
        if !fs_meta.is_dir()
            && let Some(db) = &mut self.desktop
        {
            db.register_appl(&target, &name, &meta, parent_id);
        }

        let mut reply = Vec::new();
        reply.extend_from_slice(&file_bitmap.to_be_bytes());
        reply.extend_from_slice(&dir_bitmap.to_be_bytes());
        if fs_meta.is_dir() {
            let offspring = visible_offspring(&target, self.settings.hidden_files);
            reply.push(dir_flag());
            // The reserved byte after the File/DirFlag exists in both
            // AFP 1.1 and AFP 2.0
            reply.push(0);
            reply.extend_from_slice(&dir_params(
                dir_bitmap,
                &name,
                &meta,
                offspring,
                self.dir_id(&target),
                parent_id,
                self.settings.read_only,
            ));
        } else {
            reply.push(0);
            reply.push(0);
            let file_id = self.ids.id_of(&target).unwrap_or(0);
            reply.extend_from_slice(&file_params(file_bitmap, &name, &meta, parent_id, file_id));
        }
        (ok(), reply)
    }

    /// The wire name of a resolved host path, registering the mangling
    /// table entry when the name has to be mangled
    fn mac_name_of(&mut self, target: &Path) -> String {
        let host_name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (mac_name, original) = crate::name::to_mac(&host_name);
        if let Some(original) = original {
            self.mangles.insert(mac_name.clone(), original);
        }
        mac_name
    }

    /// FPEnumerate: list the offspring of a directory
    fn cmd_enumerate(
        &mut self,
        _version: AfpVersion,
        params: &[u8],
        file_flag: Option<u8>,
    ) -> (Vec<u8>, Vec<u8>) {
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(dir_id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(file_bitmap) = params.get(6..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(dir_bitmap) = params.get(8..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(req_count) = params.get(10..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(start_index) = params.get(12..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(max_reply_size) = params.get(14..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some((path, _)) = params.get(17..).and_then(get_pstring) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if file_bitmap & !FILE_BITMAP_ALLOWED != 0 || dir_bitmap & !DIR_BITMAP_ALLOWED != 0 {
            return (err(ERR_BITMAP), Vec::new());
        }
        // Both bitmaps null is a bitmap error; a null File Bitmap
        // enumerates only directories, a null Directory Bitmap only
        // files
        if file_bitmap == 0 && dir_bitmap == 0 {
            return (err(ERR_BITMAP), Vec::new());
        }

        let Some(dir) = self.resolve_path(dir_id, &path) else {
            return (err(ERR_DIR_NOT_FOUND), Vec::new());
        };
        // Enumerating a file answers afpObjectTypeErr
        if !std::fs::metadata(&dir).is_ok_and(|m| m.is_dir()) {
            return (err(ERR_OBJECT_TYPE), Vec::new());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return (err(ERR_DIR_NOT_FOUND), Vec::new());
        };
        // The parent Directory ID the offspring report
        let parent_id = self.dir_id(&dir);

        let mut reply = Vec::new();
        reply.extend_from_slice(&file_bitmap.to_be_bytes());
        reply.extend_from_slice(&dir_bitmap.to_be_bytes());
        reply.extend_from_slice(&0u16.to_be_bytes()); // actual count, filled in

        // An empty request enumerates nothing
        if req_count == 0 {
            return (ok(), reply);
        }

        // The visible offspring (dotfiles, .AppleDouble entries and ._
        // sidecars are hidden), sorted by wire name for a stable
        // enumeration order
        let mut offspring: Vec<(PathBuf, String)> = entries
            .flatten()
            .map(|e| e.path())
            .filter_map(|p| {
                let host_name = p.file_name()?.to_string_lossy().into_owned();
                let hidden = crate::name::is_hidden(&host_name);
                if hidden && self.settings.hidden_files == HiddenFiles::Hide {
                    return None;
                }
                if crate::name::is_meta_name(&host_name) {
                    // The server's metadata store is never enumerated
                    return None;
                }
                let (mac_name, original) = crate::name::to_mac(&host_name);
                if let Some(original) = original {
                    self.mangles.insert(mac_name.clone(), original);
                }
                Some((p, mac_name))
            })
            .collect();
        // The null bitmaps and the 1.1 FileFlag filter the record set;
        // the Start Index counts the filtered list
        let files_included = file_flag.is_none_or(|f| f & 0x80 == 0);
        let is_dir_entry = |p: &PathBuf| std::fs::metadata(p).is_ok_and(|m| m.is_dir());
        if file_bitmap == 0 || !files_included {
            offspring.retain(|(p, _)| is_dir_entry(p));
        } else if dir_bitmap == 0 {
            offspring.retain(|(p, _)| !is_dir_entry(p));
        }
        offspring.sort_by(|a, b| a.1.cmp(&b.1));

        // The enumeration index space: the book's ObjectNotFound means
        // "no more offspring exist to be enumerated" — the client pages
        // until it gets that error, so a Start Index past the last
        // visible offspring must end the enumeration
        let skip = usize::from(start_index.saturating_sub(1));
        if skip >= offspring.len() {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }

        let mut count = 0u16;
        for (target, mac_name) in offspring.iter().skip(skip) {
            let Ok(fs_meta) = std::fs::metadata(target) else {
                continue;
            };
            let mut meta = self.meta_open(target);
            meta.attrs |= self.open_attrs(target);
            let (flag, params) = if fs_meta.is_dir() {
                let offspring_count = visible_offspring(target, self.settings.hidden_files);
                (
                    dir_flag(),
                    dir_params(
                        dir_bitmap,
                        mac_name,
                        &meta,
                        offspring_count,
                        self.dir_id(target),
                        parent_id,
                        self.settings.read_only,
                    ),
                )
            } else {
                let file_id = self.ids.id_of(target).unwrap_or(0);
                (
                    0,
                    file_params(file_bitmap, mac_name, &meta, parent_id, file_id),
                )
            };
            let mut record = Vec::new();
            record.push(0); // struct length, filled in
            record.push(flag);
            record.extend_from_slice(&params);
            if record.len() % 2 != 0 {
                record.push(0);
            }
            record[0] = record.len() as u8;
            // Honor the client's MaxReplySize: no partial offspring
            // structures are returned
            if reply.len() + record.len() > max_reply_size as usize {
                break;
            }
            reply.extend_from_slice(&record);
            count += 1;
            if count >= req_count {
                break;
            }
        }
        reply[4..6].copy_from_slice(&count.to_be_bytes());
        (ok(), reply)
    }

    /// FPOpenFork: open the data fork or the resource fork of a file
    fn cmd_open_fork(
        &mut self,
        version: AfpVersion,
        session: u8,
        params: &[u8],
    ) -> (Vec<u8>, Vec<u8>) {
        let Some(rsrc_data_flag) = params.first().copied() else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(volume_id) = params.get(1..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(dir_id) = params.get(3..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(bitmap) = params.get(7..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if bitmap & !FILE_BITMAP_ALLOWED != 0 {
            return (err(ERR_BITMAP), Vec::new());
        }
        // The access mode is 16 bits: bit 0 requests read, bit 1
        // write (the deny bits are ignored: single client). A write
        // on a read-only volume is afpVolLocked in 2.x and
        // afpAccessDenied in 1.1 (the book's rule)
        let access = params.get(9..).and_then(get_u16).unwrap_or(0);
        let write = access & 0x0002 != 0;
        if write && self.settings.read_only {
            let code = if version == AfpVersion::V11 {
                ERR_ACCESS_DENIED
            } else {
                ERR_VOL_LOCKED
            };
            return (err(code), Vec::new());
        }
        let Some((path, _)) = params.get(12..).and_then(get_pstring) else {
            return (err(ERR_PARAM), Vec::new());
        };

        let Some(target) = self.resolve_path(dir_id, &path) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        let Ok(fs_meta) = std::fs::metadata(&target) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        if !fs_meta.is_file() {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let meta = self.meta_open(&target);
        let parent_id = self.parent_id_of(&target);
        // Feed the desktop database while the file is in hand
        let fork_name = self.mac_name_of(&target);
        if let Some(db) = &mut self.desktop {
            db.register_appl(&target, &fork_name, &meta, parent_id);
        }

        // The fork: the data fork lives at data_offset inside the host
        // file (nonzero for AppleSingle); the resource fork lives in
        // the metadata container. An absent resource fork reads as an
        // empty fork (a write creates it lazily).
        let rsrc = rsrc_data_flag & 0x80 != 0;
        let (file_path, offset, len) = if rsrc {
            match meta.rsrc_file.clone() {
                Some(file) => (file, meta.rsrc_offset, u64::from(meta.rsrc_fork_len)),
                None => (target.clone(), 0, 0),
            }
        } else {
            (target.clone(), meta.data_offset, meta.data_len)
        };
        let Ok(file) = open_with_access(&file_path, write) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };

        let fork_ref = self.next_fork_ref;
        self.next_fork_ref = self.next_fork_ref.wrapping_add(1);
        self.forks.insert(
            fork_ref,
            Fork {
                file,
                len,
                offset,
                name: fork_name.clone(),
                meta: meta.clone(),
                parent: parent_id,
                marker: 0,
                path: target.clone(),
                file_id: self.ids.id_of(&target).unwrap_or(0),
                write,
                written: false,
                date_set_by_client: false,
                file_path,
                session,
                rsrc,
            },
        );
        let mac_name = fork_name;
        log::debug!(
            "AFP: opened fork {} on '{}' ({} bytes{}{} at host offset {})",
            fork_ref,
            mac_name,
            len,
            if rsrc { " of the resource fork" } else { "" },
            if write { " for writing" } else { "" },
            offset
        );

        // The reply: the bitmap, the fork reference number, then the
        // requested file parameters
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        reply.extend_from_slice(&fork_ref.to_be_bytes());
        let file_id = self.ids.id_of(&target).unwrap_or(0);
        reply.extend_from_slice(&file_params(bitmap, &mac_name, &meta, parent_id, file_id));
        (ok(), reply)
    }

    /// FPRead: read data from an open fork
    fn cmd_read(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(fork_ref) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(offset) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(count) = params.get(6..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(fork) = self.forks.get_mut(&fork_ref) else {
            return (err(ERR_PARAM), Vec::new());
        };

        let offset = u64::from(offset);
        // The NewLineMask/NewLineChar pair (a text-mode read,
        // netatalk's OSRead semantics): a line break is a byte whose
        // masked bits equal the NewLineChar. The carriage-return
        // reader also stopping at a line feed (and translating it) is
        // netatalk's legacy OSRead branch, not the spec — it stays so
        // text stored with Unix line endings reads as Mac text. An
        // absent pair (the common case) reads bytes only.
        let mask = params.get(10).copied().unwrap_or(0);
        let nlchr = params.get(11).copied().unwrap_or(0);
        // The read is capped at the response capacity the requester
        // offered, so the reply fits its buffers without truncation
        let count = (count as usize).min(self.req_capacity);
        if offset >= fork.len {
            return (err(ERR_EOF), Vec::new());
        }
        let count = (count as u64).min(fork.len - offset) as usize;
        if fork
            .file
            .seek(SeekFrom::Start(fork.offset + offset))
            .is_err()
        {
            return (err(ERR_EOF), Vec::new());
        }
        let mut data = vec![0; count];
        let mut total = 0;
        let mut newline_hit = false;
        while total < count {
            match fork.file.read(&mut data[total..]) {
                Ok(0) => break,
                Ok(n) => {
                    if mask != 0 {
                        let window = &data[total..total + n];
                        let stop = if nlchr == b'\r' {
                            window
                                .iter()
                                .position(|&b| b & mask == b'\r' || b & mask == b'\n')
                        } else {
                            window.iter().position(|&b| b & mask == nlchr)
                        };
                        if let Some(pos) = stop {
                            let end = total + pos + 1;
                            // The 2.0 special case: a carriage-return
                            // reader gets the line feed translated
                            if nlchr == b'\r' && data[total + pos] == b'\n' {
                                data[total + pos] = b'\r';
                            }
                            total = end;
                            newline_hit = true;
                            break;
                        }
                    }
                    total += n;
                }
                Err(_) => break,
            }
        }
        data.truncate(total);
        fork.marker = offset + total as u64;
        log::debug!(
            "AFP: read {} bytes at {} on fork {}",
            total,
            offset,
            fork_ref
        );
        if newline_hit {
            // The line break ended the read: a complete text line
            (ok(), data)
        } else if total < count {
            // The fork ended before the request did: the bytes read so
            // far go back with afpEOFErr (ch13's FPRead rule)
            (err(ERR_EOF), data)
        } else {
            (ok(), data)
        }
    }

    /// FPGetForkParms: the bitmap is the file bitmap; the reply is the
    /// bitmap copy plus the requested file parameters
    fn cmd_get_fork_parms(&self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(fork_ref) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(bitmap) = params.get(2..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if bitmap & !FILE_BITMAP_ALLOWED != 0 {
            return (err(ERR_BITMAP), Vec::new());
        }
        let Some(fork) = self.forks.get(&fork_ref) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        reply.extend_from_slice(&file_params(
            bitmap,
            &fork.name,
            &fork.meta,
            fork.parent,
            fork.file_id,
        ));
        (ok(), reply)
    }
    /// FPOpenDT: open the volume's desktop database and hand out its
    /// reference number
    fn cmd_open_dt(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        self.desktop.get_or_insert_with(DeskDb::new).set_open(true);
        let mut reply = Vec::new();
        reply.extend_from_slice(&DT_REF_NUM.to_be_bytes());
        (ok(), reply)
    }

    /// FPCloseDT: close the desktop database
    fn cmd_close_dt(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(ref_num) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if ref_num != DT_REF_NUM {
            return (err(ERR_PARAM), Vec::new());
        }
        if let Some(db) = &mut self.desktop {
            db.set_open(false);
        }
        (ok(), Vec::new())
    }

    /// FPGetAPPL: one APPL mapping for a creator, by index
    fn cmd_get_appl(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(ref_num) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if ref_num != DT_REF_NUM {
            return (err(ERR_PARAM), Vec::new());
        }
        let Some(creator) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(index) = params.get(6..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(bitmap) = params.get(8..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if bitmap & !FILE_BITMAP_ALLOWED != 0 {
            return (err(ERR_BITMAP), Vec::new());
        }
        let appls = self
            .desktop
            .as_ref()
            .map(|db| db.applications(creator))
            .unwrap_or_default();
        // An APPL index of 0 returns the first mapping
        let Some(appl) = appls.get(index.max(1) as usize - 1) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        let file_id = self.ids.id_of(&appl.path).unwrap_or(0);
        reply.extend_from_slice(&file_id.to_be_bytes()); // APPL tag
        reply.extend_from_slice(&file_params(
            bitmap,
            &appl.name,
            &appl.meta,
            appl.dir_id,
            file_id,
        ));
        (ok(), reply)
    }

    /// FPGetIconInfo: information about one icon of a creator, by index
    fn cmd_get_icon_info(&self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(ref_num) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if ref_num != DT_REF_NUM {
            return (err(ERR_PARAM), Vec::new());
        }
        let Some(creator) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(index) = params.get(6..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let icons = self
            .desktop
            .as_ref()
            .map(|db| db.icons(creator))
            .unwrap_or_default();
        let Some(icon) = icons.get(index.max(1) as usize - 1) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        // Advertise the best format available: 8-bit color (IconType
        // 3), else 4-bit color (2), else monochrome (1)
        let (icon_type, size) = if icon.color8.is_some() {
            (3, crate::rfork::ICL8_LEN)
        } else if icon.color4.is_some() {
            (2, crate::rfork::ICL4_LEN)
        } else {
            (1, icon.mono.len())
        };
        let mut reply = Vec::new();
        reply.extend_from_slice(&icon.ftype.to_be_bytes()); // the icon tag
        reply.extend_from_slice(&icon.ftype.to_be_bytes());
        reply.push(icon_type);
        reply.push(0); // reserved
        reply.extend_from_slice(&(size as u16).to_be_bytes());
        (ok(), reply)
    }

    /// FPGetIcon: the icon bitmap for a creator and file type
    fn cmd_get_icon(&self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(ref_num) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if ref_num != DT_REF_NUM {
            return (err(ERR_PARAM), Vec::new());
        }
        let Some(creator) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(ftype) = params.get(6..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(icon_type) = params.get(10).copied() else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(length) = params.get(12..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let icon = self.desktop.as_ref().and_then(|db| db.icon(creator, ftype));
        let Some(icon) = icon else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        // The bitmap for the requested IconType: 1 large monochrome,
        // 2 large 4-bit, 3 large 8-bit, 4 small 4-bit, 5 small
        // monochrome, 6 small 8-bit
        let bitmap: Option<&Vec<u8>> = match icon_type {
            1 => Some(&icon.mono),
            2 => icon.color4.as_ref(),
            3 => icon.color8.as_ref(),
            4 => icon.small4.as_ref(),
            5 => icon.small_mono.as_ref(),
            6 => icon.small8.as_ref(),
            _ => None,
        };
        let Some(bitmap) = bitmap else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        // The reply is the icon bitmap, truncated to the requested length;
        // a length of 0 tests presence only
        let len = (length as usize).min(bitmap.len());
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap[..len]);
        (ok(), reply)
    }

    /// Refresh any open fork on a file after its metadata changed (an
    /// AppleSingle conversion can move the data fork): the handle is
    /// reopened and the offsets, length and metadata refreshed
    /// Refresh the open forks of a file after a backend write: the
    /// lengths and the metadata follow the new container state, and
    /// only a REWRITE (which replaced the file: a new inode) re-binds
    /// the ID and reopens the handles. Re-binding unconditionally
    /// would make a host-side replacement silently follow the old ID
    /// (aliasing), and a plain data write replaces nothing.
    fn refresh_forks(&mut self, target: &Path, rewritten: bool) {
        if rewritten {
            self.ids.rebind(target);
        }
        let meta = self.meta_open(target);
        for fork in self.forks.values_mut() {
            if fork.path != target {
                continue;
            }
            let (file_path, offset, len) = if fork.rsrc {
                match meta.rsrc_file.clone() {
                    // An absent resource fork is empty: reads end at
                    // EOF before the placeholder handle is touched
                    Some(file) => (file, meta.rsrc_offset, u64::from(meta.rsrc_fork_len)),
                    None => (target.to_path_buf(), meta.rsrc_offset, 0),
                }
            } else {
                (target.to_path_buf(), meta.data_offset, meta.data_len)
            };
            if (rewritten || fork.file_path != file_path)
                && let Ok(file) = open_with_access(&file_path, fork.write)
            {
                fork.file = file;
                fork.file_path = file_path.clone();
            }
            fork.offset = offset;
            fork.len = len;
            fork.meta = meta.clone();
        }
    }

    /// Bump the volume modification stamp: every write command calls
    /// this, so the client's 10-second volume poll sees server-side
    /// changes anywhere in the tree (a directory's mtime only moves
    /// when its own entries change; host-side deep changes need the
    /// inotify/kqueue work from the state notes)
    fn touch_volume(&mut self) {
        self.vol_modified = afp_now();
    }

    /// Close everything a session owns: its open forks (their handles
    /// die with it) and its negotiated AFP version. FPLogout,
    /// CloseSess and the session-maintenance timeout all land here.
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

    /// Update the length fields of every fork open on a file after a
    /// plain data-fork write: a write replaces nothing, so the ID
    /// bindings and the handles stay valid, and only the lengths and
    /// the recorded metadata need to follow
    fn bump_data_len(&mut self, path: &Path, new_len: u64) {
        for fork in self.forks.values_mut() {
            if fork.path == path {
                fork.meta.data_len = new_len;
                if !fork.rsrc {
                    fork.len = new_len;
                }
            }
        }
    }

    /// Drop the mangling table entry of a host file (after a delete
    /// or rename); the values are host names
    fn drop_mangles(&mut self, target: &Path) {
        let Some(name) = target.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return;
        };
        self.mangles.retain(|_, host| *host != name);
    }

    /// FPCreateFile / FPCreateDir: [vol][dir id][path name]; the path
    /// may name ancestors ("sub\0new"). FPCreateFile's `flag` is the
    /// CreateFlag: bit 7 = hard create (an existing closed file is
    /// deleted and recreated; an open one answers afpFileBusy, a
    /// directory afpObjectTypeErr). FPCreateDir replies with the New
    /// Directory ID.
    fn cmd_create(&mut self, is_dir: bool, flag: u8, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        if self.settings.read_only {
            return (err(ERR_VOL_LOCKED), Vec::new());
        }
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(dir_id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some((name, _)) = params.get(6..).and_then(get_long_path) else {
            return (err(ERR_PARAM), Vec::new());
        };
        // Everything before the last separator resolves as a path
        // (ancestors), the final component is the new name
        let (dir_part, new_name) = match name.rfind('\0') {
            Some(pos) => (&name[..pos], &name[pos + 1..]),
            None => ("", name.as_str()),
        };
        let Some(base) = self.resolve_path(dir_id, dir_part) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        let host = crate::name::to_host(new_name);
        if host.starts_with('.') && self.settings.hidden_files == HiddenFiles::Hide {
            // Hidden entries are not addressable in Hide mode
            return (err(ERR_PARAM), Vec::new());
        }
        if crate::name::is_meta_name(&host) {
            // The server's metadata store is never creatable
            return (err(ERR_PARAM), Vec::new());
        }
        let target = base.join(&host);
        if !target.exists() && self.dir_entry(&base, &host).is_some() {
            // The Mac file system is case-insensitive: a case variant
            // of an existing name is ObjectExists even where the host
            // would store both side by side
            return (err(ERR_OBJECT_EXISTS), Vec::new());
        }
        if !is_dir && flag & 0x80 != 0 && target.exists() {
            // A hard create: replace an existing closed file
            if std::fs::metadata(&target).is_ok_and(|m| m.is_dir()) {
                return (err(ERR_OBJECT_TYPE), Vec::new());
            }
            if self.forks.values().any(|f| f.path == target) {
                return (err(ERR_FILE_BUSY), Vec::new());
            }
            if std::fs::remove_file(&target).is_err() {
                return (err(ERR_PARAM), Vec::new());
            }
            self.settings.write_backend.remove_meta(&target);
            self.drop_mangles(&target);
            self.ids.remove_path(&target);
            if let Some(db) = &mut self.desktop {
                db.remove_appl(&target);
            }
        }
        let created = if is_dir {
            std::fs::create_dir(&target)
        } else {
            // A soft create: an existing name is an error
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map(|_| ())
        };
        match created {
            Ok(()) => {
                self.touch_volume();
                if is_dir {
                    // FPCreateDir replies with the New Directory ID
                    let id = self.ids.id_of(&target).unwrap_or(0);
                    (ok(), id.to_be_bytes().to_vec())
                } else {
                    (ok(), Vec::new())
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                (err(ERR_OBJECT_EXISTS), Vec::new())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (err(ERR_OBJECT_NOT_FOUND), Vec::new())
            }
            Err(_) => (err(ERR_PARAM), Vec::new()),
        }
    }

    /// FPDelete: [vol][dir id][path name]; the metadata containers go
    /// with the file
    fn cmd_delete(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        if self.settings.read_only {
            return (err(ERR_VOL_LOCKED), Vec::new());
        }
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(dir_id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some((name, _)) = params.get(6..).and_then(get_long_path) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(target) = self.resolve_path(dir_id, &name) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        if target == self.root {
            return (err(ERR_ACCESS_DENIED), Vec::new());
        }
        // DeleteInhibit (attribute bit 8) answers afpObjectLocked; an
        // open fork answers afpFileBusy (unlinking it would orphan the
        // handle's writes)
        let attrs = self.meta_open(&target).attrs;
        if attrs & 0x0100 != 0 {
            return (err(ERR_OBJECT_LOCKED), Vec::new());
        }
        if self.forks.values().any(|f| f.path == target) {
            return (err(ERR_FILE_BUSY), Vec::new());
        }
        let is_dir = std::fs::metadata(&target).is_ok_and(|m| m.is_dir());
        // Emptiness ignores the server's own metadata: a folder full of
        // `._*` orphans (a macOS copy) is still deletable. The cleanup
        // itself only happens once the delete succeeds, so a failed
        // delete never destroys the folder's metadata.
        if is_dir
            && std::fs::read_dir(&target).is_ok_and(|d| {
                d.flatten()
                    .any(|e| !crate::name::is_meta_name(&e.file_name().to_string_lossy()))
            })
        {
            return (err(ERR_DIR_NOT_EMPTY), Vec::new());
        }
        let removed = if is_dir {
            // Only metadata remains: clear it so remove_dir succeeds
            let _ = std::fs::remove_dir_all(target.join(".AppleDouble"));
            if let Ok(entries) = std::fs::read_dir(&target) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if crate::name::is_meta_name(&name) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            std::fs::remove_dir(&target)
        } else {
            std::fs::remove_file(&target)
        };
        if let Err(e) = removed {
            let code = match e.kind() {
                std::io::ErrorKind::NotFound => ERR_OBJECT_NOT_FOUND,
                std::io::ErrorKind::DirectoryNotEmpty => ERR_DIR_NOT_EMPTY,
                _ => ERR_PARAM,
            };
            return (err(code), Vec::new());
        }
        // Only after the delete succeeded: the folder's own metadata
        // (its `._` sidecar and the parent's `.AppleDouble` entry)
        self.settings.write_backend.remove_meta(&target);
        self.drop_mangles(&target);
        // The subtree's IDs and their identity bindings go (a reused
        // inode must get a fresh ID, never alias the deleted object)
        self.ids.remove_path(&target);
        if let Some(db) = &mut self.desktop {
            db.remove_appl(&target);
        }
        self.touch_volume();
        (ok(), Vec::new())
    }

    /// FPRename: [vol][dir id][old path name][new path name]
    fn cmd_rename(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        self.cmd_rename_inner(params, false)
    }

    /// FPMoveAndRename: [vol][source dir][dest dir][source path]
    /// [dest path][new name]; a null source path addresses a directory
    /// by its own Directory ID, an empty new name keeps the source's
    fn cmd_move_and_rename(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        self.cmd_rename_inner(params, true)
    }

    fn cmd_rename_inner(&mut self, params: &[u8], move_rename: bool) -> (Vec<u8>, Vec<u8>) {
        if self.settings.read_only {
            return (err(ERR_VOL_LOCKED), Vec::new());
        }
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(src_dir_id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        // FPMoveAndRename carries the destination Directory ID, the
        // source path, the destination path and the new name
        // ([vol][src dir][dst dir][src path][dst path][new name]); an
        // empty new name keeps the source's. FPRename has the old and
        // the new name right after the source ID.
        let (dst_dir_id, rest) = if move_rename {
            let Some(dst_dir_id) = params.get(6..).and_then(get_u32) else {
                return (err(ERR_PARAM), Vec::new());
            };
            let Some((old_name, rest)) = params.get(10..).and_then(get_long_path) else {
                return (err(ERR_PARAM), Vec::new());
            };
            let Some((dst_path, rest)) = get_long_path(rest) else {
                return (err(ERR_PARAM), Vec::new());
            };
            let Some((new_name, _)) = get_long_path(rest) else {
                return (err(ERR_PARAM), Vec::new());
            };
            (Some(dst_dir_id), (old_name, dst_path, new_name))
        } else {
            let Some((old_name, rest)) = params.get(6..).and_then(get_long_path) else {
                return (err(ERR_PARAM), Vec::new());
            };
            let Some((new_name, _)) = get_long_path(rest) else {
                return (err(ERR_PARAM), Vec::new());
            };
            (None, (old_name, String::new(), new_name))
        };
        let (old_name, dst_path, new_name) = rest;
        let Some(target) = self.resolve_path(src_dir_id, &old_name) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        if target == self.root {
            return (err(ERR_ACCESS_DENIED), Vec::new());
        }
        // RenameInhibit (attribute bit 7) answers afpObjectLocked
        if self.meta_open(&target).attrs & 0x0080 != 0 {
            return (err(ERR_OBJECT_LOCKED), Vec::new());
        }
        // The new name: an empty one keeps the source's own host name
        // (the resolved target's file name — the wire form would lose
        // a mangled name, and a null source path, which addresses a
        // directory by its own Directory ID, would make the destination
        // the directory itself)
        let new_name = if new_name.is_empty() {
            target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            crate::name::to_host(&new_name)
        };
        let (dst_base, _) = if let Some(dst_dir_id) = dst_dir_id {
            let Some(dst_base) = self.resolve_path(dst_dir_id, &dst_path) else {
                return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
            };
            if !dst_base.is_dir() {
                return (err(ERR_OBJECT_TYPE), Vec::new());
            }
            (dst_base, ())
        } else {
            let parent = target.parent().unwrap_or(&self.root).to_path_buf();
            (parent, ())
        };
        if new_name.starts_with('.') && self.settings.hidden_files == HiddenFiles::Hide {
            // Hidden entries are not addressable in Hide mode
            return (err(ERR_PARAM), Vec::new());
        }
        if crate::name::is_meta_name(&new_name) {
            // The server's metadata store is never renamable
            return (err(ERR_PARAM), Vec::new());
        }
        let new_target = dst_base.join(&new_name);
        // The Mac file system is case-insensitive: renaming onto a
        // case variant of ANOTHER entry is ObjectExists. A case-only
        // rename of the object itself stays allowed — and on a
        // case-insensitive host the destination "exists" (it IS the
        // object), so it needs a plain rename there
        // The Mac file system is case-insensitive: renaming onto an
        // existing name — even a case variant — is ObjectExists, unless
        // the destination IS the source itself (a case-only rename).
        // The exact destination is checked first and compared by
        // identity, never by path: a case-sensitive host holds both
        // spellings as two files, and a plain rename would silently
        // replace the other one
        // A case-only rename of the source itself: the destination
        // exists, is the same object (by identity, never by path),
        // sits in the source's folder, and the names match ignoring
        // case. Anything else that exists is ObjectExists — a hard
        // link to the same file under another name included, which a
        // POSIX rename would leave untouched while the bookkeeping
        // below would still move
        let exact_dest = new_target.symlink_metadata().is_ok();
        let same_object = exact_dest
            && same_file(&new_target, &target)
            && target.parent().is_some_and(|p| p == dst_base)
            && target.file_name().is_some_and(|n| {
                n.to_string_lossy().nfc().collect::<String>().to_lowercase()
                    == new_name.nfc().collect::<String>().to_lowercase()
            });
        if exact_dest && !same_object {
            return (err(ERR_OBJECT_EXISTS), Vec::new());
        }
        if !exact_dest
            && self
                .case_variant_other(&dst_base, &new_name, &target)
                .is_some()
        {
            return (err(ERR_OBJECT_EXISTS), Vec::new());
        }
        // Moving a directory into itself or one of its children is
        // afpCantMove
        if (target.is_dir() && dst_base == target)
            || (target.is_dir() && dst_base.starts_with(&target))
        {
            return (err(ERR_CANT_MOVE), Vec::new());
        }
        let rename_result = if same_object {
            // Only a case-insensitive host makes this possible (the
            // destination resolves to the source): its no-replace
            // rename would refuse it, so a plain rename
            std::fs::rename(&target, &new_target)
        } else {
            rename_no_replace(&target, &new_target)
        };
        match rename_result {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return (err(ERR_OBJECT_EXISTS), Vec::new());
            }
            Err(_) => return (err(ERR_PARAM), Vec::new()),
        }
        let _ = self.settings.write_backend.move_meta(&target, &new_target);
        self.drop_mangles(&target);
        // The subtree's recorded paths follow the move (the identity
        // bindings are unchanged, so the IDs stay usable)
        self.ids.move_path(&target, &new_target);
        for fork in self.forks.values_mut() {
            if fork.path == target {
                // The renamed file's forks carry the new wire name too
                fork.name = crate::name::to_mac(new_name.as_str()).0;
                fork.path = new_target.clone();
            } else if let Ok(rest) = fork.path.strip_prefix(&target) {
                // Forks of files inside a moved directory follow it
                fork.path = new_target.join(rest);
            }
        }
        if let Some(db) = &mut self.desktop {
            db.remove_appl(&target);
        }
        self.touch_volume();
        (ok(), Vec::new())
    }

    /// FPWrite: [fork ref][offset u32][request count u32]; the data
    /// arrives through the ASP Write's WriteContinue. The request's
    /// header byte is the Start/EndFlag: bit 7 means the offset is a
    /// signed distance from the end of the fork. The reply carries
    /// LastWritten: the offset just past the last byte written.
    fn cmd_write(&mut self, flag: u8, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(fork_ref) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(offset) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(count) = params.get(6..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let data_len = params.len().saturating_sub(10);
        if (count as usize) != data_len {
            return (err(ERR_PARAM), Vec::new());
        }
        let Some(fork) = self.forks.get_mut(&fork_ref) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if !fork.write {
            return (err(ERR_ACCESS_DENIED), Vec::new());
        }
        let mut offset = u64::from(offset);
        if flag & 0x80 != 0 {
            // The offset is a SIGNED distance from the end of the fork
            // (ch13: negative to indicate a byte within the fork; a
            // positive value writes past the end)
            let pos = fork.len as i64 + offset as i32 as i64;
            if pos < 0 {
                return (err(ERR_PARAM), Vec::new());
            }
            offset = pos as u64;
        } else if (offset as i32) < 0 {
            // A negative start-relative offset is an error
            return (err(ERR_PARAM), Vec::new());
        }
        let last_written = (offset + data_len as u64) as u32;
        let reply = last_written.to_be_bytes().to_vec();
        if fork.rsrc {
            // Resource forks write through the metadata backend
            let path = fork.path.clone();
            match self
                .settings
                .write_backend
                .write_rsrc(&path, offset, &params[10..])
            {
                Ok((new_len, rewritten)) => {
                    fork.len = new_len;
                    fork.written = true;
                    self.refresh_forks(&path, rewritten);
                }
                Err(_) => return (err(ERR_PARAM), Vec::new()),
            }
            self.touch_volume();
            return (ok(), reply);
        }
        // The data fork writes through the open handle; a fork that
        // grew past the container's recorded length is patched
        if fork
            .file
            .seek(SeekFrom::Start(fork.offset + offset))
            .is_err()
            || fork.file.write_all(&params[10..]).is_err()
        {
            return (err(ERR_PARAM), Vec::new());
        }
        let new_len = offset + params[10..].len() as u64;
        fork.written = true;
        if new_len > fork.len {
            if fork.offset > 0 {
                let path = fork.path.clone();
                let _ = self.settings.write_backend.patch_data_len(&path, new_len);
            }
            fork.len = new_len;
            // The growth must show in this fork's parameters and in
            // every other fork open on the file; a plain data write
            // replaces nothing, so the IDs and handles stay valid
            let path = fork.path.clone();
            self.bump_data_len(&path, new_len);
        }
        self.touch_volume();
        (ok(), reply)
    }

    /// FPSetForkParms: [fork ref][bitmap 2][fork len u32]
    fn cmd_set_fork_params(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(fork_ref) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(bitmap) = params.get(2..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(len) = params.get(4..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(fork) = self.forks.get_mut(&fork_ref) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if !fork.write {
            return (err(ERR_ACCESS_DENIED), Vec::new());
        }
        // The bitmap must carry exactly the fork-length bit of the
        // fork that is open: 0x0200 the data fork, 0x0400 the resource
        // fork
        let wanted = if fork.rsrc { 0x0400 } else { 0x0200 };
        if bitmap != wanted {
            return (err(ERR_BITMAP), Vec::new());
        }
        let len = u64::from(len);
        if fork.rsrc {
            let path = fork.path.clone();
            match self.settings.write_backend.truncate_rsrc(&path, len) {
                Ok((new_len, rewritten)) => {
                    fork.len = new_len;
                    self.refresh_forks(&path, rewritten);
                }
                Err(_) => return (err(ERR_PARAM), Vec::new()),
            }
        } else {
            if fork.file.set_len(fork.offset + len).is_err() {
                return (err(ERR_PARAM), Vec::new());
            }
            if fork.offset > 0 {
                let path = fork.path.clone();
                let _ = self.settings.write_backend.patch_data_len(&path, len);
            }
            fork.len = len;
            // The length change shows in every other fork open on the
            // file, without a metadata re-read or a rebind
            let path = fork.path.clone();
            self.bump_data_len(&path, len);
        }
        self.touch_volume();
        (ok(), Vec::new())
    }

    /// FPFlush / FPFlushFork: sync the open forks to disk
    fn cmd_flush(&self, cmd: u8, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let flush_all = cmd == CMD_FLUSH;
        for (fork_ref, fork) in &self.forks {
            if !flush_all && get_u16(params) != Some(*fork_ref) {
                continue;
            }
            if fork.file.sync_all().is_err() {
                return (err(ERR_PARAM), Vec::new());
            }
        }
        (ok(), Vec::new())
    }

    /// FPSetVolParms: the volume dates are ignored (they stay unknown)
    fn cmd_set_vol_params(&self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        if self.settings.read_only {
            return (err(ERR_VOL_LOCKED), Vec::new());
        }
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        (ok(), Vec::new())
    }

    /// FPSetFileDirParms: [vol][dir id][file bitmap][dir bitmap]
    /// [path name][parameters]; the settable bits are the attributes
    /// (0), the dates (2-4) and the Finder information (5)
    fn cmd_set_file_dir_params(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        if self.settings.read_only {
            return (err(ERR_VOL_LOCKED), Vec::new());
        }
        const SETTABLE: u16 = 0x003D; // bits 0, 2, 3, 4, 5
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(dir_id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(bitmap) = params.get(6..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if bitmap & !SETTABLE != 0 {
            return (err(ERR_BITMAP), Vec::new());
        }
        let Some((name, mut rest)) = params.get(8..).and_then(get_long_path) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(target) = self.resolve_path(dir_id, &name) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        // The parameter values start at the next even offset after the
        // path name (the wire convention)
        let consumed = params.len() - rest.len();
        if consumed % 2 == 1 {
            rest = rest.get(1..).unwrap_or(&[]);
        }
        // The parameter values in ascending bit order
        let current = self.meta_open(&target);
        let mut attrs = current.attrs;
        let (mut create, mut modify, mut backup) =
            (current.create_date, current.mod_date, current.backup_date);
        let mut finder_info = current.finder_info;
        let mut attrs_wanted = false;
        let mut dates_wanted = false;
        let mut modify_set = false;
        let mut pos = rest;
        for bit in 0..16 {
            if bitmap & (1 << bit) == 0 {
                continue;
            }
            match bit {
                0 => {
                    let Some(v) = get_u16(pos) else {
                        return (err(ERR_PARAM), Vec::new());
                    };
                    pos = &pos[2..];
                    // Bit 15 is Set/Clear: 1 sets the given bits, 0
                    // clears them. The already-open bits (3, 4) can
                    // never be requested and the file-only bits (5, 6)
                    // do not apply to directories.
                    let is_dir = std::fs::metadata(&target).is_ok_and(|m| m.is_dir());
                    let mask = 0x0018 | if is_dir { 0x0060 } else { 0 };
                    let change = v & 0x7FFF & !mask;
                    attrs = if v & 0x8000 != 0 {
                        attrs | change
                    } else {
                        attrs & !change
                    };
                    attrs_wanted = true;
                }
                2..=4 => {
                    // A zero date means "no change" (the convention
                    // the classic clients send)
                    let Some(v) = get_u32(pos) else {
                        return (err(ERR_PARAM), Vec::new());
                    };
                    pos = &pos[4..];
                    dates_wanted = true;
                    if v != 0 {
                        match bit {
                            2 => create = v,
                            3 => {
                                modify = v;
                                modify_set = true;
                            }
                            _ => backup = v,
                        }
                    }
                }
                5 => {
                    if pos.len() < 32 {
                        return (err(ERR_PARAM), Vec::new());
                    }
                    finder_info.copy_from_slice(&pos[..32]);
                    pos = &pos[32..];
                }
                _ => return (err(ERR_PARAM), Vec::new()),
            }
        }
        // A backend rewrite (an AppleSingle rewrite or conversion)
        // replaced the file: the IDs and the handles must follow
        let mut rewritten = false;
        if attrs_wanted && attrs != current.attrs {
            match self.settings.write_backend.set_attrs(&target, attrs) {
                Ok(r) => rewritten |= r,
                Err(_) => return (err(ERR_PARAM), Vec::new()),
            }
        }
        if dates_wanted {
            match self
                .settings
                .write_backend
                .set_dates(&target, create, modify, backup)
            {
                Ok(r) => rewritten |= r,
                Err(_) => return (err(ERR_PARAM), Vec::new()),
            }
        }
        if finder_info != current.finder_info {
            match self
                .settings
                .write_backend
                .set_finder_info(&target, &finder_info)
            {
                Ok(r) => rewritten |= r,
                Err(_) => return (err(ERR_PARAM), Vec::new()),
            }
        }
        if modify_set {
            // Close must not stamp a date the client set while the
            // fork was open
            for fork in self.forks.values_mut() {
                if fork.path == target {
                    fork.date_set_by_client = true;
                }
            }
        }
        self.touch_volume();
        self.refresh_forks(&target, rewritten);
        let meta = self.meta_open(&target);
        let parent_id = self.parent_id_of(&target);
        let fork_name = self.mac_name_of(&target);
        if let Some(db) = &mut self.desktop {
            db.register_appl(&target, &fork_name, &meta, parent_id);
        }
        (ok(), Vec::new())
    }

    /// FPCreateID: [vol][dir id][path name] → the file ID (the
    /// identity table's ID, minted on first contact)
    fn cmd_create_id(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(dir_id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some((name, _)) = params.get(6..).and_then(get_long_path) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(target) = self.resolve_path(dir_id, &name) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        if std::fs::metadata(&target).is_ok_and(|m| m.is_dir()) {
            return (err(ERR_OBJECT_TYPE), Vec::new());
        }
        // The identity table mints the ID on first contact and keeps
        // it stable thereafter
        let Some(id) = self.ids.id_of(&target) else {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        };
        (ok(), id.to_be_bytes().to_vec())
    }

    /// FPDeleteID: [vol][file id] — invalidate a file ID
    fn cmd_delete_id(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        // Only a file's ID can be invalidated: a directory's ID is the
        // server's own naming, an unknown or replaced ID answers
        // afpIDNotFound
        let Some(path) = self.ids.verified_path(id).map(|p| p.to_path_buf()) else {
            return (err(ERR_ID_NOT_FOUND), Vec::new());
        };
        match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => return (err(ERR_OBJECT_TYPE), Vec::new()),
            Ok(_) => {}
            Err(_) => return (err(ERR_ID_NOT_FOUND), Vec::new()),
        }
        self.ids.remove_id(id);
        (ok(), Vec::new())
    }

    /// FPResolveID: [vol][file id][file bitmap] → the file parameters
    fn cmd_resolve_id(&mut self, params: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let Some(volume_id) = get_u16(params) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if volume_id != VOLUME_ID {
            return (err(ERR_OBJECT_NOT_FOUND), Vec::new());
        }
        let Some(id) = params.get(2..).and_then(get_u32) else {
            return (err(ERR_PARAM), Vec::new());
        };
        let Some(bitmap) = params.get(6..).and_then(get_u16) else {
            return (err(ERR_PARAM), Vec::new());
        };
        if bitmap & !FILE_BITMAP_ALLOWED != 0 {
            return (err(ERR_BITMAP), Vec::new());
        }
        // The recorded path must still hold the recorded file: a host
        // save-by-rename replaces the inode, and the old ID must fail,
        // never alias the replacement
        let Some(target) = self.ids.verified_path(id).map(|p| p.to_path_buf()) else {
            return (err(ERR_BAD_ID), Vec::new());
        };
        // A directory's ID is not a file ID
        if std::fs::metadata(&target).is_ok_and(|m| m.is_dir()) {
            return (err(ERR_OBJECT_TYPE), Vec::new());
        }
        let mut meta = self.meta_open(&target);
        meta.attrs |= self.open_attrs(&target);
        let name = self.mac_name_of(&target);
        let parent_id = self.parent_id_of(&target);
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        // The resolved ID itself is the file number (the CNID)
        reply.extend_from_slice(&file_params(bitmap, &name, &meta, parent_id, id));
        (ok(), reply)
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
        log::debug!("AFP: desktop scan started on '{}'", root.display());
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
                        db.merge_group(group, dir_id, &mut self.mangles);
                    }
                    let (appls, icons) = self
                        .desktop
                        .as_ref()
                        .map(|db| (db.application_count(), db.icon_count()))
                        .unwrap_or((0, 0));
                    log::debug!(
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
                    log::debug!(
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

/// Convert a MacRoman Pascal string from the wire to a UTF-8 name
fn get_pstring(data: &[u8]) -> Option<(String, &[u8])> {
    let len = usize::from(*data.first()?);
    if data.len() < len + 1 {
        return None;
    }
    let (s, _, _) = encoding_rs::MACINTOSH.decode(&data[1..=len]);
    Some((s.into_owned(), &data[len + 1..]))
}

/// The AFP 2.x path name: [path type][Pascal string] (the same shape
/// FPGetFileDirParms uses). The path type is ignored (every type
/// addresses the same volume).
fn get_long_path(data: &[u8]) -> Option<(String, &[u8])> {
    let len = usize::from(*data.get(1)?);
    let end = 2 + len;
    if data.len() < end {
        return None;
    }
    let (s, _, _) = encoding_rs::MACINTOSH.decode(&data[2..end]);
    Some((s.into_owned(), &data[end..]))
}

/// Rename a host file without replacing an existing destination (the
/// AFP semantics: an existing name answers afpObjectExists).
/// `std::fs::rename` silently replaces on Linux, which would be data
/// loss; the check-then-rename fallback is the non-atomic case.
#[cfg(target_os = "linux")]
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes()).unwrap_or_default();
    let to = CString::new(to.as_os_str().as_bytes()).unwrap_or_default();
    // SAFETY: valid NUL-terminated paths and constants
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes()).unwrap_or_default();
    let to = CString::new(to.as_os_str().as_bytes()).unwrap_or_default();
    // SAFETY: valid NUL-terminated paths and constants
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
    }
    std::fs::rename(from, to)
}

/// Open a file with the requested write access
fn open_with_access(path: &Path, write: bool) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(write)
        .open(path)
}

fn get_u16(data: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes([*data.first()?, *data.get(1)?]))
}

fn get_u32(data: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes([
        *data.first()?,
        *data.get(1)?,
        *data.get(2)?,
        *data.get(3)?,
    ]))
}

/// The volume parameters requested by a bitmap: the fixed parameters in
/// bitmap order, the variable one (the volume name) at the end
///
/// Bit 0: attributes, bit 1: signature, bit 2: creation date, bit 3:
/// modification date, bit 4: backup date, bit 5: volume ID, bit 6:
/// bytes free, bit 7: bytes total, bit 8: volume name, bit 9 (0x0200):
/// extended bytes free (8 bytes), bit 10 (0x0400): extended bytes
/// total (8 bytes), bit 11 (0x0800): block size
fn vol_params(
    bitmap: u16,
    name: &str,
    root: &Path,
    read_only: bool,
    supports_file_ids: bool,
    vol_modified: u32,
) -> Vec<u8> {
    let (bytes_free, bytes_total) = statvfs(root);

    let mut fixed = Vec::new();
    if bitmap & 0x0001 != 0 {
        // Attributes: bit 0 = read-only, bit 2 = supports file IDs
        // (an AFP 2.1 session and up)
        let mut attrs = if read_only { 1u16 } else { 0u16 };
        if supports_file_ids {
            attrs |= 0x0004;
        }
        fixed.extend_from_slice(&attrs.to_be_bytes());
    }
    if bitmap & 0x0002 != 0 {
        // Signature: 2 = a volume with fixed Directory IDs
        fixed.extend_from_slice(&2u16.to_be_bytes());
    }
    if bitmap & 0x0004 != 0 {
        fixed.extend_from_slice(&0u32.to_be_bytes()); // creation date
    }
    if bitmap & 0x0008 != 0 {
        // The newer of the server's own write stamp and the root
        // mtime (host-side changes deeper in the tree still need the
        // inotify/kqueue work from the state notes to surface here).
        // The AFP dates are signed: the comparison must be too, or a
        // pre-2000 root mtime would always win as a u32
        let modify = (vol_modified as i32).max(host_mtime_date(root) as i32);
        fixed.extend_from_slice(&(modify as u32).to_be_bytes());
    }
    if bitmap & 0x0010 != 0 {
        fixed.extend_from_slice(&0u32.to_be_bytes()); // backup date
    }
    if bitmap & 0x0020 != 0 {
        fixed.extend_from_slice(&VOLUME_ID.to_be_bytes());
    }
    if bitmap & 0x0040 != 0 {
        // The legacy 32-bit byte counts clamp at 4 GiB (the 2.2
        // extended bits below carry the real values)
        fixed.extend_from_slice(&clamp32(bytes_free).to_be_bytes());
    }
    if bitmap & 0x0080 != 0 {
        fixed.extend_from_slice(&clamp32(bytes_total).to_be_bytes());
    }
    // The name-offset field (bit 8) comes before the 2.2 extended
    // fields in bitmap order
    let mut name_off_pos = None;
    if bitmap & 0x0100 != 0 {
        // Placeholder for the offset to the volume name
        name_off_pos = Some(fixed.len());
        fixed.extend_from_slice(&0u16.to_be_bytes());
    }
    // The AFP 2.2 extended volume-size bits: the 64-bit values
    if bitmap & 0x0200 != 0 {
        fixed.extend_from_slice(&bytes_free.to_be_bytes());
    }
    if bitmap & 0x0400 != 0 {
        fixed.extend_from_slice(&bytes_total.to_be_bytes());
    }
    if bitmap & 0x0800 != 0 {
        fixed.extend_from_slice(&4096u32.to_be_bytes()); // block size
    }
    let name_offset = fixed.len() as u16;
    if let Some(pos) = name_off_pos {
        fixed[pos..pos + 2].copy_from_slice(&name_offset.to_be_bytes());
    }
    let mut reply = Vec::new();
    reply.extend_from_slice(&bitmap.to_be_bytes());
    reply.extend_from_slice(&fixed);
    if bitmap & 0x0100 != 0 {
        reply.extend_from_slice(&pstring(name));
    }
    reply
}

/// Clamp a 64-bit byte count to the legacy 32-bit volume fields
fn clamp32(v: u64) -> u32 {
    v.min(u64::from(u32::MAX)) as u32
}

/// The bytes free and the bytes total of the file system holding the
/// volume root (0, 0 when unknown)
#[cfg(unix)]
#[allow(clippy::useless_conversion)] // the field widths differ across platforms
fn statvfs(path: &Path) -> (u64, u64) {
    let mut buf = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();
    // SAFETY: buf is a valid statvfs buffer and c_path is a valid
    // null-terminated path
    if unsafe { libc::statvfs(c_path.as_ptr(), buf.as_mut_ptr()) } != 0 {
        return (0, 0);
    }
    // SAFETY: statvfs initialized the buffer on success
    let buf = unsafe { buf.assume_init() };
    // The field widths differ across platforms (macOS has 32-bit
    // block counts and a 64-bit fragment size), so widen explicitly
    let block = u64::from(buf.f_frsize);
    (
        u64::from(buf.f_bavail).saturating_mul(block),
        u64::from(buf.f_blocks).saturating_mul(block),
    )
}

#[cfg(not(unix))]
fn statvfs(_path: &Path) -> (u64, u64) {
    (0, 0)
}

/// Pack the parameters of a file or directory in bitmap order (bit 0
/// first). The variable-length name fields (bits 6 and 7, the long and
/// short names) are represented by offsets from the start of the
/// parameters, with the names packed after all fixed parameters. The
/// closure returns the bytes of each fixed parameter; for bits 6 and 7
/// it returns the name to append.
fn pack_params<F>(bitmap: u16, mut param: F) -> Vec<u8>
where
    F: FnMut(usize) -> Vec<u8>,
{
    let mut out = Vec::new();
    let mut name_offsets: Vec<usize> = Vec::new();
    let mut names: Vec<Vec<u8>> = Vec::new();
    for bit in 0..16 {
        if bitmap & (1 << bit) == 0 {
            continue;
        }
        if bit == 6 || bit == 7 {
            // A placeholder for the offset to this name
            out.extend_from_slice(&0u16.to_be_bytes());
            name_offsets.push(out.len() - 2);
            names.push(param(bit));
        } else {
            out.extend_from_slice(&param(bit));
        }
    }
    // The names follow the fixed parameters; patch the placeholders at
    // their recorded positions
    let mut pos = out.len();
    for (i, off_pos) in name_offsets.iter().enumerate() {
        out[*off_pos..*off_pos + 2].copy_from_slice(&(pos as u16).to_be_bytes());
        pos += names[i].len();
    }
    for name in names {
        out.extend_from_slice(&name);
    }
    out
}

/// The bits of the file bitmap a 2.0 server can return
const FILE_BITMAP_ALLOWED: u16 = 0x27FF; // bits 0-10 and 13
/// The bits of the directory bitmap a 2.0 server can return
const DIR_BITMAP_ALLOWED: u16 = 0x3FFF; // bits 0-13

/// The file parameters requested by a bitmap
fn file_params(bitmap: u16, name: &str, meta: &Meta, parent_id: u32, file_id: u32) -> Vec<u8> {
    pack_params(bitmap, |bit| match bit {
        0 => meta.attrs.to_be_bytes().to_vec(), // attributes
        1 => parent_id.to_be_bytes().to_vec(),
        2 => date(meta.create_date).to_be_bytes().to_vec(),
        3 => date(meta.mod_date).to_be_bytes().to_vec(),
        4 => date(meta.backup_date).to_be_bytes().to_vec(),
        5 => finder_info(meta), // FInfo + FXInfo
        6 => pstring(name),     // long name
        7 => pstring(&short_name(name)),
        8 => file_id.to_be_bytes().to_vec(),
        9 => (meta.data_len.min(0x7FFF_FFFF) as u32)
            .to_be_bytes()
            .to_vec(),
        10 => meta.rsrc_fork_len.to_be_bytes().to_vec(),
        13 => vec![0; 6], // ProDOS info
        _ => Vec::new(),  // filtered by the allowed mask
    })
}

/// The directory parameters requested by a bitmap
fn dir_params(
    bitmap: u16,
    name: &str,
    meta: &Meta,
    offspring: u16,
    self_id: u32,
    parent_id: u32,
    read_only: bool,
) -> Vec<u8> {
    pack_params(bitmap, |bit| match bit {
        0 => meta.attrs.to_be_bytes().to_vec(), // attributes
        1 => parent_id.to_be_bytes().to_vec(),
        2 => date(meta.create_date).to_be_bytes().to_vec(),
        3 => date(meta.mod_date).to_be_bytes().to_vec(),
        4 => date(meta.backup_date).to_be_bytes().to_vec(),
        5 => finder_info(meta), // DInfo + DXInfo
        6 => pstring(name),
        7 => pstring(&short_name(name)),
        8 => self_id.to_be_bytes().to_vec(),
        9 => offspring.to_be_bytes().to_vec(),
        10 => 0u32.to_be_bytes().to_vec(), // owner ID
        11 => 0u32.to_be_bytes().to_vec(), // group ID
        12 => access_rights(read_only),
        13 => vec![0; 6], // ProDOS info
        _ => Vec::new(),  // filtered by the allowed mask
    })
}

/// The 4-byte access rights long, big-endian on the wire:
/// [UARights][world][group][owner], each byte bit 0 = search,
/// bit 1 = read, bit 2 = write (make changes). The UARights owner bit
/// is set because the volume is not owned by any registered user; a
/// read-only volume strips the write bits everywhere, and the client
/// checks them before allowing a New Folder or a drop.
fn access_rights(read_only: bool) -> Vec<u8> {
    if read_only {
        0x8303_0303u32.to_be_bytes().to_vec()
    } else {
        0x8707_0707u32.to_be_bytes().to_vec()
    }
}

/// A short (12-character) form of a name, used for the Short Name
/// parameter when requested
fn short_name(name: &str) -> String {
    name.chars().take(12).collect()
}

/// The Finder Info parameter: 32 bytes (FInfo+FXInfo / DInfo+DXInfo)
/// in both AFP 1.1 and AFP 2.0
fn finder_info(meta: &Meta) -> Vec<u8> {
    meta.finder_info.to_vec()
}

/// The number of offspring, honoring the server's hidden-file mode
fn visible_offspring(dir: &Path, mode: HiddenFiles) -> u16 {
    std::fs::read_dir(dir).map_or(0, |d| {
        d.flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                (mode != HiddenFiles::Hide || !name.starts_with('.'))
                    && !crate::name::is_meta_name(&name)
            })
            .count()
            .min(65535)
    }) as u16
}

/// An AFP 2.0 date for the wire (signed seconds since 2000-01-01):
/// 0 is a valid date (2000-01-01) and 0x80000000 is the spec's
/// "never" value — both are sent as stored (backup software relies on
/// the never value to skip files)
fn date(value: u32) -> u32 {
    value
}

/// A host file's mtime as an AFP date (signed seconds since
/// 2000-01-01); 0 when the file cannot be stat'ed
fn host_mtime_date(path: &Path) -> u32 {
    if let Ok(md) = std::fs::metadata(path)
        && let Ok(modified) = md.modified()
        && let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH)
    {
        (d.as_secs() as i64 - 946684800) as i32 as u32
    } else {
        0
    }
}

/// The current time as signed seconds since 2000-01-01 (the AFP
/// date-time epoch)
fn afp_now() -> u32 {
    let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return 0;
    };
    // The 2000 epoch is 946684800 seconds after the Unix epoch
    (d.as_secs().saturating_sub(946684800)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an AppleDouble container (version 2) holding the given
    /// entries
    fn apple_double(entries: &[(u32, &[u8])]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&[0x00, 0x05, 0x16, 0x07]); // magic
        data.extend_from_slice(&0x00020000u32.to_be_bytes()); // version
        data.extend_from_slice(&[0; 16]);
        data.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        let mut body = 26 + 12 * entries.len();
        let mut table = Vec::new();
        for (id, bytes) in entries {
            table.push((*id, body, bytes.len()));
            body += bytes.len();
        }
        for (id, offset, len) in &table {
            data.extend_from_slice(&id.to_be_bytes());
            data.extend_from_slice(&(*offset as u32).to_be_bytes());
            data.extend_from_slice(&(*len as u32).to_be_bytes());
        }
        for (_, bytes) in entries {
            data.extend_from_slice(bytes);
        }
        data
    }

    /// A server rooted at a fresh temporary directory
    pub(super) fn test_server(name: &str) -> (Afp, PathBuf) {
        let dir = std::env::temp_dir().join(format!("snow-afp-{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Afp::with_root(dir.clone()), dir)
    }

    pub(super) fn result_of(reply: (Vec<u8>, Vec<u8>)) -> (i32, Vec<u8>) {
        (i32::from_be_bytes(reply.0.try_into().unwrap()), reply.1)
    }

    #[test]
    fn test_date_never_value_and_pre_2000() {
        // A pre-2000 date (negative on the wire) is reported verbatim
        let afp_1995 = (-157_785_600i32) as u32;
        assert_eq!(date(afp_1995), afp_1995);
        // 0 is a valid date: 2000-01-01
        assert_eq!(date(0), 0);
        // The never value passes through as stored (backup software
        // relies on it to skip files)
        assert_eq!(date(0x8000_0000), 0x8000_0000);
    }

    #[test]
    fn test_dir_entry_folds_nfc_and_case() {
        let dir = std::env::temp_dir().join("snow-dir-entry");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("re\u{301}sume\u{301}.txt"), b"x").unwrap();
        let (server, _) = test_server("dir-entry");
        assert_eq!(
            server.dir_entry(&dir, "RÉSUMÉ.txt"),
            Some(dir.join("re\u{301}sume\u{301}.txt"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_vol_params_layout() {
        let vol = vol_params(0x0120, "Snow", Path::new("/"), true, true, 0);
        // Bitmap, then the volume ID, then the name offset, then "Snow"
        assert_eq!(
            &vol[..10],
            &[0x01, 0x20, 0x00, 0x01, 0x00, 0x04, 4, b'S', b'n', b'o']
        );
        assert_eq!(&vol[10..], b"w");
    }

    #[test]
    fn test_vol_params_name_offset_precedes_extended_fields() {
        // Bits 8 (name), 10 (extended total) and 11 (block size): the
        // name-offset field comes first in bitmap order, the name
        // follows all the fixed fields at the recorded offset
        let vol = vol_params(0x0D00, "Snow", Path::new("/"), true, true, 0);
        assert_eq!(&vol[..2], &[0x0D, 0x00]);
        assert_eq!(&vol[2..4], &14u16.to_be_bytes()); // the name offset
        assert_eq!(&vol[12..16], &4096u32.to_be_bytes()); // block size
        assert_eq!(&vol[16..21], &[4, b'S', b'n', b'o', b'w']); // "Snow"
    }

    #[test]
    fn test_open_vol_spec_layout() {
        let (mut server, _dir) = test_server("open-vol");
        // [OpenVol][0][bitmap 0x0120]["Snow"]
        let data = [24, 0, 0x01, 0x20, 4, b'S', b'n', b'o', b'w'];
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(&reply[..6], &[0x01, 0x20, 0x00, 0x01, 0x00, 0x04]);
        assert_eq!(&reply[7..11], b"Snow");
    }

    #[test]
    fn test_enumerate_spec_layout() {
        let (mut server, dir) = test_server("enumerate");
        std::fs::write(dir.join("hello.txt"), b"hello").unwrap();
        std::fs::create_dir(dir.join("subdir")).unwrap();
        // [Enumerate][0][vol 1][dir 2][file bm 0x0340][dir bm 0x0340]
        // [count 20][start 1][max 512][path type 2][path ""]
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[0..2], [0x03, 0x40]); // file bitmap
        assert_eq!(reply[2..4], [0x03, 0x40]); // directory bitmap
        assert_eq!(reply[4..6], [0, 2]); // two entries

        // First record: hello.txt, a file. The long-name offset comes
        // first (bit 6), then the file number and the data fork length.
        let rec0_len = reply[6] as usize;
        let rec0 = &reply[6..6 + rec0_len];
        assert_eq!(rec0[1], 0); // a file
        let off0 = u16::from_be_bytes([rec0[2], rec0[3]]) as usize;
        assert_eq!(off0, 10);
        assert_eq!(&rec0[2 + off0..2 + off0 + 10], b"\x09hello.txt");
        let file_num = u32::from_be_bytes(rec0[4..8].try_into().unwrap());
        // The file number is the file's ID from the identity table
        // (minted, stable), not a name hash
        assert_ne!(file_num, 0);
        assert_eq!(file_num, server.ids.id_of(&dir.join("hello.txt")).unwrap());
        let data_len = u32::from_be_bytes(rec0[8..12].try_into().unwrap());
        assert_eq!(data_len, 5);

        // Second record: subdir, a directory. Its own Directory ID
        // (not the root's), offspring count and the long name.
        let rec1 = &reply[6 + rec0_len..];
        assert_eq!(rec1.len(), 18);
        assert_eq!(rec1[1], dir_flag());
        let sub_id = server.dir_id(&dir.join("subdir"));
        assert_eq!(rec1[4..8], sub_id.to_be_bytes()); // directory ID
        assert_ne!(sub_id, ROOT_DIR_ID);
        let off1 = u16::from_be_bytes([rec1[2], rec1[3]]) as usize;
        assert_eq!(off1, 8);
        assert_eq!(&rec1[2 + off1..2 + off1 + 7], b"\x06subdir");
    }

    #[test]
    fn test_enumerate_subdirectory_by_id() {
        // A directory opened by its Directory ID enumerates its own
        // offspring, not the root's
        let (mut server, dir) = test_server("enumerate-sub");
        std::fs::write(dir.join("top.txt"), b"t").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("inner.txt"), b"i").unwrap();
        let sub_id = server.dir_id(&dir.join("sub"));

        // [Enumerate][0][vol 1][dir = sub_id][file bm 0x0340]
        // [dir bm 0x0340][count 20][start 1][max 512][ptype 2][path ""]
        let mut data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        data[4..8].copy_from_slice(&sub_id.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[4..6], [0, 1]); // one entry
        let rec_len = reply[6] as usize;
        assert_eq!(reply[7], 0); // a file
        let off = u16::from_be_bytes([reply[8], reply[9]]) as usize;
        assert_eq!(&reply[6 + 2 + off..6 + 2 + off + 10], b"\x09inner.txt");
        let _ = rec_len;

        // A GetFileDirParms on the same ID resolves the directory too
        // (null bitmaps: just the bitmaps and the flag come back)
        let mut g = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 0];
        g[4..8].copy_from_slice(&sub_id.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &g));
        assert_eq!(result, 0);
        assert_eq!(reply[4], 0x80); // a directory
    }

    #[test]
    fn test_nul_separated_path_grammar() {
        let (mut server, dir) = test_server("nul-paths");
        std::fs::create_dir(dir.join("a")).unwrap();
        std::fs::create_dir(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("a").join("b").join("f.txt"), b"x").unwrap();
        std::fs::write(dir.join("a").join("f.txt"), b"y").unwrap();
        // A single leading NUL is ignored: the path stays relative to
        // the Directory ID (ch13's rule, not "from the root")
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 10];
        data.extend_from_slice(b"\0a\0b\0f.txt");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        // Two consecutive NULs ascend one level: from a/b, "\0\0f.txt"
        // is a/f.txt
        let b_id = server.dir_id(&dir.join("a").join("b"));
        let mut data = vec![34, 0, 0, 1];
        data.extend_from_slice(&b_id.to_be_bytes());
        data.extend_from_slice(&[0, 0, 0, 0, 2, 7]);
        data.extend_from_slice(b"\0\0f.txt");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        // Ascending above the root answers ObjectNotFound
        let a_id = server.dir_id(&dir.join("a"));
        let mut data = vec![34, 0, 0, 1];
        data.extend_from_slice(&a_id.to_be_bytes());
        data.extend_from_slice(&[0, 0, 0, 0, 2, 8]);
        data.extend_from_slice(b"\0\0\0x.txt");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, ERR_OBJECT_NOT_FOUND);
        // Directory ID 1 plus the volume name as the first element
        // addresses the root
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 2, 12];
        data.extend_from_slice(b"Snow\0a\0f.txt");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        // A colon is not part of the path syntax: one name, not a
        // separator
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 9];
        data.extend_from_slice(b"a:b:f.txt");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, ERR_OBJECT_NOT_FOUND);
        // Parent traversal as names stays rejected
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 9];
        data.extend_from_slice(b"a\0..\0x.txt");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, ERR_OBJECT_NOT_FOUND);
    }

    #[test]
    fn test_metadata_store_hidden_in_every_mode() {
        // Even with dotfiles shown, the server's own metadata is not
        // addressable or enumerable
        let dir = std::env::temp_dir().join("snow-afp-meta-hidden");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".secret"), b"s").unwrap();
        std::fs::create_dir(dir.join(".AppleDouble")).unwrap();
        std::fs::write(dir.join(".AppleDouble").join("x"), b"m").unwrap();
        let mut server = Afp::new_with(
            Some(dir.clone()),
            None,
            MaxVersion::default(),
            AfpSettings {
                hidden_files: HiddenFiles::Show,
                ..AfpSettings::default()
            },
        );
        // The dotfile is addressable in Show mode, the metadata is not
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 7];
        data.extend_from_slice(b".secret");
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 12];
        data.extend_from_slice(b".AppleDouble");
        assert_eq!(result_of(server.command(1, &data)).0, ERR_OBJECT_NOT_FOUND);
        // Only the dotfile enumerates
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(reply[4..6], [0, 1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_open_fork_sets_already_open_bits() {
        let (mut server, dir) = test_server("open-bits");
        std::fs::write(dir.join("f.txt"), b"x").unwrap();
        // [GetFileDirParms][0][vol 1][dir 2][file bm 0x0001][dir bm 0]
        // [ptype 2][path]
        let mut gfd = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x01, 0, 0, 2, 5];
        gfd.extend_from_slice(b"f.txt");
        let (r, reply) = result_of(server.command(1, &gfd));
        assert_eq!(r, 0);
        assert_eq!(reply[6..8], [0, 0]); // no open forks: attrs = 0
        // Opening the data fork sets DAlreadyOpen
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 5];
        data.extend_from_slice(b"f.txt");
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let (r, reply) = result_of(server.command(1, &gfd));
        assert_eq!(r, 0);
        assert_eq!(reply[6..8], [0, 8]);
        // Opening the resource fork adds RAlreadyOpen
        let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 5];
        data.extend_from_slice(b"f.txt");
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let (r, reply) = result_of(server.command(1, &gfd));
        assert_eq!(r, 0);
        assert_eq!(reply[6..8], [0, 8 | 0x10]);
    }

    #[test]
    fn test_root_parent_is_one() {
        let (mut server, dir) = test_server("parent-chain");
        std::fs::create_dir(dir.join("child")).unwrap();
        // [GetFileDirParms][0][vol 1][dir 2][file bm 0][dir bm 0x0002
        // (parent ID)][path type 2][path ""] — the root's parent is the
        // reserved ID 1, where the client's parent walk terminates
        let data = [34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0x00, 0x02, 2, 0];
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[4], 0x80); // a directory
        assert_eq!(reply[6..10], [0, 0, 0, 1]); // parent = 1
        // A first-level directory's parent is the root (ID 2)
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0x00, 0x02, 2, 5];
        data.extend_from_slice(b"child");
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[6..10], [0, 0, 0, 2]);
    }

    #[test]
    fn test_desktop_database_commands() {
        let (mut server, dir) = test_server("desktop-db");
        // An APPL file whose AppleDouble sidecar carries its Finder
        // info and a resource fork with a BNDL/FREF/ICN# bundle
        std::fs::write(dir.join("App"), b"x").unwrap();
        std::fs::create_dir(dir.join(".AppleDouble")).unwrap();
        let mut finder = Vec::new();
        finder.extend_from_slice(b"APPL"); // file type
        finder.extend_from_slice(b"TEST"); // creator
        finder.extend_from_slice(&[0u8; 24]);
        let fork = crate::rfork::test_support::fork(
            &crate::rfork::test_support::bndl_with_fref(),
            Some(&crate::rfork::test_support::fref_entries()),
        );
        let container = apple_double(&[(2, &fork), (9, &finder)]);
        std::fs::write(dir.join(".AppleDouble").join("App"), container).unwrap();

        // FPOpenDT hands out the desktop database reference number
        let (r, reply) = result_of(server.command(1, &[48, 0, 0, 1]));
        assert_eq!(r, 0);
        assert_eq!(reply, [0, 1]);

        // FPGetFileDirParms on the application registers it in the
        // desktop database: the tables are fed by the files the client
        // asks about
        let mut gfd = vec![34, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 2, 3];
        gfd.extend_from_slice(b"App");
        let (r, _) = result_of(server.command(1, &gfd));
        assert_eq!(r, 0);

        // FPGetAPPL: creator 'TEST', index 0, bitmap 0x0340 (long name,
        // file number, data fork length)
        let mut cmd = vec![55, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(&[0, 0]);
        cmd.extend_from_slice(&0x0340u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert_eq!(reply[0..2], 0x0340u16.to_be_bytes());
        assert_eq!(reply[6..8], [0, 10]); // the long name offset
        assert_eq!(&reply[16..20], &[3, b'A', b'p', b'p']);
        assert_eq!(reply[12..16], [0, 0, 0, 1]); // the data fork length

        // FPGetIconInfo: creator 'TEST', index 0 — the APPL icon
        let mut cmd = vec![52, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(&[0, 0]);
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert_eq!(&reply[0..4], b"APPL"); // the icon tag
        assert_eq!(&reply[4..8], b"APPL"); // the file type
        assert_eq!(reply[8], 3); // large 8-bit color, the best available
        assert_eq!(reply[9], 0); // reserved
        assert_eq!(reply[10..12], [0x04, 0x00]); // 1024 bytes

        // FPGetIcon: 'TEST'/'APPL', IconType 1 — the icon bitmap
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[1, 0]);
        cmd.extend_from_slice(&256u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert_eq!(reply.len(), 256);
        assert_eq!(reply[0], 0x11);

        // A request for only the first 128 bytes of the icon
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[1, 0]);
        cmd.extend_from_slice(&128u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert_eq!(reply.len(), 128);

        // A zero length only tests for the icon's presence
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[1, 0, 0, 0]);
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert!(reply.is_empty());

        // FPGetIcon for the 8-bit color icon (IconType 3)
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[3, 0]);
        cmd.extend_from_slice(&1024u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert_eq!(reply.len(), 1024);
        assert_eq!(reply[0], 0x33);

        // FPGetIcon for the 4-bit color icon (IconType 2)
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[2, 0]);
        cmd.extend_from_slice(&512u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert_eq!(reply.len(), 512);
        assert_eq!(reply[0], 0x55);

        // FPGetIcon for the small 4-bit icon (IconType 4)
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[4, 0]);
        cmd.extend_from_slice(&128u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert_eq!(reply.len(), 128);
        assert_eq!(reply[0], 0x99);

        // FPGetIcon for the small monochrome icon (IconType 5)
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[5, 0]);
        cmd.extend_from_slice(&64u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        assert_eq!(reply.len(), 64);
        assert_eq!(reply[0], 0xbb);

        // An IconType the server has nothing for
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[9, 0]);
        cmd.extend_from_slice(&64u16.to_be_bytes());
        let (r, _) = result_of(server.command(1, &cmd));
        assert_eq!(r, ERR_OBJECT_NOT_FOUND);

        // FPGetAPPL past the end of the mappings
        let mut cmd = vec![55, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(&[0, 2]);
        cmd.extend_from_slice(&0x0340u16.to_be_bytes());
        let (r, _) = result_of(server.command(1, &cmd));
        assert_eq!(r, ERR_OBJECT_NOT_FOUND);

        // FPGetIconInfo past the end
        let mut cmd = vec![52, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(&[0, 9]);
        let (r, _) = result_of(server.command(1, &cmd));
        assert_eq!(r, ERR_OBJECT_NOT_FOUND);

        // FPGetIcon for a creator with no icons
        let mut cmd = vec![51, 0, 0, 1];
        cmd.extend_from_slice(b"NOPE");
        cmd.extend_from_slice(b"APPL");
        cmd.extend_from_slice(&[1, 0]);
        cmd.extend_from_slice(&256u16.to_be_bytes());
        let (r, _) = result_of(server.command(1, &cmd));
        assert_eq!(r, ERR_OBJECT_NOT_FOUND);

        // FPAddAPPL is refused on the read-only volume
        let mut cmd = vec![53, 0, 0, 1, 0, 0, 0, 2];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(&[0, 0, 0, 0, 2, 0]);
        let (r, _) = result_of(server.command(1, &cmd));
        assert_eq!(r, ERR_VOL_LOCKED);

        // A bad DTRefNum is a parameter error
        let (r, _) = result_of(server.command(1, &[52, 0, 0, 9, b'T', b'E', b'S', b'T', 0, 0]));
        assert_eq!(r, ERR_PARAM);

        // A volume the server does not have cannot be opened
        let (r, _) = result_of(server.command(1, &[48, 0, 0, 9]));
        assert_eq!(r, ERR_OBJECT_NOT_FOUND);
    }

    #[test]
    fn test_desktop_scan_merges_in_background() {
        let (mut server, dir) = test_server("desktop-scan");
        std::fs::create_dir(dir.join(".AppleDouble")).unwrap();
        let mut finder = Vec::new();
        finder.extend_from_slice(b"APPL");
        finder.extend_from_slice(b"TEST");
        finder.extend_from_slice(&[0u8; 24]);
        for i in 0..5 {
            let name = format!("app{i}");
            std::fs::write(dir.join(&name), b"x").unwrap();
            std::fs::write(
                dir.join(".AppleDouble").join(&name),
                apple_double(&[(9, &finder)]),
            )
            .unwrap();
        }
        // The scan walks the tree on its own thread and the findings
        // merge as they arrive
        server.start_scan();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            server.poll_scan();
            if server
                .desktop
                .as_ref()
                .map(|db| db.application_count())
                .unwrap_or(0)
                >= 5
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the scan never merged"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // FPGetAPPL finds one of the scanned applications
        let mut cmd = vec![55, 0, 0, 1];
        cmd.extend_from_slice(b"TEST");
        cmd.extend_from_slice(&[0, 0]);
        cmd.extend_from_slice(&0u16.to_be_bytes());
        let (r, _) = result_of(server.command(1, &cmd));
        assert_eq!(r, 0);
        // Starting the scan again is a no-op
        server.start_scan();
        assert_eq!(server.desktop.as_ref().unwrap().application_count(), 5);
    }

    /// A server with write access, using the given backend
    pub(super) fn test_server_writable(
        name: &str,
        backend: crate::meta::Backend,
    ) -> (Afp, PathBuf) {
        let dir = std::env::temp_dir().join(format!("snow-afp-{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = AfpSettings {
            read_only: false,
            write_backend: backend,
            ..AfpSettings::default()
        };
        (
            Afp::new_with(Some(dir.clone()), None, MaxVersion::default(), settings),
            dir,
        )
    }

    /// An AFP 2.x path name: [type 2][Pascal string]
    fn long_path(name: &str) -> Vec<u8> {
        let mut out = vec![2, name.len() as u8];
        out.extend_from_slice(name.as_bytes());
        out
    }

    #[test]
    fn test_write_commands_are_locked_when_read_only() {
        let (mut server, dir) = test_server("write-locked");
        // Create a file on the host so the lookups can succeed
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        // FPCreateFile -> afpVolLocked
        let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("new.txt"));
        let (r, _) = result_of(server.command(1, &data));
        assert_eq!(r, -5031);
        // FPDelete -> afpVolLocked
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("a.txt"));
        let (r, _) = result_of(server.command(1, &data));
        assert_eq!(r, -5031);
        // Opening a fork for writing -> afpVolLocked
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 5];
        data.extend_from_slice(b"a.txt");
        let (r, _) = result_of(server.command(1, &data));
        assert_eq!(r, -5031);
        // Desktop database writes -> afpVolLocked
        let (r, _) = result_of(server.command(1, &[53, 0]));
        assert_eq!(r, -5031);
        // The file is untouched
        assert!(dir.join("a.txt").exists());
        assert!(!dir.join("new.txt").exists());
    }

    #[test]
    fn test_file_id_calls_and_volume_attributes() {
        let (mut server, dir) = test_server_writable("file-ids", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("f.txt"), b"x").unwrap();
        // A 2.0 session sees no SupportsFileIDs bit and the file-ID
        // calls answer afpCallNotSupported
        let (r, reply) = result_of(server.command(1, &[17, 0, 0, 1, 0, 1]));
        assert_eq!(r, 0);
        assert_eq!(reply, [0, 1, 0, 0]);
        let mut data = vec![39, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("f.txt"));
        assert_eq!(
            result_of(server.command(1, &data)).0,
            ERR_CALL_NOT_SUPPORTED
        );
        // A 2.1 session sees the bit, and FPCreateID answers the
        // file's ID
        server.versions.insert(1, AfpVersion::V21);
        let (r, reply) = result_of(server.command(1, &[17, 0, 0, 1, 0, 1]));
        assert_eq!(r, 0);
        assert_eq!(reply, [0, 1, 0, 4]);
        let mut data = vec![39, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("f.txt"));
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let id = u32::from_be_bytes(reply.try_into().unwrap());
        // FPResolveID serves the file's parameters by ID
        let mut data = vec![41, 0, 0, 1];
        data.extend_from_slice(&id.to_be_bytes());
        data.extend_from_slice(&0x0340u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(reply[0..2], 0x0340u16.to_be_bytes());
        assert_eq!(reply[4..8], id.to_be_bytes()); // the file number
        // FPResolveID on a directory's ID answers afpObjectTypeErr
        std::fs::create_dir(dir.join("sub")).unwrap();
        let dir_id = server.dir_id(&dir.join("sub"));
        let mut data = vec![41, 0, 0, 1];
        data.extend_from_slice(&dir_id.to_be_bytes());
        data.extend_from_slice(&0x0340u16.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, ERR_OBJECT_TYPE);
        // FPDeleteID on a directory's ID answers afpObjectTypeErr too
        let mut data = vec![40, 0, 0, 1];
        data.extend_from_slice(&dir_id.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, ERR_OBJECT_TYPE);
        // FPDeleteID on an unknown ID answers afpIDNotFound
        let data = vec![40, 0, 0, 1, 0x7F, 0xFF, 0xFF, 0xFE];
        assert_eq!(result_of(server.command(1, &data)).0, ERR_ID_NOT_FOUND);
        // FPDeleteID on the file's ID invalidates it permanently
        let mut data = vec![40, 0, 0, 1];
        data.extend_from_slice(&id.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let mut data = vec![41, 0, 0, 1];
        data.extend_from_slice(&id.to_be_bytes());
        data.extend_from_slice(&0x0340u16.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, ERR_BAD_ID);
    }

    #[test]
    fn test_volume_attributes_follow_read_only() {
        let (mut server, _) = test_server("attrs-ro");
        // [GetVolParms][0][vol 1][bitmap 0x0001] -> attributes = 1
        let (r, reply) = result_of(server.command(1, &[17, 0, 0, 1, 0, 1]));
        assert_eq!(r, 0);
        assert_eq!(reply, [0, 1, 0, 1]);
        let (mut server, _) = test_server_writable("attrs-rw", crate::meta::Backend::AppleDouble);
        let (r, reply) = result_of(server.command(1, &[17, 0, 0, 1, 0, 1]));
        assert_eq!(r, 0);
        assert_eq!(reply, [0, 1, 0, 0]);
    }

    #[test]
    fn test_create_enumerate_delete() {
        let (mut server, dir) =
            test_server_writable("write-create", crate::meta::Backend::AppleDouble);
        // Create a file and a directory
        let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("hello.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let mut data = vec![6, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("folder"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(dir.join("hello.txt").is_file());
        assert!(dir.join("folder").is_dir());
        // Both enumerate
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(reply[4..6], [0, 5]); // the two created + the three
        // convention directories the writable server creates (the
        // Finder hides those through their invisible attribute)
        for name in ["Network Trash Folder", "Temporary Items", "VM Storage"] {
            assert!(dir.join(name).is_dir(), "{name}");
            let meta = crate::meta::open(&dir.join(name));
            assert_eq!(meta.attrs, 0x0001, "{name}");
            let flags = u16::from_be_bytes(meta.finder_info[8..10].try_into().unwrap());
            assert_eq!(flags, 0x4000, "{name}");
        }
        // Delete both
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("hello.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("folder"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(!dir.join("hello.txt").exists());
        assert!(!dir.join("folder").exists());
        // Deleting a missing file -> afpObjectNotFound
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("gone.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, -5018);
    }

    #[test]
    fn test_delete_inhibit_busy_and_metadata_cleanup() {
        let (mut server, dir) =
            test_server_writable("write-delrules", crate::meta::Backend::AppleDouble);
        // DeleteInhibit (attribute bit 8) answers afpObjectLocked
        std::fs::write(dir.join("locked.txt"), b"x").unwrap();
        crate::meta::Backend::AppleDouble
            .set_attrs(&dir.join("locked.txt"), 0x0100)
            .unwrap();
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("locked.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, -5032);
        assert!(dir.join("locked.txt").exists());
        // An open fork answers afpFileBusy and stays on disk
        std::fs::write(dir.join("open.txt"), b"x").unwrap();
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 8];
        data.extend_from_slice(b"open.txt");
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("open.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, -5010);
        assert!(dir.join("open.txt").exists());
        // A folder that only holds the server's own metadata deletes
        // (its `.AppleDouble` subtree and `._` sidecar are removed
        // first)
        std::fs::create_dir(dir.join("metafolder")).unwrap();
        std::fs::create_dir(dir.join("metafolder").join(".AppleDouble")).unwrap();
        std::fs::write(
            dir.join("metafolder").join(".AppleDouble").join("orphan"),
            b"stale",
        )
        .unwrap();
        std::fs::write(dir.join("._metafolder"), b"sidecar").unwrap();
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("metafolder"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(!dir.join("metafolder").exists());
        assert!(!dir.join("._metafolder").exists());
        // A folder holding other hidden files still answers
        // afpDirNotEmpty, and its own metadata survives the failed
        // delete
        std::fs::create_dir(dir.join("dsstore")).unwrap();
        std::fs::write(dir.join("dsstore").join(".DS_Store"), b"x").unwrap();
        std::fs::write(dir.join("._dsstore"), b"sidecar").unwrap();
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("dsstore"));
        assert_eq!(result_of(server.command(1, &data)).0, -5007);
        assert!(dir.join("dsstore").exists());
        assert!(dir.join("._dsstore").exists());
        // A folder holding only `._*` orphans (a macOS copy) deletes:
        // the emptiness check ignores the server's metadata
        std::fs::create_dir(dir.join("orphans")).unwrap();
        std::fs::write(dir.join("orphans").join("._x"), b"o").unwrap();
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("orphans"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(!dir.join("orphans").exists());
        // RenameInhibit (attribute bit 7) answers afpObjectLocked
        std::fs::write(dir.join("renlocked.txt"), b"x").unwrap();
        crate::meta::Backend::AppleDouble
            .set_attrs(&dir.join("renlocked.txt"), 0x0080)
            .unwrap();
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("renlocked.txt"));
        data.extend_from_slice(&long_path("other.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, -5032);
        assert!(dir.join("renlocked.txt").exists());
    }

    #[test]
    fn test_create_duplicate_and_dir_not_empty() {
        let (mut server, dir) =
            test_server_writable("write-errors", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::create_dir(dir.join("full")).unwrap();
        std::fs::write(dir.join("full").join("child"), b"c").unwrap();
        // A duplicate create -> afpObjectExists
        let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("a.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, -5017);
        // A non-empty directory -> afpDirNotEmpty
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("full"));
        assert_eq!(result_of(server.command(1, &data)).0, -5007);
    }

    #[test]
    fn test_rename_and_move() {
        let (mut server, dir) =
            test_server_writable("write-rename", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("old.txt"), b"data").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        // Rename within the root
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("old.txt"));
        data.extend_from_slice(&long_path("new.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(!dir.join("old.txt").exists());
        assert!(dir.join("new.txt").exists());
        // Renaming onto an existing name answers afpObjectExists and
        // replaces nothing
        std::fs::write(dir.join("keep.txt"), b"keep").unwrap();
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("new.txt"));
        data.extend_from_slice(&long_path("keep.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, -5017);
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"keep");
        assert!(dir.join("new.txt").exists());
        // The sub directory's ID, straight from the server's table
        let sub_id = server.dir_id(&dir.join("sub"));
        assert_ne!(sub_id, ROOT_DIR_ID);
        // FPMoveAndRename:
        // [vol][src dir 2][dst dir sub][src path "new.txt"]
        // [dst path ""][new name "moved.txt"]
        let mut data = vec![23, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&sub_id.to_be_bytes());
        data.extend_from_slice(&long_path("new.txt"));
        data.extend_from_slice(&long_path(""));
        data.extend_from_slice(&long_path("moved.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(dir.join("sub").join("moved.txt").exists());
        assert!(!dir.join("new.txt").exists());
        // An empty new name keeps the source's: move it back to the
        // root from the subdirectory
        let mut data = vec![23, 0, 0, 1];
        data.extend_from_slice(&sub_id.to_be_bytes()); // src dir
        data.extend_from_slice(&2u32.to_be_bytes()); // dst dir
        data.extend_from_slice(&long_path("moved.txt"));
        data.extend_from_slice(&long_path(""));
        data.extend_from_slice(&long_path(""));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(dir.join("moved.txt").exists());
        // Moving a directory into itself is afpCantMove
        let mut data = vec![23, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&sub_id.to_be_bytes());
        data.extend_from_slice(&long_path("sub"));
        data.extend_from_slice(&long_path(""));
        data.extend_from_slice(&long_path(""));
        assert_eq!(result_of(server.command(1, &data)).0, -5005);
    }

    #[test]
    fn test_apple_single_forks_follow_container_rewrites() {
        let (mut server, dir) =
            test_server_writable("write-rsrc-refresh", crate::meta::Backend::AppleSingle);
        std::fs::write(dir.join("doc.txt"), b"hello").unwrap();
        // The first resource-fork write converts the plain file into a
        // container (the fork flag byte is 0x80)
        let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 7];
        data.extend_from_slice(b"doc.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let rsrc_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut write = vec![33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4];
        write[2..4].copy_from_slice(&rsrc_ref.to_be_bytes());
        write.extend_from_slice(b"rsrc");
        assert_eq!(result_of(server.command(1, &write)).0, 0);
        // The data fork, open for writing
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 7];
        data.extend_from_slice(b"doc.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let data_ref = u16::from_be_bytes([reply[2], reply[3]]);
        // Extending the resource fork rewrites the container (a new
        // inode replaces the file)
        let mut write = vec![33, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 4];
        write[2..4].copy_from_slice(&rsrc_ref.to_be_bytes());
        write.extend_from_slice(b"more");
        assert_eq!(result_of(server.command(1, &write)).0, 0);
        // The stale data-fork handle was refreshed: this write lands in
        // the live file, not the orphaned inode
        let mut write = vec![33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4];
        write[2..4].copy_from_slice(&data_ref.to_be_bytes());
        write.extend_from_slice(b"XXXX");
        assert_eq!(result_of(server.command(1, &write)).0, 0);
        // The file reads back with both forks intact
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 7];
        data.extend_from_slice(b"doc.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &read));
        assert_eq!(r, 0);
        assert_eq!(&reply[..5], b"XXXXo");
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0];
        read[2..4].copy_from_slice(&rsrc_ref.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &read));
        assert_eq!(r, 0);
        assert_eq!(&reply[..8], b"rsrcmore");
    }

    #[test]
    fn test_write_end_flag_and_last_written() {
        let (mut server, dir) =
            test_server_writable("write-endflag", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("f.txt"), b"abcde").unwrap();
        // Open for writing
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 5];
        data.extend_from_slice(b"f.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        // [Write][EndFlag 0x80][ref][offset -2 (signed)][count 2]
        // [data]: 2 bytes before the end of the 5-byte fork = position 3
        let mut write = vec![33, 0x80, 0, 0, 0xFF, 0xFF, 0xFF, 0xFE, 0, 0, 0, 2];
        write[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        write.extend_from_slice(b"XX");
        let (r, reply) = result_of(server.command(1, &write));
        assert_eq!(r, 0);
        // LastWritten: the offset just past the last byte written
        assert_eq!(reply, 5u32.to_be_bytes());
        // An end-relative offset before the start of the fork is a
        // parameter error
        let mut write = vec![33, 0x80, 0, 0, 0xFF, 0xFF, 0xFF, 0xFA, 0, 0, 0, 1];
        write[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        write.extend_from_slice(b"Z");
        assert_eq!(result_of(server.command(1, &write)).0, ERR_PARAM);
        // Read back through a fresh read-only fork: "abcXX"
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 5];
        data.extend_from_slice(b"f.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &read));
        assert_eq!(r, 0);
        assert_eq!(&reply[..5], b"abcXX");
    }

    #[test]
    fn test_write_and_read_data_fork() {
        let (mut server, dir) =
            test_server_writable("write-data", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("hello.txt"), b"Hello from the Snow AFP server!").unwrap();
        // Open for writing: access byte 1
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 3, 2, 9];
        data.extend_from_slice(b"hello.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        // [Write][0][ref][offset 6][count 5][data]
        let mut write = vec![33, 0, 0, 0, 0, 0, 0, 6, 0, 0, 0, 5];
        write[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        write.extend_from_slice(b"Snow!");
        let (r, _) = result_of(server.command(1, &write));
        assert_eq!(r, 0);
        // Read it back through a fresh read-only fork
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 9];
        data.extend_from_slice(b"hello.txt");
        let (_, reply) = result_of(server.command(1, &data));
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 31, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &read));
        assert_eq!(r, 0);
        assert_eq!(&reply[..11], b"Hello Snow!");
        // The write extended nothing: the fork length is unchanged
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 9];
        data.extend_from_slice(b"hello.txt");
        let (_, reply) = result_of(server.command(1, &data));
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let _ = fork_ref;
    }

    #[test]
    fn test_write_past_end_extends_the_fork() {
        let (mut server, dir) =
            test_server_writable("write-extend", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("f.txt"), b"abc").unwrap();
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 3, 2, 5];
        data.extend_from_slice(b"f.txt");
        let (_, reply) = result_of(server.command(1, &data));
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        // Write 4 bytes at offset 8: the fork extends to 12
        let mut write = vec![33, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 4];
        write[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        write.extend_from_slice(b"WXYZ");
        assert_eq!(result_of(server.command(1, &write)).0, 0);
        // A fresh open reports the new length
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 5];
        data.extend_from_slice(b"f.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let len = u32::from_be_bytes(reply[4..8].try_into().unwrap());
        assert_eq!(len, 12);
        assert_eq!(std::fs::read(dir.join("f.txt")).unwrap().len(), 12);
    }

    #[test]
    fn test_set_fork_params_truncates() {
        let (mut server, dir) =
            test_server_writable("write-trunc", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("f.txt"), b"abcdefghij").unwrap();
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 3, 2, 5];
        data.extend_from_slice(b"f.txt");
        let (_, reply) = result_of(server.command(1, &data));
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        // [SetForkParms][0][ref][bitmap 0x0200][len 3]
        let mut set = vec![31, 0, 0, 0, 0x02, 0x00, 0, 0, 0, 3];
        set[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        assert_eq!(result_of(server.command(1, &set)).0, 0);
        assert_eq!(std::fs::read(dir.join("f.txt")).unwrap(), b"abc");
    }

    #[test]
    fn test_rsrc_fork_round_trip_apple_double() {
        let (mut server, dir) =
            test_server_writable("write-rsrc", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("app"), b"data").unwrap();
        // Open the resource fork (flag 0x80) for writing
        let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 3, 2, 3];
        data.extend_from_slice(b"app");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let write_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut write = vec![33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8];
        write[2..4].copy_from_slice(&write_ref.to_be_bytes());
        write.extend_from_slice(b"resource");
        assert_eq!(result_of(server.command(1, &write)).0, 0);
        // The AppleDouble container now exists
        assert!(dir.join(".AppleDouble").join("app").exists());
        // The data fork is untouched
        assert_eq!(std::fs::read(dir.join("app")).unwrap(), b"data");
        // A fresh open reads the fork back
        let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 3];
        data.extend_from_slice(b"app");
        let (_, reply) = result_of(server.command(1, &data));
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &read));
        assert_eq!(r, 0);
        assert_eq!(reply, b"resource");
        // Truncate it (through the write fork)
        let mut set = vec![31, 0, 0, 0, 0x04, 0x00, 0, 0, 0, 4];
        set[2..4].copy_from_slice(&write_ref.to_be_bytes());
        assert_eq!(result_of(server.command(1, &set)).0, 0);
        let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 3];
        data.extend_from_slice(b"app");
        let (_, reply) = result_of(server.command(1, &data));
        let len = u32::from_be_bytes(reply[4..8].try_into().unwrap());
        assert_eq!(len, 4);
    }

    #[test]
    fn test_create_dir_id_ancestors_and_hard_create() {
        let (mut server, dir) =
            test_server_writable("write-hard", crate::meta::Backend::AppleDouble);
        // FPCreateDir replies with the New Directory ID
        let mut data = vec![6, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("newfolder"));
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(reply.len(), 4);
        let new_id = u32::from_be_bytes(reply.try_into().unwrap());
        assert_eq!(new_id, server.dir_id(&dir.join("newfolder")));
        // The ID opens the folder and enumerates its child
        std::fs::write(dir.join("newfolder").join("child.txt"), b"c").unwrap();
        let mut data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        data[4..8].copy_from_slice(&new_id.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(reply[4..6], [0, 1]);
        let off = u16::from_be_bytes([reply[8], reply[9]]) as usize;
        assert_eq!(&reply[8 + off..8 + off + 10], b"\x09child.txt");
        // The path may name ancestors ("sub\0inner.txt")
        let mut data = vec![6, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("sub"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("sub\0inner.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(dir.join("sub").join("inner.txt").is_file());
        // A hard create (CreateFlag bit 7) replaces a closed file
        std::fs::write(dir.join("sub").join("inner.txt"), b"payload").unwrap();
        let mut data = vec![7, 0x80, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("sub\0inner.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert_eq!(
            std::fs::read(dir.join("sub").join("inner.txt")).unwrap(),
            b""
        );
        // A hard create onto a directory answers afpObjectTypeErr
        let mut data = vec![7, 0x80, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("sub"));
        assert_eq!(result_of(server.command(1, &data)).0, -5025);
        // A hard create onto an open file answers afpFileBusy
        std::fs::write(dir.join("open.txt"), b"x").unwrap();
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 2, 8];
        data.extend_from_slice(b"open.txt");
        let (r, _) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let mut data = vec![7, 0x80, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("open.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, -5010);
    }

    #[test]
    fn test_move_by_own_dir_id_and_mangled_names() {
        let (mut server, dir) =
            test_server_writable("write-move-null", crate::meta::Backend::AppleDouble);
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::create_dir(dir.join("folder")).unwrap();
        // MoveAndRename with a null Source Pathname: the directory is
        // addressed by its own Directory ID
        let folder_id = server.dir_id(&dir.join("folder"));
        let sub_id = server.dir_id(&dir.join("sub"));
        let mut data = vec![23, 0, 0, 1];
        data.extend_from_slice(&folder_id.to_be_bytes()); // src dir = itself
        data.extend_from_slice(&sub_id.to_be_bytes()); // dst dir
        data.extend_from_slice(&long_path("")); // null source path
        data.extend_from_slice(&long_path("")); // null destination path
        data.extend_from_slice(&long_path("moved")); // the new name
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(dir.join("sub").join("moved").is_dir());
        assert!(!dir.join("folder").exists());
        // Moving a file with a mangled name and an empty NewName keeps
        // its host name (the wire name would rename it on the host)
        std::fs::write(dir.join("你好.txt"), b"hi").unwrap();
        let mangled = server.mac_name_of(&dir.join("你好.txt"));
        assert!(mangled.starts_with('#'));
        let mut data = vec![23, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&sub_id.to_be_bytes());
        data.extend_from_slice(&long_path(&mangled));
        data.extend_from_slice(&long_path(""));
        data.extend_from_slice(&long_path(""));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(dir.join("sub").join("你好.txt").is_file());
        assert!(!dir.join("你好.txt").exists());
    }

    #[test]
    fn test_rename_folder_keeps_dir_id_usable() {
        let (mut server, dir) =
            test_server_writable("write-rename-dir", crate::meta::Backend::AppleDouble);
        std::fs::create_dir(dir.join("folder")).unwrap();
        std::fs::write(dir.join("folder").join("child.txt"), b"c").unwrap();
        // Mint the folder's Directory ID
        let folder_id = server.dir_id(&dir.join("folder"));
        assert_ne!(folder_id, ROOT_DIR_ID);
        // Rename the folder
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("folder"));
        data.extend_from_slice(&long_path("renamed"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        // The same Directory ID still opens it and enumerates the child
        let mut data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        data[4..8].copy_from_slice(&folder_id.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(reply[4..6], [0, 1]); // the child
        let off = u16::from_be_bytes([reply[8], reply[9]]) as usize;
        assert_eq!(&reply[8 + off..8 + off + 10], b"\x09child.txt");
        // The renamed ID resolves the child's path for a delete too
        let mut data = vec![8, 0, 0, 1];
        data.extend_from_slice(&folder_id.to_be_bytes());
        data.extend_from_slice(&long_path("child.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        // Delete the now-empty folder through the root
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("renamed"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        // The Directory ID no longer resolves
        let mut data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        data[4..8].copy_from_slice(&folder_id.to_be_bytes());
        let (r, _) = result_of(server.command(1, &data));
        assert_eq!(r, ERR_DIR_NOT_FOUND);
    }

    #[test]
    fn test_attr_set_clear_bit() {
        let (mut server, dir) =
            test_server_writable("write-attrs-sc", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("f.txt"), b"x").unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        // Set Invisible (Set/Clear bit = 1): attrs becomes 0x0001
        let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x01];
        data.extend_from_slice(&long_path("f.txt"));
        data.push(0);
        data.extend_from_slice(&0x8001u16.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let meta = crate::meta::open(&dir.join("f.txt"));
        assert_eq!(meta.attrs, 0x0001);
        // Clear it again (Set/Clear bit = 0)
        let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x01];
        data.extend_from_slice(&long_path("f.txt"));
        data.push(0);
        data.extend_from_slice(&0x0001u16.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let meta = crate::meta::open(&dir.join("f.txt"));
        assert_eq!(meta.attrs, 0x0000);
        // The already-open bit (3) is never stored
        let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x01];
        data.extend_from_slice(&long_path("f.txt"));
        data.push(0);
        data.extend_from_slice(&0x8008u16.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let meta = crate::meta::open(&dir.join("f.txt"));
        assert_eq!(meta.attrs, 0x0000);
        // The file-only bit (5) is ignored on directories
        let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x01];
        data.extend_from_slice(&long_path("d"));
        data.push(0);
        data.extend_from_slice(&0x8020u16.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let meta = crate::meta::open(&dir.join("d"));
        assert_eq!(meta.attrs, 0x0000);
    }

    #[test]
    fn test_set_file_dir_params_apple_double() {
        let (mut server, dir) =
            test_server_writable("write-setparms", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("doc.txt"), b"hello").unwrap();
        // Set Finder info (bit 5), dates (bits 2-4) and attributes
        // (bit 0): the values follow in ascending bit order
        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"TEXT");
        finder[4..8].copy_from_slice(b"CARO");
        let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x3D];
        data.extend_from_slice(&long_path("doc.txt"));
        data.push(0); // the values start at the next even offset
        data.extend_from_slice(&0x8001u16.to_be_bytes()); // attrs: set Invisible
        data.extend_from_slice(&100u32.to_be_bytes()); // create
        data.extend_from_slice(&200u32.to_be_bytes()); // modify
        data.extend_from_slice(&300u32.to_be_bytes()); // backup
        data.extend_from_slice(&finder);
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        // The container exists and the data fork is untouched
        assert!(dir.join(".AppleDouble").join("doc.txt").exists());
        assert_eq!(std::fs::read(dir.join("doc.txt")).unwrap(), b"hello");
        // GetFileDirParms reads it all back
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x3D, 0x00, 0x3D, 2, 7];
        data.extend_from_slice(b"doc.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(reply[6..8], [0x00, 0x01]); // attributes: Invisible set
        assert_eq!(reply[8..12], 100u32.to_be_bytes()); // create
        assert_eq!(reply[12..16], 200u32.to_be_bytes()); // modify
        assert_eq!(reply[16..20], 300u32.to_be_bytes()); // backup
        assert_eq!(&reply[20..24], b"TEXT");
        assert_eq!(&reply[24..28], b"CARO");
    }

    #[test]
    fn test_set_file_dir_params_apple_single() {
        let (mut server, dir) =
            test_server_writable("write-setparms-single", crate::meta::Backend::AppleSingle);
        std::fs::write(dir.join("doc.txt"), b"payload").unwrap();
        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"TEXT");
        finder[4..8].copy_from_slice(b"CARO");
        let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x20];
        data.extend_from_slice(&long_path("doc.txt"));
        data.push(0); // the values start at the next even offset
        data.extend_from_slice(&finder);
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        // The file was converted into a container; no sidecars
        assert!(!dir.join(".AppleDouble").exists());
        let meta = crate::meta::open(&dir.join("doc.txt"));
        assert_eq!(&meta.finder_info[..4], b"TEXT");
        assert_eq!(&meta.finder_info[4..8], b"CARO");
        assert_eq!(meta.data_len, 7);
        assert!(meta.data_offset > 0);
        // GetFileDirParms sees the Finder info
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x20, 0, 0, 2, 7];
        data.extend_from_slice(b"doc.txt");
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(&reply[6..10], b"TEXT");
        assert_eq!(&reply[10..14], b"CARO");
    }

    #[test]
    fn test_name_bijection_on_writes() {
        let (mut server, dir) =
            test_server_writable("write-names", crate::meta::Backend::AppleDouble);
        // A Mac name containing '/' (the separator) maps to ':' on the
        // host
        let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("a/b"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(dir.join("a:b").is_file());
        assert!(!dir.join("a").exists());
        // The enumeration serves it back as "a/b"
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert!(reply.windows(3).any(|w| w == b"a/b"));
        // GetFileDirParms by "a/b" resolves to "a:b"
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x00, 0xE0, 0, 0, 2, 3];
        data.extend_from_slice(b"a/b");
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        // Rename "a/b" to "c/d": the host file moves to "c:d"
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("a/b"));
        data.extend_from_slice(&long_path("c/d"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(!dir.join("a:b").exists());
        assert!(dir.join("c:d").is_file());
        // Delete by "c/d"
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("c/d"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(!dir.join("c:d").exists());
    }

    #[test]
    fn test_mangled_name_rename_and_delete() {
        let (mut server, dir) =
            test_server_writable("write-mangled", crate::meta::Backend::AppleDouble);
        let long = format!("{}verylong.txt", "x".repeat(40));
        std::fs::write(dir.join(&long), b"data").unwrap();
        // Enumerating registers the mangled name
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let mangled = crate::name::to_mac(&long).0;
        // Rename through the mangled wire name
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path(&mangled));
        data.extend_from_slice(&long_path("short.txt"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(!dir.join(&long).exists());
        assert!(dir.join("short.txt").exists());
        // Delete through a mangled name again
        let long2 = format!("{}verylong.txt", "y".repeat(40));
        std::fs::write(dir.join(&long2), b"data").unwrap();
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let _ = result_of(server.command(1, &data));
        let mangled2 = crate::name::to_mac(&long2).0;
        let mut data = vec![8, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path(&mangled2));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert!(!dir.join(&long2).exists());
        let _ = reply;
    }

    #[test]
    fn test_file_id_commands() {
        let (mut server, dir) =
            test_server_writable("write-fileids", crate::meta::Backend::AppleDouble);
        server.versions.insert(1, AfpVersion::V21);
        std::fs::write(dir.join("f.txt"), b"data").unwrap();
        // [CreateID][0][vol 1][dir 2][type 2][len 5]["f.txt"]
        let mut data = vec![39, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("f.txt"));
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        let id = u32::from_be_bytes(reply[..4].try_into().unwrap());
        // [ResolveID][0][vol 1][id][file bm 0x0040: the long name]
        let mut data = vec![41, 0, 0, 1];
        data.extend_from_slice(&id.to_be_bytes());
        data.extend_from_slice(&0x0040u16.to_be_bytes());
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0);
        assert_eq!(reply[0..2], [0x00, 0x40]); // bitmap
        assert!(reply.windows(5).any(|w| w == b"f.txt"));
        // [DeleteID][0][vol 1][id]
        let mut data = vec![40, 0, 0, 1];
        data.extend_from_slice(&id.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        // Resolving the deleted ID fails with afpBadIDErr
        let mut data = vec![41, 0, 0, 1];
        data.extend_from_slice(&id.to_be_bytes());
        data.extend_from_slice(&0x0040u16.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, -5039);
        // CreateID on a directory: afpObjectTypeErr
        std::fs::create_dir(dir.join("sub")).unwrap();
        let mut data = vec![39, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&long_path("sub"));
        assert_eq!(result_of(server.command(1, &data)).0, -5025);
    }

    #[test]
    fn test_mod_date_follows_writes_with_a_container() {
        let (mut server, dir) =
            test_server_writable("rv3-moddate", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("f.txt"), b"abc").unwrap();
        std::fs::create_dir(dir.join("D")).unwrap();
        let old = (-157_785_600i32) as u32; // 1995
        let set_dates = |server: &mut Afp, name: &str, d: u32| {
            let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x0C];
            data.extend_from_slice(&long_path(name));
            if data.len() % 2 == 1 {
                data.push(0);
            }
            data.extend_from_slice(&d.to_be_bytes());
            data.extend_from_slice(&d.to_be_bytes());
            assert_eq!(result_of(server.command(1, &data)).0, 0);
        };
        let mod_date = |server: &mut Afp, name: &str, is_dir: bool| {
            let (fb, db) = if is_dir {
                (0u16, 0x0008u16)
            } else {
                (0x0008, 0)
            };
            let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2];
            data.extend_from_slice(&fb.to_be_bytes());
            data.extend_from_slice(&db.to_be_bytes());
            data.extend_from_slice(&long_path(name));
            let (r, reply) = result_of(server.command(1, &data));
            assert_eq!(r, 0);
            u32::from_be_bytes(reply[6..10].try_into().unwrap())
        };
        set_dates(&mut server, "f.txt", old);
        set_dates(&mut server, "D", old);
        // Write the file through AFP and create a file inside D: both
        // modification dates must move off the stored value
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x00, 0x00, 0x03, 2, 5];
        data.extend_from_slice(b"f.txt");
        let (_, reply) = result_of(server.command(1, &data));
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut w = vec![33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, b'Z'];
        w[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        assert_eq!(result_of(server.command(1, &w)).0, 0);
        let mut c = vec![4, 0];
        c.extend_from_slice(&fork_ref.to_be_bytes());
        assert_eq!(result_of(server.command(1, &c)).0, 0);
        let mut cr = vec![7, 0, 0, 1, 0, 0, 0, 2];
        cr.extend_from_slice(&long_path("D\0new"));
        assert_eq!(result_of(server.command(1, &cr)).0, 0);
        assert_ne!(mod_date(&mut server, "f.txt", false), old);
        assert_ne!(mod_date(&mut server, "D", true), old);
    }

    #[test]
    fn test_desktop_db_writes_acknowledge_when_writable() {
        let (mut server, _) = test_server_writable("write-dtdb", crate::meta::Backend::AppleDouble);
        for cmd in [53u8, 54, 56, 57, 192] {
            let (r, _) = result_of(server.command(1, &[cmd, 0]));
            assert_eq!(r, 0, "command {cmd}");
        }
    }

    #[test]
    fn test_enumerate_past_end_returns_object_not_found() {
        let (mut server, dir) = test_server("enumerate-end");
        std::fs::write(dir.join("one.txt"), b"1").unwrap();
        // [Enumerate][0][vol 1][dir 2][file bm 0x0340][dir bm 0x0340]
        // [count 20][start 2 (past the single entry)][max 512]
        // [path type 2][path ""]
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 2, 0x02, 0x00, 2, 0,
        ];
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, ERR_OBJECT_NOT_FOUND);
    }

    #[test]
    fn test_enumerate_zero_count_and_file_target() {
        let (mut server, dir) = test_server("enumerate-zero");
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::write(dir.join("b.txt"), b"b").unwrap();
        // A request for zero entries answers an empty enumeration
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 0, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[4..6], [0, 0]);
        // Enumerating a file answers afpObjectTypeErr
        let mut data = vec![
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 5,
        ];
        data.extend_from_slice(b"a.txt");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, ERR_OBJECT_TYPE);
    }

    #[test]
    fn test_enumerate_empty_dir_returns_object_not_found() {
        let (mut server, dir) = test_server("enumerate-empty");
        std::fs::create_dir(dir.join("sub")).unwrap();
        // Enumerate the empty "sub" directory from the start
        let mut data = vec![
            9, 0, 0, 1, 0, 0, 0, 2, 0x03, 0x40, 0x03, 0x40, 0, 20, 0, 1, 0x02, 0x00, 2, 3,
        ];
        data.extend_from_slice(b"sub");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, ERR_OBJECT_NOT_FOUND);
    }

    #[test]
    fn test_read_newline_mask_stops_and_translates() {
        let (mut server, dir) = test_server("nl-mask");
        std::fs::write(dir.join("text.txt"), b"hello\nworld").unwrap();
        // [OpenFork][flag data][vol 1][dir 2][bitmap 0x4000][access
        // read][path type 2]["text.txt"]
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 8];
        data.extend_from_slice(b"text.txt");
        let (_, reply) = result_of(server.command(1, &data));
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        // A carriage-return mask reads a text line: the stored line
        // feed stops the read and comes back translated
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 0xFF, b'\r'];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &read));
        assert_eq!(result, 0, "a newline cut is a complete read");
        assert_eq!(reply, b"hello\r");
        // The next read continues past the line
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 6, 0, 0, 0, 100, 0xFF, b'\r'];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &read));
        assert_eq!(result, 0);
        assert_eq!(reply, b"world");
        // A zero mask reads bytes only (the common case)
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &read));
        assert_eq!(result, 0);
        assert_eq!(reply, b"hello\nworld");
    }

    #[test]
    fn test_open_fork_read_spec_layout() {
        let (mut server, dir) = test_server("open-fork");
        std::fs::write(dir.join("hello.txt"), b"Hello from the Snow AFP server!").unwrap();
        // [OpenFork][flag data][vol 1][dir 2][bitmap 0x4000][access
        // read][path type 2]["hello.txt"]
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 1, 2, 9];
        data.extend_from_slice(b"hello.txt");
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[0..2], [0x02, 0x00]); // bitmap
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        // The data fork length follows the fork reference number
        let len = u32::from_be_bytes(reply[4..8].try_into().unwrap());
        assert_eq!(len, 31);

        // [Read][0][fork ref][offset 0][count 8][nl mask 0][nl char 0]
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &read));
        assert_eq!(result, 0);
        assert_eq!(reply, b"Hello fr");

        // [GetForkParms][0][fork ref][file bitmap 0x0600]: the data
        // and resource fork lengths
        let mut parms = vec![14, 0, 0, 0, 0x06, 0x00];
        parms[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &parms));
        assert_eq!(result, 0);
        assert_eq!(reply[0..2], [0x06, 0x00]);
        assert_eq!(u32::from_be_bytes(reply[2..6].try_into().unwrap()), 31);
        assert_eq!(u32::from_be_bytes(reply[6..10].try_into().unwrap()), 0);

        // [CloseFork][0][fork ref]
        let mut close = vec![4, 0, 0, 0];
        close[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, _) = result_of(server.command(1, &close));
        assert_eq!(result, 0);
        assert!(server.forks.is_empty());
    }

    #[test]
    fn test_read_of_multipacket_length() {
        // A fork longer than one ATP response packet (574 bytes) reads
        // back in full; the ATP layer splits the reply into packets
        let (mut server, dir) = test_server("big-read");
        let mut content = vec![0u8; 1500];
        for (i, b) in content.iter_mut().enumerate() {
            *b = b'0' + (i % 10) as u8;
        }
        std::fs::write(dir.join("big.txt"), &content).unwrap();
        // [OpenFork][flag data][vol 1][dir 2][bitmap 0][access
        // read][path type 2]["big.txt"]
        let mut data = vec![26, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 7];
        data.extend_from_slice(b"big.txt");
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);

        // [Read][0][fork ref][offset 0][count 1500][nl mask 0]
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05, 0xDC, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &read));
        assert_eq!(result, 0);
        assert_eq!(reply.len(), 1500);
        assert_eq!(reply, content);
    }

    #[test]
    fn test_open_resource_fork_from_sidecar() {
        let (mut server, dir) = test_server("rsrc-fork");
        std::fs::write(dir.join("app.bin"), b"data").unwrap();
        let sidecar = apple_double(&[(2, b"RESOURCE!")]); // entry 2: rsrc fork
        std::fs::write(dir.join("._app.bin"), sidecar).unwrap();

        // [OpenFork][flag rsrc][vol 1][dir 2][bitmap 0][access
        // read][path type 2]["app.bin"]
        let mut data = vec![26, 0x80, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 2, 7];
        data.extend_from_slice(b"app.bin");
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);

        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &read));
        assert_eq!(result, 0);
        assert_eq!(reply, b"RESOURCE!");
    }

    #[test]
    fn test_access_rights_wire_order() {
        // Big-endian: [UARights][world][group][owner]; UARights has
        // the owner bit set plus search and read, and the write bits
        // only on a writable volume
        assert_eq!(access_rights(true), [0x83, 0x03, 0x03, 0x03]);
        assert_eq!(access_rights(false), [0x87, 0x07, 0x07, 0x07]);
    }

    #[test]
    fn test_dir_params_access_rights_bit() {
        let meta = crate::meta::open(Path::new("/nonexistent"));
        // Bit 12 (Access Rights) + bit 6 (Long Name)
        let params = dir_params(0x1000 | 0x0040, "sub", &meta, 0, ROOT_DIR_ID, 1, true);
        // Long-name offset (2 bytes) then the 4-byte access rights,
        // then the name
        assert_eq!(&params[0..2], &[0x00, 0x06]);
        assert_eq!(&params[2..6], &[0x83, 0x03, 0x03, 0x03]);
        assert_eq!(&params[6..10], &[3, b's', b'u', b'b']);
    }

    #[test]
    fn test_file_dir_parms_finder_info() {
        let (mut server, dir) = test_server("finder-info");
        std::fs::write(dir.join("doc.txt"), b"body").unwrap();
        let mut finder = [0u8; 32];
        finder[..4].copy_from_slice(b"ttro");
        let sidecar = apple_double(&[(9, &finder)]); // entry 9: Finder info
        std::fs::write(dir.join("._doc.txt"), sidecar).unwrap();

        // [GetFileDirParms][0][vol 1][dir 2][file bm 0x0007][dir bm 0]
        // [path type 2]["doc.txt"]
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x00, 0xE0, 0, 0, 2, 7];
        data.extend_from_slice(b"doc.txt");
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[0..2], [0x00, 0xE0]); // file bitmap
        assert_eq!(reply[2..4], [0, 0]); // directory bitmap
        assert_eq!(reply[4], 0); // a file
        assert_eq!(reply[5], 0); // the reserved byte after the flag
        // Parameters: Finder info, then the long-name and short-name
        // offsets, then the names. The offsets live after the 32-byte
        // Finder info block, at the end of the fixed parameters.
        let long_off = u16::from_be_bytes([reply[38], reply[39]]) as usize;
        let short_off = u16::from_be_bytes([reply[40], reply[41]]) as usize;
        assert_eq!(long_off, 36);
        assert_eq!(short_off, 44);
        assert_eq!(&reply[6..10], b"ttro"); // the Finder info
        assert_eq!(&reply[6 + long_off..6 + long_off + 8], b"\x07doc.txt");
    }

    #[test]
    fn test_login_spec_layout() {
        let (mut server, _dir) = test_server("login");
        // [Login]["AFPVersion 2.0"]["No User Authent"] (no reserved
        // byte)
        let mut data = vec![18, 14];
        data.extend_from_slice(b"AFPVersion 2.0");
        data.push(15);
        data.extend_from_slice(b"No User Authent");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
    }

    #[test]
    fn test_login_rejects_bad_version() {
        let (mut server, _dir) = test_server("login-bad");
        let mut data = vec![18, 6];
        data.extend_from_slice(b"AFP3.1");
        data.push(15);
        data.extend_from_slice(b"No User Authent");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, ERR_BAD_VERS);
    }
}

#[cfg(test)]
mod name_tests {
    use super::tests::{result_of, test_server};
    use super::*;

    fn enumerate(server: &mut Afp, file_bm: u16) -> (i32, Vec<u8>) {
        // [Enumerate][0][vol 1][dir 2][file bm][dir bm 0][count 20]
        // [start 1][max 512][path type 2][path ""]
        let mut data = vec![
            9,
            0,
            0,
            1,
            0,
            0,
            0,
            2,
            (file_bm >> 8) as u8,
            file_bm as u8,
            0,
            0,
            0,
            20,
            0,
            1,
            0x02,
            0x00,
            2,
            0,
        ];
        data[8..10].copy_from_slice(&file_bm.to_be_bytes());
        result_of(server.command(1, &data))
    }

    fn open_fork(server: &mut Afp, name: &str, rsrc: bool) -> (i32, Vec<u8>) {
        let mut data = vec![
            26,
            if rsrc { 0x80 } else { 0 },
            0,
            1,
            0,
            0,
            0,
            2,
            0,
            0,
            0,
            1,
            2,
            name.len() as u8,
        ];
        data.extend_from_slice(name.as_bytes());
        result_of(server.command(1, &data))
    }

    #[test]
    fn test_enumerate_hides_metadata() {
        let (mut server, dir) = test_server("hide");
        std::fs::write(dir.join("visible.txt"), b"v").unwrap();
        std::fs::write(dir.join(".secret"), b"s").unwrap();
        std::fs::write(dir.join("._visible.txt"), b"sidecar").unwrap();
        std::fs::create_dir_all(dir.join(".AppleDouble")).unwrap();
        let (result, reply) = enumerate(&mut server, 0x0040);
        assert_eq!(result, 0);
        assert_eq!(reply[4..6], [0, 1]); // only visible.txt
        // The record's name is visible.txt
        let rec0_len = reply[6] as usize;
        let rec0 = &reply[6..6 + rec0_len];
        let off = u16::from_be_bytes([rec0[2], rec0[3]]) as usize;
        assert_eq!(&rec0[2 + off..], b"\x0bvisible.txt");
    }

    #[test]
    fn test_open_mangled_name_via_table() {
        let (mut server, dir) = test_server("mangle");
        std::fs::write(dir.join("你好.txt"), b"mangled!").unwrap();
        let (result, _) = enumerate(&mut server, 0x0040);
        assert_eq!(result, 0);
        let mangled = server.mangles.keys().next().unwrap().clone();
        assert!(mangled.starts_with('#') && mangled.ends_with(".txt"));

        let (result, reply) = open_fork(&mut server, &mangled, false);
        assert_eq!(result, 0, "reply: {reply:?}");
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &read));
        assert_eq!(result, 0);
        assert_eq!(reply, b"mangled!");
    }

    #[test]
    fn test_open_colon_bijection() {
        let (mut server, dir) = test_server("colon");
        std::fs::write(dir.join("a:b.txt"), b"slash").unwrap();
        // The wire name uses '/'
        let (result, reply) = open_fork(&mut server, "a/b.txt", false);
        assert_eq!(result, 0);
        let fork_ref = u16::from_be_bytes([reply[2], reply[3]]);
        let mut read = vec![27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0];
        read[2..4].copy_from_slice(&fork_ref.to_be_bytes());
        let (result, reply) = result_of(server.command(1, &read));
        assert_eq!(result, 0);
        assert_eq!(reply, b"slash");
        // The OpenFork reply reports the wire name
        assert_eq!(reply, b"slash");
    }

    #[test]
    fn test_hidden_and_traversal_are_not_addressable() {
        let (mut server, dir) = test_server("harden");
        std::fs::write(dir.join(".secret"), b"s").unwrap();
        std::fs::write(dir.join("ok.txt"), b"ok").unwrap();
        for name in [".secret", "..", "a:b", ".AppleDouble"] {
            let (result, _) = open_fork(&mut server, name, false);
            assert_eq!(result, ERR_OBJECT_NOT_FOUND, "opening {name:?}");
        }
        // A single leading NUL is ignored (the path stays relative);
        // a trailing one is lenient; a colon is not path syntax
        let (result, _) = open_fork(&mut server, "\0ok.txt", false);
        assert_eq!(result, 0);
        let (result, _) = open_fork(&mut server, "ok.txt\0", false);
        assert_eq!(result, 0);
        let (result, _) = open_fork(&mut server, ":ok.txt", false);
        assert_eq!(result, ERR_OBJECT_NOT_FOUND);
        // The plain file still opens
        let (result, _) = open_fork(&mut server, "ok.txt", false);
        assert_eq!(result, 0);
    }
}

#[cfg(test)]
mod config_tests {
    use super::tests::result_of;
    use super::*;

    #[test]
    fn test_custom_volume_name_and_root() {
        let dir = std::env::temp_dir().join("snow-afp-custom");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.txt"), b"x").unwrap();
        let mut server = Afp::new_with(
            Some(dir),
            Some("Shared".to_string()),
            MaxVersion::default(),
            AfpSettings::default(),
        );

        // FPGetSrvrParms lists the custom name
        let (result, reply) = result_of(server.command(1, &[16, 0]));
        assert_eq!(result, 0);
        assert_eq!(reply[4], 1);
        assert_eq!(reply[6], 6);
        assert_eq!(&reply[7..13], b"Shared");

        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("snow-afp-custom"));
    }
}

#[cfg(test)]
mod version_tests {
    use super::tests::{result_of, test_server};
    use super::*;

    fn login(server: &mut Afp, session: u8, version: &str) -> i32 {
        let mut data = vec![18, version.len() as u8];
        data.extend_from_slice(version.as_bytes());
        data.push(15);
        data.extend_from_slice(b"No User Authent");
        let (result, _) = result_of(server.command(session, &data));
        result
    }

    #[test]
    fn test_both_versions_accepted_and_tracked() {
        let (mut server, _dir) = test_server("versions-both");
        assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
        assert_eq!(login(&mut server, 2, "AFPVersion 2.0"), 0);
        assert_eq!(server.versions[&1], AfpVersion::V11);
        assert_eq!(server.versions[&2], AfpVersion::V20);
    }

    #[test]
    fn test_v11_session_commands_have_no_pad_byte() {
        let (mut server, _dir) = test_server("versions-v11-cmd");
        assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
        // FPGetSrvrParms in 1.1 is the bare function byte
        let (result, reply) = result_of(server.command(1, &[16]));
        assert_eq!(result, 0);
        assert_eq!(reply[4], 1); // one volume (after the server time)
        assert_eq!(reply[6], 4); // volume name length
        assert_eq!(&reply[7..11], b"Snow");
    }

    #[test]
    fn test_v11_open_vol_has_pad_byte() {
        // The exact OpenVol the System 7.1 client sends: the reserved
        // byte is present in 1.1 too
        let (mut server, _dir) = test_server("versions-v11-openvol");
        assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
        let data = [24, 0, 0x00, 0x20, 4, b'S', b'n', b'o', b'w'];
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[0..2], [0x00, 0x20]); // bitmap copy
        assert_eq!(reply[2..4], [0x00, 0x01]); // volume ID
    }

    #[test]
    fn test_v11_enumerate_has_file_flag() {
        let (mut server, dir) = test_server("versions-v11-enum");
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
        // [Enumerate][FileFlag 0][vol 1][dir 2][file bm 0x0040 (long
        // name)][dir bm 0: files only][count 20][start 1][max 512]
        // [ptype 2][path ""]
        let data = [
            9, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x40, 0, 0, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[4..6], [0, 1]); // the null Directory Bitmap
        // keeps the directory out
        // The first record's flag byte is 0 (a file), and the name is
        // a.txt
        assert_eq!(reply[7], 0);
        let off = u16::from_be_bytes([reply[8], reply[9]]) as usize;
        assert_eq!(&reply[6 + 2 + off..6 + 2 + off + 6], b"\x05a.txt");

        // With the FileFlag bit 7 set, only directories are returned
        let data = [
            9, 0x80, 0, 1, 0, 0, 0, 2, 0x00, 0x40, 0, 0, 0, 20, 0, 1, 0x02, 0x00, 2, 0,
        ];
        let (result, reply) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        assert_eq!(reply[4..6], [0, 1]); // the directory only
        assert_eq!(reply[7], dir_flag());
    }

    #[test]
    fn test_bare_get_srvr_parms_in_2x() {
        // The 2.1 client sends FPGetSrvrParms as a bare function byte,
        // like 1.1
        let (mut server, _dir) = test_server("bare-gsp");
        let mut data = vec![18, 14];
        data.extend_from_slice(b"AFPVersion 2.0");
        data.push(15);
        data.extend_from_slice(b"No User Authent");
        let (result, _) = result_of(server.command(1, &data));
        assert_eq!(result, 0);
        // [GetSrvrParms] alone: no pad, no parameters
        let (result, _) = result_of(server.command(1, &[16]));
        assert_eq!(result, 0);
        // [Logout] alone
        let (result, _) = result_of(server.command(1, &[20]));
        assert_eq!(result, 0);
    }

    #[test]
    fn test_short_commands_do_not_panic() {
        let (mut server, _dir) = test_server("short-cmds");
        // Every truncated command must answer an error, never panic
        let mut cases: Vec<Vec<u8>> = vec![
            vec![18],       // login with no parameters
            vec![9],        // enumerate with no parameters
            vec![9, 0],     // enumerate with only the pad
            vec![34],       // GetFileDirParms
            vec![34, 0, 0], // GetFileDirParms with a short volume id
            vec![51],       // GetIcon
            vec![55],       // GetAPPL
            vec![7],        // an unsupported command
        ];
        // The lengths the review reproduced: every parser must answer
        // an error, never slice past the checked bytes
        cases.push(vec![34, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // GFDParms: 10 param bytes
        let mut enumerate = vec![9, 0]; // Enumerate: 16 param bytes
        enumerate.extend_from_slice(&[0; 16]);
        cases.push(enumerate);
        for n in 9..=11 {
            let mut open_fork = vec![26, 0]; // OpenFork: 9-11 param bytes
            open_fork.resize(open_fork.len() + n, 0);
            cases.push(open_fork);
        }
        let mut icon = vec![51, 0]; // GetIcon: 11 param bytes
        icon.extend_from_slice(&[0; 11]);
        cases.push(icon);
        for data in &cases {
            let (result, _) = result_of(server.command(1, data));
            assert!(result < 0, "a short command must be an error");
        }
        // SetFileDirParms with an odd-length path and no pad byte
        // after it: the values must exist at the next even offset
        let (mut wserver, wdir) =
            super::tests::test_server_writable("short-cmds-w", crate::meta::Backend::AppleDouble);
        std::fs::write(wdir.join("f.txt"), b"x").unwrap();
        let odd = [
            35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x01, 2, 5, b'f', b'.', b't', b'x', b't',
        ];
        let (result, _) = result_of(wserver.command(1, &odd));
        assert!(result < 0);
    }

    #[test]
    fn test_v11_only_rejects_20() {
        let dir = std::env::temp_dir().join("snow-afp-v11-only");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Afp::new_with(
            Some(dir.clone()),
            None,
            MaxVersion::Max11,
            AfpSettings::default(),
        );
        assert_eq!(login(&mut server, 1, "AFPVersion 1.1"), 0);
        assert_eq!(login(&mut server, 2, "AFP2.0"), ERR_BAD_VERS); // the shorthand is not a version string
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_max20_rejects_21() {
        let dir = std::env::temp_dir().join("snow-afp-max20");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Afp::new_with(
            Some(dir.clone()),
            None,
            MaxVersion::Max20,
            AfpSettings::default(),
        );
        assert_eq!(login(&mut server, 1, "AFPVersion 2.0"), 0);
        assert_eq!(login(&mut server, 2, "AFPVersion 1.1"), 0);
        assert_eq!(login(&mut server, 3, "AFPVersion 2.1"), ERR_BAD_VERS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_max22_accepts_22_and_lists_it_first() {
        let dir = std::env::temp_dir().join("snow-afp-max22");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Afp::new_with(
            Some(dir.clone()),
            None,
            MaxVersion::Max22,
            AfpSettings::default(),
        );
        assert_eq!(login(&mut server, 1, "AFP2.2"), 0);
        assert_eq!(login(&mut server, 2, "AFPVersion 2.1"), 0);
        assert_eq!(login(&mut server, 3, "AFPVersion 1.1"), 0);
        let info = server.server_info();
        let versions_off = u16::from_be_bytes([info[2], info[3]]) as usize;
        assert_eq!(info[versions_off], 4); // four version strings
        assert_eq!(&info[versions_off + 1..versions_off + 8], b"\x06AFP2.2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_vol_params_extended_size_bits() {
        // AFP 2.2's bits 0x0200/0x0400 are 64-bit sizes, 0x0800 the
        // 4-byte block size, in bitmap order
        let params = vol_params(0x0E00, "Snow", Path::new("/"), true, true, 0);
        assert_eq!(&params[0..2], &[0x0E, 0x00]); // the bitmap
        assert_eq!(params.len(), 2 + 8 + 8 + 4);
    }

    #[test]
    fn test_server_info_lists_versions_high_first() {
        let (server, _dir) = test_server("versions-info");
        let info = server.server_info();
        let versions_off = u16::from_be_bytes([info[2], info[3]]) as usize;
        assert_eq!(info[versions_off], 4); // four version strings
        assert_eq!(&info[versions_off + 1..versions_off + 8], b"\x06AFP2.2");
        assert_eq!(
            &info[versions_off + 8..versions_off + 23],
            b"\x0eAFPVersion 2.1"
        );
        assert_eq!(
            &info[versions_off + 23..versions_off + 38],
            b"\x0eAFPVersion 2.0"
        );
        assert_eq!(
            &info[versions_off + 38..versions_off + 53],
            b"\x0eAFPVersion 1.1"
        );
    }
}
/// The round-4 review regressions (W1-W4)
#[cfg(test)]
mod review4 {
    use super::tests::{result_of, test_server_writable};
    use super::*;

    fn lp(name: &str) -> Vec<u8> {
        // The wire carries MacRoman bytes
        let (b, _, _) = encoding_rs::MACINTOSH.encode(name);
        let mut v = vec![2, b.len() as u8];
        v.extend_from_slice(&b);
        v
    }
    fn open_rw(server: &mut Afp, name: &str, rsrc: bool) -> u16 {
        let mut data = vec![
            26,
            if rsrc { 0x80 } else { 0 },
            0,
            1,
            0,
            0,
            0,
            2,
            0x00,
            0x00,
            0x00,
            0x03,
        ];
        data.extend_from_slice(&lp(name));
        let (r, reply) = result_of(server.command(1, &data));
        assert_eq!(r, 0, "open {name}");
        u16::from_be_bytes([reply[2], reply[3]])
    }
    fn write(server: &mut Afp, f: u16, off: u32, bytes: &[u8]) {
        let mut w = vec![33, 0];
        w.extend_from_slice(&f.to_be_bytes());
        w.extend_from_slice(&off.to_be_bytes());
        w.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        w.extend_from_slice(bytes);
        assert_eq!(result_of(server.command(1, &w)).0, 0);
    }
    fn read(server: &mut Afp, f: u16, n: u32) -> (i32, Vec<u8>) {
        let mut r = vec![27, 0];
        r.extend_from_slice(&f.to_be_bytes());
        r.extend_from_slice(&0u32.to_be_bytes());
        r.extend_from_slice(&n.to_be_bytes());
        r.extend_from_slice(&[0, 0]);
        result_of(server.command(1, &r))
    }

    #[test]
    fn review4_nfd_host_name_resolves() {
        let (mut server, dir) = test_server_writable("rv4-nfd", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("re\u{301}sume\u{301}.txt"), b"x").unwrap();
        // The wire name the server serves for it is NFC
        let (wire, _) = crate::name::to_mac("re\u{301}sume\u{301}.txt");
        let mut data = vec![34, 0, 0, 1, 0, 0, 0, 2, 0x02, 0x00, 0, 0];
        data.extend_from_slice(&lp(&wire));
        let r = result_of(server.command(1, &data)).0;
        assert_eq!(r, 0, "the NFC wire name of an NFD host file must resolve");
    }

    #[test]
    fn review4_create_case_variant_is_object_exists() {
        let (mut server, dir) = test_server_writable("rv4-case", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("foo"), b"x").unwrap();
        let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&lp("Foo"));
        let r = result_of(server.command(1, &data)).0;
        assert_eq!(r, ERR_OBJECT_EXISTS);
    }

    #[test]
    fn review4_client_date_set_while_open_survives_close() {
        let (mut server, dir) =
            test_server_writable("rv4-close", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("f.txt"), b"").unwrap();
        let f = open_rw(&mut server, "f.txt", false);
        write(&mut server, f, 0, b"data");
        // The client copies the original's dates while the fork is open
        let old = (-157_785_600i32) as u32;
        let mut data = vec![35, 0, 0, 1, 0, 0, 0, 2, 0x00, 0x0C];
        data.extend_from_slice(&lp("f.txt"));
        if data.len() % 2 == 1 {
            data.push(0);
        }
        data.extend_from_slice(&old.to_be_bytes());
        data.extend_from_slice(&old.to_be_bytes());
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        let mut c = vec![4, 0];
        c.extend_from_slice(&f.to_be_bytes());
        assert_eq!(result_of(server.command(1, &c)).0, 0);
        let m = server
            .meta_open(&dir.canonicalize().unwrap().join("f.txt"))
            .mod_date;
        assert_eq!(m, old, "the client's date was overwritten at close");
    }

    #[test]
    fn review4_rsrc_fork_reads_back_after_first_write() {
        let (mut server, dir) = test_server_writable("rv4-rsrc", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("doc"), b"DATAFORKDATAFORK").unwrap();
        let f = open_rw(&mut server, "doc", true);
        write(&mut server, f, 0, b"RSRC");
        let (r, got) = read(&mut server, f, 4);
        assert_eq!(
            r, 0,
            "the resource fork must be readable after its first write"
        );
        assert_eq!(got, b"RSRC");
    }
}
/// The round-5 review regressions (X1, X2)
#[cfg(test)]
mod review5 {
    use super::ERR_OBJECT_EXISTS;
    use super::tests::{result_of, test_server_writable};

    fn lp(name: &str) -> Vec<u8> {
        // The wire carries MacRoman bytes
        let (b, _, _) = encoding_rs::MACINTOSH.encode(name);
        let mut v = vec![2, b.len() as u8];
        v.extend_from_slice(&b);
        v
    }

    #[test]
    fn case_only_rename_never_overwrites_the_other_spelling() {
        let (mut server, dir) =
            test_server_writable("review5-casedup", crate::meta::Backend::AppleDouble);
        // A case-sensitive host holding both spellings: the lower-case
        // one is created first, so read_dir lists it first on file
        // systems that keep creation order (btrfs)
        let mut clobbered = Vec::new();
        for i in 0..16 {
            let lower = format!("name{i}x");
            let upper = format!("NAME{i}X");
            std::fs::write(dir.join(&lower), b"mine").unwrap();
            std::fs::write(dir.join(&upper), b"OTHER").unwrap();
            let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
            data.extend_from_slice(&lp(&lower));
            data.extend_from_slice(&lp(&upper));
            let r = result_of(server.command(1, &data)).0;
            if std::fs::read(dir.join(&upper)).unwrap() != b"OTHER" {
                clobbered.push((i, r));
            }
        }
        assert!(
            clobbered.is_empty(),
            "renames that overwrote the other spelling: {clobbered:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn volume_mod_date_compares_signed_dates() {
        let (mut server, dir) =
            test_server_writable("review5-volmod", crate::meta::Backend::AppleDouble);
        // A share root whose mtime predates 2000 (a restored vintage
        // tree): a negative AFP date
        let set_1995 = || {
            let c = std::ffi::CString::new(dir.to_string_lossy().as_bytes()).unwrap();
            let t = [
                libc::timespec {
                    tv_sec: 788_918_400,
                    tv_nsec: 0,
                },
                libc::timespec {
                    tv_sec: 788_918_400,
                    tv_nsec: 0,
                },
            ];
            // SAFETY: valid C path and timespec array
            unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), t.as_ptr(), 0) };
        };
        std::fs::create_dir(dir.join("sub")).unwrap();
        set_1995();
        // A server-side write below the root
        let mut data = vec![7, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&lp("sub\0new"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        set_1995();
        // GetVolParms, bitmap 0x0008: the modification date
        let (r, reply) = result_of(server.command(1, &[17, 0, 0, 1, 0x00, 0x08]));
        assert_eq!(r, 0);
        let date = u32::from_be_bytes(reply[2..6].try_into().unwrap()) as i32;
        assert!(
            date > 0,
            "the write stamp must beat a pre-2000 root mtime (got {date})"
        );
    }

    #[test]
    #[cfg(unix)]
    fn rename_onto_a_hard_link_is_object_exists() {
        let (mut server, dir) =
            test_server_writable("review5-hardlink", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("a"), b"data").unwrap();
        std::fs::hard_link(dir.join("a"), dir.join("b")).unwrap();
        // Renaming a onto its own hard link must not answer noErr and
        // move the bookkeeping to a name the file never took
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&lp("a"));
        data.extend_from_slice(&lp("b"));
        assert_eq!(result_of(server.command(1, &data)).0, ERR_OBJECT_EXISTS);
        // The source keeps its name and both links stay in place
        assert!(dir.join("a").exists());
        assert!(dir.join("b").exists());
    }

    #[test]
    fn case_only_rename_of_a_single_spelling_stays_allowed() {
        let (mut server, dir) =
            test_server_writable("review5-casesolo", crate::meta::Backend::AppleDouble);
        std::fs::write(dir.join("foo"), b"mine").unwrap();
        let mut data = vec![28, 0, 0, 1, 0, 0, 0, 2];
        data.extend_from_slice(&lp("foo"));
        data.extend_from_slice(&lp("Foo"));
        assert_eq!(result_of(server.command(1, &data)).0, 0);
        assert_eq!(std::fs::read(dir.join("Foo")).unwrap(), b"mine");
        assert!(!dir.join("foo").exists());
    }
}
