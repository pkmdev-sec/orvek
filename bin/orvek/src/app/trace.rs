//! Trace commands are offline unless the operator explicitly selects reexecute.
use super::{
    config::Config,
    error::{Error, Result},
};
use clap::Subcommand;
use orvek_harness::{
    Digest,
    state::TaskId,
    trace::{TraceBundle, TraceLimits},
};
use std::{collections::BTreeSet, fs, path::PathBuf};
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub(crate) enum TraceCommand {
    /// Copy a pinned full host journal prefix and its bounded artifact closure. Local only.
    Export {
        #[arg(long)]
        host_root: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        through: Option<u64>,
        /// Exclude a payload by SHA-256. This always makes the bundle non-exact.
        #[arg(long)]
        omit: Vec<Digest>,
    },
    /// Validate and reduce recorded events. Never runs commands or contacts providers.
    Replay { bundle: PathBuf },
    /// Emit intent, contract, candidate/patch, checks, costs, and unresolved facts.
    Review { bundle: PathBuf },
    /// Write N-1 fixtures before each recorded model dispatch.
    Prefixes {
        bundle: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// EXPERIMENTAL: submit only original intent with fresh identities in an empty --workspace.
    Reexecute {
        bundle: PathBuf,
        #[arg(long)]
        task: Uuid,
        #[arg(long, required = true)]
        experimental: bool,
    },
}
impl TraceCommand {
    pub(crate) const fn offline(&self) -> bool {
        !matches!(self, Self::Reexecute { .. })
    }
    pub(crate) fn run_offline(self) -> Result<()> {
        let run = || -> std::result::Result<(), orvek_harness::trace::TraceError> {
            match self {
                Self::Export {
                    host_root,
                    output,
                    through,
                    omit,
                } => {
                    let bundle = TraceBundle::export(
                        &host_root,
                        through,
                        TraceLimits::default(),
                        &omit.into_iter().collect::<BTreeSet<_>>(),
                        Some(env!("ORVEK_GIT_SHA").into()),
                    )?;
                    bundle.write(&output)?;
                    println!(
                        "{}",
                        serde_json::json!({"output":output,"through":bundle.through,"exact":bundle.exact})
                    );
                }
                Self::Replay { bundle } => println!(
                    "{}",
                    serde_json::to_string(&TraceBundle::read(&bundle)?.replay()?)?
                ),
                Self::Review { bundle } => println!(
                    "{}",
                    serde_json::to_string_pretty(&TraceBundle::read(&bundle)?.review()?)?
                ),
                Self::Prefixes { bundle, output } => {
                    let trace = TraceBundle::read(&bundle)?;
                    let fixtures = trace.prefixes()?;
                    fs::create_dir(&output)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&output, fs::Permissions::from_mode(0o700))?;
                    }
                    let mut count = 0usize;
                    for fixture in fixtures {
                        let fixture = fixture?;
                        count += 1;
                        fixture.prefix.write(
                            &output.join(format!("before-{}.json", fixture.before_sequence)),
                        )?;
                        fs::write(
                            output.join(format!("decision-{}.json", fixture.before_sequence)),
                            serde_json::to_vec_pretty(&fixture.decision)?,
                        )?;
                    }
                    println!("{}", serde_json::json!({"fixtures":count,"output":output}));
                }
                Self::Reexecute { .. } => unreachable!("reexecution requires configuration"),
            }
            Ok(())
        };
        run().map_err(|error| Error::HostRequest(error.to_string()))
    }
    pub(crate) async fn reexecute(self, config: &Config) -> Result<()> {
        let Self::Reexecute {
            bundle,
            task,
            experimental,
        } = self
        else {
            unreachable!("offline command")
        };
        if !experimental {
            return Err(Error::HostRequest(
                "reexecution requires --experimental".into(),
            ));
        }
        let trace =
            TraceBundle::read(&bundle).map_err(|error| Error::HostRequest(error.to_string()))?;
        let report = trace
            .replay()
            .map_err(|error| Error::HostRequest(error.to_string()))?;
        let intent = trace
            .reexecution_intent(TaskId(task))
            .map_err(|error| Error::HostRequest(error.to_string()))?;
        let workspace = config.agent().workspace().canonicalize()?;
        if fs::read_dir(&workspace)?.next().is_some()
            || report
                .sessions
                .values()
                .any(|s| s.workspace().canonicalize().ok().as_ref() == Some(&workspace))
        {
            return Err(Error::HostRequest("experimental reexecution requires a new empty --workspace; no historical effects or receipts are reused".into()));
        }
        eprintln!(
            "Experimental fresh run: only original intent is reused. This is not offline replay. New model calls/tools may incur cost and side effects."
        );
        let outcome = super::headless::run(
            config,
            config.agent().model(),
            intent,
            None,
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
        if outcome.exit_code() != 0 {
            return Err(Error::TaskOutcome { outcome });
        }
        Ok(())
    }
}
