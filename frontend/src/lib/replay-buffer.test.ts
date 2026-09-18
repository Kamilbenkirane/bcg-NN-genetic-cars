import { expect, test } from "bun:test";
import {
  completedLapsAt,
  decodeGhost,
  decodeReplayChunk,
  ghostPoseAt,
  interpolateHeading,
  lapClock,
  ReplayCache,
  sampleWindow,
  traceFrameAt,
} from "./replay-buffer";

test("replay buffers validate dimensions, preserve views and evict unpinned data", () => {
  const bytes = new ArrayBuffer(40);
  new Uint32Array(bytes, 0, 4).set([2, 0, 2, 1]);
  new Float32Array(bytes, 16).set([0, 1, 0, 2, 3, Math.PI]);
  const chunk = decodeReplayChunk(bytes);
  expect(chunk.poses.buffer).toBe(bytes);
  expect(chunk.frameCount).toBe(2);
  expect(() => decodeReplayChunk(bytes.slice(0, 39))).toThrow();
  const cache = new ReplayCache(40);
  cache.set("first", chunk);
  cache.pin(["first"]);
  cache.set("second", chunk);
  expect(cache.get("first")).toBe(chunk);
  expect(cache.get("second")).toBeUndefined();
  expect(
    Math.abs(interpolateHeading(Math.PI - 0.1, -Math.PI + 0.1, 0.5) - Math.PI),
  ).toBeLessThan(1e-6);
});

test("sampled playback seeks in simulation steps and keeps exact lap boundaries", () => {
  const manifest = { lastStep: 100000, sampleStride: 11, totalFrames: 9092 };
  expect(sampleWindow(manifest, 99995)).toEqual({
    before: 9090,
    after: 9091,
    fraction: 0.5,
  });
  expect(sampleWindow(manifest, 100000).fraction).toBe(1);
  expect(sampleWindow(manifest, 55)).toEqual({
    before: 5,
    after: 6,
    fraction: 0,
  });
  expect(
    completedLapsAt(
      [100, 201, 305].map((step) => ({ step, fraction: 0 })),
      200.9,
    ),
  ).toBe(1);
  expect(
    completedLapsAt(
      [100, 201, 305].map((step) => ({ step, fraction: 0 })),
      305,
    ),
  ).toBe(3);
  const frames = [0, 11, 22, 25].map((step) => ({
    step,
    x: step,
    y: 0,
    heading: 0,
    steering: 0,
    sensors: [1, 2, 3, 4, 5] as [number, number, number, number, number],
  }));
  const trace = {
    runId: "test",
    generation: 0,
    carId: 0,
    sampleStride: 11,
    frames,
  };
  expect(traceFrameAt(trace, 21)?.step).toBe(11);
  expect(traceFrameAt(trace, 24.5)?.step).toBe(22);
  expect(traceFrameAt(trace, 25)?.step).toBe(25);
});

test("fractional lap clocks and ghost seeking stay aligned through lap boundaries", () => {
  const car = {
    carId: 0,
    fitness: 1,
    status: "finished" as const,
    terminalStep: 21,
    completedLaps: 2,
    lapEnds: [
      { step: 10, fraction: 0.25 },
      { step: 20, fraction: 0.5 },
    ],
    x: 0,
    y: 0,
    heading: 0,
  };
  expect(lapClock(car, 10.2).elapsed).toBe(10.2);
  expect(lapClock(car, 10.25)).toEqual({ elapsed: 0, lastLap: 10.25 });
  expect(lapClock(car, 11).elapsed).toBe(0.75);
  expect(lapClock(car, 100)).toEqual({ elapsed: 10.25, lastLap: 10.25 });
  expect(completedLapsAt(car.lapEnds, 20.49)).toBe(1);
  const bytes = new Float32Array([0, 0, 0, 0, 5, 10, 0, 0, 10.25, 20, 0, 0])
    .buffer;
  const ghost = decodeGhost(bytes, 3);
  expect(ghostPoseAt(ghost, 2.5)).toEqual([5, 0, 0]);
  expect(ghostPoseAt(ghost, 100)).toEqual([20, 0, 0]);
  expect(ghostPoseAt(ghost, 0)).toEqual([0, 0, 0]);
  expect(() => decodeGhost(bytes, 2)).toThrow();
});
