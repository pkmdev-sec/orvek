//! Frozen review presentation built from authenticated host artifacts.
mod anchors;
mod host;
use crate::app::host::HostClient;
use orvek_harness::{Digest, review::ReviewRange as HostRange, state::TaskId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct ReviewRange {
    pub(crate) from: usize,
    pub(crate) to: usize,
}
#[derive(Clone, Debug, Serialize)]
pub(super) struct ReviewTarget {
    pub(super) index: usize,
    pub(super) kind: ReviewTargetKind,
    pub(super) short_id: String,
    pub(super) title: String,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReviewTargetKind {
    Trunk,
    Commit,
    WorkingTree,
}
#[derive(Clone)]
struct RangePoint {
    target: ReviewTarget,
    revision: Option<String>,
}
#[derive(Clone)]
pub(super) struct ReviewContext {
    client: HostClient,
    root: PathBuf,
    task: Option<TaskId>,
    repository: String,
    trunk: String,
    points: Vec<RangePoint>,
    default_from: usize,
    version: WorkspaceVersion,
}
#[derive(Clone, Serialize)]
pub(super) struct DiffSnapshot {
    pub(super) patch: String,
    #[serde(skip)]
    pub(super) overview: OverviewContext,
    pub(super) repository: String,
    pub(super) scope: String,
    pub(super) base: String,
    pub(super) manifest: Digest,
    pub(super) source_identity: Digest,
    pub(super) metadata_changes: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OverviewContext {
    pub(super) repository: PathBuf,
    pub(super) range: OverviewRange,
    pub(super) manifest: Digest,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum OverviewRange {
    Commits { base: String, head: String },
    WorkingTree { base: String },
    Task { task: TaskId },
}
#[derive(Clone, Copy)]
pub(super) enum PatchSide {
    Additions,
    Deletions,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkspaceVersion(Digest);
impl ReviewContext {
    pub(super) fn repository(&self) -> &str {
        &self.repository
    }
    pub(super) fn trunk_name(&self) -> &str {
        &self.trunk
    }
    pub(super) fn range_targets(&self) -> Vec<ReviewTarget> {
        self.points
            .iter()
            .map(|point| point.target.clone())
            .collect()
    }
    pub(super) fn default_range(&self) -> ReviewRange {
        ReviewRange {
            from: self.default_from,
            to: self.points.len() - 1,
        }
    }
    pub(super) fn version(&self) -> WorkspaceVersion {
        self.version.clone()
    }
    pub(super) fn range_label(&self, range: ReviewRange) -> Result<String, DiffError> {
        self.validate_range(range)?;
        if self.task.is_some() {
            return Ok("Task baseline → candidate".into());
        }
        if range.to == self.points.len() - 1 && range.from + 1 == range.to {
            return Ok("Uncommitted changes".into());
        }
        Ok(format!(
            "{} → {}",
            self.points[range.from].target.short_id, self.points[range.to].target.short_id
        ))
    }
    fn validate_range(&self, range: ReviewRange) -> Result<(), DiffError> {
        if range.from < range.to && range.to < self.points.len() {
            Ok(())
        } else {
            Err(DiffError::InvalidRange {
                from: range.from,
                to: range.to,
                target_count: self.points.len(),
            })
        }
    }
    pub(super) async fn collect(&self, range: ReviewRange) -> Result<DiffSnapshot, DiffError> {
        self.validate_range(range)?;
        let (inspection, overview_range, base) = if let Some(task) = self.task {
            (
                host::inspect_task(&self.client, task).await?,
                OverviewRange::Task { task },
                "Task baseline".into(),
            )
        } else {
            let base = self.points[range.from]
                .revision
                .clone()
                .ok_or(DiffError::InvalidCommitMetadata)?;
            let (host_range, overview_range) = match &self.points[range.to].revision {
                Some(head) => (
                    HostRange::Between {
                        base: base.clone(),
                        head: head.clone(),
                    },
                    OverviewRange::Commits {
                        base: base.clone(),
                        head: head.clone(),
                    },
                ),
                None => (
                    HostRange::WorkingTree {
                        base: Some(base.clone()),
                    },
                    OverviewRange::WorkingTree { base: base.clone() },
                ),
            };
            (
                host::inspect_workspace(&self.client, &self.root, host_range).await?,
                overview_range,
                base,
            )
        };
        let patch = host::patch(&self.client, inspection.patch).await?;
        let mut scope = self.range_label(range)?;
        if !inspection.metadata_changes.is_empty() {
            scope.push_str(&format!(
                " · {} metadata changes: {}",
                inspection.metadata_changes.len(),
                inspection
                    .metadata_changes
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            if inspection.metadata_changes.len() > 8 {
                scope.push_str("; remaining paths are retained in the review manifest");
            }
        }
        Ok(DiffSnapshot {
            patch,
            overview: OverviewContext {
                repository: self.root.clone(),
                range: overview_range,
                manifest: inspection.manifest,
            },
            repository: self.repository.clone(),
            scope,
            base,
            manifest: inspection.manifest,
            source_identity: inspection.source_identity,
            metadata_changes: inspection.metadata_changes,
        })
    }
}
pub(super) async fn load(
    client: &HostClient,
    workspace: &Path,
    task: Option<TaskId>,
) -> Result<ReviewContext, DiffError> {
    let version = current_version(client, workspace, task).await?;
    let (root, trunk, points, default_from) = if task.is_some() {
        (
            workspace.to_owned(),
            "Task baseline".into(),
            vec![
                RangePoint {
                    target: ReviewTarget {
                        index: 0,
                        kind: ReviewTargetKind::Trunk,
                        short_id: "BASE".into(),
                        title: "Immutable task baseline".into(),
                    },
                    revision: None,
                },
                RangePoint {
                    target: ReviewTarget {
                        index: 1,
                        kind: ReviewTargetKind::WorkingTree,
                        short_id: "CANDIDATE".into(),
                        title: "Immutable task candidate".into(),
                    },
                    revision: None,
                },
            ],
            0,
        )
    } else {
        let catalog = host::catalog(client, workspace).await?;
        let root = PathBuf::from(&catalog.repository);
        let (trunk, points, default_from) = range_points(catalog)?;
        (root, trunk, points, default_from)
    };
    let repository = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository")
        .to_owned();
    Ok(ReviewContext {
        client: client.clone(),
        root,
        task,
        repository,
        trunk,
        points,
        default_from,
        version,
    })
}
fn range_points(
    catalog: orvek_harness::review::ReviewCatalog,
) -> Result<(String, Vec<RangePoint>, usize), DiffError> {
    let branch = catalog.default_branch;
    let trunk = branch
        .name
        .strip_prefix("refs/remotes/")
        .or_else(|| branch.name.strip_prefix("refs/heads/"))
        .unwrap_or(&branch.name)
        .to_owned();
    let base = branch.merge_base;
    let mut points = Vec::new();
    if !catalog.commits.iter().any(|commit| commit.revision == base) {
        points.push(RangePoint {
            target: ReviewTarget {
                index: 0,
                kind: ReviewTargetKind::Trunk,
                short_id: base.chars().take(8).collect(),
                title: format!("{trunk} merge base"),
            },
            revision: Some(base.clone()),
        });
    }
    for commit in catalog.commits.into_iter().rev() {
        let index = points.len();
        let at_base = commit.revision == base;
        points.push(RangePoint {
            target: ReviewTarget {
                index,
                kind: if at_base {
                    ReviewTargetKind::Trunk
                } else {
                    ReviewTargetKind::Commit
                },
                short_id: commit.revision.chars().take(8).collect(),
                title: commit.title,
            },
            revision: Some(commit.revision),
        });
    }
    let default_from = points
        .iter()
        .position(|point| point.revision.as_deref() == Some(&base))
        .ok_or(DiffError::InvalidCommitMetadata)?;
    let index = points.len();
    points.push(RangePoint {
        target: ReviewTarget {
            index,
            kind: ReviewTargetKind::WorkingTree,
            short_id: "WT".into(),
            title: if catalog.commits_truncated {
                "Uncommitted changes (older history omitted)".into()
            } else {
                "Uncommitted changes".into()
            },
        },
        revision: None,
    });
    Ok((trunk, points, default_from))
}
pub(super) async fn current_version(
    client: &HostClient,
    workspace: &Path,
    task: Option<TaskId>,
) -> Result<WorkspaceVersion, DiffError> {
    let inspection = if let Some(task) = task {
        host::inspect_task(client, task).await?
    } else {
        host::inspect_workspace(client, workspace, HostRange::WorkingTree { base: None })
            .await
            .map_err(|error| not_repository(error, workspace))?
    };
    Ok(WorkspaceVersion(inspection.source_identity))
}
/// Git's own "not a git repository" fatal message is stable across versions; detect
/// it here so the UI can give a direct fix instead of a raw host error string.
fn not_repository(error: DiffError, workspace: &Path) -> DiffError {
    match &error {
        DiffError::Host(message) if message.contains("not a git repository") => {
            DiffError::NotRepository(workspace.to_owned())
        }
        _ => error,
    }
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum DiffError {
    #[error("host review failed: {0}")]
    Host(String),
    #[error("host returned invalid review data")]
    Protocol,
    #[error("review workspace is not in a Git repository: {0}")]
    NotRepository(PathBuf),
    #[error("invalid review range {from}..{to} for {target_count} targets")]
    InvalidRange {
        from: usize,
        to: usize,
        target_count: usize,
    },
    #[error("invalid review commit metadata")]
    InvalidCommitMetadata,
    #[error("review source changed during capture")]
    WorkspaceChangedDuringSnapshot,
    #[error("review patch is not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use orvek_harness::review::{ReviewBranch, ReviewCatalog, ReviewCommit};
    #[test]
    fn picker_preserves_named_default_base_and_keeps_recent_ranges_available() {
        let commit = |revision: &str| ReviewCommit {
            revision: revision.into(),
            parents: Vec::new(),
            title: revision.into(),
        };
        let catalog = ReviewCatalog {
            repository: "/fixture".into(),
            head: "head".into(),
            default_branch: ReviewBranch {
                name: "refs/remotes/origin/integration".into(),
                revision: "base".into(),
                merge_base: "base".into(),
            },
            branches: Vec::new(),
            commits: vec![commit("head"), commit("base"), commit("older")],
            branches_truncated: false,
            commits_truncated: false,
        };
        let (name, points, default) = range_points(catalog).unwrap();
        assert_eq!(name, "origin/integration");
        assert_eq!(default, 1);
        assert_eq!(points.len(), 4);
        assert_eq!(points[0].revision.as_deref(), Some("older"));
    }
}
