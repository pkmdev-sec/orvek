use super::*;
use crate::monitor::{
    Cohort, Comparison, Episode, EpisodeState, Measure, Measurement, MonitorReport, MonitorStatus,
    Origin, Release, Sampling,
};
use serde::de::DeserializeOwned;

pub(super) fn initialize(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch("BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS monitor_state(id INTEGER PRIMARY KEY CHECK(id=1), record BLOB NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS monitor_releases(id TEXT PRIMARY KEY, record BLOB NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS monitor_activations(sequence INTEGER PRIMARY KEY, previous TEXT, current TEXT NOT NULL, reason TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS monitor_facts(id TEXT PRIMARY KEY, record BLOB NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS monitor_measures(id TEXT PRIMARY KEY, record BLOB NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS monitor_episodes(id TEXT PRIMARY KEY, record BLOB NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS monitor_origins(session TEXT PRIMARY KEY, origin BLOB NOT NULL) STRICT;
        PRAGMA user_version=11; COMMIT;")?;
    let release = Release::baseline();
    connection.execute(
        "INSERT OR IGNORE INTO monitor_releases VALUES(?1,?2)",
        params![release.id().to_string(), serde_json::to_vec(&release)?],
    )?;
    let status = MonitorStatus {
        cursor: 0,
        sample_every: 1,
        sampling: Sampling::default(),
        last_error: None,
        active: release.id(),
        previous: None,
    };
    connection.execute(
        "INSERT OR IGNORE INTO monitor_state VALUES(1,?1)",
        [serde_json::to_vec(&status)?],
    )?;
    Ok(())
}
fn decode<T: DeserializeOwned>(bytes: Vec<u8>) -> Result<T, StoreError> {
    Ok(serde_json::from_slice(&bytes)?)
}

impl Store {
    pub(crate) fn monitor_status(&self) -> Result<MonitorStatus, StoreError> {
        decode(self.connection.query_row(
            "SELECT record FROM monitor_state WHERE id=1",
            [],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn monitor_save_status(&self, status: &MonitorStatus) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE monitor_state SET record=?1 WHERE id=1",
            [serde_json::to_vec(status)?],
        )?;
        Ok(())
    }
    pub(crate) fn monitor_release(&self, id: Digest) -> Result<Release, StoreError> {
        let release: Release = decode(self.connection.query_row(
            "SELECT record FROM monitor_releases WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?)?;
        if release.id() != id {
            return Err(StoreError::Integrity("monitor release digest"));
        }
        release.validate().map_err(StoreError::Invalid)?;
        Ok(release)
    }
    pub(crate) fn monitor_origin(&self, session: SessionId) -> Result<Origin, StoreError> {
        self.connection
            .query_row(
                "SELECT origin FROM monitor_origins WHERE session=?1",
                [session.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()
            .map(|o| o.unwrap_or(Origin::User))
    }
    pub(crate) fn monitor_set_origin(
        &self,
        session: SessionId,
        origin: Origin,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO monitor_origins VALUES(?1,?2)",
            params![session.to_string(), serde_json::to_vec(&origin)?],
        )?;
        Ok(())
    }
    pub(crate) fn monitor_task_session(
        &self,
        task: TaskId,
    ) -> Result<Option<SessionId>, StoreError> {
        let id: Option<String> = self.connection.query_row("SELECT aggregate FROM events WHERE kind='session' AND json_extract(event,'$.data.command.type')='task_linked' AND json_extract(event,'$.data.command.data.task')=?1 LIMIT 1", [task.to_string()], |r|r.get(0)).optional()?;
        id.map(|id| {
            id.parse()
                .map_err(|_| StoreError::Integrity("monitor task session"))
        })
        .transpose()
    }
    pub(crate) fn monitor_cohort(&self, session: SessionId) -> Result<Option<Cohort>, StoreError> {
        let state = self.load_session(session)?;
        Ok(state.admission().map(|p| Cohort {
            target: p.binding().target(),
            build: None,
            behavior: p.binding().revision(),
            release: p.native_read().map(|b| b.release),
        }))
    }
    pub(crate) fn monitor_pending(&self) -> Result<Option<Episode>, StoreError> {
        self.connection.query_row("SELECT record FROM monitor_episodes WHERE json_extract(record,'$.state.status') IN ('diagnosed','evaluating') ORDER BY rowid LIMIT 1", [], |r|r.get(0)).optional()?.map(decode).transpose()
    }
    pub(crate) fn monitor_episodes(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Episode>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT record FROM monitor_episodes ORDER BY rowid LIMIT ?1 OFFSET ?2")?;
        statement
            .query_map(params![limit as i64, offset as i64], |r| r.get(0))?
            .map(|r| decode(r?))
            .collect()
    }
    pub(crate) fn monitor_save_episode(&self, episode: &Episode) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE monitor_episodes SET record=?2 WHERE id=?1",
            params![episode.id.to_string(), serde_json::to_vec(episode)?],
        )?;
        Ok(())
    }
    pub(crate) fn monitor_has_episode(&self, id: Digest) -> Result<bool, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM monitor_episodes WHERE id=?1",
                [id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
    pub(crate) fn monitor_commit_page(
        &mut self,
        status: &MonitorStatus,
        measures: &[Measurement],
        episodes: &[Episode],
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        for observation in measures {
            let delta = &observation.value;
            let key = Digest::of_value(&(&delta.cohort, delta.signature))?.to_string();
            let fact_key = Digest::of_value(&(observation.identity, delta.signature))?.to_string();
            let prior_fact: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT record FROM monitor_facts WHERE id=?1",
                    [&fact_key],
                    |r| r.get(0),
                )
                .optional()?;
            let old: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT record FROM monitor_measures WHERE id=?1",
                    [&key],
                    |r| r.get(0),
                )
                .optional()?;
            let mut value = old
                .map(decode)
                .transpose()?
                .unwrap_or_else(|| Measure::new(delta.cohort.clone(), delta.signature, 0, 0, 0));
            if let Some(prior) = prior_fact {
                let prior: Measure = decode(prior)?;
                if prior.cohort != delta.cohort {
                    return Err(StoreError::Integrity("monitor opportunity cohort changed"));
                }
                value.opportunities = value
                    .opportunities
                    .checked_sub(prior.opportunities)
                    .ok_or(StoreError::Integrity("monitor denominator"))?;
                value.failures = value
                    .failures
                    .checked_sub(prior.failures)
                    .ok_or(StoreError::Integrity("monitor numerator"))?;
                value.measured = value
                    .measured
                    .checked_sub(prior.measured)
                    .ok_or(StoreError::Integrity("monitor measured count"))?;
                value.recorded_usd = value
                    .recorded_usd
                    .checked_sub(prior.recorded_usd)
                    .ok_or(StoreError::Integrity("monitor recorded cost"))?;
            }
            value.opportunities += delta.opportunities;
            value.failures += delta.failures;
            value.measured += delta.measured;
            value.recorded_usd = value
                .recorded_usd
                .checked_add(delta.recorded_usd)
                .ok_or(StoreError::Integrity("monitor cost overflow"))?;
            tx.execute(
                "INSERT OR REPLACE INTO monitor_measures VALUES(?1,?2)",
                params![key, serde_json::to_vec(&value)?],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO monitor_facts VALUES(?1,?2)",
                params![fact_key, serde_json::to_vec(delta)?],
            )?;
        }
        for episode in episodes {
            tx.execute(
                "INSERT OR IGNORE INTO monitor_episodes VALUES(?1,?2)",
                params![episode.id.to_string(), serde_json::to_vec(episode)?],
            )?;
        }
        tx.execute(
            "UPDATE monitor_state SET record=?1 WHERE id=1",
            [serde_json::to_vec(status)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// CAS activation and episode result share a transaction. A lost reply cannot activate twice.
    pub(crate) fn monitor_activate(
        &mut self,
        expected: Digest,
        release: Release,
        reason: &str,
        mut episode: Option<Episode>,
    ) -> Result<Digest, StoreError> {
        release.validate().map_err(StoreError::Invalid)?;
        let mut status = self.monitor_status()?;
        if status.active != expected {
            return Err(StoreError::Invalid("active behavior changed"));
        }
        let id = release.id();
        status.previous = Some(status.active);
        status.active = id;
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO monitor_releases VALUES(?1,?2)",
            params![id.to_string(), serde_json::to_vec(&release)?],
        )?;
        tx.execute(
            "INSERT INTO monitor_activations(previous,current,reason) VALUES(?1,?2,?3)",
            params![expected.to_string(), id.to_string(), reason],
        )?;
        tx.execute(
            "UPDATE monitor_state SET record=?1 WHERE id=1",
            [serde_json::to_vec(&status)?],
        )?;
        if let Some(episode) = episode.as_mut() {
            episode.state = EpisodeState::Promoted { release: id };
            tx.execute(
                "UPDATE monitor_episodes SET record=?2 WHERE id=?1",
                params![episode.id.to_string(), serde_json::to_vec(episode)?],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }
    pub(crate) fn monitor_report(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<MonitorReport, StoreError> {
        if limit == 0 || limit > 100 || offset > i64::MAX as usize {
            return Err(StoreError::Invalid("monitor page limit must be 1..100"));
        }
        let mut statement = self
            .connection
            .prepare("SELECT record FROM monitor_measures ORDER BY id LIMIT ?1 OFFSET ?2")?;
        let measures: Vec<Measure> = statement
            .query_map(params![limit as i64, offset as i64], |r| r.get(0))?
            .map(|r| decode(r?))
            .collect::<Result<_, _>>()?;
        let mut comparisons = Vec::new();
        for after in &measures {
            let Some(release) = after.cohort.release else {
                continue;
            };
            let Some(build) = after.cohort.build else {
                continue;
            };
            let Some(parent) = self.monitor_release(release)?.parent else {
                continue;
            };
            let signature = serde_json::to_value(after.signature)?;
            let before: Option<Vec<u8>> = self.connection.query_row(
                "SELECT record FROM monitor_measures WHERE json_extract(record,'$.cohort.release')=?1 AND json_extract(record,'$.cohort.build')=?2 AND json_extract(record,'$.cohort.target')=?3 AND json_extract(record,'$.signature')=?4 LIMIT 1",
                params![parent.to_string(),build.to_string(),serde_json::to_string(&after.cohort.target)?,signature.as_str()], |r|r.get(0)).optional()?;
            if let Some(before) = before {
                comparisons.push(Comparison { signature: after.signature, before: decode(before)?, after: after.clone(), assessment: "uncertain: descriptive matched exposure only; sparse data, correlated failures and provider outages do not establish a behavior-quality effect".into() });
            }
        }
        let episodes = self.monitor_episodes(offset, limit)?;
        let next = (measures.len() == limit || episodes.len() == limit).then_some(offset + limit);
        Ok(MonitorReport { status: self.monitor_status()?, measures, episodes, comparisons, next, interpretation: "Descriptive only. Match target model/settings, build, environment, protocol and task profile before comparing releases. Historical unknown builds are not matched evidence. Sparse data and provider outages remain uncertain; feedback is a correction signal, not a task grade. Missing usage/cost is opportunities minus measured, never zero-filled. No independent-trial or live-model quality claim.".into() })
    }
}
