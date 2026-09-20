//! Candidate JSON has no execution authority. Tests live in host artifacts,
//! never under the candidate directory. The grader executes only built-in reads.
use super::{Episode, ReadCase};
use crate::{
    Digest,
    artifacts::ArtifactStore,
    capabilities::host::{HostToolContext, HostTools},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, path::Path};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateConfig {
    pub native_read_output_bytes: u32,
}

pub(crate) fn heldout() -> Vec<ReadCase> {
    vec![
        ReadCase {
            bytes: vec![],
            offset: 0,
            max_bytes: 4096,
        },
        ReadCase {
            bytes: b"short".to_vec(),
            offset: 2,
            max_bytes: 4096,
        },
        ReadCase {
            bytes: b"eof".to_vec(),
            offset: 50,
            max_bytes: 12,
        },
        ReadCase {
            bytes: vec![b'"'; 4096],
            offset: 0,
            max_bytes: 4096,
        },
        ReadCase {
            bytes: vec![255; 4096],
            offset: 13,
            max_bytes: 4000,
        },
        ReadCase {
            bytes: "界".repeat(1500).into_bytes(),
            offset: 1,
            max_bytes: 4096,
        },
    ]
}
pub(crate) fn decoded_content(value: &Value) -> Option<Vec<u8>> {
    let content = &value["result"]["content"];
    match content["encoding"].as_str()? {
        "utf8" => Some(content["data"].as_str()?.as_bytes().to_vec()),
        "base64" => STANDARD.decode(content["data"].as_str()?).ok(),
        _ => None,
    }
}
async fn check(case: &ReadCase, capacity: u32) -> Result<bool, String> {
    let workspace = tempfile::tempdir().map_err(|e| e.to_string())?;
    fs::write(workspace.path().join("input"), &case.bytes).map_err(|e| e.to_string())?;
    let run = HostTools::new()
        .execute_recorded(
            "read_file",
            json!({"path":"input","offset":case.offset,"max_bytes":case.max_bytes}),
            HostToolContext {
                cwd: workspace.path().to_owned(),
                task_id: Uuid::new_v4(),
                generation: 1,
                job_id: Uuid::new_v4(),
                timeout_ms: Some(60_000),
                max_output_bytes: capacity as usize,
            },
            CancellationToken::new(),
        )
        .await;
    let value = run.result.map_err(|e| e.to_string())?;
    let start = case.offset.min(case.bytes.len());
    let end = start.saturating_add(case.max_bytes).min(case.bytes.len());
    Ok(
        decoded_content(&value).as_deref() == Some(&case.bytes[start..end])
            && value["result"]["digest"] == json!(Digest::of(&case.bytes))
            && value["result"]["offset"] == json!(start)
            && value["result"]["size_bytes"] == json!(case.bytes.len())
            && value["result"]["truncated"] == json!(end < case.bytes.len()),
    )
}

pub(crate) async fn evaluate(
    artifacts: &ArtifactStore,
    episode: &Episode,
    candidate: &Path,
    before: u32,
    expected: u32,
) -> Result<Value, String> {
    let bytes = fs::read(candidate).map_err(|e| e.to_string())?;
    if bytes.len() > 1024 {
        return Err("candidate exceeds bound".into());
    }
    let config: CandidateConfig = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if config.native_read_output_bytes != expected {
        return Err("candidate is not the diagnosed parent configuration".into());
    }
    if episode.candidate != Some(Digest::of(&bytes)) {
        return Err("candidate digest changed after freeze".into());
    }
    let regression: ReadCase = serde_json::from_slice(
        &artifacts
            .read(episode.regression)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let heldout: Vec<ReadCase> =
        serde_json::from_slice(&artifacts.read(episode.heldout).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    // Expectations are pinned before candidate creation; a content digest is checked again
    // by ArtifactStore. Candidate JSON cannot name a grader, file, expectation or command.
    if check(&regression, before).await? {
        return Ok(
            json!({"accepted":false,"reason":"triggering control did not reproduce","origin":"evaluation"}),
        );
    }
    let mut outcomes = Vec::new();
    for repeat in 0..2 {
        for (index, case) in std::iter::once(&regression).chain(&heldout).enumerate() {
            let passed = check(case, expected).await?;
            outcomes.push(json!({"repeat":repeat,"case":index,"passed":passed}));
            if !passed {
                return Ok(
                    json!({"accepted":false,"reason":format!("outcome check failed at case {index}"),"checks":outcomes,"origin":"evaluation"}),
                );
            }
        }
    }
    Ok(
        json!({"accepted":true,"origin":"evaluation","kind":"deterministic_native_read_capacity","regression":episode.regression,"heldout":episode.heldout,"candidate":episode.candidate,"control_failed":true,"checks":outcomes,"model_comparisons":null,"scope":"restores bounded native read replies only; no model or task-quality inference"}),
    )
}
