//! Durable snapshots and append-only events. The hub owns per-run locking.
use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct RunStore {
    dir: PathBuf,
}
impl RunStore {
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)
            .with_context(|| format!("{}: create run directory", dir.display()))?;
        Ok(Self {
            dir: dir.to_owned(),
        })
    }
    pub fn write_run<T: Serialize>(&self, state: &T) -> Result<()> {
        let path = self.dir.join("run.json");
        // Serialize first: serializer failure must not alter committed state.
        let bytes = serde_json::to_vec_pretty(state)
            .with_context(|| format!("{}: serialize state", path.display()))?;
        let result = (|| -> Result<()> {
            let mut temp = tempfile::Builder::new()
                .prefix(".run.json.")
                .suffix(".tmp")
                .tempfile_in(&self.dir)?;
            temp.write_all(&bytes)?;
            temp.write_all(b"\n")?;
            temp.as_file().sync_all()?;
            temp.persist(&path)?;
            self.sync_directory()?;
            Ok(())
        })();
        result.with_context(|| format!("{}: atomic state write", path.display()))
    }
    pub fn read_run<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        let path = self.dir.join("run.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("{}: read state", path.display()));
            }
        };
        // Temp files are never promoted, even when committed state is absent.
        serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("{}: invalid committed state", path.display()))
    }
    pub fn append_event<T: Serialize>(&self, event: &T) -> Result<()> {
        let path = self.dir.join("events.jsonl");
        let mut bytes = serde_json::to_vec(event)
            .with_context(|| format!("{}: serialize event", path.display()))?;
        bytes.push(b'\n');
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            self.sync_directory()?;
            Ok(())
        })();
        result.with_context(|| format!("{}: append event", path.display()))
    }
    fn sync_directory(&self) -> Result<()> {
        // Hermon targets Unix. Sync the rename/file creation and file contents.
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
}
