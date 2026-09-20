//! Bounded artifact reads tied to the host's content identities.

use super::{
    error::{Error, Result},
    host::HostClient,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use orvek_harness::{
    Digest,
    ipc::{Command, Response},
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct HostArtifacts {
    client: HostClient,
}
impl HostArtifacts {
    pub(crate) fn new(client: HostClient) -> Self {
        Self { client }
    }
    pub(crate) async fn read(
        &self,
        digest: Digest,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        if limit == 0 || limit > 128 * 1024 * 1024 {
            return Err(Error::HostRequest(
                "artifact read limit must be 1..128 MiB".into(),
            ));
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Chunk {
            digest: Digest,
            offset: usize,
            bytes: usize,
            encoding: String,
            data: String,
            next: Option<usize>,
        }
        let mut content = Vec::new();
        let mut total = None;
        loop {
            let response = tokio::select! {()=cancel.cancelled()=>return Err(Error::HostRequest("artifact read cancelled".into())),response=self.client.query(Command::ReadArtifact {digest,offset:content.len(),limit:65536})=>response?};
            let Response::Artifact(value) = response else {
                return Err(Error::HostRequest("expected artifact chunk".into()));
            };
            let chunk: Chunk = serde_json::from_value(value)
                .map_err(|_| Error::HostRequest("invalid artifact chunk".into()))?;
            if chunk.digest != digest
                || chunk.offset != content.len()
                || chunk.encoding != "base64"
                || chunk.bytes > limit
                || chunk.data.len() > 87384
                || total.is_some_and(|total| total != chunk.bytes)
            {
                return Err(Error::HostRequest(
                    "artifact chunk identity or size mismatch".into(),
                ));
            }
            total = Some(chunk.bytes);
            let bytes = STANDARD
                .decode(chunk.data)
                .map_err(|_| Error::HostRequest("invalid artifact encoding".into()))?;
            if bytes.len() > 65536 || content.len().saturating_add(bytes.len()) > chunk.bytes {
                return Err(Error::HostRequest(
                    "artifact chunk exceeds its declared extent".into(),
                ));
            }
            content.extend(bytes);
            match chunk.next {
                Some(next)
                    if next == content.len() && next < chunk.bytes && next > chunk.offset => {}
                None if content.len() == chunk.bytes => break,
                _ => {
                    return Err(Error::HostRequest(
                        "artifact cursor did not advance consistently".into(),
                    ));
                }
            }
        }
        if Digest::of(&content) != digest {
            return Err(Error::HostRequest(
                "downloaded artifact digest mismatch".into(),
            ));
        }
        Ok(content)
    }
}
