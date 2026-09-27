use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{
    Row, Sqlite, SqlitePool, Transaction,
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
    },
};
use tokio::sync::watch;
use uuid::Uuid;

use crate::model::*;

const REPLAY_BUDGET: i64 = 2 * 1024 * 1024 * 1024;
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS runs (
 id TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE, name TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
 config TEXT NOT NULL, stages TEXT NOT NULL, total_generations INTEGER NOT NULL, status TEXT NOT NULL,
 started INTEGER NOT NULL DEFAULT 0,
 completed INTEGER NOT NULL DEFAULT 0, queue_order INTEGER NOT NULL,
 error TEXT, device TEXT NOT NULL, engine TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS lap_records (
 id TEXT PRIMARY KEY, record_key TEXT NOT NULL, lap_seconds REAL NOT NULL,
 metadata TEXT NOT NULL, track TEXT NOT NULL, vehicle TEXT NOT NULL, engine TEXT NOT NULL,
 genome BLOB NOT NULL, ghost BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS fastest_records ON lap_records(record_key,lap_seconds);
CREATE TABLE IF NOT EXISTS checkpoints (
 run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 generation INTEGER NOT NULL, population BLOB NOT NULL,
 PRIMARY KEY(run_id,generation)
);
CREATE TABLE IF NOT EXISTS generations (
 run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 generation INTEGER NOT NULL, summary TEXT NOT NULL, outcomes TEXT NOT NULL,
 PRIMARY KEY(run_id,generation)
);
CREATE TABLE IF NOT EXISTS events (
 id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS replays (
 run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 generation INTEGER NOT NULL, status TEXT NOT NULL, follow INTEGER NOT NULL,
 accessed INTEGER NOT NULL DEFAULT (unixepoch()), error TEXT,
 PRIMARY KEY(run_id,generation)
);
CREATE TABLE IF NOT EXISTS chunks (
 run_id TEXT NOT NULL, generation INTEGER NOT NULL, chunk_index INTEGER NOT NULL,
 data BLOB NOT NULL, PRIMARY KEY(run_id,generation,chunk_index),
 FOREIGN KEY(run_id,generation) REFERENCES replays(run_id,generation) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS inspections (
 run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 generation INTEGER NOT NULL, car_id INTEGER NOT NULL, status TEXT NOT NULL,
 data TEXT, error TEXT, accessed INTEGER NOT NULL DEFAULT (unixepoch()), PRIMARY KEY(run_id,generation,car_id)
);
"#;

#[derive(Clone)]
pub struct Store {
    writes: SqlitePool,
    reads: SqlitePool,
    pub changed: watch::Sender<i64>,
}

pub struct StoredEvent {
    pub id: i64,
    pub kind: String,
    pub data: String,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let writes = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        sqlx::raw_sql(SCHEMA).execute(&writes).await?;
        let reads = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options.create_if_missing(false).read_only(true))
            .await?;
        let latest: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM events")
            .fetch_one(&reads)
            .await?;
        let (changed, _) = watch::channel(latest);
        Ok(Self {
            writes,
            reads,
            changed,
        })
    }

    pub async fn create_run(
        &self,
        request: &CreateRunRequest,
        stages: &[RunStage],
        device: &str,
    ) -> Result<RunDetail> {
        let mut tx = self.writes.begin().await?;
        if let Some(row) = sqlx::query("SELECT * FROM runs WHERE request_id=?")
            .bind(&request.request_id)
            .fetch_optional(&mut *tx)
            .await?
        {
            let existing = run_detail(row)?;
            ensure!(
                serde_json::to_value(&existing.run.config)?
                    == serde_json::to_value(&request.config)?
                    && existing.run.name == request.name,
                "requestId already belongs to a different request"
            );
            return Ok(existing);
        }
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE status='queued'")
            .fetch_one(&mut *tx)
            .await?;
        ensure!(queued < 100, "Training queue is full (100 runs)");
        let id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO runs(id,request_id,name,config,stages,total_generations,status,queue_order,device,engine) VALUES(?,?,?,?,?,?,'queued',(SELECT COALESCE(MAX(queue_order),0)+1 FROM runs),?,?)")
            .bind(&id).bind(&request.request_id).bind(&request.name).bind(serde_json::to_string(&request.config)?)
            .bind(serde_json::to_string(stages)?).bind(stages.len() as u32 * request.config.training.generations_per_circuit).bind(device).bind(engine_identity(device)).execute(&mut *tx).await?;
        let detail = read_run_tx(&mut tx, &id).await?;
        let event = append_event(&mut tx, "run", &detail.run).await?;
        tx.commit().await?;
        self.changed.send_replace(event);
        Ok(detail)
    }

    pub async fn run(&self, id: &str) -> Result<Option<RunDetail>> {
        sqlx::query("SELECT * FROM runs WHERE id=?")
            .bind(id)
            .fetch_optional(&self.reads)
            .await?
            .map(run_detail)
            .transpose()
    }

    pub async fn runs(&self) -> Result<Vec<RunSummary>> {
        sqlx::query("SELECT * FROM runs ORDER BY created_at DESC,id DESC LIMIT 1000")
            .fetch_all(&self.reads)
            .await?
            .into_iter()
            .map(|row| Ok(run_detail(row)?.run))
            .collect()
    }

    pub async fn recover(&self) -> Result<()> {
        let mut tx = self.writes.begin().await?;
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM runs WHERE status IN ('running','stopping')")
                .fetch_all(&mut *tx)
                .await?;
        let mut latest = None;
        for id in ids {
            sqlx::query("UPDATE runs SET status=CASE status WHEN 'running' THEN 'queued' ELSE 'stopped' END WHERE id=?").bind(&id).execute(&mut *tx).await?;
            let run = read_run_tx(&mut tx, &id).await?.run;
            latest = Some(append_event(&mut tx, "run", &run).await?);
        }
        sqlx::query("DELETE FROM chunks WHERE EXISTS(SELECT 1 FROM replays r WHERE r.run_id=chunks.run_id AND r.generation=chunks.generation AND r.status='running')").execute(&mut *tx).await?;
        sqlx::query("UPDATE replays SET status='queued' WHERE status='running'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE inspections SET status='queued' WHERE status='running'")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if let Some(id) = latest {
            self.changed.send_replace(id);
        }
        Ok(())
    }

    pub async fn claim_run(&self) -> Result<Option<RunDetail>> {
        let mut tx = self.writes.begin().await?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM runs WHERE status='queued' ORDER BY queue_order,id LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else { return Ok(None) };
        sqlx::query("UPDATE runs SET status='running',error=NULL WHERE id=?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        let mut detail = read_run_tx(&mut tx, &id).await?;
        let started: bool = sqlx::query_scalar("SELECT started FROM runs WHERE id=?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
        if !started {
            for stage in &mut detail.stages {
                stage.baseline_record_id = sqlx::query_scalar("SELECT id FROM lap_records WHERE record_key=? ORDER BY lap_seconds,rowid LIMIT 1")
                    .bind(&stage.record_key).fetch_optional(&mut *tx).await?;
            }
            sqlx::query("UPDATE runs SET started=1,stages=? WHERE id=?")
                .bind(serde_json::to_string(&detail.stages)?)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
        }
        let event = append_event(&mut tx, "run", &detail.run).await?;
        tx.commit().await?;
        self.changed.send_replace(event);
        Ok(Some(detail))
    }

    pub async fn stop(&self, id: &str) -> Result<RunDetail> {
        self.transition(id, "UPDATE runs SET status=CASE status WHEN 'queued' THEN 'stopped' WHEN 'running' THEN 'stopping' ELSE status END WHERE id=?").await
    }

    pub async fn resume(&self, id: &str, device: &str) -> Result<RunDetail> {
        let detail = self.run(id).await?.context("Run not found")?;
        ensure_engine(&detail.run, device)?;
        ensure!(
            matches!(
                detail.run.status,
                RunStatus::Stopped | RunStatus::Failed | RunStatus::Queued | RunStatus::Running
            ),
            "This run cannot be resumed"
        );
        self.transition(id, "UPDATE runs SET status='queued',error=NULL,queue_order=(SELECT COALESCE(MAX(queue_order),0)+1 FROM runs) WHERE id=? AND status IN ('stopped','failed')").await
    }

    async fn transition(&self, id: &str, statement: &str) -> Result<RunDetail> {
        let mut tx = self.writes.begin().await?;
        let before = read_run_tx(&mut tx, id).await?;
        sqlx::query(statement).bind(id).execute(&mut *tx).await?;
        let after = read_run_tx(&mut tx, id).await?;
        let event = if before.run.status != after.run.status {
            Some(append_event(&mut tx, "run", &after.run).await?)
        } else {
            None
        };
        tx.commit().await?;
        if let Some(event) = event {
            self.changed.send_replace(event);
        }
        Ok(after)
    }

    pub async fn finish_stop(&self, id: &str) -> Result<()> {
        self.transition(
            id,
            "UPDATE runs SET status='stopped' WHERE id=? AND status='stopping'",
        )
        .await?;
        Ok(())
    }

    pub async fn fail_run(&self, id: &str, error: &str) -> Result<()> {
        let mut tx = self.writes.begin().await?;
        sqlx::query("UPDATE runs SET status='failed',error=? WHERE id=?")
            .bind(error)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let run = read_run_tx(&mut tx, id).await?.run;
        let event = append_event(&mut tx, "run", &run).await?;
        tx.commit().await?;
        self.changed.send_replace(event);
        Ok(())
    }

    pub async fn champion(&self, key: &str) -> Result<Option<LapRecord>> {
        let metadata: Option<String> = sqlx::query_scalar("SELECT metadata FROM lap_records WHERE record_key=? ORDER BY lap_seconds,rowid LIMIT 1")
            .bind(key).fetch_optional(&self.reads).await?;
        metadata
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
    }

    pub async fn records(&self, id: &str) -> Result<Vec<CircuitRecord>> {
        let detail = self.run(id).await?.context("Run not found")?;
        let mut records = Vec::with_capacity(detail.stages.len());
        for stage in detail.stages {
            let baseline: Option<String> = if let Some(id) = stage.baseline_record_id {
                sqlx::query_scalar("SELECT metadata FROM lap_records WHERE id=?")
                    .bind(id)
                    .fetch_optional(&self.reads)
                    .await?
            } else {
                None
            };
            records.push(CircuitRecord {
                stage_index: stage.index,
                champion: self.champion(&stage.record_key).await?,
                baseline: baseline
                    .map(|json| serde_json::from_str(&json))
                    .transpose()?,
            });
        }
        Ok(records)
    }

    pub async fn ghost(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(
            sqlx::query_scalar("SELECT ghost FROM lap_records WHERE id=?")
                .bind(id)
                .fetch_optional(&self.reads)
                .await?,
        )
    }

    pub async fn population(&self, id: &str, generation: u32) -> Result<Option<Vec<f32>>> {
        let data: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT population FROM checkpoints WHERE run_id=? AND generation=?",
        )
        .bind(id)
        .bind(generation)
        .fetch_optional(&self.reads)
        .await?;
        data.map(|bytes| {
            ensure!(bytes.len() % 4 == 0, "Malformed population checkpoint");
            let values: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            ensure!(
                values.iter().all(|v| v.is_finite()),
                "Non-finite population checkpoint"
            );
            Ok(values)
        })
        .transpose()
    }

    pub async fn initial_checkpoint(&self, id: &str, population: &[f32]) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO checkpoints(run_id,generation,population) VALUES(?,0,?)",
        )
        .bind(id)
        .bind(float_bytes(population))
        .execute(&self.writes)
        .await?;
        Ok(())
    }

    pub async fn commit_generation(
        &self,
        id: &str,
        summary: &GenerationSummary,
        outcomes: &[CarOutcome],
        next_population: Option<&[f32]>,
        record: Option<&RecordCandidate>,
    ) -> Result<RunDetail> {
        let mut tx = self.writes.begin().await?;
        let before = read_run_tx(&mut tx, id).await?;
        ensure!(
            before.run.completed_generations == summary.generation,
            "Generation checkpoint cursor changed"
        );
        ensure!(
            matches!(before.run.status, RunStatus::Running | RunStatus::Stopping),
            "Run is no longer active"
        );
        let stage = before.stage(summary.generation)?;
        ensure!(
            stage.index == summary.stage_index,
            "Generation belongs to the wrong circuit"
        );
        ensure!(
            next_population.is_some() == (summary.generation + 1 < before.run.total_generations),
            "Missing or unexpected next checkpoint"
        );
        if let Some(record) = record {
            let seconds = (record.end.ticks() - record.start.ticks()) / SIMULATION_HZ;
            let previous: Option<f64> =
                sqlx::query_scalar("SELECT MIN(lap_seconds) FROM lap_records WHERE record_key=?")
                    .bind(&stage.record_key)
                    .fetch_one(&mut *tx)
                    .await?;
            ensure!(
                seconds.is_finite() && seconds > 0.0,
                "Invalid record duration"
            );
            ensure!(
                record.ghost.len() >= 8
                    && record.ghost.len() % 4 == 0
                    && record.ghost.iter().all(|v| v.is_finite())
                    && record.ghost[0] == 0.0
                    && record.ghost[record.ghost.len() - 4]
                        == (record.end.ticks() - record.start.ticks()) as f32
                    && record
                        .ghost
                        .chunks_exact(4)
                        .map(|v| v[0])
                        .collect::<Vec<_>>()
                        .windows(2)
                        .all(|p| p[0] < p[1]),
                "Invalid record ghost"
            );
            ensure!(
                record.genome.len() == GENOME_SIZE && record.genome.iter().all(|v| v.is_finite()),
                "Invalid record genome"
            );
            if previous.is_none_or(|time| seconds < time) {
                let created_at: String =
                    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
                        .fetch_one(&mut *tx)
                        .await?;
                let metadata = LapRecord {
                    id: Uuid::new_v4().to_string(),
                    record_key: stage.record_key.clone(),
                    circuit_id: stage.circuit.id.clone(),
                    created_at,
                    run_id: id.into(),
                    run_name: before.run.name.clone(),
                    generation: summary.generation,
                    car_id: record.car_id,
                    lap: record.lap,
                    lap_seconds: seconds,
                    ghost_frames: (record.ghost.len() / 4) as u32,
                };
                sqlx::query("INSERT INTO lap_records(id,record_key,lap_seconds,metadata,track,vehicle,engine,genome,ghost) VALUES(?,?,?,?,?,?,?,?,?)")
                    .bind(&metadata.id).bind(&stage.record_key).bind(seconds).bind(serde_json::to_string(&metadata)?)
                    .bind(serde_json::to_string(&stage.circuit.track)?).bind(serde_json::to_string(&before.run.config.vehicle)?)
                    .bind(&before.run.engine_version).bind(float_bytes(&record.genome)).bind(float_bytes(&record.ghost))
                    .execute(&mut *tx).await?;
                append_event(&mut tx, "record", &metadata).await?;
            }
        }
        sqlx::query("INSERT INTO generations(run_id,generation,summary,outcomes) VALUES(?,?,?,?)")
            .bind(id)
            .bind(summary.generation)
            .bind(serde_json::to_string(summary)?)
            .bind(serde_json::to_string(outcomes)?)
            .execute(&mut *tx)
            .await?;
        if let Some(population) = next_population {
            sqlx::query("INSERT INTO checkpoints(run_id,generation,population) VALUES(?,?,?)")
                .bind(id)
                .bind(summary.generation + 1)
                .bind(float_bytes(population))
                .execute(&mut *tx)
                .await?;
        }
        let status = if summary.generation + 1 == before.run.total_generations {
            "completed"
        } else if before.run.status == RunStatus::Stopping {
            "stopped"
        } else {
            "running"
        };
        sqlx::query("UPDATE runs SET completed=?,status=? WHERE id=?")
            .bind(summary.generation + 1)
            .bind(status)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        append_event(
            &mut tx,
            "generation",
            &json!({"runId":id,"summary":summary}),
        )
        .await?;
        let after = read_run_tx(&mut tx, id).await?;
        let event = append_event(&mut tx, "run", &after.run).await?;
        tx.commit().await?;
        self.changed.send_replace(event);
        Ok(after)
    }

    pub async fn generations(&self, id: &str) -> Result<Vec<GenerationSummary>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT summary FROM generations WHERE run_id=? ORDER BY generation",
        )
        .bind(id)
        .fetch_all(&self.reads)
        .await?;
        rows.into_iter()
            .map(|s| Ok(serde_json::from_str(&s)?))
            .collect()
    }

    pub async fn outcomes(&self, id: &str, generation: u32) -> Result<Vec<CarOutcome>> {
        let data: Option<String> =
            sqlx::query_scalar("SELECT outcomes FROM generations WHERE run_id=? AND generation=?")
                .bind(id)
                .bind(generation)
                .fetch_optional(&self.reads)
                .await?;
        Ok(serde_json::from_str(
            &data.context("Generation not found")?,
        )?)
    }

    pub async fn events(&self, after: i64) -> Result<Vec<StoredEvent>> {
        Ok(
            sqlx::query("SELECT id,kind,data FROM events WHERE id>? ORDER BY id LIMIT 128")
                .bind(after)
                .fetch_all(&self.reads)
                .await?
                .into_iter()
                .map(|row| StoredEvent {
                    id: row.get("id"),
                    kind: row.get("kind"),
                    data: row.get("data"),
                })
                .collect(),
        )
    }

    pub async fn latest_event_id(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM events")
            .fetch_one(&self.reads)
            .await?)
    }

    pub async fn request_replay(
        &self,
        id: &str,
        generation: u32,
        follow: bool,
    ) -> Result<ReplayManifest> {
        self.outcomes(id, generation).await?;
        let mut tx = self.writes.begin().await?;
        if follow {
            sqlx::query("DELETE FROM replays WHERE run_id=? AND follow=1 AND status='queued' AND generation<>?").bind(id).bind(generation).execute(&mut *tx).await?;
        }
        let existing: Option<String> =
            sqlx::query_scalar("SELECT status FROM replays WHERE run_id=? AND generation=?")
                .bind(id)
                .bind(generation)
                .fetch_optional(&mut *tx)
                .await?;
        if existing.is_none() || existing.as_deref() == Some("failed") {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM replays WHERE status='queued'")
                    .fetch_one(&mut *tx)
                    .await?;
            ensure!(count < 8, "Replay queue is full (8 requests)");
            sqlx::query("INSERT INTO replays(run_id,generation,status,follow) VALUES(?,?,'queued',?) ON CONFLICT(run_id,generation) DO UPDATE SET status='queued',error=NULL,follow=excluded.follow")
                .bind(id).bind(generation).bind(follow).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM chunks WHERE run_id=? AND generation=?")
                .bind(id)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
        } else if !follow {
            sqlx::query("UPDATE replays SET follow=0 WHERE run_id=? AND generation=?")
                .bind(id)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.replay(id, generation)
            .await?
            .context("Replay not found")
    }

    pub async fn replay(&self, id: &str, generation: u32) -> Result<Option<ReplayManifest>> {
        let row = sqlx::query("SELECT status,error FROM replays WHERE run_id=? AND generation=?")
            .bind(id)
            .bind(generation)
            .fetch_optional(&self.reads)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let outcomes = self.outcomes(id, generation).await?;
        let last_step = outcomes.iter().map(|o| o.terminal_step).max().unwrap_or(0);
        let sample_stride = sample_stride(last_step);
        let total_frames = last_step.div_ceil(sample_stride) + 1;
        let available_chunks: Vec<u32> = sqlx::query_scalar(
            "SELECT chunk_index FROM chunks WHERE run_id=? AND generation=? ORDER BY chunk_index",
        )
        .bind(id)
        .bind(generation)
        .fetch_all(&self.reads)
        .await?;
        let status: String = row.get("status");
        let detail = self.run(id).await?.context("Run not found")?;
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM generations WHERE run_id=? AND generation=?")
                .bind(id)
                .bind(generation)
                .fetch_one(&self.reads)
                .await?;
        let summary: GenerationSummary = serde_json::from_str(&summary)?;
        Ok(Some(ReplayManifest {
            run_id: id.into(),
            generation,
            stage_index: detail.stage(generation)?.index,
            best_car_id: summary.best_car_id,
            simulation_hz: SIMULATION_HZ,
            status: serde_json::from_value(json!(status))?,
            car_count: outcomes.len() as u32,
            total_frames,
            last_step,
            sample_stride,
            frames_per_chunk: REPLAY_CHUNK_FRAMES,
            chunks: total_frames.div_ceil(REPLAY_CHUNK_FRAMES),
            available_chunks,
            outcomes,
            error: row.get("error"),
        }))
    }

    pub async fn claim_replay(&self) -> Result<Option<(String, u32)>> {
        let mut tx = self.writes.begin().await?;
        let row=sqlx::query("SELECT run_id,generation FROM replays WHERE status='queued' ORDER BY follow,accessed,run_id,generation LIMIT 1").fetch_optional(&mut *tx).await?;
        let Some(row) = row else { return Ok(None) };
        let id: String = row.get("run_id");
        let generation: u32 = row.get("generation");
        sqlx::query("UPDATE replays SET status='running' WHERE run_id=? AND generation=?")
            .bind(&id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some((id, generation)))
    }

    pub async fn write_chunk(
        &self,
        id: &str,
        generation: u32,
        index: u32,
        data: &[u8],
        last: bool,
    ) -> Result<()> {
        let mut tx = self.writes.begin().await?;
        sqlx::query("INSERT INTO chunks(run_id,generation,chunk_index,data) VALUES(?,?,?,?)")
            .bind(id)
            .bind(generation)
            .bind(index)
            .bind(data)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE replays SET status=?,accessed=unixepoch() WHERE run_id=? AND generation=?",
        )
        .bind(if last { "ready" } else { "running" })
        .bind(id)
        .bind(generation)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.evict_cache(id, generation, None).await
    }

    async fn evict_cache(&self, keep: &str, generation: u32, car: Option<u32>) -> Result<()> {
        let mut tx = self.writes.begin().await?;
        loop {
            let bytes:i64=sqlx::query_scalar("SELECT (SELECT COALESCE(SUM(length(data)),0) FROM chunks)+(SELECT COALESCE(SUM(length(data)),0) FROM inspections)").fetch_one(&mut *tx).await?;
            if bytes <= REPLAY_BUDGET {
                break;
            }
            let row=sqlx::query("SELECT 'replay' AS kind,run_id,generation,-1 AS car_id,accessed FROM replays WHERE status='ready' AND NOT(run_id=? AND generation=?) UNION ALL SELECT 'trace' AS kind,run_id,generation,car_id,accessed FROM inspections WHERE status='ready' AND NOT(run_id=? AND generation=? AND car_id=?) ORDER BY accessed LIMIT 1")
                .bind(keep).bind(if car.is_none(){i64::from(generation)}else{-1}).bind(keep).bind(generation).bind(car.map(i64::from).unwrap_or(-1)).fetch_optional(&mut *tx).await?;
            let Some(row) = row else { break };
            if row.get::<String, _>("kind") == "replay" {
                sqlx::query("DELETE FROM replays WHERE run_id=? AND generation=?")
                    .bind(row.get::<String, _>("run_id"))
                    .bind(row.get::<u32, _>("generation"))
                    .execute(&mut *tx)
                    .await?;
            } else {
                sqlx::query("DELETE FROM inspections WHERE run_id=? AND generation=? AND car_id=?")
                    .bind(row.get::<String, _>("run_id"))
                    .bind(row.get::<u32, _>("generation"))
                    .bind(row.get::<i64, _>("car_id"))
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn chunk(&self, id: &str, generation: u32, index: u32) -> Result<Option<Vec<u8>>> {
        let bytes = sqlx::query_scalar(
            "SELECT data FROM chunks WHERE run_id=? AND generation=? AND chunk_index=?",
        )
        .bind(id)
        .bind(generation)
        .bind(index)
        .fetch_optional(&self.reads)
        .await?;
        sqlx::query("UPDATE replays SET accessed=unixepoch() WHERE run_id=? AND generation=?")
            .bind(id)
            .bind(generation)
            .execute(&self.writes)
            .await?;
        Ok(bytes)
    }

    pub async fn fail_replay(&self, id: &str, generation: u32, error: &str) -> Result<()> {
        let mut tx = self.writes.begin().await?;
        sqlx::query("DELETE FROM chunks WHERE run_id=? AND generation=?")
            .bind(id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE replays SET status='failed',error=? WHERE run_id=? AND generation=?")
            .bind(error)
            .bind(id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn request_trace(
        &self,
        id: &str,
        generation: u32,
        car: u32,
    ) -> Result<Option<CarTrace>> {
        let outcomes = self.outcomes(id, generation).await?;
        ensure!((car as usize) < outcomes.len(), "Car not found");
        let row=sqlx::query("SELECT status,data,error FROM inspections WHERE run_id=? AND generation=? AND car_id=?").bind(id).bind(generation).bind(car).fetch_optional(&self.reads).await?;
        if let Some(row) = row {
            let status: String = row.get("status");
            if status == "ready" {
                sqlx::query("UPDATE inspections SET accessed=unixepoch() WHERE run_id=? AND generation=? AND car_id=?").bind(id).bind(generation).bind(car).execute(&self.writes).await?;
                return Ok(Some(serde_json::from_str(&row.get::<String, _>("data"))?));
            }
            if status == "failed" {
                bail!("Inspection failed: {}", row.get::<String, _>("error"))
            }
            return Ok(None);
        }
        let mut tx = self.writes.begin().await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inspections WHERE status IN ('queued','running')",
        )
        .fetch_one(&mut *tx)
        .await?;
        ensure!(count < 8, "Inspection queue is full (8 requests)");
        sqlx::query("INSERT OR IGNORE INTO inspections(run_id,generation,car_id,status) VALUES(?,?,?,'queued')").bind(id).bind(generation).bind(car).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(None)
    }

    pub async fn claim_trace(&self) -> Result<Option<(String, u32, u32)>> {
        let mut tx = self.writes.begin().await?;
        let row=sqlx::query("SELECT run_id,generation,car_id FROM inspections WHERE status='queued' ORDER BY rowid LIMIT 1").fetch_optional(&mut *tx).await?;
        let Some(row) = row else { return Ok(None) };
        let id: String = row.get("run_id");
        let generation: u32 = row.get("generation");
        let car: u32 = row.get("car_id");
        sqlx::query(
            "UPDATE inspections SET status='running' WHERE run_id=? AND generation=? AND car_id=?",
        )
        .bind(&id)
        .bind(generation)
        .bind(car)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some((id, generation, car)))
    }

    pub async fn save_trace(&self, trace: &CarTrace) -> Result<()> {
        sqlx::query("UPDATE inspections SET status='ready',data=?,accessed=unixepoch() WHERE run_id=? AND generation=? AND car_id=?")
            .bind(serde_json::to_string(trace)?).bind(&trace.run_id).bind(trace.generation).bind(trace.car_id).execute(&self.writes).await?;
        self.evict_cache(&trace.run_id, trace.generation, Some(trace.car_id))
            .await
    }

    pub async fn fail_trace(&self, id: &str, generation: u32, car: u32, error: &str) -> Result<()> {
        sqlx::query("UPDATE inspections SET status='failed',data=NULL,error=? WHERE run_id=? AND generation=? AND car_id=?").bind(error).bind(id).bind(generation).bind(car).execute(&self.writes).await?;
        Ok(())
    }
}

pub fn float_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub fn engine_identity(device: &str) -> String {
    let hash = Sha256::digest(include_bytes!("engine.metal"));
    format!("{ENGINE_VERSION}-{:x}-{device}", hash)
}

pub fn ensure_engine(run: &RunSummary, device: &str) -> Result<()> {
    ensure!(
        run.engine_version == engine_identity(device),
        "Checkpoint belongs to an incompatible engine or device"
    );
    Ok(())
}

async fn read_run_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<RunDetail> {
    run_detail(
        sqlx::query("SELECT * FROM runs WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .context("Run not found")?,
    )
}

fn run_detail(row: SqliteRow) -> Result<RunDetail> {
    let status: String = row.try_get("status")?;
    Ok(RunDetail {
        run: RunSummary {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            created_at: row.try_get("created_at")?,
            status: serde_json::from_value(json!(status))?,
            config: serde_json::from_str(&row.try_get::<String, _>("config")?)?,
            completed_generations: row.try_get("completed")?,
            last_error: row.try_get("error")?,
            device_name: row.try_get("device")?,
            engine_version: row.try_get("engine")?,
            total_generations: row.try_get("total_generations")?,
        },
        stages: serde_json::from_str(&row.try_get::<String, _>("stages")?)?,
    })
}

async fn append_event<T: serde::Serialize>(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    data: &T,
) -> Result<i64> {
    Ok(
        sqlx::query_scalar("INSERT INTO events(kind,data) VALUES(?,?) RETURNING id")
            .bind(kind)
            .bind(serde_json::to_string(data)?)
            .fetch_one(&mut **tx)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generation_commit_and_restart_preserve_resume_intent() -> Result<()> {
        let directory = std::env::temp_dir().join(format!("genetic-store-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let store = Store::open(directory.join("test.sqlite3")).await?;
        let mut config = RunConfig::default();
        config.training.population = 1;
        config.training.elite_count = 1;
        config.training.generations_per_circuit = 2;
        let stages = crate::circuits::tour(&crate::circuits::catalog()?, &config)?;
        let track = &stages[0].circuit.track;
        let request = CreateRunRequest {
            request_id: Uuid::new_v4().to_string(),
            name: "Checkpoint check".into(),
            config,
        };
        let run = store
            .create_run(&request, &stages, "test Metal device")
            .await?;
        assert_eq!(
            store
                .create_run(&request, &stages, "test Metal device")
                .await?
                .run
                .id,
            run.run.id
        );
        let id = &run.run.id;
        assert_eq!(
            store.claim_run().await?.unwrap().run.status,
            RunStatus::Running
        );
        let population = vec![0.25; GENOME_SIZE];
        store.initial_checkpoint(id, &population).await?;
        let summary = GenerationSummary {
            generation: 0,
            stage_index: 0,
            best_car_id: 0,
            best_lap_seconds: None,
            best_fitness: 0.0,
            mean_fitness: 0.0,
            finished: 0,
            crashed: 0,
            timed_out: 1,
            steps: 0,
            elapsed_ms: 1.0,
            car_steps: 0.0,
        };
        let outcomes = vec![CarOutcome {
            car_id: 0,
            fitness: 0.0,
            status: CarStatus::TimedOut,
            terminal_step: 0,
            completed_laps: 0,
            lap_ends: vec![],
            x: track.spawn[0],
            y: track.spawn[1],
            heading: track.spawn[2],
        }];
        let record = RecordCandidate {
            car_id: 0,
            lap: 1,
            start: LapCrossing::default(),
            end: LapCrossing {
                step: 10,
                fraction: 0.5,
            },
            genome: population.clone(),
            ghost: vec![0.0, 0.0, 0.0, 0.0, 10.5, 1.0, 0.0, 0.0],
        };
        store
            .commit_generation(id, &summary, &outcomes, Some(&population), Some(&record))
            .await?;
        let champion = store.champion(&stages[0].record_key).await?.unwrap();
        assert_eq!(champion.lap_seconds, 0.35);
        assert_eq!(
            store.ghost(&champion.id).await?,
            Some(float_bytes(&record.ghost))
        );
        assert_eq!(store.population(id, 1).await?, Some(population));
        let cursor = *store.changed.borrow();
        assert!(
            store
                .commit_generation(id, &summary, &outcomes, None, None)
                .await
                .is_err()
        );
        assert_eq!(*store.changed.borrow(), cursor);
        assert_eq!(store.generations(id).await?.len(), 1);
        store.stop(id).await?;
        store.recover().await?;
        assert_eq!(store.run(id).await?.unwrap().run.status, RunStatus::Stopped);
        assert!(store.claim_run().await?.is_none());
        store.resume(id, "test Metal device").await?;
        store.claim_run().await?;
        store.recover().await?;
        let resumed = store.claim_run().await?.unwrap();
        assert_eq!(resumed.run.completed_generations, 1);
        assert_eq!(resumed.run.status, RunStatus::Running);
        store.request_replay(id, 0, false).await?;
        store.write_chunk(id, 0, 0, &[0; 16], false).await?;
        store.fail_replay(id, 0, "Interrupted replay").await?;
        assert!(store.chunk(id, 0, 0).await?.is_none());
        let failed = store.replay(id, 0).await?.unwrap();
        assert_eq!(failed.status, ReplayStatus::Failed);
        assert!(failed.available_chunks.is_empty());
        let mut boundary = summary.clone();
        boundary.generation = 1;
        store
            .commit_generation(
                id,
                &boundary,
                &outcomes,
                Some(&record.genome),
                Some(&record),
            )
            .await?;
        assert_eq!(
            store.population(id, 1).await?,
            store.population(id, 2).await?
        );
        assert_eq!(
            store.champion(&stages[0].record_key).await?.unwrap().id,
            champion.id,
            "ties preserve the incumbent"
        );
        store.stop(id).await?;
        store.recover().await?;
        store.resume(id, "test Metal device").await?;
        let resumed = store.claim_run().await?.unwrap();
        assert_eq!(resumed.stage(resumed.run.completed_generations)?.index, 1);
        assert_eq!(
            resumed.stages[0].baseline_record_id, None,
            "restart must not change the original benchmark"
        );
        let mut next_request = request.clone();
        next_request.request_id = Uuid::new_v4().to_string();
        let next = store
            .create_run(&next_request, &stages, "test Metal device")
            .await?;
        let claimed = store.claim_run().await?.unwrap();
        assert_eq!(claimed.run.id, next.run.id);
        assert_eq!(
            claimed.stages[0].baseline_record_id.as_deref(),
            Some(champion.id.as_str())
        );
        // Evictable source data and even the source run cannot remove a record.
        sqlx::query("DELETE FROM runs WHERE id=?")
            .bind(id)
            .execute(&store.writes)
            .await?;
        assert_eq!(
            store.ghost(&champion.id).await?,
            Some(float_bytes(&record.ghost))
        );
        let events = store.events(0).await?;
        assert!(events.windows(2).all(|pair| pair[0].id < pair[1].id));
        assert_eq!(store.latest_event_id().await?, events.last().unwrap().id);
        store.reads.close().await;
        store.writes.close().await;
        let reopened = Store::open(directory.join("test.sqlite3")).await?;
        assert_eq!(
            reopened.champion(&stages[0].record_key).await?.unwrap().id,
            champion.id
        );
        assert!(reopened.ghost(&champion.id).await?.is_some());
        reopened.reads.close().await;
        reopened.writes.close().await;
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
