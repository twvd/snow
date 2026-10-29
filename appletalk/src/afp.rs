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

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// AFP commands
const CMD_CLOSE_VOL: u8 = 2;
const CMD_GET_SRVR_PARMS: u8 = 16;
const CMD_GET_VOL_PARMS: u8 = 17;
const CMD_LOGIN: u8 = 18;
const CMD_LOGOUT: u8 = 20;
const CMD_OPEN_VOL: u8 = 24;

/// AFP error codes (afpdocs reference.md "Result Codes")
const ERR_BAD_UAM: i32 = -5002;
const ERR_BAD_VERS: i32 = -5003;
const ERR_OBJECT_NOT_FOUND: i32 = -5018;
const ERR_PARAM: i32 = -5019;
const ERR_CALL_NOT_SUPPORTED: i32 = -5024;

/// The identifier of the single volume
const VOLUME_ID: u16 = 1;

/// The identifier of the root directory's parent (the HFS convention)
#[allow(dead_code)] // read by the path resolution once the directory commands land
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
    #[allow(dead_code)] // read by the path resolution once the directory commands land
    mangles: HashMap<String, String>,
    /// The AFP version negotiated at login, per session
    versions: HashMap<u8, AfpVersion>,
    /// The advanced server settings
    settings: AfpSettings,
    /// The volume's ID table (file and directory IDs)
    #[allow(dead_code)] // read by the directory commands once they land
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
    #[allow(dead_code)] // called by the directory commands once they land
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
    #[allow(dead_code)] // called by the directory commands once they land
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
    #[allow(dead_code)] // called by the directory commands once they land
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
    #[allow(dead_code)] // called by the directory commands once they land
    fn dir_base(&self, dir_id: u32) -> Option<PathBuf> {
        if dir_id == ROOT_DIR_ID {
            return Some(self.root.clone());
        }
        self.ids.verified_path(dir_id).map(|p| p.to_path_buf())
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

fn get_u16(data: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes([*data.first()?, *data.get(1)?]))
}

#[allow(dead_code)] // used by the directory commands once they land
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
