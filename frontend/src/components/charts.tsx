"use client";

import { memo } from "react";
import {
  Area,
  AreaChart,
  CartesianGrid,
  Line,
  LineChart,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { completion } from "@/lib/circuits";
import type { GenerationSummary, RunStage } from "@/lib/types";

export default memo(function Charts({
  summaries,
  stages,
  targetLaps,
}: {
  summaries: GenerationSummary[];
  stages: RunStage[];
  targetLaps: number;
}) {
  const data = summaries.map((summary) => ({
    ...summary,
    label: summary.generation + 1,
    bestDistance: stages[summary.stageIndex]
      ? completion(
          summary.bestFitness,
          stages[summary.stageIndex].circuit.track,
          targetLaps,
        )
      : 0,
    meanDistance: stages[summary.stageIndex]
      ? completion(
          summary.meanFitness,
          stages[summary.stageIndex].circuit.track,
          targetLaps,
        )
      : 0,
  }));
  return (
    <div className="charts-grid">
      <section className="chart-card">
        <div className="chart-heading">
          <div>
            <span className="eyebrow">LEARNING CURVE</span>
            <h2>
              Progress across {targetLaps} {targetLaps === 1 ? "lap" : "laps"}
            </h2>
          </div>
          <div className="chart-key">
            <span>
              <i className="key-dot best" />
              Best
            </span>
            <span>
              <i className="key-dot mean" />
              Mean
            </span>
          </div>
        </div>
        <div className="chart-body">
          {data.length ? (
            <ResponsiveContainer width="100%" height="100%">
              <LineChart
                data={data}
                margin={{ top: 12, right: 16, left: -18, bottom: 4 }}
              >
                {stages.slice(1).map((stage) => (
                  <ReferenceLine
                    key={stage.index}
                    x={stage.firstGeneration + 1}
                    stroke="#a3b89d"
                    strokeDasharray="4 4"
                  />
                ))}
                <CartesianGrid stroke="#ecece5" vertical={false} />
                <XAxis
                  dataKey="label"
                  axisLine={false}
                  tickLine={false}
                  minTickGap={30}
                  tick={{ fontSize: 11, fill: "#838b83" }}
                />
                <YAxis
                  domain={[0, 100]}
                  unit="%"
                  axisLine={false}
                  tickLine={false}
                  tick={{ fontSize: 11, fill: "#838b83" }}
                />
                <Tooltip
                  contentStyle={{
                    border: "1px solid #e3e5dc",
                    borderRadius: 8,
                    fontSize: 12,
                  }}
                  labelFormatter={(label) =>
                    `Generation ${label} · ${stages[summaries.find((s) => s.generation + 1 === Number(label))?.stageIndex ?? 0]?.circuit.name ?? ""}`
                  }
                />
                <Line
                  dataKey="bestDistance"
                  name="Best distance (%)"
                  stroke="#2e6654"
                  strokeWidth={2.5}
                  dot={data.length < 12}
                  isAnimationActive={false}
                />
                <Line
                  dataKey="meanDistance"
                  name="Average distance (%)"
                  stroke="#a3b89d"
                  strokeWidth={2}
                  dot={false}
                  isAnimationActive={false}
                />
              </LineChart>
            </ResponsiveContainer>
          ) : (
            <div className="chart-empty">
              <span className="empty-chart">↗</span>Progress appears after the
              first generation.
            </div>
          )}
        </div>
        <div className="chart-axis-label">
          GENERATION <span>LAP TARGET COMPLETION (%)</span>
        </div>
      </section>
      <section className="chart-card">
        <div className="chart-heading">
          <div>
            <span className="eyebrow">POPULATION OUTCOMES</span>
            <h2>More cars make it through</h2>
          </div>
          <div className="chart-key">
            <span>
              <i className="key-dot finish" />
              Finished
            </span>
            <span>
              <i className="key-dot crash" />
              Crashed
            </span>
            <span>
              <i className="key-dot timeout" />
              Safety stop
            </span>
          </div>
        </div>
        <div className="chart-body">
          {data.length ? (
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart
                data={data}
                margin={{ top: 12, right: 16, left: -18, bottom: 4 }}
              >
                {stages.slice(1).map((stage) => (
                  <ReferenceLine
                    key={stage.index}
                    x={stage.firstGeneration + 1}
                    stroke="#a3b89d"
                    strokeDasharray="4 4"
                  />
                ))}
                <CartesianGrid stroke="#ecece5" vertical={false} />
                <XAxis
                  dataKey="label"
                  axisLine={false}
                  tickLine={false}
                  minTickGap={30}
                  tick={{ fontSize: 11, fill: "#838b83" }}
                />
                <YAxis
                  allowDecimals={false}
                  axisLine={false}
                  tickLine={false}
                  tick={{ fontSize: 11, fill: "#838b83" }}
                />
                <Tooltip
                  contentStyle={{
                    border: "1px solid #e3e5dc",
                    borderRadius: 8,
                    fontSize: 12,
                  }}
                  labelFormatter={(label) =>
                    `Generation ${label} · ${stages[summaries.find((s) => s.generation + 1 === Number(label))?.stageIndex ?? 0]?.circuit.name ?? ""}`
                  }
                />
                <Area
                  dataKey="crashed"
                  name="Crashed"
                  stackId="outcomes"
                  stroke="#bd8c85"
                  fill="#e1c9c2"
                  isAnimationActive={false}
                />
                <Area
                  dataKey="timedOut"
                  name="Safety stop"
                  stackId="outcomes"
                  stroke="#a19bb5"
                  fill="#d1cddd"
                  isAnimationActive={false}
                />
                <Area
                  dataKey="finished"
                  name="Finished"
                  stackId="outcomes"
                  stroke="#ad9342"
                  fill="#e7d896"
                  isAnimationActive={false}
                />
              </AreaChart>
            </ResponsiveContainer>
          ) : (
            <div className="chart-empty">
              <span className="empty-chart">▥</span>Completed generations reveal
              the population.
            </div>
          )}
        </div>
        <div className="chart-axis-label">
          GENERATION <span>NUMBER OF CARS</span>
        </div>
      </section>
    </div>
  );
});
