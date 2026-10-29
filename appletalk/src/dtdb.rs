//! The desktop database and the background scan that feeds it
//!
//! The desktop database holds the APPL mappings and the icon families
//! gathered for the applications the volume serves, keyed by creator.
//! It is fed on demand — a file the client asks about registers its
//! application and its bundle icons immediately — and by a background
//! scan that walks the volume on its own thread and reports its
//! findings in batches. The AFP server (afp.rs) owns the scan thread
//! and merges the batches as they arrive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::afp::{AfpSettings, HiddenFiles};
use crate::meta::Meta;
use crate::rfork::IconFamilyEntry;

/// The desktop database reference number handed out by FPOpenDT (the
/// server has one volume, so one desktop database)
pub(crate) const DT_REF_NUM: u16 = 1;

/// An APPL mapping: an application file whose metadata the server has
/// served (the desktop tables are built from the files the client asks
/// about, not by walking the volume)
pub(crate) struct DeskAppl {
    /// The application's creator code
    pub(crate) creator: u32,
    /// The application's wire name
    pub(crate) name: String,
    /// The Directory ID of the application's parent directory
    pub(crate) dir_id: u32,
    /// The application's metadata
    pub(crate) meta: Meta,
    /// The canonical host path (deduplication)
    pub(crate) path: PathBuf,
}

/// The background desktop scan's progress, for display in the UI
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScanStatus {
    /// APPL files found so far
    pub apps: usize,
    /// Icons extracted so far
    pub icons: usize,
    /// Directory entries visited so far
    pub entries: usize,
    /// The total number of entries, once the counting pass is done
    pub total: Option<usize>,
    /// Whether the volume walk has finished
    pub done: bool,
}

/// An icon the desktop extraction found in an application's BNDL
pub(crate) struct DeskIcon {
    /// The owning application's creator code
    pub(crate) creator: u32,
    /// The mapped file type
    pub(crate) ftype: u32,
    /// The 256-byte monochrome icon bitmap
    pub(crate) mono: Vec<u8>,
    /// The 1024-byte 8-bit color pixmap, when the family has one
    pub(crate) color8: Option<Vec<u8>>,
    /// The 512-byte 4-bit color pixmap, when the family has one
    pub(crate) color4: Option<Vec<u8>>,
    /// The 64-byte small monochrome icon, when the family has one
    pub(crate) small_mono: Option<Vec<u8>>,
    /// The 256-byte small 8-bit pixmap, when the family has one
    pub(crate) small8: Option<Vec<u8>>,
    /// The 128-byte small 4-bit pixmap, when the family has one
    pub(crate) small4: Option<Vec<u8>>,
}

/// The desktop database: the APPL mappings and icons gathered from the
/// files the client has asked about, keyed by creator
pub(crate) struct DeskDb {
    appls: Vec<DeskAppl>,
    icons: Vec<DeskIcon>,
    /// Whether the client has the database open (FPOpenDT/FPCloseDT)
    open: bool,
}

impl DeskDb {
    pub(crate) fn new() -> Self {
        Self {
            appls: Vec::new(),
            icons: Vec::new(),
            open: false,
        }
    }

    /// The FPOpenDT/FPCloseDT state
    pub(crate) fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// The number of APPL mappings gathered so far
    pub(crate) fn application_count(&self) -> usize {
        self.appls.len()
    }

    /// The number of icon families gathered so far
    pub(crate) fn icon_count(&self) -> usize {
        self.icons.len()
    }

    /// The APPL mappings of one creator
    pub(crate) fn applications(&self, creator: u32) -> Vec<&DeskAppl> {
        self.appls.iter().filter(|a| a.creator == creator).collect()
    }

    /// The icon families of one creator
    pub(crate) fn icons(&self, creator: u32) -> Vec<&DeskIcon> {
        self.icons.iter().filter(|i| i.creator == creator).collect()
    }

    /// The icon family of one (creator, file type) pair
    pub(crate) fn icon(&self, creator: u32, ftype: u32) -> Option<&DeskIcon> {
        self.icons
            .iter()
            .find(|i| i.creator == creator && i.ftype == ftype)
    }

    /// Drop a file's APPL entry (after a delete or rename); the
    /// bundle icons stay (they are shared between applications of a
    /// creator)
    pub(crate) fn remove_appl(&mut self, target: &Path) {
        self.appls.retain(|a| a.path != target);
    }

    /// Register an APPL file and its bundle icons while its parameters
    /// are being served, so the desktop calls that follow find them
    /// without any volume walk
    pub(crate) fn register_appl(&mut self, target: &Path, name: &str, meta: &Meta, parent_id: u32) {
        if !self.open || &meta.finder_info[0..4] != b"APPL" {
            return;
        }
        if self.appls.iter().any(|a| a.path == target) {
            return;
        }
        let creator = u32::from_be_bytes(meta.finder_info[4..8].try_into().unwrap_or([0; 4]));
        if let Some(fork) = read_rsrc_fork(meta) {
            for icon in crate::rfork::bundle_icons(&fork) {
                self.icons.push(DeskIcon {
                    creator,
                    ftype: icon.ftype,
                    mono: icon.mono,
                    color8: icon.color8,
                    color4: icon.color4,
                    small_mono: icon.small_mono,
                    small8: icon.small8,
                    small4: icon.small4,
                });
            }
        }
        self.appls.push(DeskAppl {
            creator,
            name: name.to_string(),
            dir_id: parent_id,
            meta: meta.clone(),
            path: target.to_path_buf(),
        });
    }

    /// Merge one application the background scan found. The mangled
    /// wire name is recorded in the caller's mangling table, and the
    /// Directory ID comes from the caller, which owns the fixed-ID map.
    pub(crate) fn merge_group(
        &mut self,
        group: DeskGroup,
        dir_id: u32,
        mangles: &mut HashMap<String, String>,
    ) {
        if self.appls.iter().any(|a| a.path == group.path) {
            return;
        }
        if let Some(original) = group.original {
            mangles.entry(group.mac_name.clone()).or_insert(original);
        }
        self.appls.push(DeskAppl {
            creator: group.creator,
            name: group.mac_name,
            dir_id,
            meta: group.meta,
            path: group.path,
        });
        for (ftype, (mono, color8, color4, small_mono, small8, small4)) in group.icons {
            self.icons.push(DeskIcon {
                creator: group.creator,
                ftype,
                mono,
                color8,
                color4,
                small_mono,
                small8,
                small4,
            });
        }
    }
}

/// The findings of one application of the background desktop scan
pub(crate) struct DeskGroup {
    pub(crate) path: PathBuf,
    pub(crate) mac_name: String,
    pub(crate) original: Option<String>,
    pub(crate) creator: u32,
    pub(crate) meta: Meta,
    pub(crate) parent: PathBuf,
    pub(crate) icons: Vec<IconFamilyEntry>,
}

/// A batch of the background desktop scan's findings
pub(crate) struct DeskBatch {
    pub(crate) groups: Vec<DeskGroup>,
}

/// The background desktop scan: walk the volume for APPL files and
/// report each with its bundle icons in batches
pub(crate) fn scan_volume(
    root: PathBuf,
    tx: &std::sync::mpsc::Sender<DeskBatch>,
    abort: &std::sync::atomic::AtomicBool,
    status: Option<&crossbeam_channel::Sender<ScanStatus>>,
    settings: AfpSettings,
) {
    // A cap on the walk: bounds its time and the memory the visited set
    // can grow to (the depth limit is the defense against endless
    // trees, such as bind-mount cycles)
    let max_entries = settings.scan_entries_cap as usize;
    let jail = root.clone();
    let mut stack: Vec<(PathBuf, u32)> = vec![(root, 0)];
    // Hashes of the paths already visited (a hash keeps the set small
    // on trees with millions of directories)
    let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
    // Phase 1: count the entries the scan will visit (directories
    // only, no metadata probes) so the progress can be determinate
    let total = count_entries(&jail, &settings, status, abort);

    let mut pending: Vec<DeskGroup> = Vec::new();
    let mut apps_found = 0usize;
    let mut icons_found = 0usize;
    let mut entries_seen = 0usize;
    // Progress reports are throttled: at most every quarter second and
    // no more often than every 4096 newly visited entries, so a fast
    // scan does not spend its time on display updates
    let mut last_report = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut next_report_at = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if abort.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if abort.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let path = entry.path();
            if entries_seen >= max_entries {
                log::warn!(
                    "AFP: desktop scan capped at {} entries on '{}'",
                    max_entries,
                    path.display()
                );
                break;
            }
            let Some(host_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // Hidden entries: skipped entirely in Hide mode; shown
            // (with the Finder's invisible attribute) otherwise
            let hidden = crate::name::is_hidden(host_name);
            if hidden && settings.hidden_files == HiddenFiles::Hide {
                continue;
            }
            entries_seen += 1;
            if entries_seen >= next_report_at
                && last_report.elapsed() >= std::time::Duration::from_millis(250)
            {
                report(
                    status,
                    apps_found,
                    icons_found,
                    entries_seen,
                    Some(total),
                    false,
                );
                last_report = std::time::Instant::now();
                next_report_at = entries_seen + 4096;
            }
            let Ok(fs_meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if fs_meta.file_type().is_symlink() {
                // Symlinks are only followed when enabled AND when
                // they stay inside the share root: the walk never
                // escapes the jail
                if !settings.follow_symlinks || depth + 1 >= settings.scan_depth {
                    continue;
                }
                let Ok(real) = std::fs::metadata(&path) else {
                    continue;
                };
                if !real.is_dir() {
                    continue;
                }
                let Ok(canonical) = path.canonicalize() else {
                    continue;
                };
                if canonical.strip_prefix(&jail).is_err() {
                    continue;
                }
                if visited.insert(hash_path(&canonical)) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if fs_meta.is_dir() {
                if depth + 1 >= settings.scan_depth {
                    // The depth limit bounds recursion (a bind-mount
                    // loop cannot go deeper than this)
                    continue;
                }
                if visited.insert(hash_path(&path)) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !fs_meta.is_file() {
                continue;
            }
            let mut meta = crate::meta::open_with(
                &path,
                &crate::meta::ProbeFlags {
                    apple_double: settings.probe_apple_double,
                    apple_single: settings.probe_apple_single,
                },
            );
            if hidden && settings.hidden_files == HiddenFiles::Invisible {
                // fdFlags: kIsInvisible = bit 6 = 0x4000
                meta.finder_info[8] |= 0x40;
            }
            if &meta.finder_info[0..4] != b"APPL" {
                continue;
            }
            let creator = u32::from_be_bytes(meta.finder_info[4..8].try_into().unwrap_or([0; 4]));
            let (mac_name, original) = crate::name::to_mac(host_name);
            let icons: Vec<IconFamilyEntry> = read_rsrc_fork(&meta)
                .map(|fork| {
                    crate::rfork::bundle_icons(&fork)
                        .into_iter()
                        .map(|icon| {
                            (
                                icon.ftype,
                                (
                                    icon.mono,
                                    icon.color8,
                                    icon.color4,
                                    icon.small_mono,
                                    icon.small8,
                                    icon.small4,
                                ),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            icons_found += icons.len();
            apps_found += 1;
            let parent = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            pending.push(DeskGroup {
                path: path.canonicalize().unwrap_or(path),
                mac_name,
                original,
                creator,
                meta,
                parent,
                icons,
            });
            if pending.len() >= 64 {
                let groups = std::mem::take(&mut pending);
                if tx.send(DeskBatch { groups }).is_err() {
                    return;
                }
                report(
                    status,
                    apps_found,
                    icons_found,
                    entries_seen,
                    Some(total),
                    false,
                );
            }
        }
    }
    if !pending.is_empty() {
        let groups = std::mem::take(&mut pending);
        let _ = tx.send(DeskBatch { groups });
    }
    report(
        status,
        apps_found,
        icons_found,
        entries_seen,
        Some(total),
        true,
    );
}

/// Phase 1 of the desktop scan: count the directory entries the scan
/// will visit, so its progress can be determinate. The walk rules
/// mirror scan_volume exactly (depth limit, entry cap, hidden policy,
/// symlink policy), but nothing is stat'ed or read beyond listing the
/// directories.
fn count_entries(
    root: &Path,
    settings: &AfpSettings,
    status: Option<&crossbeam_channel::Sender<ScanStatus>>,
    abort: &std::sync::atomic::AtomicBool,
) -> usize {
    let max_entries = settings.scan_entries_cap as usize;
    let mut stack: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), 0)];
    let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut count = 0usize;
    let mut last_report = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut next_report_at = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if abort.load(std::sync::atomic::Ordering::Relaxed) {
            return count;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if abort.load(std::sync::atomic::Ordering::Relaxed) {
                return count;
            }
            if count >= max_entries {
                return count;
            }
            let path = entry.path();
            let Some(host_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let hidden = crate::name::is_hidden(host_name);
            if hidden && settings.hidden_files == HiddenFiles::Hide {
                continue;
            }
            count += 1;
            if count >= next_report_at
                && last_report.elapsed() >= std::time::Duration::from_millis(250)
            {
                report(status, 0, 0, count, None, false);
                last_report = std::time::Instant::now();
                next_report_at = count + 4096;
            }
            // The descent rules mirror the scan: symlinks only when
            // enabled and inside the root, dirs down to the depth limit
            let is_symlink = std::fs::symlink_metadata(&path)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            if is_symlink {
                if !settings.follow_symlinks || depth + 1 >= settings.scan_depth {
                    continue;
                }
                let Ok(canonical) = path.canonicalize() else {
                    continue;
                };
                if canonical.strip_prefix(root).is_err() {
                    continue;
                }
                if visited.insert(hash_path(&canonical)) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let is_dir = std::fs::symlink_metadata(&path)
                .map(|m| m.is_dir())
                .unwrap_or(false);
            if is_dir && depth + 1 < settings.scan_depth && visited.insert(hash_path(&path)) {
                stack.push((path, depth + 1));
            }
        }
    }
    count
}

/// A cheap hash of a path for the visited set
fn hash_path(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

/// Report the scan's progress to the UI and the log
#[allow(clippy::too_many_arguments)]
fn report(
    status: Option<&crossbeam_channel::Sender<ScanStatus>>,
    apps: usize,
    icons: usize,
    entries: usize,
    total: Option<usize>,
    done: bool,
) {
    log::debug!(
        "AFP: desktop scan progress: {} applications and {} icons over {}/{} entries{}",
        apps,
        icons,
        entries,
        total.map_or_else(|| "-".to_string(), |t| t.to_string()),
        if done { " (done)" } else { "" }
    );
    if let Some(status) = status {
        let _ = status.send(ScanStatus {
            apps,
            icons,
            entries,
            total,
            done,
        });
    }
}

/// Read an APPL file's resource fork out of its metadata container,
/// capped for sanity
fn read_rsrc_fork(meta: &Meta) -> Option<Vec<u8>> {
    const MAX_FORK: u64 = 32 * 1024 * 1024;
    if meta.rsrc_fork_len == 0 || u64::from(meta.rsrc_fork_len) > MAX_FORK {
        return None;
    }
    let file = meta.rsrc_file.as_ref()?;
    use std::io::{Read, Seek};
    let mut f = std::fs::File::open(file).ok()?;
    f.seek(std::io::SeekFrom::Start(meta.rsrc_offset)).ok()?;
    let mut buf = vec![0u8; meta.rsrc_fork_len as usize];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}
