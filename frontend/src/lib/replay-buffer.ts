import type { CarTrace, ReplayManifest } from "./types";

export interface ReplayChunk {
  firstFrame: number;
  frameCount: number;
  carCount: number;
  poses: Float32Array;
  byteLength: number;
}

export function decodeReplayChunk(buffer: ArrayBuffer): ReplayChunk {
  if (buffer.byteLength < 16) throw new Error("Replay chunk is truncated.");
  const header = new DataView(buffer);
  const version = header.getUint32(0, true);
  const firstFrame = header.getUint32(4, true);
  const frameCount = header.getUint32(8, true);
  const carCount = header.getUint32(12, true);
  if (version !== 2)
    throw new Error(
      `Unsupported replay version ${version}. Refresh the application.`,
    );
  if (frameCount < 1 || frameCount > 32 || carCount < 1 || carCount > 2000) {
    throw new Error("Replay chunk dimensions are invalid.");
  }
  const count = frameCount * carCount * 3;
  if (buffer.byteLength !== 16 + count * 4)
    throw new Error("Replay chunk has an incorrect byte length.");
  const poses = new Float32Array(buffer, 16, count);
  for (const value of poses)
    if (!Number.isFinite(value))
      throw new Error("Replay contains a non-finite pose.");
  return {
    firstFrame,
    frameCount,
    carCount,
    poses,
    byteLength: buffer.byteLength,
  };
}

export class ReplayCache {
  private entries = new Map<string, ReplayChunk>();
  private used = 0;
  private pinned = new Set<string>();

  constructor(readonly budget = 64 * 1024 * 1024) {}

  get bytes() {
    return this.used;
  }

  get(key: string): ReplayChunk | undefined {
    const value = this.entries.get(key);
    if (value) {
      this.entries.delete(key);
      this.entries.set(key, value);
    }
    return value;
  }

  set(key: string, chunk: ReplayChunk): void {
    const existing = this.entries.get(key);
    if (existing) this.used -= existing.byteLength;
    this.entries.delete(key);
    this.entries.set(key, chunk);
    this.used += chunk.byteLength;
    this.evict();
  }

  pin(keys: string[]): void {
    this.pinned = new Set(keys);
    this.evict();
  }

  private evict(): void {
    for (const [key, chunk] of this.entries) {
      if (this.used <= this.budget) break;
      if (this.pinned.has(key)) continue;
      this.entries.delete(key);
      this.used -= chunk.byteLength;
    }
  }
}

export function interpolateHeading(
  from: number,
  to: number,
  fraction: number,
): number {
  const difference = Math.atan2(Math.sin(to - from), Math.cos(to - from));
  return from + difference * fraction;
}

export function sampleWindow(
  manifest: Pick<ReplayManifest, "lastStep" | "sampleStride" | "totalFrames">,
  step: number,
) {
  const before = Math.min(
    Math.floor(step / manifest.sampleStride),
    manifest.totalFrames - 1,
  );
  const after = Math.min(before + 1, manifest.totalFrames - 1);
  const start = Math.min(before * manifest.sampleStride, manifest.lastStep);
  const end = Math.min(after * manifest.sampleStride, manifest.lastStep);
  return {
    before,
    after,
    fraction: end === start ? 0 : (step - start) / (end - start),
  };
}

export function traceFrameAt(trace: CarTrace | null, step: number) {
  const last = trace?.frames.at(-1);
  if (!trace || !last) return undefined;
  if (step >= last.step) return last;
  return trace.frames[Math.max(0, Math.floor(step / trace.sampleStride))];
}

export function completedLapsAt(
  lapEnds: import("./types").LapCrossing[],
  step: number,
) {
  return lapEnds.filter((end) => end.step + end.fraction <= step).length;
}

export function terminalTick(car: import("./types").CarOutcome) {
  const end = car.lapEnds.at(-1);
  return car.status === "finished" && end
    ? end.step + end.fraction
    : car.terminalStep;
}

export function lapClock(car: import("./types").CarOutcome, step: number) {
  const ends = car.lapEnds.map((end) => end.step + end.fraction);
  const terminal = terminalTick(car);
  const now = Math.min(step, terminal);
  const completed = ends.filter((end) => end <= now).length;
  const start =
    car.status === "finished" && now >= terminal
      ? (ends.at(-2) ?? 0)
      : (ends[completed - 1] ?? 0);
  return {
    elapsed: Math.max(0, now - start),
    lastLap: completed
      ? ends[completed - 1] - (ends[completed - 2] ?? 0)
      : null,
  };
}

export function decodeGhost(
  buffer: ArrayBuffer,
  frameCount: number,
): Float32Array {
  if (
    frameCount < 2 ||
    frameCount > 100002 ||
    buffer.byteLength !== frameCount * 16
  )
    throw new Error("Champion ghost dimensions are invalid.");
  const poses = new Float32Array(buffer);
  if (poses[0] !== 0)
    throw new Error("Champion ghost must start at the timing line.");
  for (let i = 0; i < poses.length; i++) {
    if (
      !Number.isFinite(poses[i]) ||
      (i >= 4 && i % 4 === 0 && poses[i] <= poses[i - 4])
    )
      throw new Error("Champion ghost contains invalid samples.");
  }
  return poses;
}

export function ghostPoseAt(
  poses: Float32Array,
  ticks: number,
): [number, number, number] {
  let low = 0,
    high = poses.length / 4 - 1;
  ticks = Math.max(0, Math.min(ticks, poses[high * 4]));
  while (low + 1 < high) {
    const middle = (low + high) >>> 1;
    if (poses[middle * 4] <= ticks) low = middle;
    else high = middle;
  }
  const a = low * 4,
    b = high * 4;
  const t = (ticks - poses[a]) / (poses[b] - poses[a]);
  return [
    poses[a + 1] + (poses[b + 1] - poses[a + 1]) * t,
    poses[a + 2] + (poses[b + 2] - poses[a + 2]) * t,
    interpolateHeading(poses[a + 3], poses[b + 3], t),
  ];
}
