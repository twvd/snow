//! AppleTalk Filing Protocol (AFP), server side
//!
//! See Inside AppleTalk, 2nd edition, chapter 13.
//!
//! Implements the core of the built-in AFP server: the server
//! information block (served as the ASP GetStatus status block), the
//! version negotiation (the server advertises every AFP version up to
//! its configured maximum, highest first, and accepts logins with any
//! of them), guest authentication, the server parameters and the
//! volume list. The volume is a read-only view of a host directory,
//! with host file names converted between UTF-8 and MacRoman.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ids::{IdTable, ROOT_DIR_ID};
use crate::meta::Meta;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// AFP commands
const CMD_CLOSE_VOL: u8 = 2;
const CMD_GET_SRVR_PARMS: u8 = 16;
const CMD_GET_VOL_PARMS: u8 = 17;
const CMD_LOGIN: u8 = 18;
const CMD_LOGOUT: u8 = 20;
const CMD_ENUMERATE: u8 = 9;
const CMD_OPEN_VOL: u8 = 24;
const CMD_GET_FILE_DIR_PARMS: u8 = 34;
const CMD_CREATE_ID: u8 = 39;
const CMD_DELETE_ID: u8 = 40;
const CMD_RESOLVE_ID: u8 = 41;

/// AFP error codes (afpdocs reference.md "Result Codes")
const ERR_BAD_UAM: i32 = -5002;
const ERR_BAD_VERS: i32 = -5003;
const ERR_OBJECT_NOT_FOUND: i32 = -5018;
const ERR_PARAM: i32 = -5019;
const ERR_CALL_NOT_SUPPORTED: i32 = -5024;
const ERR_BITMAP: i32 = -5004;
#[allow(dead_code)] // used by the fork commands once they land
const ERR_EOF: i32 = -5009;
const ERR_BAD_ID: i32 = -5039;
const ERR_ID_NOT_FOUND: i32 = -5034;
/// A file ID already exists for the file (kept for the error
/// reference table; the identity-table design never reports it)
#[allow(dead_code)]
const ERR_ID_EXISTS: i32 = -5035;
const ERR_DIR_NOT_FOUND: i32 = -5029;
const ERR_OBJECT_TYPE: i32 = -5025;

/// The identifier of the single volume
const VOLUME_ID: u16 = 1;

/// The identifier of the root directory's parent (the HFS convention)
const ROOT_PARENT_DIR_ID: u32 = 1;

/// The AFP protocol versions the server speaks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfpVersion {
    /// AFP 1.1
    V11,
    /// AFP 2.0
    V20,
    /// AFP 2.1: handled on the same wire as 2.0
    V21,
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

impl AfpVersion {
    pub fn from_string(s: &str) -> Option<Self> {
        match s {
            "AFPVersion 1.1" => Some(Self::V11),
            "AFPVersion 2.0" | "AFP2.0" => Some(Self::V20),
            "AFPVersion 2.1" | "AFP2.1" => Some(Self::V21),
            _ => None,
        }
    }

    pub fn string(&self) -> &'static str {
        match self {
            Self::V11 => "AFPVersion 1.1",
            Self::V20 => "AFPVersion 2.0",
            Self::V21 => "AFPVersion 2.1",
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
    #[default]
    Max21,
}

impl MaxVersion {
    /// The versions to advertise, highest first
    pub fn advertised(&self) -> &'static [AfpVersion] {
        match self {
            Self::Max11 => &[AfpVersion::V11],
            Self::Max20 => &[AfpVersion::V20, AfpVersion::V11],
            Self::Max21 => &[AfpVersion::V21, AfpVersion::V20, AfpVersion::V11],
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
    /// Whether the volume accepts writes (unchecked by default: a
    /// write answers afpVolLocked)
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
            probe_apple_single: false,
            wire_debug: false,
            read_only: true,
        }
    }
}

/// AFP protocol module: the server
pub struct Afp {
    /// Canonicalized host directory exposed as the volume
    root: PathBuf,
    /// Volume name
    volume_name: String,
    /// The highest AFP version to advertise and accept
    max_version: MaxVersion,
    /// Mangled wire name → original host name (the per-volume mangling
    /// table)
    mangles: HashMap<String, String>,
    /// The AFP version negotiated at login, per session
    versions: HashMap<u8, AfpVersion>,
    /// The advanced server settings
    settings: AfpSettings,
    /// The volume's ID table (file and directory IDs)
    ids: IdTable,
    /// The response capacity of the request being served (the ATP
    /// requester's offered buffers in bytes): FPRead caps its read at
    /// it. MAX_RESPONSE until the ASP layer records the real value,
    /// so a direct command() call answers like a full window.
    #[allow(dead_code)] // read by FPRead once the fork commands land
    req_capacity: usize,
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
        Self {
            root,
            volume_name: volume_name.unwrap_or_else(|| "Snow".to_string()),
            max_version,
            mangles: HashMap::new(),
            versions: HashMap::new(),
            settings,
            ids: IdTable::new(),
            req_capacity: crate::atp::MAX_RESPONSE,
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
        if data.is_empty() {
            return (err(ERR_PARAM), Vec::new());
        }
        let cmd = data[0];
        match cmd {
            // FPLogin has no reserved byte after the function in either
            // version
            CMD_LOGIN => self.cmd_login(session, data.get(1..).unwrap_or(&[])),
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
                    CMD_CLOSE_VOL | CMD_LOGOUT => (ok(), Vec::new()),
                    CMD_GET_FILE_DIR_PARMS => self.cmd_get_file_dir_parms(version, params),
                    // The file-ID calls are AFP 2.1 and up
                    CMD_CREATE_ID | CMD_DELETE_ID | CMD_RESOLVE_ID
                        if !matches!(version, AfpVersion::V21) =>
                    {
                        (err(ERR_CALL_NOT_SUPPORTED), Vec::new())
                    }
                    CMD_CREATE_ID => self.cmd_create_id(params),
                    CMD_DELETE_ID => self.cmd_delete_id(params),
                    CMD_RESOLVE_ID => self.cmd_resolve_id(params),
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
                matches!(version, AfpVersion::V21),
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
                matches!(version, AfpVersion::V21),
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

    /// Record the requester's response capacity for the command being
    /// served: FPRead caps its read at it, so a reply never exceeds
    /// the buffers the requester offered (the ATP window is 8
    /// packets, MAX_RESPONSE bytes)
    pub(crate) fn set_request_capacity(&mut self, buffers: u8) {
        self.req_capacity = usize::from(buffers) * crate::atp::MAX_RESPONSE_DATA;
    }

    /// Close everything a session owns: its negotiated AFP version
    /// now, its open forks once the fork commands land. FPLogout,
    /// CloseSess and the session-maintenance timeout all land here.
    pub(crate) fn close_session(&mut self, session: u8) {
        self.versions.remove(&session);
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

    /// The host directory a Directory ID refers to (the root is the
    /// fixed ID 2), only while the recorded path still holds the
    /// recorded directory
    fn dir_base(&self, dir_id: u32) -> Option<PathBuf> {
        if dir_id == ROOT_DIR_ID {
            return Some(self.root.clone());
        }
        self.ids.verified_path(dir_id).map(|p| p.to_path_buf())
    }

    fn meta_open(&self, path: &Path) -> Meta {
        crate::meta::open_with(
            path,
            &crate::meta::ProbeFlags {
                apple_double: self.settings.probe_apple_double,
                apple_single: self.settings.probe_apple_single,
            },
        )
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
        let meta = self.meta_open(&target);
        let name = if target == self.root {
            self.volume_name.clone()
        } else {
            self.mac_name_of(&target)
        };
        let parent_id = self.parent_id_of(&target);
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
            let meta = self.meta_open(target);
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
        let meta = self.meta_open(&target);
        let name = self.mac_name_of(&target);
        let parent_id = self.parent_id_of(&target);
        let mut reply = Vec::new();
        reply.extend_from_slice(&bitmap.to_be_bytes());
        // The resolved ID itself is the file number (the CNID)
        reply.extend_from_slice(&file_params(bitmap, &name, &meta, parent_id, id));
        (ok(), reply)
    }
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
        0 => 0u16.to_be_bytes().to_vec(), // attributes (they arrive with the write backends)
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
        0 => 0u16.to_be_bytes().to_vec(), // attributes (they arrive with the write backends)
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
/// bytes free, bit 7: bytes total, bit 8: volume name
fn vol_params(
    bitmap: u16,
    name: &str,
    root: &Path,
    read_only: bool,
    supports_file_ids: bool,
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
        // The real root mtime, so the client's 10-second volume poll
        // sees host-side changes (a directory's mtime moves when its
        // contents change)
        fixed.extend_from_slice(&host_mtime_date(root).to_be_bytes());
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

/// The current time as signed seconds since 2000-01-01 (the AFP
/// date-time epoch)
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
        let vol = vol_params(0x0120, "Snow", Path::new("/"), true, true);
        // Bitmap, then the volume ID, then the name offset, then "Snow"
        assert_eq!(
            &vol[..10],
            &[0x01, 0x20, 0x00, 0x01, 0x00, 0x04, 4, b'S', b'n', b'o']
        );
        assert_eq!(&vol[10..], b"w");
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
    /// An AFP 2.x path name: [type 2][Pascal string]
    fn long_path(name: &str) -> Vec<u8> {
        let mut out = vec![2, name.len() as u8];
        out.extend_from_slice(name.as_bytes());
        out
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
    fn test_file_id_calls_and_volume_attributes() {
        let (mut server, dir) = test_server("file-ids");
        std::fs::write(dir.join("f.txt"), b"x").unwrap();
        // A 2.0 session sees no SupportsFileIDs bit and the file-ID
        // calls answer afpCallNotSupported
        let (r, reply) = result_of(server.command(1, &[17, 0, 0, 1, 0, 1]));
        assert_eq!(r, 0);
        assert_eq!(reply, [0, 1, 0, 1]); // read-only, no file IDs
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
        assert_eq!(reply, [0, 1, 0, 5]); // read-only + SupportsFileIDs
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
        assert_eq!(login(&mut server, 2, "AFP2.0"), ERR_BAD_VERS); // the shorthand is not advertised under Max11
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
    fn test_server_info_lists_versions_high_first() {
        let (server, _dir) = test_server("versions-info");
        let info = server.server_info();
        let versions_off = u16::from_be_bytes([info[2], info[3]]) as usize;
        assert_eq!(info[versions_off], 3); // three version strings
        assert_eq!(
            &info[versions_off + 1..versions_off + 16],
            b"\x0eAFPVersion 2.1"
        );
        assert_eq!(
            &info[versions_off + 16..versions_off + 31],
            b"\x0eAFPVersion 2.0"
        );
        assert_eq!(
            &info[versions_off + 31..versions_off + 46],
            b"\x0eAFPVersion 1.1"
        );
    }
}
