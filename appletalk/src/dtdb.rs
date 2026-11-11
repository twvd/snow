//! The desktop database and the background scan that feeds it
//!
//! The desktop database holds the APPL mappings and the icon families
//! gathered for the applications the volume serves, keyed by creator.
//! It is fed on demand — a file the client asks about registers its
//! application and its bundle icons immediately — and by a background
//! scan that walks the volume on its own thread and reports its
//! findings in batches. The AFP server (afp.rs) owns the scan thread
//! and merges the batches as they arrive.

use std::path::{Path, PathBuf};

use crate::afp::{AfpSettings, HiddenFiles};
use crate::meta::Meta;
use crate::rfork::BundleIcon;
use crate::wire::wire_log;

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
    /// Whether the volume walk has finished
    pub done: bool,
}

/// The desktop database: the APPL mappings and icons gathered from the
/// files the client has asked about, keyed by creator
pub(crate) struct DeskDb {
    appls: Vec<DeskAppl>,
    /// The icon families, each with its application's creator
    icons: Vec<(u32, BundleIcon)>,
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
    pub(crate) fn icons(&self, creator: u32) -> Vec<&BundleIcon> {
        self.icons
            .iter()
            .filter(|(c, _)| *c == creator)
            .map(|(_, icon)| icon)
            .collect()
    }

    /// The icon family of one (creator, file type) pair
    pub(crate) fn icon(&self, creator: u32, ftype: u32) -> Option<&BundleIcon> {
        self.icons(creator).into_iter().find(|i| i.ftype == ftype)
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
                self.add_icon(creator, icon);
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

    /// Add an icon family. ch13 (FPAddIcon): "If an icon of the same
    /// FileCreator, FileType, and IconType already exists, the icon is
    /// replaced", so an application registered again, after a rename or
    /// by the scan and a lookup both, does not add its icons twice.
    fn add_icon(&mut self, creator: u32, icon: BundleIcon) {
        match self
            .icons
            .iter_mut()
            .find(|(c, i)| *c == creator && i.ftype == icon.ftype)
        {
            Some((_, old)) => *old = icon,
            None => self.icons.push((creator, icon)),
        }
    }

    /// Merge one application the background scan found. The Directory
    /// ID comes from the caller, which owns the fixed-ID map.
    pub(crate) fn merge_group(&mut self, group: DeskGroup, dir_id: u32) {
        if self.appls.iter().any(|a| a.path == group.path) {
            return;
        }
        self.appls.push(DeskAppl {
            creator: group.creator,
            name: group.mac_name,
            dir_id,
            meta: group.meta,
            path: group.path,
        });
        for icon in group.icons {
            self.add_icon(group.creator, icon);
        }
    }
}

/// The findings of one application of the background desktop scan
pub(crate) struct DeskGroup {
    pub(crate) path: PathBuf,
    pub(crate) mac_name: String,
    pub(crate) creator: u32,
    pub(crate) meta: Meta,
    pub(crate) parent: PathBuf,
    pub(crate) icons: Vec<BundleIcon>,
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
    // Hashes of the directories reached through symbolic links (a link
    // can lead back to a directory the walk has seen)
    let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();

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
            if (hidden && settings.hidden_files == HiddenFiles::Hide)
                || crate::name::is_meta_name(host_name)
            {
                continue;
            }
            entries_seen += 1;
            if entries_seen >= next_report_at
                && last_report.elapsed() >= std::time::Duration::from_millis(250)
            {
                report(status, apps_found, icons_found, entries_seen, false);
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
                // A plain directory is reached by one path only; the depth
                // limit bounds a bind-mount loop
                if depth + 1 < settings.scan_depth {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !fs_meta.is_file() {
                continue;
            }
            let mut meta = crate::meta::open_with(&path, settings.probe_apple_double);
            if hidden && settings.hidden_files == HiddenFiles::Invisible {
                // fdFlags: kIsInvisible = bit 6 = 0x4000
                meta.finder_info[8] |= 0x40;
            }
            if &meta.finder_info[0..4] != b"APPL" {
                continue;
            }
            let creator = u32::from_be_bytes(meta.finder_info[4..8].try_into().unwrap_or([0; 4]));
            let mac_name = crate::name::to_mac(host_name).0;
            let icons = read_rsrc_fork(&meta)
                .map(|fork| crate::rfork::bundle_icons(&fork))
                .unwrap_or_default();
            icons_found += icons.len();
            apps_found += 1;
            let parent = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            pending.push(DeskGroup {
                path: path.canonicalize().unwrap_or(path),
                mac_name,
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
                report(status, apps_found, icons_found, entries_seen, false);
            }
        }
    }
    if !pending.is_empty() {
        let groups = std::mem::take(&mut pending);
        let _ = tx.send(DeskBatch { groups });
    }
    report(status, apps_found, icons_found, entries_seen, true);
}

/// A cheap hash of a path for the visited set
fn hash_path(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

/// Report the scan's progress to the UI and the log
fn report(
    status: Option<&crossbeam_channel::Sender<ScanStatus>>,
    apps: usize,
    icons: usize,
    entries: usize,
    done: bool,
) {
    wire_log!(
        "AFP: desktop scan progress: {} applications and {} icons over {} entries{}",
        apps,
        icons,
        entries,
        if done { " (done)" } else { "" }
    );
    if let Some(status) = status {
        let _ = status.send(ScanStatus {
            apps,
            icons,
            entries,
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
