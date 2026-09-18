//! Native Metal execution. All simulation and evolution arithmetic runs in engine.metal.
//! The scheduler owns this object on one thread; every submission completes before return.

use std::{
    cell::RefCell,
    mem::size_of,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use anyhow::{Context, Result, anyhow, ensure};
use bytemuck::{Pod, Zeroable};
use objc2::{
    rc::{Retained, autoreleasepool},
    runtime::ProtocolObject,
};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder, MTLCommandQueue,
    MTLCompileOptions, MTLComputeCommandEncoder, MTLComputePipelineState,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLLanguageVersion, MTLLibrary,
    MTLMathFloatingPointFunctions, MTLMathMode, MTLResourceOptions, MTLSize,
};

use crate::{
    geometry::PreparedTrack,
    model::{
        BLOCK_STEPS, CarOutcome, CarStatus, GENOME_SIZE, GenerationSummary, LapCrossing, RunConfig,
        SIMULATION_HZ, TraceFrame, fastest_lap,
    },
};

// Required by MTLCreateSystemDefaultDevice even though no CoreGraphics API is called.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {}

type Buffer = Retained<ProtocolObject<dyn MTLBuffer>>;
type Pipeline = Retained<ProtocolObject<dyn MTLComputePipelineState>>;

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Params {
    population: u32,
    max_steps: u32,
    elite_count: u32,
    mutation_count: u32,
    generation: u32,
    seed_low: u32,
    seed_high: u32,
    inspect_car: u32,
    block_steps: u32,
    record: u32,
    node_count: u32,
    center_count: u32,
    step_distance: f32,
    sensor_range: f32,
    max_heading_change: f32,
    mutation_scale: f32,
    spawn_x: f32,
    spawn_y: f32,
    spawn_heading: f32,
    spawn_distance: f32,
    lap_length: f32,
    tolerance: f32,
    finish_dx: f32,
    finish_dy: f32,
    first_step: u32,
    target_laps: u32,
    gate_index: u32,
    reserved: u32,
}

impl Params {
    fn from_config(config: &RunConfig, generation: u32) -> Result<Self> {
        let seed = config.seed_value()?;
        Ok(Self {
            population: config.training.population,
            target_laps: config.training.target_laps,
            elite_count: config.training.elite_count,
            mutation_count: config.training.mutations,
            generation,
            seed_low: seed as u32,
            seed_high: (seed >> 32) as u32,
            inspect_car: u32::MAX,
            step_distance: config.vehicle.step_distance,
            sensor_range: config.vehicle.sensor_range,
            max_heading_change: config.vehicle.max_heading_change,
            mutation_scale: config.training.mutation_scale,
            ..Self::zeroed()
        })
    }
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct GpuCarState {
    x: f32,
    y: f32,
    heading: f32,
    fitness: f32,
    steps: u32,
    status: u32,
    distance: f32,
    previous_progress: f32,
    completed_laps: u32,
    finish_fraction: f32,
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct GpuTrace {
    step: u32,
    x: f32,
    y: f32,
    heading: f32,
    sensors: [f32; 5],
    steering: f32,
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct GpuSummary {
    best_fitness: f32,
    mean_fitness: f32,
    finished: u32,
    crashed: u32,
    timed_out: u32,
    steps: u32,
    car_steps: u32,
    best_car_id: u32,
}

pub struct MetalEngine {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    device_name: String,
    initialize: Pipeline,
    reset: Pipeline,
    simulate: Pipeline,
    rank: Pipeline,
    breed: Pipeline,
    summarize: Pipeline,
    // Rc/RefCell also keep the engine on its sole owner thread.
    geometry_cache: RefCell<Vec<Weak<GpuGeometry>>>,
}

struct GpuGeometry {
    points: Vec<[f32; 2]>,
    width: f32,
    walls: Buffer,
    nodes: Buffer,
    centerline: Buffer,
}

pub struct Evaluation {
    params: Params,
    selected_car: Option<u32>,
    step: u32,
    done: bool,
    genomes: Buffer,
    next_genomes: Buffer,
    states: Buffer,
    geometry: Rc<GpuGeometry>,
    recorded_poses: Buffer,
    lap_ends: Buffer,
    traces: Buffer,
    control: Buffer,
    ranked: Buffer,
    summary: Buffer,
}

impl Evaluation {
    pub fn step(&self) -> u32 {
        self.step
    }
    pub fn is_done(&self) -> bool {
        self.done
    }
}

pub struct BlockResult {
    pub first_step: u32,
    pub frame_count: u32,
    pub poses: Vec<f32>,
    pub telemetry: Vec<TraceFrame>,
}

impl MetalEngine {
    pub fn new() -> Result<Self> {
        autoreleasepool(|_| {
            let device = MTLCreateSystemDefaultDevice().context("no Metal GPU is available")?;
            let device_name = device.name().to_string();
            let queue = device
                .newCommandQueue()
                .context("could not create Metal command queue")?;
            let options = MTLCompileOptions::new();
            options.setLanguageVersion(MTLLanguageVersion::Version3_1);
            options.setMathMode(MTLMathMode::Safe);
            options.setMathFloatingPointFunctions(MTLMathFloatingPointFunctions::Precise);
            let library = device
                .newLibraryWithSource_options_error(
                    &NSString::from_str(include_str!("engine.metal")),
                    Some(&options),
                )
                .map_err(|error| anyhow!("Metal shader compilation failed: {error}"))?;
            let pipeline = |name: &str| -> Result<Pipeline> {
                let function = library
                    .newFunctionWithName(&NSString::from_str(name))
                    .with_context(|| format!("Metal kernel {name} was not found"))?;
                device
                    .newComputePipelineStateWithFunction_error(&function)
                    .map_err(|error| anyhow!("Metal pipeline {name} failed: {error}"))
            };
            Ok(Self {
                initialize: pipeline("initialize_population")?,
                reset: pipeline("reset_states")?,
                simulate: pipeline("simulate_block")?,
                rank: pipeline("rank_population")?,
                breed: pipeline("breed_population")?,
                summarize: pipeline("summarize")?,
                device,
                queue,
                device_name,
                geometry_cache: RefCell::new(Vec::new()),
            })
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn initial_population(&self, config: &RunConfig) -> Result<Vec<f32>> {
        config.validate()?;
        let params = Params::from_config(config, 0)?;
        let count = GENOME_SIZE * params.population as usize;
        let genomes = self.allocate::<f32>(count)?;
        self.dispatch(&self.initialize, &params, &[&genomes], params.population)?;
        read_buffer(&genomes, count)
    }

    pub fn begin(
        &self,
        track: &PreparedTrack,
        config: &RunConfig,
        max_steps: u32,
        generation: u32,
        population: &[f32],
        inspect_car: Option<u32>,
    ) -> Result<Evaluation> {
        config.validate()?;
        ensure!(
            (1..=100_000).contains(&max_steps),
            "Invalid stored driving budget"
        );
        let original_count = config.training.population as usize;
        ensure!(
            population.len() == GENOME_SIZE * original_count,
            "population has the wrong shape"
        );
        ensure!(
            population.iter().all(|value| value.is_finite()),
            "population contains non-finite weights"
        );
        if let Some(car) = inspect_car {
            ensure!(
                (car as usize) < original_count,
                "inspection car is outside the population"
            );
        }
        ensure!(
            !track.walls.is_empty() && !track.nodes.is_empty() && !track.center_segments.is_empty(),
            "track geometry is empty"
        );
        let mut params = Params::from_config(config, generation)?;
        params.max_steps = max_steps;
        params.gate_index = track
            .walls
            .iter()
            .position(|wall| wall.kind == 1)
            .context("Missing timing gate")? as u32;
        params.node_count = track.nodes.len() as u32;
        params.center_count = track.center_segments.len() as u32;
        params.spawn_x = track.spawn[0];
        params.spawn_y = track.spawn[1];
        params.spawn_heading = track.spawn[2];
        params.spawn_distance = track.spawn_distance;
        params.lap_length = track.lap_length;
        params.tolerance = track.tolerance;
        params.finish_dx = track.finish_direction[0];
        params.finish_dy = track.finish_direction[1];
        let selected;
        let population = if let Some(car) = inspect_car {
            params.population = 1;
            params.inspect_car = 0;
            selected = (0..GENOME_SIZE)
                .map(|parameter| population[parameter * original_count + car as usize])
                .collect::<Vec<_>>();
            selected.as_slice()
        } else {
            population
        };
        let count = params.population as usize;
        let evaluation = Evaluation {
            params,
            selected_car: inspect_car,
            step: 0,
            done: false,
            genomes: self.upload(population)?,
            next_genomes: self.allocate::<f32>(GENOME_SIZE * count)?,
            states: self.allocate::<GpuCarState>(count)?,
            geometry: self.geometry_buffers(track)?,
            recorded_poses: self.allocate::<f32>(BLOCK_STEPS as usize * count * 3)?,
            lap_ends: self.allocate::<LapCrossing>(count * params.target_laps as usize)?,
            traces: self.allocate::<GpuTrace>(BLOCK_STEPS as usize)?,
            control: self.allocate::<u32>(2)?,
            ranked: self.allocate::<u32>(count)?,
            summary: self.allocate::<GpuSummary>(1)?,
        };
        self.dispatch(
            &self.reset,
            &params,
            &[&evaluation.states],
            params.population,
        )?;
        Ok(evaluation)
    }

    /// Reuse a training allocation after its generation checkpoint has been committed.
    pub fn restart(
        &self,
        evaluation: &mut Evaluation,
        generation: u32,
        population: &[f32],
    ) -> Result<()> {
        ensure!(
            evaluation.done && evaluation.selected_car.is_none(),
            "restart requires a completed full population"
        );
        ensure!(
            population.len() == GENOME_SIZE * evaluation.params.population as usize,
            "population has the wrong shape"
        );
        ensure!(
            population.iter().all(|value| value.is_finite()),
            "population contains non-finite weights"
        );
        write_buffer(&evaluation.genomes, population)?;
        evaluation.params.generation = generation;
        self.dispatch(
            &self.reset,
            &evaluation.params,
            &[&evaluation.states],
            evaluation.params.population,
        )?;
        evaluation.step = 0;
        evaluation.done = false;
        Ok(())
    }

    /// Frames are post-action poses. A zero-step call returns current inspection telemetry only.
    pub fn advance(
        &self,
        evaluation: &mut Evaluation,
        steps: u32,
        record: bool,
    ) -> Result<BlockResult> {
        ensure!(
            steps <= BLOCK_STEPS,
            "a Metal block may contain at most {BLOCK_STEPS} steps"
        );
        let steps = if evaluation.done {
            0
        } else {
            steps.min(evaluation.params.max_steps - evaluation.step)
        };
        let first_step = evaluation.step + u32::from(steps != 0);
        if steps == 0 && evaluation.selected_car.is_none() {
            return Ok(BlockResult {
                first_step,
                frame_count: 0,
                poses: vec![],
                telemetry: vec![],
            });
        }
        let mut params = evaluation.params;
        params.block_steps = steps;
        params.first_step = first_step;
        params.record = u32::from(record);
        // The preceding dispatch has completed; CPU/GPU never access this shared word together.
        write_buffer(&evaluation.control, &[0u32, 0])?;
        self.dispatch(
            &self.simulate,
            &params,
            &[
                &evaluation.genomes,
                &evaluation.states,
                &evaluation.geometry.walls,
                &evaluation.geometry.nodes,
                &evaluation.geometry.centerline,
                &evaluation.recorded_poses,
                &evaluation.traces,
                &evaluation.control,
                &evaluation.lap_ends,
            ],
            params.population,
        )?;
        let control: Vec<u32> = read_buffer(&evaluation.control, 2)?;
        evaluation.done = control[0] == 0;
        let next_step = if evaluation.done {
            control[1]
        } else {
            evaluation.step + steps
        };
        let frame_count = next_step.saturating_sub(evaluation.step);
        evaluation.step = next_step;
        let poses = if record {
            read_buffer(
                &evaluation.recorded_poses,
                frame_count as usize * params.population as usize * 3,
            )?
        } else {
            vec![]
        };
        let telemetry = if evaluation.selected_car.is_some() {
            let count = if steps == 0 { 1 } else { frame_count as usize };
            read_buffer::<GpuTrace>(&evaluation.traces, count)?
                .into_iter()
                .map(|trace| TraceFrame {
                    step: trace.step,
                    x: trace.x,
                    y: trace.y,
                    heading: trace.heading,
                    sensors: trace.sensors,
                    steering: trace.steering,
                })
                .collect()
        } else {
            vec![]
        };
        Ok(BlockResult {
            first_step,
            frame_count,
            poses,
            telemetry,
        })
    }

    pub fn poses(&self, evaluation: &Evaluation) -> Result<Vec<f32>> {
        Ok(
            read_buffer::<GpuCarState>(&evaluation.states, evaluation.params.population as usize)?
                .into_iter()
                .flat_map(|state| [state.x, state.y, state.heading])
                .collect(),
        )
    }

    pub fn outcomes(&self, evaluation: &Evaluation) -> Result<Vec<CarOutcome>> {
        let stride = evaluation.params.target_laps as usize;
        let lap_ends = read_buffer::<LapCrossing>(
            &evaluation.lap_ends,
            evaluation.params.population as usize * stride,
        )?;
        read_buffer::<GpuCarState>(&evaluation.states, evaluation.params.population as usize)?
            .into_iter()
            .enumerate()
            .map(|(car, state)| {
                ensure!(
                    [state.x, state.y, state.heading, state.fitness]
                        .iter()
                        .all(|value| value.is_finite()),
                    "Metal produced a non-finite car state"
                );
                Ok(CarOutcome {
                    car_id: evaluation.selected_car.unwrap_or(car as u32),
                    fitness: state.fitness,
                    status: match state.status {
                        0 => CarStatus::Running,
                        1 => CarStatus::Crashed,
                        2 => CarStatus::Finished,
                        3 => CarStatus::TimedOut,
                        other => return Err(anyhow!("Metal produced unknown car status {other}")),
                    },
                    terminal_step: state.steps,
                    completed_laps: state.completed_laps,
                    lap_ends: lap_ends[car * stride..car * stride + state.completed_laps as usize]
                        .to_vec(),
                    x: state.x,
                    y: state.y,
                    heading: state.heading,
                })
            })
            .collect()
    }

    pub fn summary(
        &self,
        evaluation: &Evaluation,
        outcomes: &[CarOutcome],
    ) -> Result<GenerationSummary> {
        ensure!(
            evaluation.done,
            "generation summary requires a completed evaluation"
        );
        self.dispatch(
            &self.summarize,
            &evaluation.params,
            &[&evaluation.states, &evaluation.summary],
            1,
        )?;
        let summary = read_buffer::<GpuSummary>(&evaluation.summary, 1)?[0];
        Ok(GenerationSummary {
            generation: evaluation.params.generation,
            stage_index: 0,
            best_car_id: summary.best_car_id,
            best_lap_seconds: fastest_lap(outcomes)
                .map(|(_, _, start, end)| (end.ticks() - start.ticks()) / SIMULATION_HZ),
            best_fitness: summary.best_fitness,
            mean_fitness: summary.mean_fitness,
            finished: summary.finished,
            crashed: summary.crashed,
            timed_out: summary.timed_out,
            steps: summary.steps,
            elapsed_ms: 0.0,
            car_steps: f64::from(summary.car_steps),
        })
    }

    pub fn population(&self, evaluation: &Evaluation) -> Result<Vec<f32>> {
        read_buffer(
            &evaluation.genomes,
            GENOME_SIZE * evaluation.params.population as usize,
        )
    }

    pub fn evolve(&self, evaluation: &Evaluation) -> Result<Vec<f32>> {
        ensure!(
            evaluation.done && evaluation.selected_car.is_none(),
            "breeding requires a completed full population"
        );
        self.dispatch(
            &self.rank,
            &evaluation.params,
            &[&evaluation.states, &evaluation.ranked],
            evaluation.params.population,
        )?;
        let mut params = evaluation.params;
        // Reproduction draws are indexed by the generation in which the children are evaluated.
        params.generation = params
            .generation
            .checked_add(1)
            .context("generation overflow")?;
        self.dispatch(
            &self.breed,
            &params,
            &[
                &evaluation.genomes,
                &evaluation.ranked,
                &evaluation.next_genomes,
            ],
            params.population,
        )?;
        read_buffer(
            &evaluation.next_genomes,
            GENOME_SIZE * params.population as usize,
        )
    }

    fn allocate<T: Pod>(&self, count: usize) -> Result<Buffer> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .context("Metal allocation size overflow")?;
        ensure!(bytes > 0, "empty Metal allocation");
        self.device
            .newBufferWithLength_options(bytes, MTLResourceOptions::StorageModeShared)
            .with_context(|| format!("Metal could not allocate {bytes} bytes"))
    }

    fn upload<T: Pod>(&self, values: &[T]) -> Result<Buffer> {
        let buffer = self.allocate::<T>(values.len())?;
        write_buffer(&buffer, values)?;
        Ok(buffer)
    }

    fn geometry_buffers(&self, track: &PreparedTrack) -> Result<Rc<GpuGeometry>> {
        let mut cache = self.geometry_cache.borrow_mut();
        cache.retain(|entry| entry.strong_count() != 0);
        if let Some(buffers) = cache.iter().filter_map(Weak::upgrade).find(|buffers| {
            buffers.width == track.track.width && buffers.points == track.track.points
        }) {
            return Ok(buffers);
        }
        let buffers = Rc::new(GpuGeometry {
            points: track.track.points.clone(),
            width: track.track.width,
            walls: self.upload(&track.walls)?,
            nodes: self.upload(&track.nodes)?,
            centerline: self.upload(&track.center_segments)?,
        });
        cache.push(Rc::downgrade(&buffers));
        Ok(buffers)
    }

    fn dispatch(
        &self,
        pipeline: &Pipeline,
        params: &Params,
        buffers: &[&Buffer],
        threads: u32,
    ) -> Result<()> {
        autoreleasepool(|_| {
            let command = self
                .queue
                .commandBuffer()
                .context("Metal command buffer allocation failed")?;
            let encoder = command
                .computeCommandEncoder()
                .context("Metal compute encoder allocation failed")?;
            encoder.setComputePipelineState(pipeline);
            // Each internal call supplies the shader's exact POD layout and buffer order.
            // Metal retains the buffers, and this method waits before any host access resumes.
            unsafe {
                encoder.setBytes_length_atIndex(
                    NonNull::from(params).cast(),
                    size_of::<Params>(),
                    0,
                );
                for (index, buffer) in buffers.iter().enumerate() {
                    encoder.setBuffer_offset_atIndex(Some(buffer), 0, index + 1);
                }
            }
            let width = pipeline.threadExecutionWidth();
            let group_size = (width * 4).min(pipeline.maxTotalThreadsPerThreadgroup());
            encoder.dispatchThreads_threadsPerThreadgroup(
                MTLSize {
                    width: threads as usize,
                    height: 1,
                    depth: 1,
                },
                MTLSize {
                    width: group_size,
                    height: 1,
                    depth: 1,
                },
            );
            encoder.endEncoding();
            command.commit();
            command.waitUntilCompleted();
            if let Some(error) = command.error() {
                return Err(anyhow!("Metal execution failed: {error}"));
            }
            ensure!(
                command.status() == MTLCommandBufferStatus::Completed,
                "Metal command did not complete"
            );
            Ok(())
        })
    }
}

// These helpers are private: all callers hold buffers with no GPU work in flight.
fn read_buffer<T: Pod>(buffer: &Buffer, count: usize) -> Result<Vec<T>> {
    let bytes = count
        .checked_mul(size_of::<T>())
        .context("Metal read size overflow")?;
    ensure!(bytes <= buffer.length(), "Metal read exceeds buffer length");
    let mut values = vec![T::zeroed(); count];
    // Shared storage is CPU-visible; copy after completion and keep no aliased GPU slice.
    unsafe {
        std::ptr::copy_nonoverlapping(
            buffer.contents().as_ptr().cast::<u8>(),
            values.as_mut_ptr().cast::<u8>(),
            bytes,
        );
    }
    Ok(values)
}

fn write_buffer<T: Pod>(buffer: &Buffer, values: &[T]) -> Result<()> {
    let bytes = std::mem::size_of_val(values);
    ensure!(
        bytes <= buffer.length(),
        "Metal write exceeds buffer length"
    );
    unsafe {
        std::ptr::copy_nonoverlapping(
            values.as_ptr().cast::<u8>(),
            buffer.contents().as_ptr().cast::<u8>(),
            bytes,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{geometry, model::Track};

    #[test]
    #[ignore = "requires native Metal access on Apple Silicon"]
    fn laps_require_forward_circuits_and_preserve_motion() -> Result<()> {
        let engine = MetalEngine::new()?;
        let radius = 80.0f32;
        let mut points: Vec<_> = (0..96)
            .map(|i| {
                let angle = i as f32 * std::f32::consts::TAU / 96.0;
                [radius * angle.cos(), radius * angle.sin()]
            })
            .collect();
        points.push(points[0]);
        let track = geometry::prepare(&Track {
            points,
            left: vec![],
            right: vec![],
            width: 16.0,
            spawn: [0.0; 3],
            spawn_distance: 0.0,
            lap_length: 0.0,
        })?;
        let mut config = RunConfig::default();
        config.training.population = 1;
        config.training.elite_count = 1;
        config.training.target_laps = 1;
        let steering = 2.0 * (config.vehicle.step_distance / (2.0 * radius)).asin()
            / config.vehicle.max_heading_change;
        let mut genome = vec![0.0; GENOME_SIZE];
        genome[21] = steering.atanh();
        for target in [1, 5, 100] {
            config.training.target_laps = target;
            let mut reference = None;
            for _ in 0..2 {
                let mut evaluation = engine.begin(
                    &track,
                    &config,
                    crate::model::driving_budget(&config, track.lap_length)?,
                    0,
                    &genome,
                    Some(0),
                )?;
                let initial = engine.advance(&mut evaluation, 0, false)?;
                assert!(
                    initial.telemetry[0].sensors[2] > 20.0,
                    "sensors see through the timing line"
                );
                engine.advance(&mut evaluation, 1, false)?;
                assert_eq!(
                    engine.outcomes(&evaluation)?[0].completed_laps,
                    0,
                    "starting on the line earns no lap"
                );
                while !evaluation.done {
                    let block = engine.advance(&mut evaluation, BLOCK_STEPS, false)?;
                    if target > 1 {
                        let car = engine.outcomes(&evaluation)?.remove(0);
                        for &crossing in car.lap_ends.iter().take(target as usize - 1) {
                            if crossing.step + 1 > block.first_step
                                && crossing.step + 1 < block.first_step + block.frame_count
                            {
                                let index = (crossing.step + 1 - block.first_step) as usize;
                                let before = &block.telemetry[index - 1];
                                let after = &block.telemetry[index];
                                assert!(
                                    ((after.x - before.x).hypot(after.y - before.y)
                                        - config.vehicle.step_distance)
                                        .abs()
                                        < 0.001,
                                    "intermediate crossings preserve the complete movement step"
                                );
                            }
                        }
                    }
                }
                let car = engine.outcomes(&evaluation)?.remove(0);
                assert_eq!(car.status, CarStatus::Finished);
                assert!((90 * target..115 * target).contains(&car.terminal_step));
                assert_eq!(car.fitness, track.lap_length * target as f32);
                assert_eq!(car.completed_laps, target);
                assert_eq!(car.lap_ends.len(), target as usize);
                assert_eq!(car.lap_ends.last().unwrap().step + 1, car.terminal_step);
                assert!(
                    car.lap_ends
                        .iter()
                        .all(|v| (0.0..=1.0).contains(&v.fraction))
                );
                assert!(
                    car.lap_ends
                        .windows(2)
                        .all(|pair| pair[0].ticks() < pair[1].ticks())
                );
                let lap = fastest_lap(std::slice::from_ref(&car)).unwrap();
                let inspection = engine.begin(
                    &track,
                    &config,
                    crate::model::driving_budget(&config, track.lap_length)?,
                    0,
                    &genome,
                    Some(0),
                )?;
                let record = crate::scheduler::record_lap(
                    &engine,
                    inspection,
                    config.vehicle.step_distance,
                    lap,
                    || false,
                )?
                .unwrap();
                assert_eq!(record.genome, genome);
                assert_eq!(record.ghost[0], 0.0);
                assert_eq!(
                    record.ghost[record.ghost.len() - 4],
                    (lap.3.ticks() - lap.2.ticks()) as f32
                );
                assert!(
                    record
                        .ghost
                        .chunks_exact(4)
                        .map(|p| p[0])
                        .collect::<Vec<_>>()
                        .windows(2)
                        .all(|p| p[0] < p[1])
                );
                let result = (
                    car.terminal_step,
                    car.x.to_bits(),
                    car.y.to_bits(),
                    car.lap_ends,
                );
                if let Some(first) = reference {
                    assert_eq!(result, first);
                }
                reference = Some(result);
            }
        }
        // Finishers beat partial progress; sub-step time beats the old car-ID tie-break.
        config.training.population = 4;
        config.training.elite_count = 4;
        let weights: Vec<f32> = (0..GENOME_SIZE)
            .flat_map(|_| [0.0, 1.0, 2.0, 3.0])
            .collect();
        let mut ranked = engine.begin(&track, &config, 1000, 0, &weights, None)?;
        let mut states = [(110, 0.1, 2), (100, 0.9, 2), (100, 0.2, 2), (99, 0.0, 1)].map(
            |(steps, finish_fraction, status)| GpuCarState {
                steps,
                finish_fraction,
                status,
                fitness: if status == 2 { 500.0 } else { 999.0 },
                ..GpuCarState::zeroed()
            },
        );
        write_buffer(&ranked.states, &states)?;
        ranked.done = true;
        assert_eq!(engine.summary(&ranked, &[])?.best_car_id, 2);
        assert_eq!(&engine.evolve(&ranked)?[..4], &[2.0, 1.0, 0.0, 3.0]);
        states[0].steps = 101;
        states[0].finish_fraction = 0.0;
        states[1].steps = 100;
        states[1].finish_fraction = 1.0;
        states[2].steps = 120;
        write_buffer(&ranked.states, &states)?;
        assert_eq!(&engine.evolve(&ranked)?[..4], &[0.0, 1.0, 2.0, 3.0]);
        config.training.population = 1;
        config.training.elite_count = 1;
        // Returning across the line after an already-counted lap cannot count it twice.
        config.training.target_laps = 5;
        let straight = vec![0.0; GENOME_SIZE];
        let mut bounce = engine.begin(&track, &config, 1000, 0, &straight, None)?;
        let mut state = read_buffer::<GpuCarState>(&bounce.states, 1)?[0];
        state.x -= track.finish_direction[0];
        state.y -= track.finish_direction[1];
        state.distance = track.lap_length - 1.0;
        state.previous_progress -= 1.0;
        state.completed_laps = 1;
        write_buffer(&bounce.states, &[state])?;
        write_buffer(
            &bounce.lap_ends,
            &[LapCrossing {
                step: 0,
                fraction: 1.0,
            }],
        )?;
        engine.advance(&mut bounce, 1, false)?;
        assert_eq!(engine.outcomes(&bounce)?[0].completed_laps, 1);
        let mut corner = engine.begin(&track, &config, 1000, 0, &straight, None)?;
        let gate = track.walls.iter().find(|wall| wall.kind == 1).unwrap();
        state.x = gate.a[0];
        state.y = gate.a[1];
        state.distance = track.lap_length;
        state.completed_laps = 0;
        write_buffer(&corner.states, &[state])?;
        engine.advance(&mut corner, 1, false)?;
        assert_eq!(
            engine.outcomes(&corner)?[0].status,
            CarStatus::Crashed,
            "wall contacts win at the timing-line corners"
        );
        let mut reversed = track.clone();
        reversed.spawn[2] += std::f32::consts::PI;
        genome[21] = -genome[21];
        let mut evaluation = engine.begin(&reversed, &config, 300, 0, &genome, None)?;
        while !evaluation.done {
            engine.advance(&mut evaluation, BLOCK_STEPS, false)?;
        }
        let car = engine.outcomes(&evaluation)?.remove(0);
        assert_eq!(
            car.status,
            CarStatus::TimedOut,
            "reverse laps do not finish"
        );
        assert_eq!(car.fitness, 0.0);
        Ok(())
    }
}
