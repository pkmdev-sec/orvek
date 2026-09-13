use super::{ReviewError, ReviewLimits, git};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewBranch {
    pub name: String,
    pub revision: String,
    pub merge_base: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCommit {
    pub revision: String,
    pub parents: Vec<String>,
    pub title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCatalog {
    pub repository: String,
    pub head: String,
    pub default_branch: ReviewBranch,
    pub branches: Vec<ReviewBranch>,
    pub commits: Vec<ReviewCommit>,
    pub branches_truncated: bool,
    pub commits_truncated: bool,
}
/// Bounded picker data; pass returned object IDs to `inspect` to pin a range.
pub async fn catalog(
    workspace: &Path,
    cancellation: &CancellationToken,
) -> Result<ReviewCatalog, ReviewError> {
    let git = git::Git::new(workspace, &ReviewLimits::default(), cancellation).await?;
    let head = git.resolve("HEAD").await?;
    let refs = git
        .original(
            &[
                "for-each-ref",
                "--count=33",
                "--sort=refname",
                "--format=%(refname)%00%(objectname)",
                "refs/heads/",
                "refs/remotes/",
            ],
            32768,
        )
        .await?;
    let lines = git::text(&refs)?.lines().collect::<Vec<_>>();
    let branches_truncated = lines.len() > 32;
    let mut branches = Vec::new();
    for line in lines.into_iter().take(32) {
        let (name, revision) = line.split_once('\0').ok_or(ReviewError::Protocol)?;
        if name.len() > 512 || !git::oid_valid(revision, &git.object_format) {
            return Err(ReviewError::Protocol);
        }
        let base = git
            .isolated(
                &["merge-base".into(), head.clone(), revision.into()],
                None,
                Vec::new(),
                256,
            )
            .await;
        match base {
            Ok(bytes) => {
                let merge_base = git::text(&bytes)?.trim().to_owned();
                if !git::oid_valid(&merge_base, &git.object_format) {
                    return Err(ReviewError::Protocol);
                }
                branches.push(ReviewBranch {
                    name: name.into(),
                    revision: revision.into(),
                    merge_base,
                });
            }
            // Unrelated histories have no merge base. They are not range defaults.
            Err(ReviewError::Git(message)) if message.is_empty() => {}
            Err(error) => return Err(error),
        }
    }
    let log = git
        .isolated(
            &[
                "log".into(),
                "--no-show-signature".into(),
                "--first-parent".into(),
                "--max-count=65".into(),
                "--format=%H%x00%P%x00%s%x00".into(),
                head.clone(),
                "--".into(),
            ],
            None,
            Vec::new(),
            128 * 1024,
        )
        .await?;
    let fields = git::text(&log)?.split('\0').collect::<Vec<_>>();
    if fields.len() % 3 != 1 || fields.last().is_none_or(|last| !last.trim().is_empty()) {
        return Err(ReviewError::Protocol);
    }
    let mut commits = Vec::new();
    for fields in fields[..fields.len() - 1].as_chunks::<3>().0 {
        let revision = fields[0].trim_start_matches('\n');
        let parents = fields[1]
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !git::oid_valid(revision, &git.object_format)
            || parents
                .iter()
                .any(|id| !git::oid_valid(id, &git.object_format))
            || fields[2].len() > 4096
        {
            return Err(ReviewError::Protocol);
        }
        commits.push(ReviewCommit {
            revision: revision.into(),
            parents,
            title: fields[2].into(),
        });
    }
    let commits_truncated = commits.len() > 64;
    commits.truncate(64);
    let default_branch = default_branch(&git, &head).await?;
    if git.resolve("HEAD").await? != head {
        return Err(ReviewError::Changed);
    }
    Ok(ReviewCatalog {
        repository: git.root.to_string_lossy().into_owned(),
        head,
        default_branch,
        branches,
        commits,
        branches_truncated,
        commits_truncated,
    })
}

async fn default_branch(git: &git::Git, head: &str) -> Result<ReviewBranch, ReviewError> {
    let mut candidates = Vec::new();
    for name in ["refs/remotes/origin/HEAD", "refs/remotes/upstream/HEAD"] {
        let target = git
            .original(
                &["for-each-ref", "--count=1", "--format=%(symref)", name],
                1024,
            )
            .await?;
        let target = git::text(&target)?.trim();
        if !target.is_empty() {
            candidates.push(target.to_owned());
        }
    }
    let current = match git
        .original(&["symbolic-ref", "--quiet", "HEAD"], 1024)
        .await
    {
        Ok(bytes) => Some(git::text(&bytes)?.trim().to_owned()),
        Err(ReviewError::Git(message)) if message.is_empty() => None,
        Err(error) => return Err(error),
    };
    if let Some(current) = &current {
        let upstream = git
            .original(
                &["for-each-ref", "--count=1", "--format=%(upstream)", current],
                1024,
            )
            .await?;
        let upstream = git::text(&upstream)?.trim();
        let short = current
            .strip_prefix("refs/heads/")
            .ok_or(ReviewError::Protocol)?;
        if !upstream.is_empty() && upstream != current && !upstream.ends_with(&format!("/{short}"))
        {
            candidates.push(upstream.to_owned());
        }
    }
    candidates
        .extend(["main", "master", "trunk", "develop"].map(|name| format!("refs/heads/{name}")));
    if let Some(current) = current {
        candidates.push(current);
    }
    for name in candidates {
        if name.len() > 512 || !name.starts_with("refs/") || name.contains(['\0', '\n', '\r']) {
            return Err(ReviewError::Protocol);
        }
        let refs = git
            .original(
                &[
                    "for-each-ref",
                    "--count=2",
                    "--format=%(refname)%00%(objectname)",
                    &name,
                ],
                2048,
            )
            .await?;
        for line in git::text(&refs)?.lines() {
            let (found, revision) = line.split_once('\0').ok_or(ReviewError::Protocol)?;
            if found != name {
                continue;
            }
            if !git::oid_valid(revision, &git.object_format) {
                return Err(ReviewError::Protocol);
            }
            let bytes = git
                .isolated(
                    &["merge-base".into(), head.into(), revision.into()],
                    None,
                    Vec::new(),
                    256,
                )
                .await?;
            let merge_base = git::text(&bytes)?.trim().to_owned();
            if !git::oid_valid(&merge_base, &git.object_format) {
                return Err(ReviewError::Protocol);
            }
            return Ok(ReviewBranch {
                name,
                revision: revision.into(),
                merge_base,
            });
        }
    }
    Ok(ReviewBranch {
        name: "HEAD".into(),
        revision: head.into(),
        merge_base: head.into(),
    })
}
