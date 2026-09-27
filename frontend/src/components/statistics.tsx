import { completion, lapTime } from "@/lib/circuits";
import type { GenerationSummary, Track } from "@/lib/types";

function Sparkline({
  values,
  color = "#608766",
}: {
  values: number[];
  color?: string;
}) {
  if (values.length < 2) return null;
  const low = Math.min(...values),
    span = Math.max(...values) - low || 1;
  const points = values
    .map(
      (value, index) =>
        `${(index / (values.length - 1)) * 100},${32 - ((value - low) / span) * 27}`,
    )
    .join(" ");
  return (
    <svg className="stat-sparkline" viewBox="0 0 100 36" aria-hidden="true">
      <polygon points={`0,36 ${points} 100,36`} fill={color} opacity="0.08" />
      <polyline
        points={points}
        fill="none"
        stroke={color}
        strokeWidth="1.7"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
}

export default function Statistics({
  summary,
  summaries,
  track,
  population,
  targetLaps,
}: {
  summary: GenerationSummary | undefined;
  summaries: GenerationSummary[];
  track: Track;
  population: number;
  targetLaps: number;
}) {
  const history = summaries.filter(
    (item) =>
      item.stageIndex === summary?.stageIndex &&
      item.generation <= (summary?.generation ?? -1),
  );
  const first = history[0];
  const improvement =
    first && summary
      ? completion(summary.meanFitness, track, targetLaps) -
        completion(first.meanFitness, track, targetLaps)
      : 0;
  const finished = summary ? (summary.finished / population) * 100 : 0;
  const crashed = summary ? (summary.crashed / population) * 100 : 0;
  const percent = (value: number) =>
    `${completion(value, track, targetLaps).toFixed(1)}%`;
  return (
    <section className="statistics" aria-label="Generation statistics">
      <div className="stat-card">
        <span className="stat-label">BEST PROGRESS</span>
        <div className="stat-value">
          {summary ? percent(summary.bestFitness) : "—"}
          <Sparkline values={history.map((item) => item.bestFitness)} />
        </div>
        <p>
          {summary
            ? `${summary.bestFitness.toFixed(1)} fitness · ${targetLaps}-lap target`
            : "The farthest car leads the way"}
        </p>
      </div>
      <div className="stat-card">
        <span className="stat-label">AVERAGE PROGRESS</span>
        <div className="stat-value">
          {summary ? percent(summary.meanFitness) : "—"}
          <Sparkline values={history.map((item) => item.meanFitness)} />
        </div>
        <p>
          {summary
            ? `${summary.meanFitness.toFixed(1)} fitness`
            : "Across the whole population"}
          {history.length > 1 && (
            <span className="stat-change">
              {" "}
              {improvement >= 0 ? "↑" : "↓"} {Math.abs(improvement).toFixed(1)}{" "}
              points since arrival
            </span>
          )}
        </p>
      </div>
      <div className="stat-card">
        <span className="stat-label">
          COMPLETED {targetLaps} {targetLaps === 1 ? "LAP" : "LAPS"}
        </span>
        <div className="stat-value">
          {summary?.finished.toLocaleString() ?? "—"}
          <small>/ {population.toLocaleString()}</small>
          <span className="finish-rate">{finished.toFixed(0)}%</span>
        </div>
        {summary && (
          <div className="outcome-strip" aria-hidden="true">
            <i style={{ width: `${finished}%` }} />
            <i style={{ width: `${crashed}%` }} />
            <i style={{ width: `${Math.max(0, 100 - finished - crashed)}%` }} />
          </div>
        )}
        <p>
          {summary
            ? `${summary.crashed.toLocaleString()} crashed · ${summary.timedOut.toLocaleString()} safety stops`
            : "How many make it all the way"}
        </p>
      </div>
      <div className="stat-card">
        <span className="stat-label">GENERATION TIME</span>
        <div className="stat-value">
          {summary
            ? summary.elapsedMs < 1000
              ? summary.elapsedMs.toFixed(1)
              : (summary.elapsedMs / 1000).toFixed(2)
            : "—"}
          <small>{summary && (summary.elapsedMs < 1000 ? "ms" : "sec")}</small>
          <Sparkline
            values={history.map((item) => item.elapsedMs)}
            color="#ae9460"
          />
        </div>
        <p>
          {population.toLocaleString()} cars evaluated · independent of playback
        </p>
      </div>
      <div className="stat-card lap-stat">
        <span className="stat-label">FASTEST LAP</span>
        <div className="stat-value">{lapTime(summary?.bestLapSeconds)}</div>
        <p>Best completed lap in this generation</p>
      </div>
    </section>
  );
}
