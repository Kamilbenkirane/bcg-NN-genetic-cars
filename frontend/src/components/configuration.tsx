"use client";

import { useRef } from "react";
import type { RunConfig } from "@/lib/types";

const degrees = (radians: number) =>
  (Math.fround(radians) * 180) / Math.fround(Math.PI);

function NumberField({
  label,
  value,
  min,
  max,
  step = 1,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number | "any";
  onChange: (value: number) => void;
}) {
  return (
    <label className="number-field">
      <span>{label}</span>
      <input
        type="number"
        min={min}
        max={max}
        step={step}
        required
        value={Number.isFinite(value) ? Number(value.toPrecision(7)) : ""}
        onChange={(event) => onChange(event.target.valueAsNumber)}
      />
    </label>
  );
}

export default function Configuration({
  config,
  onChange,
  circuitCount,
  name,
  onName,
}: {
  config: RunConfig;
  onChange: (config: RunConfig) => void;
  circuitCount: number;
  name: string;
  onName: (name: string) => void;
}) {
  const panel = useRef<HTMLDialogElement>(null);
  const training = (key: keyof RunConfig["training"], value: number) =>
    onChange({ ...config, training: { ...config.training, [key]: value } });
  const vehicle = (key: keyof RunConfig["vehicle"], value: number) =>
    onChange({ ...config, vehicle: { ...config.vehicle, [key]: value } });
  const population = (value: number) =>
    onChange({
      ...config,
      training: {
        ...config.training,
        population: value,
        eliteCount:
          Number.isFinite(value) && value >= 1
            ? Math.min(config.training.eliteCount, value)
            : config.training.eliteCount,
      },
    });
  return (
    <div className="configuration">
      <div className="section-heading">
        <span>01</span>
        <h2>A tour of six circuits</h2>
      </div>
      <p className="tour-intro">
        One population. A fresh order each run. See what carries over to the
        next circuit.
      </p>
      <div className="drive-controls">
        <div className="population-control">
          <div className="section-heading">
            <span>02</span>
            <h2>Cars</h2>
          </div>
          <label className="population-number">
            <span className="sr-only">Number of cars</span>
            <input
              type="number"
              required
              min={1}
              max={2000}
              step={1}
              value={
                Number.isFinite(config.training.population)
                  ? config.training.population
                  : ""
              }
              onChange={(event) => population(event.target.valueAsNumber)}
            />
            <span>cars</span>
          </label>
          <input
            aria-label="Adjust number of cars"
            type="range"
            min={1}
            max={2000}
            step={1}
            value={
              Number.isFinite(config.training.population)
                ? config.training.population
                : 1
            }
            onChange={(event) => population(event.target.valueAsNumber)}
          />
          <div className="population-presets">
            {[50, 200, 500, 2000].map((count) => (
              <button
                type="button"
                key={count}
                aria-pressed={config.training.population === count}
                onClick={() => population(count)}
              >
                {count.toLocaleString()}
              </button>
            ))}
          </div>
        </div>
        <div className="population-control lap-control">
          <div className="section-heading">
            <span>03</span>
            <h2>Laps</h2>
          </div>
          <label className="population-number">
            <span className="sr-only">Number of laps</span>
            <input
              type="number"
              required
              min={1}
              max={100}
              step={1}
              value={
                Number.isFinite(config.training.targetLaps)
                  ? config.training.targetLaps
                  : ""
              }
              onChange={(event) =>
                training("targetLaps", event.target.valueAsNumber)
              }
            />
            <span>laps per car</span>
          </label>
          <div className="population-presets">
            {[1, 5, 10, 25, 100].map((laps) => (
              <button
                type="button"
                key={laps}
                aria-pressed={config.training.targetLaps === laps}
                onClick={() => training("targetLaps", laps)}
              >
                {laps}
              </button>
            ))}
          </div>
        </div>
      </div>
      <div className="tour-generations">
        <NumberField
          label="Generations per circuit"
          value={config.training.generationsPerCircuit}
          min={1}
          max={100}
          onChange={(value) => training("generationsPerCircuit", value)}
        />
        <p>
          {Number.isFinite(config.training.generationsPerCircuit)
            ? config.training.generationsPerCircuit * circuitCount
            : "—"}{" "}
          generations across {circuitCount} circuits
        </p>
      </div>
      <button
        type="button"
        className="advanced-button"
        onClick={() => panel.current?.showModal()}
      >
        Advanced settings <span>↗</span>
      </button>
      <dialog
        ref={panel}
        className="settings-dialog"
        aria-labelledby="settings-title"
        onInvalidCapture={() => panel.current?.showModal()}
      >
        <div className="dialog-heading">
          <div>
            <span className="eyebrow">EXPERIMENT CONTROLS</span>
            <h2 id="settings-title">Advanced settings</h2>
          </div>
          <button
            type="button"
            className="close-button"
            aria-label="Close advanced settings"
            onClick={() => panel.current?.close()}
          >
            ×
          </button>
        </div>
        <p className="muted">
          These settings apply to your next experiment. The driving budget is
          calculated from the circuit and lap target.
        </p>
        <label className="field">
          <span>Experiment name</span>
          <input
            maxLength={80}
            value={name}
            onChange={(event) => onName(event.target.value)}
          />
        </label>
        <label className="field seed-field">
          <span>Run seed</span>
          <input
            placeholder="Random each run"
            inputMode="numeric"
            pattern="[0-9]+"
            maxLength={20}
            value={config.seed}
            onChange={(event) =>
              onChange({ ...config, seed: event.target.value })
            }
          />
        </label>
        <h3>Learning</h3>
        <div className="field-grid">
          <NumberField
            label="Best cars kept"
            value={config.training.eliteCount}
            min={1}
            max={config.training.population}
            onChange={(value) => training("eliteCount", value)}
          />
          <NumberField
            label="Mutations per car"
            value={config.training.mutations}
            min={0}
            max={100}
            onChange={(value) => training("mutations", value)}
          />
          <NumberField
            label="Mutation strength"
            value={config.training.mutationScale}
            min={0}
            max={100}
            step="any"
            onChange={(value) => training("mutationScale", value)}
          />
        </div>
        <h3>Driving & sensors</h3>
        <div className="field-grid">
          <NumberField
            label="Distance per simulation step"
            value={config.vehicle.stepDistance}
            min={Number.MIN_VALUE}
            max={100}
            step="any"
            onChange={(value) => vehicle("stepDistance", value)}
          />
          <NumberField
            label="Sensor range"
            value={config.vehicle.sensorRange}
            min={Number.MIN_VALUE}
            max={10000}
            step="any"
            onChange={(value) => vehicle("sensorRange", value)}
          />
          <NumberField
            label="Maximum steering (°)"
            value={degrees(config.vehicle.maxHeadingChange)}
            min={0}
            max={180}
            step="any"
            onChange={(value) =>
              vehicle("maxHeadingChange", (value * Math.PI) / 180)
            }
          />
        </div>
        <p className="settings-footnote">
          The seed reproduces the circuit order and learning. Leave it blank for
          a fresh run. Experiments are saved automatically.
        </p>
        <button
          type="button"
          className="start-button"
          onClick={() => {
            const inputs = panel.current?.querySelectorAll("input");
            if (
              inputs &&
              Array.from(inputs).every((input) => input.reportValidity())
            )
              panel.current?.close();
          }}
        >
          Done
        </button>
      </dialog>
    </div>
  );
}
