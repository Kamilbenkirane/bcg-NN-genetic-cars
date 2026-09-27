use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const ENGINE_VERSION: &str = "metal-tour-f32-philox-v4";
pub const SIMULATION_HZ: f64 = 30.0;
// Change only when road legality, movement, sensors or timing rules change.
pub const RECORD_RULES: &str = "forward-laps-f32-v1";
pub const GENOME_SIZE: usize = 22;
pub const BLOCK_STEPS: u32 = 32;
pub const REPLAY_VERSION: u32 = 2;
pub const REPLAY_CHUNK_FRAMES: u32 = 32;

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VehicleConfig {
    pub step_distance: f32,
    pub sensor_range: f32,
    pub max_heading_change: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrainingConfig {
    pub population: u32,
    pub generations_per_circuit: u32,
    pub target_laps: u32,
    pub elite_count: u32,
    pub mutations: u32,
    pub mutation_scale: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunConfig {
    pub seed: String,
    pub vehicle: VehicleConfig,
    pub training: TrainingConfig,
}
// Product defaults live in the frontend; this fixture serves the Rust tests.
#[cfg(test)]
impl Default for RunConfig {
    fn default() -> Self {
        Self {
            seed: "42".into(),
            vehicle: VehicleConfig {
                step_distance: 5.0,
                sensor_range: 200.0,
                max_heading_change: std::f32::consts::PI / 8.0,
            },
            training: TrainingConfig {
                population: 500,
                generations_per_circuit: 50,
                target_laps: 5,
                elite_count: 10,
                mutations: 1,
                mutation_scale: 1.0,
            },
        }
    }
}
impl RunConfig {
    pub fn seed_value(&self) -> anyhow::Result<u64> {
        anyhow::ensure!(
            !self.seed.is_empty() && self.seed.bytes().all(|b| b.is_ascii_digit()),
            "seed must be an unsigned decimal integer"
        );
        Ok(self.seed.parse()?)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        self.seed_value()?;
        let v = &self.vehicle;
        anyhow::ensure!(
            v.step_distance.is_finite() && v.step_distance > 0.0 && v.step_distance <= 100.0,
            "stepDistance must be in (0, 100]"
        );
        anyhow::ensure!(
            v.sensor_range.is_finite() && v.sensor_range > 0.0 && v.sensor_range <= 10_000.0,
            "sensorRange must be in (0, 10000]"
        );
        anyhow::ensure!(
            v.max_heading_change.is_finite()
                && (0.0..=std::f32::consts::PI).contains(&v.max_heading_change),
            "maxHeadingChange must be in [0, pi]"
        );
        let t = &self.training;
        anyhow::ensure!(
            (1..=2000).contains(&t.population),
            "population must be in [1, 2000]"
        );
        anyhow::ensure!(
            (1..=100).contains(&t.generations_per_circuit),
            "generationsPerCircuit must be in [1, 100]"
        );
        anyhow::ensure!(
            (1..=100).contains(&t.target_laps),
            "targetLaps must be in [1, 100]"
        );
        anyhow::ensure!(
            t.elite_count >= 1 && t.elite_count <= t.population,
            "eliteCount must be in [1, population]"
        );
        anyhow::ensure!(t.mutations <= 100, "mutations must be in [0, 100]");
        anyhow::ensure!(
            t.mutation_scale.is_finite() && (0.0..=100.0).contains(&t.mutation_scale),
            "mutationScale must be in [0, 100]"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub points: Vec<[f32; 2]>,
    pub left: Vec<[f32; 2]>,
    pub right: Vec<[f32; 2]>,
    pub width: f32,
    pub spawn: [f32; 3],
    pub spawn_distance: f32,
    pub lap_length: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CarStatus {
    Running,
    Crashed,
    Finished,
    TimedOut,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CarOutcome {
    pub car_id: u32,
    pub fitness: f32,
    pub status: CarStatus,
    pub terminal_step: u32,
    pub completed_laps: u32,
    pub lap_ends: Vec<LapCrossing>,
    pub x: f32,
    pub y: f32,
    pub heading: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GenerationSummary {
    pub generation: u32,
    pub stage_index: u32,
    pub best_car_id: u32,
    pub best_lap_seconds: Option<f64>,
    pub best_fitness: f32,
    pub mean_fitness: f32,
    pub finished: u32,
    pub crashed: u32,
    pub timed_out: u32,
    pub steps: u32,
    pub elapsed_ms: f64,
    pub car_steps: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Stopping,
    Stopped,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub status: RunStatus,
    pub config: RunConfig,
    pub completed_generations: u32,
    pub last_error: Option<String>,
    pub device_name: String,
    pub engine_version: String,
    pub total_generations: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    pub run: RunSummary,
    pub stages: Vec<RunStage>,
}

impl RunDetail {
    pub fn stage(&self, generation: u32) -> anyhow::Result<&RunStage> {
        self.stages
            .get((generation / self.run.config.training.generations_per_circuit) as usize)
            .ok_or_else(|| anyhow::anyhow!("Generation outside circuit tour"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunStage {
    pub index: u32,
    pub circuit: Circuit,
    pub first_generation: u32,
    pub max_steps: u32,
    pub record_key: String,
    pub baseline_record_id: Option<String>,
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    TS,
    bytemuck::Pod,
    bytemuck::Zeroable,
)]
#[repr(C)]
#[serde(rename_all = "camelCase")]
pub struct LapCrossing {
    pub step: u32,
    pub fraction: f32,
}

impl LapCrossing {
    pub fn ticks(self) -> f64 {
        f64::from(self.step) + f64::from(self.fraction)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LapRecord {
    pub id: String,
    pub record_key: String,
    pub circuit_id: String,
    pub created_at: String,
    pub run_id: String,
    pub run_name: String,
    pub generation: u32,
    pub car_id: u32,
    pub lap: u32,
    pub lap_seconds: f64,
    pub ghost_frames: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CircuitRecord {
    pub stage_index: u32,
    pub champion: Option<LapRecord>,
    pub baseline: Option<LapRecord>,
}

pub struct RecordCandidate {
    pub car_id: u32,
    pub lap: u32,
    pub start: LapCrossing,
    pub end: LapCrossing,
    pub genome: Vec<f32>,
    // Interleaved relative simulation tick, x, y, heading; full-resolution single lap.
    pub ghost: Vec<f32>,
}

pub fn fastest_lap(outcomes: &[CarOutcome]) -> Option<(u32, u32, LapCrossing, LapCrossing)> {
    outcomes
        .iter()
        .flat_map(|car| {
            car.lap_ends.iter().enumerate().map(move |(i, end)| {
                (
                    car.car_id,
                    i as u32 + 1,
                    i.checked_sub(1)
                        .map(|j| car.lap_ends[j])
                        .unwrap_or_default(),
                    *end,
                )
            })
        })
        .min_by(|a, b| (a.3.ticks() - a.2.ticks()).total_cmp(&(b.3.ticks() - b.2.ticks())))
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRunRequest {
    pub request_id: String,
    pub name: String,
    pub config: RunConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Circuit {
    pub id: String,
    pub name: String,
    pub description: String,
    pub track: Track,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSnapshot {
    pub run_id: String,
    pub generation: u32,
    pub stage_index: u32,
    pub step: u32,
    pub car_count: u32,
    pub cars: Vec<CarOutcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Queued,
    Running,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReplayManifest {
    pub run_id: String,
    pub generation: u32,
    pub stage_index: u32,
    pub best_car_id: u32,
    pub simulation_hz: f64,
    pub status: ReplayStatus,
    pub car_count: u32,
    pub total_frames: u32,
    pub last_step: u32,
    pub sample_stride: u32,
    pub frames_per_chunk: u32,
    pub chunks: u32,
    pub available_chunks: Vec<u32>,
    pub outcomes: Vec<CarOutcome>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayRequest {
    #[serde(default)]
    pub follow: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TraceFrame {
    pub step: u32,
    pub x: f32,
    pub y: f32,
    pub heading: f32,
    pub sensors: [f32; 5],
    pub steering: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CarTrace {
    pub run_id: String,
    pub generation: u32,
    pub car_id: u32,
    pub frames: Vec<TraceFrame>,
    pub sample_stride: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub status: String,
    pub device_name: String,
    pub engine_version: String,
}

pub fn typescript() -> String {
    let declarations = [
        VehicleConfig::decl(),
        TrainingConfig::decl(),
        RunConfig::decl(),
        Track::decl(),
        CarStatus::decl(),
        CarOutcome::decl(),
        GenerationSummary::decl(),
        RunStatus::decl(),
        RunSummary::decl(),
        RunDetail::decl(),
        RunStage::decl(),
        LapCrossing::decl(),
        LapRecord::decl(),
        CircuitRecord::decl(),
        CreateRunRequest::decl(),
        Circuit::decl(),
        PreviewSnapshot::decl(),
        ReplayStatus::decl(),
        ReplayManifest::decl(),
        ReplayRequest::decl(),
        TraceFrame::decl(),
        CarTrace::decl(),
        Health::decl(),
    ];
    format!(
        "// Generated from backend/src/model.rs. Run bun run types.\n{}\n",
        declarations
            .into_iter()
            .map(|s| format!("export {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Bound derived playback data independently of the simulation duration.
pub fn sample_stride(last_step: u32) -> u32 {
    last_step.div_ceil(9_999).max(1)
}

pub fn driving_budget(config: &RunConfig, lap_length: f32) -> anyhow::Result<u32> {
    config.validate()?;
    let steps = (4.0 * f64::from(lap_length) * f64::from(config.training.target_laps)
        / f64::from(config.vehicle.step_distance))
    .ceil()
    .max(256.0);
    anyhow::ensure!(
        lap_length.is_finite() && lap_length > 0.0,
        "Invalid circuit length"
    );
    anyhow::ensure!(
        steps <= 100_000.0,
        "This vehicle setup needs too many steps. Reduce the lap target or increase distance per simulation step in Advanced settings."
    );
    Ok(steps as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn playback_sample_budget_keeps_both_endpoints() {
        for last in [0, 1, 9_999, 10_000, 17_749, 100_000u32] {
            let stride = sample_stride(last);
            let frames = last.div_ceil(stride) + 1;
            assert!(frames <= 10_000);
            let retained: Vec<_> = (0..last).step_by(stride as usize).chain([last]).collect();
            assert_eq!(retained.len() as u32, frames);
            assert_eq!(retained.first(), Some(&0));
            assert_eq!(retained.last(), Some(&last));
            assert!(retained.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
}
