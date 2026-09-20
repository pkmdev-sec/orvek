//! Dependency-light transport records. No provider, controller, evaluator, or
//! task-completion implementation is linked into the sandbox executable.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const VERSION: u32 = 2;
pub const MAGIC: &[u8; 8] = b"TACTEX02";
pub const MAX_COMMAND_BYTES: usize = 64 * 1024;
// JSON escapes each control byte as six bytes; the remaining fixed fields fit in 1 KiB.
pub const MAX_REQUEST_BYTES: usize = 6 * MAX_COMMAND_BYTES + 1024;
pub const MAX_FRAME_BYTES: usize = 128 * 1024;
pub const CHUNK_BYTES: usize = 64 * 1024;
pub const HELLO: u8 = 1;
pub const STDOUT: u8 = 2;
pub const STDERR: u8 = 3;
pub const ENTRY: u8 = 4;
pub const FILE_DATA: u8 = 5;
pub const COMPLETE: u8 = 6;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub job_id: String,
    pub nonce: String,
    pub command: String,
    pub readonly: bool,
    pub timeout_ms: u64,
    pub output_bytes: u64,
    pub workspace_bytes: u64,
    pub workspace_inodes: u64,
    pub cache_bytes: u64,
    pub cache_inodes: u64,
    pub temporary_bytes: u64,
    pub temporary_inodes: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub version: u32,
    pub job_id: String,
    pub nonce: String,
    pub root_uid: u32,
    pub child_uid: u32,
    pub readonly: bool,
    pub quota: Option<Quota>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quota {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Exited { code: i32 },
    Cancelled,
    TimedOut,
    OutputLimit,
    MemoryLimit,
    QuotaLimit,
    InvalidWorkspace,
    SupervisorError,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Complete {
    pub version: u32,
    pub nonce: String,
    pub outcome: Outcome,
    pub exported: bool,
    pub entries: u64,
    pub bytes: u64,
    pub quiescent: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Entry {
    Directory { path: String, mode: u32 },
    File { path: String, mode: u32, bytes: u64 },
    Symlink { path: String, target: String },
}
impl Entry {
    pub fn path(&self) -> &str {
        match self {
            Self::Directory { path, .. } | Self::File { path, .. } | Self::Symlink { path, .. } => {
                path
            }
        }
    }
}

pub fn write_frame(out: &mut impl Write, kind: u8, data: &[u8]) -> io::Result<()> {
    if data.len() > MAX_FRAME_BYTES {
        return Err(io::Error::other("frame limit"));
    }
    out.write_all(&[kind])?;
    out.write_all(&(data.len() as u32).to_le_bytes())?;
    out.write_all(data)?;
    out.flush()
}
pub fn write_json(out: &mut impl Write, kind: u8, value: &impl Serialize) -> io::Result<()> {
    write_frame(out, kind, &serde_json::to_vec(value)?)
}
pub fn read_request(input: &mut impl Read) -> io::Result<Request> {
    let mut size = [0; 4];
    input.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    if size > MAX_REQUEST_BYTES {
        return Err(io::Error::other("request limit"));
    }
    let mut bytes = vec![0; size];
    input.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximum_command_survives_json_expansion_on_the_executor_wire() {
        let command = format!(": #{}", "\u{1}".repeat(MAX_COMMAND_BYTES - 3));
        let request = Request {
            version: VERSION,
            job_id: "0".repeat(36),
            nonce: "0".repeat(36),
            command: command.clone(),
            readonly: true,
            timeout_ms: u64::MAX,
            output_bytes: u64::MAX,
            workspace_bytes: u64::MAX,
            workspace_inodes: u64::MAX,
            cache_bytes: u64::MAX,
            cache_inodes: u64::MAX,
            temporary_bytes: u64::MAX,
            temporary_inodes: u64::MAX,
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(
            bytes.len() > 256 * 1024,
            "exercise escaped JSON, not just text length"
        );
        let mut wire = (bytes.len() as u32).to_le_bytes().to_vec();
        wire.extend(bytes);
        let decoded = read_request(&mut wire.as_slice())
            .expect("every supported command must fit the helper request envelope");
        assert_eq!(decoded.command, command);
    }

    #[test]
    fn oversized_wire_request_is_rejected_before_reading_its_body() {
        let prefix = ((MAX_REQUEST_BYTES + 1) as u32).to_le_bytes();
        let error = read_request(&mut prefix.as_slice()).unwrap_err();
        assert_eq!(error.to_string(), "request limit");
    }
}
