//! AppleTalk Filing Protocol (AFP) 1.1 to 2.1, server side
//!
//! See Inside AppleTalk, 2nd edition, chapter 13, and the AFP 2.1
//! specification.
//!
//! Serves one volume, a host directory, to guests: the server information
//! block (served as the ASP GetStatus status block), login with guest
//! authentication and the volume parameters. Read-only by default.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::wire::wire_log;

mod params;

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
const ERR_BITMAP: i32 = -5004;
const ERR_OBJECT_NOT_FOUND: i32 = -5018;
const ERR_PARAM: i32 = -5019;
const ERR_CALL_NOT_SUPPORTED: i32 = -5024;

/// The identifier of the single volume
const VOLUME_ID: u16 = 1;

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

/// The advanced AFP server settings
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AfpSettings {
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
            wire_debug: false,
            read_only: true,
        }
    }
}

/// What a command answers: its reply data, or an AFP result code
type Reply = Result<Vec<u8>, i32>;

/// AFP protocol module: the server
pub struct Afp {
    /// Canonicalized host directory exposed as the volume
    root: PathBuf,
    /// Volume name
    volume_name: String,
    /// The highest AFP version to advertise and accept
    max_version: MaxVersion,
    /// The AFP version negotiated at login, per session
    versions: HashMap<u8, AfpVersion>,
    /// The advanced server settings
    settings: AfpSettings,
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
            versions: HashMap::new(),
            settings,
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
        // A 1.1 request is just the function byte for several commands
        if data.is_empty() {
            return (err(ERR_PARAM), Vec::new());
        }
        let cmd = data[0];
        let reply = match cmd {
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
                    Ok(Vec::new())
                }
            }
            _ => {
                // Every other command has the reserved byte after the
                // function in both versions; a short command is a
                // parameter error, never a panic
                let params = data.get(2..).unwrap_or(&[]);
                match cmd {
                    CMD_OPEN_VOL => self.cmd_open_vol(params),
                    CMD_GET_VOL_PARMS => self.cmd_get_vol_parms(params),
                    CMD_CLOSE_VOL => Ok(Vec::new()),
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

    /// Close everything a session owns: its negotiated AFP version.
    /// FPLogout, CloseSess and a guest restart all land here.
    pub(crate) fn close_session(&mut self, session: u8) {
        self.versions.remove(&session);
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

    fn u8(&mut self) -> Result<u8, i32> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, i32> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    /// A Pascal string: a length byte and that many MacRoman characters
    fn pstring(&mut self) -> Result<String, i32> {
        let len = usize::from(self.u8()?);
        let (s, _, _) = encoding_rs::MACINTOSH.decode(self.bytes(len)?);
        Ok(s.into_owned())
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
