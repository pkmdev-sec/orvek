//! Portable file adapter. The manifest is written last; partial archives cannot import.
use crate::{
    LocalMemoryStore, MemoryError, MemoryImportReport, MemoryKey, MemoryLimits, MemoryRecord,
    MemoryStore, sources::digest,
};
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryArchive {
    pub version: u32,
    pub records: Vec<ArchiveEntry>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveEntry {
    pub key: MemoryKey,
    pub digest: String,
}

impl MemoryArchive {
    /// Writes a new directory containing a manifest and human-readable exact records.
    pub async fn export<S: MemoryStore>(store: &S, directory: &Path) -> Result<Self, MemoryError> {
        let records = store.export_all(None).await?;
        let directory = directory.to_owned();
        tokio::task::spawn_blocking(move || {
            fs::create_dir(&directory).map_err(MemoryError::backend)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                    .map_err(MemoryError::backend)?;
            }
            let mut manifest = Self {
                version: 1,
                records: Vec::new(),
            };
            for record in records {
                let bytes = serde_json::to_vec_pretty(&record).map_err(MemoryError::backend)?;
                let hash = digest(&bytes);
                fs::write(directory.join(format!("{hash}.json")), bytes)
                    .map_err(MemoryError::backend)?;
                manifest.records.push(ArchiveEntry {
                    key: record.key,
                    digest: hash,
                });
            }
            fs::write(
                directory.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).map_err(MemoryError::backend)?,
            )
            .map_err(MemoryError::backend)?;
            Ok(manifest)
        })
        .await
        .map_err(MemoryError::backend)?
    }

    /// Validates every payload before any store mutation. No archive-provided path is followed.
    pub async fn import(
        directory: &Path,
        store: &LocalMemoryStore,
    ) -> Result<MemoryImportReport, MemoryError> {
        let directory = directory.to_owned();
        let records = tokio::task::spawn_blocking(move || {
            let bytes = read_file(&directory.join("manifest.json"), 256 * 1024)?;
            let manifest: Self = serde_json::from_slice(&bytes).map_err(MemoryError::backend)?;
            if manifest.version != 1 || manifest.records.len() > MemoryLimits::PRODUCTION.records {
                return Err(MemoryError::InvalidMetadata);
            }
            let mut records = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for entry in manifest.records {
                if !crate::evidence::valid_digest(&entry.digest) || !seen.insert(entry.key.clone())
                {
                    return Err(MemoryError::InvalidMetadata);
                }
                let bytes =
                    read_file(&directory.join(format!("{}.json", entry.digest)), 32 * 1024)?;
                if digest(&bytes) != entry.digest {
                    return Err(MemoryError::InvalidMetadata);
                }
                let record: MemoryRecord =
                    serde_json::from_slice(&bytes).map_err(MemoryError::backend)?;
                if record.key != entry.key {
                    return Err(MemoryError::InvalidMetadata);
                }
                record.metadata.validate()?;
                records.push(record);
            }
            Ok::<_, MemoryError>(records)
        })
        .await
        .map_err(MemoryError::backend)??;
        store.import_records(records).await
    }
}
fn read_file(path: &Path, maximum: usize) -> Result<Vec<u8>, MemoryError> {
    use std::io::Read;
    let metadata = fs::symlink_metadata(path).map_err(MemoryError::backend)?;
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err(MemoryError::InvalidMetadata);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(MemoryError::backend)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(MemoryError::backend)?;
    if bytes.len() > maximum {
        return Err(MemoryError::backend(io::Error::other(
            "archive payload exceeds bound",
        )));
    }
    Ok(bytes)
}
