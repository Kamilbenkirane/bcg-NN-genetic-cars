import type { RunConfig, Track } from "./types";

export const DEFAULT_CONFIG: RunConfig = {
  seed: "",
  vehicle: { stepDistance: 5, sensorRange: 200, maxHeadingChange: Math.PI / 8 },
  training: {
    population: 500,
    generationsPerCircuit: 50,
    targetLaps: 5,
    eliteCount: 10,
    mutations: 1,
    mutationScale: 1,
  },
};

export const completion = (fitness: number, track: Track, targetLaps: number) =>
  Math.max(0, Math.min(100, (fitness / (track.lapLength * targetLaps)) * 100));

export const lapTime = (seconds: number | null | undefined) =>
  seconds == null ? "—" : `${seconds.toFixed(3)}s`;

export function nextTourGeneration(
  current: number,
  latest: number,
  perCircuit: number,
) {
  for (let start = 0; start <= latest; start += perCircuit) {
    for (const milestone of [start, start + perCircuit - 1]) {
      if (milestone > current && milestone <= latest) return milestone;
    }
  }
  return latest;
}
