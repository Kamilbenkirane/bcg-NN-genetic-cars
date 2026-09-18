import { expect, test } from "bun:test";
import { QueryClient } from "@tanstack/react-query";
import { api } from "./api";
import { completion, DEFAULT_CONFIG, nextTourGeneration } from "./circuits";
import { fitCamera } from "./race-player";
import { RunData } from "./run-data";
import type { Track } from "./types";

test("progress covers the full lap target and cameras fit wide and tall viewports", () => {
  const track: Track = {
    points: [
      [0, 0],
      [100, 0],
      [100, 135],
      [0, 135],
      [0, 0],
    ],
    left: [
      [8, 8],
      [92, 8],
      [92, 127],
      [8, 127],
      [8, 8],
    ],
    right: [
      [-8, -8],
      [108, -8],
      [108, 143],
      [-8, 143],
      [-8, -8],
    ],
    width: 16,
    spawn: [5, 0, 0],
    spawnDistance: 5,
    lapLength: 470,
  };
  expect(completion(track.lapLength, track, 5)).toBe(20);
  expect(completion(track.lapLength * 100, track, 100)).toBe(100);
  expect(completion(-10, track, 5)).toBe(0);
  for (const [width, height] of [
    [1200, 500],
    [400, 800],
  ]) {
    const bounds = [-20, -200, 500, 140];
    const camera = fitCamera(bounds, width, height);
    expect(camera.x).toBe(240);
    expect(camera.y).toBe(-30);
    expect((bounds[2] - bounds[0]) * camera.scale).toBeLessThanOrEqual(
      width - 99,
    );
    expect((bounds[3] - bounds[1]) * camera.scale).toBeLessThanOrEqual(
      height - 129,
    );
  }
});

test("tour playback visits arrival and trained populations even when training runs ahead", () => {
  let generation = 0;
  const visited = [generation];
  while (generation < 299) {
    generation = nextTourGeneration(generation, 299, 50);
    visited.push(generation);
  }
  expect(visited).toEqual([
    0, 49, 50, 99, 100, 149, 150, 199, 200, 249, 250, 299,
  ]);
  expect(nextTourGeneration(5, 8, 50)).toBe(8);
  expect(nextTourGeneration(0, 5, 1)).toBe(1);
});

test("a run starting during a record fetch refreshes its captured champion", async () => {
  const client = new QueryClient();
  const data = new RunData(client);
  const original = api.records;
  let release = () => {};
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  let calls = 0;
  api.records = async () => {
    if (++calls === 1) {
      await pending;
      return [];
    }
    return [{ stageIndex: 0, champion: null, baseline: null }];
  };
  try {
    const response = data.fetchRecords("test");
    data.applyRun({
      id: "test",
      name: "test",
      createdAt: "2026-09-17",
      status: "running",
      config: DEFAULT_CONFIG,
      completedGenerations: 0,
      totalGenerations: 300,
      lastError: null,
      deviceName: "test",
      engineVersion: "test",
    });
    release();
    expect(await response).toHaveLength(1);
    expect(calls).toBe(2);
  } finally {
    api.records = original;
    client.clear();
  }
});
