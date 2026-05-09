pub mod lossyinto;
pub mod mac;

use std::ops::{Mul, SubAssign};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use num::{PrimInt, Signed};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub fn take_from_accumulator<T: PrimInt + Signed + Mul<Output = T> + SubAssign>(
    accumulator: &mut T,
    max_amount: T,
) -> T {
    if *accumulator == T::zero() {
        return T::zero();
    }

    let sign = accumulator.signum();
    let available = accumulator.abs();
    let take_amount = max_amount.min(available);
    let actual_taken = sign * take_amount;

    *accumulator -= actual_taken;
    actual_taken
}

/// Serde default helper for Instant::now()
pub fn instant_now() -> Instant {
    Instant::now()
}

/// Atomically writes to `path`. Calls `write` against a sibling temp file,
/// fsyncs, then renames over the destination so a crash mid-write cannot
/// leave a truncated file in place.
pub fn atomic_write<F>(path: &Path, write: F) -> Result<()>
where
    F: FnOnce(&mut std::fs::File) -> Result<()>,
{
    use rand::Rng;
    use std::fs;

    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .context("Destination path has no file name")?;

    let suffix: u64 = rand::rng().random();
    let mut tmp_name = stem.to_owned();
    tmp_name.push(format!(".tmp.{:016x}", suffix));
    let tmp_path = parent.join(tmp_name);

    let result = (|| {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("Creating temp file {}", tmp_path.display()))?;
        write(&mut f)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp_path, path)
            .with_context(|| format!("Renaming {} -> {}", tmp_path.display(), path.display()))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// serialize_with helper for Arc::RwLock<T>
pub fn serialize_arc_rwlock<S, T>(val: &Arc<RwLock<T>>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    val.read().unwrap().serialize(s)
}

/// deserialize_with helper for Arc::RwLock<T>
pub fn deserialize_arc_rwlock<'de, D, T>(d: D) -> Result<Arc<RwLock<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Arc::new(RwLock::new(T::deserialize(d)?)))
}
