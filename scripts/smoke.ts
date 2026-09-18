import assert from "node:assert/strict";
import { Database } from "bun:sqlite";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const dataDir = await mkdtemp(resolve(tmpdir(), "genetic-cars-smoke-"));
const root = resolve(import.meta.dir, "..");
const port = 18501;
const base = `http://127.0.0.1:${port}/api`;
let server: ReturnType<typeof Bun.spawn>;
function start() {
  server = Bun.spawn([resolve(root, "target/release/genetic-cars"), "--port", String(port), "--data-dir", dataDir], {
    cwd: root, stdout: "inherit", stderr: "inherit",
  });
}
async function stop() { server.kill("SIGTERM"); await server.exited; }
async function json(path: string, body?: unknown) {
  const response = await fetch(base + path, body === undefined ? undefined : {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(`${path}: ${response.status} ${await response.text()}`);
  return response.json();
}
async function until<T>(operation: () => Promise<T | false>, milliseconds = 30_000): Promise<T> {
  const deadline = Date.now() + milliseconds;
  while (Date.now() < deadline) {
    const result = await operation();
    if (result !== false) return result;
    await Bun.sleep(50);
  }
  throw new Error("smoke workflow timed out");
}
async function ready() {
  return until(async () => {
    try { return await json("/health"); } catch { return false; }
  });
}
const config = {
  seed: "42",
  vehicle: { stepDistance: 5, sensorRange: 200, maxHeadingChange: Math.PI / 8 },
  training: { population: 50, generationsPerCircuit: 2, targetLaps: 5, eliteCount: 10, mutations: 1, mutationScale: 1 },
};

try {
  start();
  const health = await ready();
  assert.match(health.deviceName, /Apple/);
  const circuits = await json("/circuits");
  assert.equal(circuits.length, 6);
  assert(circuits.every((c: { track: { lapLength: number } }) => c.track.lapLength >= 600));
  const request = { requestId: crypto.randomUUID(), name: "GPU smoke", config };
  const created = await json("/runs", request);
  assert.equal(created.stages.length, 6);
  assert.equal(new Set(created.stages.map((s: { circuit: { id: string } }) => s.circuit.id)).size, 6);
  for (const stage of created.stages) {
    assert.deepEqual(stage.circuit.track.points[0], stage.circuit.track.points.at(-1));
    assert.deepEqual(stage.circuit.track.left[0], stage.circuit.track.left.at(-1));
    assert.deepEqual(stage.circuit.track.right[0], stage.circuit.track.right.at(-1));
  }
  const duplicate = await json("/runs", request);
  assert.equal(created.run.id, duplicate.run.id);
  const runPath = `/runs/${created.run.id}`;
  await until(async () => {
    const detail = await json(runPath);
    assert.notEqual(detail.run.status, "failed", detail.run.lastError);
    return detail.run.status === "completed" && detail;
  });
  const generations = await json(`${runPath}/generations`);
  assert.equal(generations.length, 12);
  assert.deepEqual(generations.map((g: { stageIndex: number }) => g.stageIndex), [0,0,1,1,2,2,3,3,4,4,5,5]);
  const db = new Database(resolve(dataDir, "race-lab.sqlite3"), { readonly: true });
  const checkpoint = (g: number) => (db.query("SELECT population FROM checkpoints WHERE run_id=? AND generation=?").get(created.run.id, g) as { population: Uint8Array }).population;
  for (const boundary of [2,4,6,8,10]) assert.deepEqual(checkpoint(boundary), checkpoint(boundary-1));
  assert.notDeepEqual(checkpoint(0), checkpoint(1));
  db.close();
  assert(generations.every((g: { bestFitness: number; meanFitness: number }) => Number.isFinite(g.bestFitness) && Number.isFinite(g.meanFitness)));
  const replayPath = `${runPath}/generations/2/replay`;
  await json(replayPath, { follow: false });
  const manifest = await until(async () => {
    const value = await json(replayPath);
    assert.notEqual(value.status, "failed", value.error);
    return value.status === "ready" && value;
  });
  assert.equal(manifest.carCount, 50);
  assert.equal(manifest.stageIndex, 1);
  assert.equal(manifest.bestCarId, generations[2].bestCarId);
  assert(manifest.totalFrames <= 10_000);
  assert.equal(manifest.lastStep, Math.max(...manifest.outcomes.map((car: { terminalStep: number }) => car.terminalStep)));
  for (const car of manifest.outcomes) {
    assert.equal(car.completedLaps, car.lapEnds.length);
    assert(car.completedLaps <= config.training.targetLaps);
    if (car.status === "finished") assert.equal(car.completedLaps, config.training.targetLaps);
  }
  assert.equal(manifest.availableChunks.length, manifest.chunks);
  const lastResponse = await fetch(`${base}${replayPath}/chunks/${manifest.chunks - 1}`);
  assert(lastResponse.ok);
  const bytes = await lastResponse.arrayBuffer();
  const header = new DataView(bytes);
  assert.equal(header.getUint32(0, true), 2);
  const firstFrame = header.getUint32(4, true);
  const frames = header.getUint32(8, true);
  const cars = header.getUint32(12, true);
  assert.equal(firstFrame + frames, manifest.totalFrames);
  assert.equal(bytes.byteLength, 16 + frames * cars * 12);
  const poses = new Float32Array(bytes, 16);
  for (const car of manifest.outcomes) {
    const offset = ((frames - 1) * cars + car.carId) * 3;
    assert(Math.abs(poses[offset] - car.x) < 0.001);
    assert(Math.abs(poses[offset + 1] - car.y) < 0.001);
  }
  const trace = await until(async () => {
    const response = await fetch(`${base}${runPath}/generations/2/cars/0/trace`);
    if (response.status === 202) return false;
    assert(response.ok);
    return response.json();
  });
  assert(trace.frames.length > 0 && trace.frames.length <= 10_000);
  assert.equal(trace.frames.at(-1).step, manifest.outcomes[0].terminalStep);
  assert.equal(trace.frames[0].sensors.length, 5);

  const longConfig = { ...config, training: { ...config.training, population: 2000, generationsPerCircuit: 100, targetLaps: 100 } };
  const stoppedRun = await json("/runs", { requestId: crypto.randomUUID(), name: "Resume smoke", config: longConfig });
  assert.equal(stoppedRun.run.config.training.targetLaps, 100);
  assert(stoppedRun.stages.every((s: { maxSteps: number }) => s.maxSteps > 1000 && s.maxSteps <= 100000));
  const stoppedPath = `/runs/${stoppedRun.run.id}`;
  await json(`${stoppedPath}/stop`, {});
  await until(async () => (await json(stoppedPath)).run.status === "stopped");
  const committed = (await json(stoppedPath)).run.completedGenerations;
  await stop();
  start();
  await ready();
  assert.equal((await json(stoppedPath)).run.status, "stopped");
  assert.equal((await json(stoppedPath)).run.config.training.targetLaps, 100);
  assert.equal((await json(stoppedPath)).run.completedGenerations, committed);
  assert.equal((await json(runPath)).run.status, "completed");
  await json(`${stoppedPath}/resume`, {});
  await until(async () => {
    const run = (await json(stoppedPath)).run;
    assert.notEqual(run.status, "failed", run.lastError);
    return run.completedGenerations > committed;
  });
  await json(`${stoppedPath}/stop`, {});
  await until(async () => (await json(stoppedPath)).run.status === "stopped");
  const completeMs = generations.reduce((sum: number, g: { elapsedMs: number }) => sum + g.elapsedMs, 0);
  console.log(`GPU smoke passed on ${health.deviceName}: completion, replay, inspection, stop, restart, resume. Twelve 50-car generations across six circuits: ${completeMs.toFixed(1)} ms engine time.`);
} finally {
  if (server!) await stop();
  await rm(dataDir, { recursive: true, force: true });
}
