//! Append-only events and durable artifacts, outside the writable sandbox.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Trace {
    file: Mutex<File>,
    error: Mutex<Option<String>>,
}

impl Trace {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
            error: Mutex::new(None),
        })
    }

    pub fn check(&self) -> Result<()> {
        if let Some(error) = &*self.error.lock().unwrap() {
            bail!("trace write failed: {error}");
        }
        Ok(())
    }

    pub fn event(&self, attempt: &str, kind: &str, fields: Value) {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut record = json!({"ts_ms": ms, "attempt": attempt, "event": kind});
        if let (Some(r), Some(f)) = (record.as_object_mut(), fields.as_object()) {
            r.extend(f.clone());
        }
        let mut line = serde_json::to_vec(&record).expect("JSON value");
        line.push(b'\n');
        if let Err(error) = self.file.lock().unwrap().write_all(&line) {
            eprintln!("trace write failed: {error}");
            *self.error.lock().unwrap() = Some(error.to_string());
        }
    }
}

/// Atomic, durable replacement in a private directory never mounted in a sandbox.
pub fn artifact(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        File::open(path.parent().expect("artifact parent"))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
