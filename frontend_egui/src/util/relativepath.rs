use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Serializable and convertable relative path
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(transparent)]
pub struct RelativePath {
    #[serde(skip)]
    abs: Option<PathBuf>,
    rel: PathBuf,
}

impl RelativePath {
    fn canonicalize_at(path: &Path, basedir: &Path) -> Result<PathBuf> {
        let curdir = env::current_dir().expect("Cannot retrieve current directory");
        if let Err(e) = env::set_current_dir(basedir) {
            env::set_current_dir(curdir).expect("Cannot change back to previous directory");
            bail!(e);
        }
        let result = fs::canonicalize(path);
        env::set_current_dir(curdir).expect("Cannot change back to previous directory");
        Ok(result?)
    }

    pub fn from_absolute(abs: &Path) -> Self {
        Self {
            abs: Some(abs.to_path_buf()),
            rel: Default::default(),
        }
    }

    pub fn after_deserialize(&mut self, basedir: &Path) -> Result<()> {
        self.abs = Some(Self::canonicalize_at(&self.rel, basedir)?);
        Ok(())
    }

    pub fn before_serialize(&mut self, basedir: &Path) -> Result<()> {
        let abs = self.abs.clone().expect("after_deserialize not called");
        self.rel = pathdiff::diff_paths(&abs, basedir).unwrap_or_else(|| {
            log::info!(
                "Cannot get relative path for {}, saving absolute path",
                abs.display()
            );
            abs
        });
        Ok(())
    }

    pub fn get_absolute(&self) -> PathBuf {
        self.abs.clone().expect("after_deserialize not called")
    }

    #[allow(dead_code)]
    pub fn set_absolute(&mut self, abs: &Path) {
        self.abs = Some(abs.to_path_buf());
        self.rel = Default::default();
    }
}
