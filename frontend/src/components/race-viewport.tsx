"use client";

import { useEffect, useRef, useState } from "react";
import { completion, lapTime } from "@/lib/circuits";
import { EMPTY_PLAYBACK, RacePlayer } from "@/lib/race-player";
import { lapClock, traceFrameAt } from "@/lib/replay-buffer";
import type {
  CarTrace,
  LapRecord,
  PreviewSnapshot,
  ReplayManifest,
  Track,
} from "@/lib/types";

interface Props {
  unavailable?: boolean;
  champion: LapRecord | null;
  track: Track | null;
  targetLaps: number;
  manifest: ReplayManifest | null;
  trace: CarTrace | null;
  preview: PreviewSnapshot | null;
  selected: number | null;
  onSelect: (id: number | null) => void;
  onEndedChange: (ended: boolean) => void;
}

export default function RaceViewport({
  unavailable = false,
  champion,
  track,
  targetLaps,
  manifest,
  trace,
  preview,
  selected,
  onSelect,
  onEndedChange,
}: Props) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const player = useRef<RacePlayer | null>(null);
  const replayKey = useRef("");
  const callbacks = useRef({ onSelect, onEndedChange });
  const [state, setState] = useState(EMPTY_PLAYBACK);
  const [error, setError] = useState<string | null>(null);
  const [speed, setSpeed] = useState(0.5);
  const [overlays, setOverlays] = useState({
    centerline: false,
    sensors: false,
    trajectory: false,
  });

  useEffect(() => {
    callbacks.current = { onSelect, onEndedChange };
  }, [onSelect, onEndedChange]);
  useEffect(() => {
    if (!canvas.current) return;
    const instance = new RacePlayer(
      canvas.current,
      setState,
      (car) => callbacks.current.onSelect(car),
      (ended) => callbacks.current.onEndedChange(ended),
      setError,
    );
    player.current = instance;
    return () => {
      instance.destroy();
      player.current = null;
    };
  }, []);
  useEffect(() => {
    player.current?.setTrack(track);
  }, [track]);
  useEffect(() => {
    const key = `${manifest?.runId}:${manifest?.generation}`;
    if (key !== replayKey.current) {
      replayKey.current = key;
      setError(null);
    }
    player.current?.setReplay(manifest);
    player.current?.setChampion(champion);
  }, [manifest, champion]);
  useEffect(() => {
    player.current?.setPreview(preview);
  }, [preview]);
  useEffect(() => {
    player.current?.setTrace(trace);
  }, [trace]);
  useEffect(() => {
    player.current?.select(selected);
  }, [selected]);
  useEffect(() => {
    player.current?.setSpeed(speed);
  }, [speed]);
  useEffect(() => {
    player.current?.setOverlays(overlays);
  }, [overlays]);
  const comparison = manifest?.outcomes[state.focusCar ?? -1];
  const clock = comparison ? lapClock(comparison, state.step) : null;
  const lapSeconds =
    clock?.lastLap == null
      ? null
      : clock.lastLap / (manifest?.simulationHz ?? 30);
  const lapDelta =
    lapSeconds !== null && state.ghostRecord
      ? lapSeconds - state.ghostRecord.lapSeconds
      : null;
  const terminal = selected === null ? null : manifest?.outcomes[selected];
  const reading = traceFrameAt(trace, state.step);
  const progress = state.lastStep
    ? Math.round((state.step / state.lastStep) * 100)
    : 0;
  const status = state.selected?.status ?? "running";
  return (
    <>
      <div className="lap-comparison">
        <span>
          {state.focusCar === null
            ? "Lap clock"
            : `Car ${state.focusCar + 1} · lap clock`}{" "}
          <strong>
            {lapTime(
              clock ? clock.elapsed / (manifest?.simulationHz ?? 30) : null,
            )}
          </strong>
        </span>
        <span>
          Last lap <strong>{lapTime(lapSeconds)}</strong>
          {lapDelta !== null && (
            <small className={lapDelta < 0 ? "record-beaten" : ""}>
              {" "}
              {lapDelta >= 0 ? "+" : "−"}
              {Math.abs(lapDelta).toFixed(3)}s
            </small>
          )}
        </span>
        <span className="ghost-label">
          ◇ Champion ghost{" "}
          <strong>{lapTime(state.ghostRecord?.lapSeconds)}</strong>
        </span>
      </div>
      <div className="viewport-wrap">
        <canvas
          ref={canvas}
          className="race-canvas"
          tabIndex={0}
          aria-label="Race viewport. Drag to pan, scroll to zoom, click a car to inspect. Space plays or pauses, arrow keys seek, F shows the whole course."
        />
        <div className="scene-tools">
          <fieldset className="camera-controls">
            <legend className="sr-only">Camera controls</legend>
            <button
              type="button"
              aria-pressed={state.cameraMode === "overview"}
              disabled={!track}
              onClick={() => player.current?.fit()}
            >
              ⌗ <span>Whole course</span>
            </button>
            <button
              type="button"
              aria-pressed={state.cameraMode === "follow"}
              disabled={!manifest || state.focusCar === null}
              onClick={() => player.current?.setFollowCar(true)}
            >
              ◎{" "}
              <span>
                {selected === null
                  ? "Follow best car"
                  : `Follow car ${selected + 1}`}
              </span>
            </button>
          </fieldset>
          <details className="display-options">
            <summary>View settings</summary>
            <div>
              {(["centerline", "sensors", "trajectory"] as const).map((key) => (
                <label key={key}>
                  <input
                    type="checkbox"
                    checked={overlays[key]}
                    onChange={(event) =>
                      setOverlays((current) => ({
                        ...current,
                        [key]: event.target.checked,
                      }))
                    }
                  />
                  {key === "centerline"
                    ? "Road centerline"
                    : key === "sensors"
                      ? "Selected car sensors"
                      : "Selected car trail"}
                </label>
              ))}
              <p>Click a car for its trail and sensors.</p>
            </div>
          </details>
        </div>
        <div className="zoom-controls">
          <button
            type="button"
            disabled={!track}
            aria-label="Zoom in"
            onClick={() => player.current?.zoom(1.3)}
          >
            +
          </button>
          <button
            type="button"
            disabled={!track}
            aria-label="Zoom out"
            onClick={() => player.current?.zoom(1 / 1.3)}
          >
            −
          </button>
        </div>
        {!track && (
          <div className="scene-empty">
            <span className="empty-track-icon">↝</span>
            <strong>
              {unavailable
                ? "Replay requires its recorded engine"
                : "Preparing the circuit…"}
            </strong>
          </div>
        )}
        {manifest && (state.buffering || manifest.status === "queued") && (
          <div className="buffer-notice" role="status">
            <span className="spinner" />
            {manifest.status === "queued"
              ? "Preparing replay…"
              : "Loading replay…"}
          </div>
        )}
        {error && (
          <div className="scene-error" role="alert">
            <span>{error}</span>
            <button
              type="button"
              onClick={() => {
                setError(null);
                player.current?.retry();
              }}
            >
              Retry
            </button>
          </div>
        )}
        <div className="scene-bottom">
          <div className="race-legend">
            <span>
              <i className="legend-dot running" />
              Driving
            </span>
            <span>
              <i className="legend-dot finished" />
              Finished
            </span>
            <span>
              <i className="legend-dot crashed" />
              Crashed
            </span>
          </div>
          <span>
            {state.cameraMode === "follow" && state.focusCar !== null
              ? `CAR ${state.focusCar + 1} · ${state.completedLaps} / ${targetLaps} LAPS COMPLETE`
              : state.cameraMode === "free"
                ? "FREE CAMERA"
                : "COURSE OVERVIEW"}
          </span>
        </div>
        {selected !== null && (
          <div className="car-inspector">
            <div className="inspector-title">
              <div>
                <span className="eyebrow">CAR INSPECTION</span>
                <strong>Car {selected + 1}</strong>
              </div>
              <button
                type="button"
                className="close-button"
                aria-label="Close car inspection"
                onClick={() => onSelect(null)}
              >
                ×
              </button>
            </div>
            <div className="inspector-stat">
              <span>Now</span>
              <strong>
                {status === "timed_out"
                  ? "Safety stop"
                  : status === "running"
                    ? "Driving"
                    : status === "finished"
                      ? "Drive complete"
                      : "Crashed"}
              </strong>
            </div>
            <div className="inspector-stat">
              <span>Final progress</span>
              <strong>
                {terminal && track
                  ? `${Math.round(completion(terminal.fitness, track, targetLaps))}%`
                  : "—"}
              </strong>
            </div>
            <div className="inspector-stat">
              <span>Laps completed</span>
              <strong>
                {state.completedLaps} / {targetLaps}
              </strong>
            </div>
            <div className="inspection-overlays">
              {(["sensors", "trajectory"] as const).map((key) => (
                <button
                  type="button"
                  key={key}
                  aria-pressed={overlays[key]}
                  onClick={() =>
                    setOverlays((value) => ({ ...value, [key]: !value[key] }))
                  }
                >
                  {key === "sensors" ? "Sensors" : "Trail"}
                </button>
              ))}
            </div>
            <details className="telemetry">
              <summary>Steering & sensor readings</summary>
              {trace && trace.sampleStride > 1 && reading && (
                <p className="sample-note">
                  Recorded at step {reading.step.toLocaleString()} · every{" "}
                  {trace.sampleStride} steps
                </p>
              )}
              <div className="inspector-stat">
                <span>Heading</span>
                <strong>
                  {state.selected
                    ? `${((state.selected.heading * 180) / Math.PI).toFixed(1)}°`
                    : "—"}
                </strong>
              </div>
              <div className="inspector-stat">
                <span>Steering</span>
                <strong>
                  {reading && status === "running"
                    ? reading.steering.toFixed(3)
                    : "—"}
                </strong>
              </div>
              <div className="sensor-readout">
                <span>Left / front left / front / front right / right</span>
                <strong>
                  {reading && status === "running"
                    ? reading.sensors
                        .map((distance) => distance.toFixed(1))
                        .join(" / ")
                    : trace
                      ? "Car stopped"
                      : "Loading readings…"}
                </strong>
              </div>
            </details>
          </div>
        )}
      </div>
      {manifest ? (
        <div className="playback-bar">
          <button
            type="button"
            className="play-button"
            disabled={manifest.status === "failed"}
            aria-label={
              state.playing && !state.ended ? "Pause playback" : "Play replay"
            }
            onClick={() => player.current?.toggle()}
          >
            {state.playing && !state.ended ? "Ⅱ" : "▶"}
          </button>
          <div className="timeline">
            <label htmlFor="replay-position">
              <span>{state.ended ? "Replay complete" : "Replay"}</span>
              <span>{progress}%</span>
            </label>
            <input
              id="replay-position"
              aria-label="Replay position"
              aria-valuetext={`${progress}% of replay`}
              type="range"
              min={0}
              max={Math.max(1, state.lastStep)}
              step={1}
              value={Math.floor(state.step)}
              disabled={!state.lastStep}
              onChange={(event) =>
                player.current?.seek(Number(event.target.value))
              }
            />
          </div>
          <label className="speed-label">
            <span className="sr-only">Playback speed</span>
            <select
              value={speed}
              onChange={(event) => setSpeed(Number(event.target.value))}
            >
              {[0.25, 0.5, 1, 2, 4, 10, 25].map((value) => (
                <option key={value} value={value}>
                  {value}× speed
                </option>
              ))}
            </select>
          </label>
          <span className="playback-count">
            {manifest.carCount.toLocaleString()} cars
          </span>
        </div>
      ) : (
        <div className="preview-caption">
          <span>
            <i className="legend-dot running" />
            Start / finish
          </span>
          <p>
            Complete {targetLaps} {targetLaps === 1 ? "lap" : "laps"}. Stay on
            the circuit and keep crossing the timing line.
          </p>
        </div>
      )}
    </>
  );
}
