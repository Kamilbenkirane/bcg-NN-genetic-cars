use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use tokio::sync::{oneshot, watch};

use crate::{
    engine::{Evaluation, MetalEngine},
    geometry,
    model::*,
    store::{Store, engine_identity, float_bytes},
};

struct Control {
    shutdown: AtomicBool,
    active: Mutex<Option<(String, Arc<AtomicBool>)>>,
    wake: SyncSender<()>,
    error: Mutex<Option<String>>,
}

pub struct Scheduler {
    pub store: Store,
    device: String,
    control: Arc<Control>,
    pub preview: watch::Sender<Option<PreviewSnapshot>>,
    finished: Mutex<Option<oneshot::Receiver<()>>>,
}

impl Scheduler {
    pub fn device_name(&self) -> &str {
        &self.device
    }
    pub fn error(&self) -> Option<String> {
        self.control.error.lock().unwrap().clone()
    }
    pub fn shutting_down(&self) -> bool {
        self.control.shutdown.load(Ordering::Acquire)
    }
    pub fn wake(&self) {
        let _ = self.control.wake.try_send(());
    }

    pub async fn stop(&self, id: &str) -> Result<RunDetail> {
        let mut detail = self.store.stop(id).await?;
        if let Some((active, flag)) = &*self.control.active.lock().unwrap()
            && active == id
        {
            flag.store(true, Ordering::Release);
        }
        if self.error().is_some() && detail.run.status == RunStatus::Stopping {
            self.store.finish_stop(id).await?;
            detail = self.store.run(id).await?.context("Run not found")?;
        }
        self.wake();
        Ok(detail)
    }

    pub async fn shutdown(&self) {
        self.control.shutdown.store(true, Ordering::Release);
        self.wake();
        let receiver = self.finished.lock().unwrap().take();
        if let Some(receiver) = receiver {
            let _ = receiver.await;
        }
    }

    pub async fn validate_generation(&self, id: &str, generation: u32) -> Result<RunDetail> {
        let detail = self.store.run(id).await?.context("Run not found")?;
        ensure!(
            detail.run.engine_version == engine_identity(&self.device),
            "Checkpoint belongs to an incompatible engine or device"
        );
        ensure!(
            generation < detail.run.completed_generations,
            "Generation not found"
        );
        Ok(detail)
    }
}

pub async fn start(store: Store) -> Result<Arc<Scheduler>> {
    store.recover().await?;
    let (wake, receiver) = mpsc::sync_channel(1);
    let control = Arc::new(Control {
        shutdown: AtomicBool::new(false),
        active: Mutex::new(None),
        wake,
        error: Mutex::new(None),
    });
    let (preview, _) = watch::channel(None);
    let (ready_tx, ready_rx) = oneshot::channel();
    let (done_tx, done_rx) = oneshot::channel();
    let worker_store = store.clone();
    let worker_control = control.clone();
    let worker_preview = preview.clone();
    // SQLx pool cleanup must keep running while the Metal owner waits for work.
    let runtime = tokio::runtime::Handle::current();
    thread::Builder::new()
        .name("metal-owner".into())
        .spawn(move || {
            let engine = match MetalEngine::new() {
                Ok(engine) => engine,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("{error:#}")));
                    let _ = done_tx.send(());
                    return;
                }
            };
            let _ = ready_tx.send(Ok(engine.device_name().to_owned()));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker(
                    &engine,
                    &runtime,
                    &worker_store,
                    &worker_control,
                    &worker_preview,
                    receiver,
                )
            }));
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("{error:#}")),
                Err(_) => Some("Metal worker panicked".into()),
            };
            if let Some(error) = error {
                tracing::error!(%error,"GPU scheduler stopped");
                *worker_control.error.lock().unwrap() = Some(error);
            }
            let _ = done_tx.send(());
        })?;
    let device = ready_rx
        .await
        .context("Metal worker failed during initialization")?
        .map_err(anyhow::Error::msg)?;
    Ok(Arc::new(Scheduler {
        store,
        device,
        control,
        preview,
        finished: Mutex::new(Some(done_rx)),
    }))
}

struct Training {
    detail: RunDetail,
    evaluation: Evaluation,
    stopped: Arc<AtomicBool>,
    started: Instant,
}

struct Replay {
    id: String,
    generation: u32,
    car_count: u32,
    evaluation: Evaluation,
    frames: Vec<f32>,
    chunk_index: u32,
    last_step: u32,
    sample_stride: u32,
}

struct Inspection {
    id: String,
    generation: u32,
    car: u32,
    evaluation: Evaluation,
    frames: Vec<TraceFrame>,
    last_step: u32,
    sample_stride: u32,
}

fn worker(
    engine: &MetalEngine,
    runtime: &tokio::runtime::Handle,
    store: &Store,
    control: &Control,
    preview: &watch::Sender<Option<PreviewSnapshot>>,
    wake: mpsc::Receiver<()>,
) -> Result<()> {
    let mut training: Option<Training> = None;
    let mut replay: Option<Replay> = None;
    let mut inspection: Option<Inspection> = None;
    let mut scan = true;
    let mut training_blocks = 0;
    let mut pending_preview = None;
    let mut preview_at = Instant::now() - Duration::from_secs(1);
    loop {
        if wake.try_recv().is_ok() {
            scan = true;
        }
        if training
            .as_ref()
            .is_some_and(|run| run.stopped.load(Ordering::Acquire))
        {
            let active = training.take().unwrap();
            runtime.block_on(store.finish_stop(&active.detail.run.id))?;
            *control.active.lock().unwrap() = None;
            scan = true;
        }
        if control.shutdown.load(Ordering::Acquire) {
            break;
        }
        if scan {
            if training.is_none()
                && let Some(detail) = runtime.block_on(store.claim_run())?
            {
                let id = detail.run.id.clone();
                match start_training(engine, runtime, store, detail) {
                    Ok(active) => {
                        *control.active.lock().unwrap() =
                            Some((id.clone(), active.stopped.clone()));
                        if runtime
                            .block_on(store.run(&id))?
                            .is_some_and(|r| r.run.status == RunStatus::Stopping)
                        {
                            active.stopped.store(true, Ordering::Release);
                        }
                        training = Some(active);
                    }
                    Err(error) => {
                        runtime.block_on(store.fail_run(&id, &format!("{error:#}")))?;
                        scan = true;
                        continue;
                    }
                }
            }
            if inspection.is_none()
                && let Some((id, generation, car)) = runtime.block_on(store.claim_trace())?
            {
                match begin_checkpoint(engine, runtime, store, &id, generation, Some(car)) {
                    Ok(evaluation) => {
                        let last_step = runtime.block_on(store.outcomes(&id, generation))?
                            [car as usize]
                            .terminal_step;
                        let mut active = Inspection {
                            id: id.clone(),
                            generation,
                            car,
                            evaluation,
                            frames: vec![],
                            last_step,
                            sample_stride: sample_stride(last_step),
                        };
                        match engine.advance(&mut active.evaluation, 0, true) {
                            Ok(block) => {
                                active.frames.extend(block.telemetry);
                                inspection = Some(active)
                            }
                            Err(error) => runtime.block_on(store.fail_trace(
                                &id,
                                generation,
                                car,
                                &format!("{error:#}"),
                            ))?,
                        }
                    }
                    Err(error) => runtime.block_on(store.fail_trace(
                        &id,
                        generation,
                        car,
                        &format!("{error:#}"),
                    ))?,
                }
            }
            if replay.is_none()
                && let Some((id, generation)) = runtime.block_on(store.claim_replay())?
            {
                match begin_checkpoint(engine, runtime, store, &id, generation, None) {
                    Ok(evaluation) => {
                        let poses = engine.poses(&evaluation)?;
                        let manifest = runtime
                            .block_on(store.replay(&id, generation))?
                            .context("Replay not found")?;
                        replay = Some(Replay {
                            id,
                            generation,
                            car_count: (poses.len() / 3) as u32,
                            evaluation,
                            frames: poses,
                            chunk_index: 0,
                            last_step: manifest.last_step,
                            sample_stride: manifest.sample_stride,
                        });
                    }
                    Err(error) => runtime.block_on(store.fail_replay(
                        &id,
                        generation,
                        &format!("{error:#}"),
                    ))?,
                }
            }
            scan = false;
        }
        let auxiliary = inspection.is_some() || replay.is_some();
        if training.is_some() && (training_blocks < 3 || !auxiliary) {
            let active = training.as_mut().unwrap();
            let result = (|| -> Result<bool> {
                if !active.evaluation.is_done() {
                    engine.advance(&mut active.evaluation, BLOCK_STEPS, false)?;
                }
                if !active.evaluation.is_done() {
                    return Ok(false);
                }
                if active.stopped.load(Ordering::Acquire)
                    || control.shutdown.load(Ordering::Acquire)
                {
                    return Ok(false);
                }
                let outcomes = engine.outcomes(&active.evaluation)?;
                let mut summary = engine.summary(&active.evaluation, &outcomes)?;
                let stage = active.detail.stage(summary.generation)?;
                summary.stage_index = stage.index;
                let boundary = (summary.generation + 1)
                    % active.detail.run.config.training.generations_per_circuit
                    == 0;
                let next = if summary.generation + 1 == active.detail.run.total_generations {
                    None
                } else if boundary {
                    Some(engine.population(&active.evaluation)?)
                } else {
                    Some(engine.evolve(&active.evaluation)?)
                };
                let cancelled = || {
                    active.stopped.load(Ordering::Acquire)
                        || control.shutdown.load(Ordering::Acquire)
                };
                let record = if let Some(lap) = fastest_lap(&outcomes) {
                    let incumbent = runtime.block_on(store.champion(&stage.record_key))?;
                    if incumbent.is_none_or(|r| {
                        (lap.3.ticks() - lap.2.ticks()) / SIMULATION_HZ < r.lap_seconds
                    }) {
                        let population = engine.population(&active.evaluation)?;
                        let evaluation = begin_stage(
                            engine,
                            &active.detail,
                            summary.generation,
                            &population,
                            Some(lap.0),
                        )?;
                        record_lap(
                            engine,
                            evaluation,
                            active.detail.run.config.vehicle.step_distance,
                            lap,
                            cancelled,
                        )?
                    } else {
                        None
                    }
                } else {
                    None
                };
                summary.elapsed_ms = active.started.elapsed().as_secs_f64() * 1000.0;
                if active.stopped.load(Ordering::Acquire)
                    || control.shutdown.load(Ordering::Acquire)
                {
                    return Ok(false);
                }
                active.detail = runtime.block_on(store.commit_generation(
                    &active.detail.run.id,
                    &summary,
                    &outcomes,
                    next.as_deref(),
                    record.as_ref(),
                ))?;
                let count = outcomes.len();
                let take = count.min(128);
                pending_preview = Some(PreviewSnapshot {
                    run_id: active.detail.run.id.clone(),
                    generation: summary.generation,
                    stage_index: summary.stage_index,
                    step: summary.steps,
                    car_count: count as u32,
                    cars: (0..take)
                        .map(|i| outcomes[i * count / take].clone())
                        .collect(),
                });
                tracing::info!(run_id=%active.detail.run.id,generation=summary.generation,elapsed_ms=summary.elapsed_ms,"Generation committed");
                if active.detail.run.status != RunStatus::Running {
                    return Ok(true);
                }
                let next = next.context("Missing next population")?;
                if boundary {
                    active.evaluation =
                        begin_stage(engine, &active.detail, summary.generation + 1, &next, None)?;
                } else {
                    engine.restart(&mut active.evaluation, summary.generation + 1, &next)?;
                }
                active.started = Instant::now();
                Ok(false)
            })();
            training_blocks += 1;
            match result {
                Ok(true) => {
                    training = None;
                    *control.active.lock().unwrap() = None;
                    scan = true;
                }
                Ok(false) => {}
                Err(error) => {
                    if error.downcast_ref::<sqlx::Error>().is_some() {
                        return Err(error);
                    }
                    let id = active.detail.run.id.clone();
                    runtime.block_on(store.fail_run(&id, &format!("{error:#}")))?;
                    training = None;
                    *control.active.lock().unwrap() = None;
                    scan = true;
                }
            }
        } else if let Some(active) = inspection.as_mut() {
            let result = (|| -> Result<bool> {
                if !active.evaluation.is_done() {
                    active.frames.extend(
                        engine
                            .advance(&mut active.evaluation, BLOCK_STEPS, true)?
                            .telemetry
                            .into_iter()
                            .filter(|frame| {
                                frame.step % active.sample_stride == 0
                                    || frame.step == active.last_step
                            }),
                    );
                }
                if active.evaluation.is_done() {
                    ensure!(
                        active.evaluation.step() == active.last_step,
                        "Inspection diverged from its checkpoint"
                    );
                    runtime.block_on(store.save_trace(&CarTrace {
                        run_id: active.id.clone(),
                        generation: active.generation,
                        car_id: active.car,
                        frames: std::mem::take(&mut active.frames),
                        sample_stride: active.sample_stride,
                    }))?;
                    return Ok(true);
                }
                Ok(false)
            })();
            training_blocks = 0;
            match result {
                Ok(true) => {
                    inspection = None;
                    scan = true
                }
                Ok(false) => {}
                Err(error) => {
                    runtime.block_on(store.fail_trace(
                        &active.id,
                        active.generation,
                        active.car,
                        &format!("{error:#}"),
                    ))?;
                    inspection = None;
                    scan = true
                }
            }
        } else if let Some(active) = replay.as_mut() {
            let result = (|| -> Result<bool> {
                let width = active.car_count as usize * 3;
                let retained = active.frames.len() / width;
                let needed = REPLAY_CHUNK_FRAMES - retained as u32;
                if !active.evaluation.is_done() && needed > 0 {
                    let steps = (needed * active.sample_stride
                        - active.evaluation.step() % active.sample_stride)
                        .min(BLOCK_STEPS);
                    let block = engine.advance(&mut active.evaluation, steps, true)?;
                    for (frame, poses) in block.poses.chunks_exact(width).enumerate() {
                        let step = block.first_step + frame as u32;
                        if step % active.sample_stride == 0 || step == active.last_step {
                            active.frames.extend_from_slice(poses);
                        }
                    }
                }
                let done = active.evaluation.is_done();
                ensure!(
                    !done || active.evaluation.step() == active.last_step,
                    "Replay diverged from its checkpoint"
                );
                if active.frames.len() / width == REPLAY_CHUNK_FRAMES as usize || done {
                    let frame_count = (active.frames.len() / width) as u32;
                    let first_frame = active.chunk_index * REPLAY_CHUNK_FRAMES;
                    let mut data = Vec::with_capacity(16 + active.frames.len() * 4);
                    for value in [REPLAY_VERSION, first_frame, frame_count, active.car_count] {
                        data.extend(value.to_le_bytes());
                    }
                    data.extend(float_bytes(&active.frames));
                    runtime.block_on(store.write_chunk(
                        &active.id,
                        active.generation,
                        active.chunk_index,
                        &data,
                        done,
                    ))?;
                    active.frames.clear();
                    active.chunk_index += 1;
                }
                Ok(done)
            })();
            training_blocks = 0;
            match result {
                Ok(true) => {
                    replay = None;
                    scan = true
                }
                Ok(false) => {}
                Err(error) => {
                    runtime.block_on(store.fail_replay(
                        &active.id,
                        active.generation,
                        &format!("{error:#}"),
                    ))?;
                    replay = None;
                    scan = true
                }
            }
        } else if wake.recv_timeout(Duration::from_millis(100)).is_ok() {
            scan = true;
        }
        if pending_preview.is_some() && preview_at.elapsed() >= Duration::from_millis(500) {
            preview.send_replace(pending_preview.take());
            preview_at = Instant::now();
        }
    }
    if let Some(active) = training {
        runtime.block_on(store.finish_stop(&active.detail.run.id))?;
    }
    *control.active.lock().unwrap() = None;
    Ok(())
}

fn start_training(
    engine: &MetalEngine,
    runtime: &tokio::runtime::Handle,
    store: &Store,
    detail: RunDetail,
) -> Result<Training> {
    ensure!(
        detail.run.engine_version == engine_identity(engine.device_name()),
        "Checkpoint belongs to an incompatible engine or device"
    );
    detail.run.config.validate()?;
    let generation = detail.run.completed_generations;
    let population = match runtime.block_on(store.population(&detail.run.id, generation))? {
        Some(population) => population,
        None => {
            ensure!(generation == 0, "Missing generation checkpoint");
            let population = engine.initial_population(&detail.run.config)?;
            runtime.block_on(store.initial_checkpoint(&detail.run.id, &population))?;
            population
        }
    };
    let evaluation = begin_stage(engine, &detail, generation, &population, None)?;
    Ok(Training {
        detail,
        evaluation,
        stopped: Arc::new(AtomicBool::new(false)),
        started: Instant::now(),
    })
}

fn begin_checkpoint(
    engine: &MetalEngine,
    runtime: &tokio::runtime::Handle,
    store: &Store,
    id: &str,
    generation: u32,
    car: Option<u32>,
) -> Result<Evaluation> {
    let detail = runtime.block_on(store.run(id))?.context("Run not found")?;
    ensure!(
        detail.run.engine_version == engine_identity(engine.device_name()),
        "Checkpoint belongs to an incompatible engine or device"
    );
    let population = runtime
        .block_on(store.population(id, generation))?
        .context("Checkpoint not found")?;
    begin_stage(engine, &detail, generation, &population, car)
}

fn begin_stage(
    engine: &MetalEngine,
    detail: &RunDetail,
    generation: u32,
    population: &[f32],
    car: Option<u32>,
) -> Result<Evaluation> {
    let stage = detail.stage(generation)?;
    engine.begin(
        &geometry::prepare(&stage.circuit.track)?,
        &detail.run.config,
        stage.max_steps,
        generation,
        population,
        car,
    )
}

pub(crate) fn record_lap(
    engine: &MetalEngine,
    mut evaluation: Evaluation,
    step_distance: f32,
    (car_id, lap, start, end): (u32, u32, LapCrossing, LapCrossing),
    cancelled: impl Fn() -> bool,
) -> Result<Option<RecordCandidate>> {
    let mut previous = engine
        .advance(&mut evaluation, 0, false)?
        .telemetry
        .remove(0);
    let mut ghost = Vec::new();
    if start.ticks() == 0.0 {
        ghost.extend([0.0, previous.x, previous.y, previous.heading]);
    }
    while evaluation.step() < end.step + 1 && !evaluation.is_done() {
        if cancelled() {
            return Ok(None);
        }
        let steps = BLOCK_STEPS.min(end.step + 1 - evaluation.step());
        for frame in engine.advance(&mut evaluation, steps, false)?.telemetry {
            let crossing_pose = |crossing: LapCrossing| {
                let distance = step_distance * crossing.fraction;
                [
                    previous.x + distance * frame.heading.cos(),
                    previous.y + distance * frame.heading.sin(),
                    frame.heading,
                ]
            };
            if start.ticks() > 0.0 && frame.step == start.step + 1 {
                ghost.push(0.0);
                ghost.extend(crossing_pose(start));
            }
            if f64::from(frame.step) > start.ticks() && f64::from(frame.step) < end.ticks() {
                ghost.extend([
                    (f64::from(frame.step) - start.ticks()) as f32,
                    frame.x,
                    frame.y,
                    frame.heading,
                ]);
            }
            if frame.step == end.step + 1 {
                ghost.push((end.ticks() - start.ticks()) as f32);
                ghost.extend(crossing_pose(end));
            }
            previous = frame;
        }
    }
    let outcome = engine.outcomes(&evaluation)?.remove(0);
    ensure!(
        outcome.lap_ends.get(lap as usize - 1) == Some(&end),
        "Record replay did not reproduce its crossing"
    );
    ensure!(
        lap == 1 || outcome.lap_ends.get(lap as usize - 2) == Some(&start),
        "Record replay did not reproduce its lap start"
    );
    let genome = engine.population(&evaluation)?;
    // A crossing can round onto the preceding f32 sample; retain the exact endpoint pose.
    let len = ghost.len();
    if len >= 12 && ghost[len - 4] == ghost[len - 8] {
        ghost.drain(len - 8..len - 4);
    }
    Ok(Some(RecordCandidate {
        car_id,
        lap,
        start,
        end,
        genome,
        ghost,
    }))
}
