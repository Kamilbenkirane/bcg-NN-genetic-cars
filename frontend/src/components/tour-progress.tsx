import { lapTime } from "@/lib/circuits";
import type {
  CircuitRecord,
  GenerationSummary,
  RunDetail,
  Track,
} from "@/lib/types";

export function CircuitMap({ track }: { track: Track }) {
  const outline = [...track.left, ...track.right.toReversed()];
  const xs = outline.map((point) => point[0]),
    ys = outline.map((point) => point[1]);
  const xmin = Math.min(...xs) - 24,
    ymin = Math.min(...ys) - 24;
  const width = Math.max(...xs) - xmin + 24,
    height = Math.max(...ys) - ymin + 24;
  return (
    <svg
      className="course-map"
      viewBox={`${xmin} ${ymin} ${width} ${height}`}
      aria-hidden="true"
    >
      <g transform={`translate(0 ${2 * ymin + height}) scale(1 -1)`}>
        <path
          d={[track.left, track.right]
            .map((side) => `M${side.map((p) => p.join(",")).join("L")}Z`)
            .join(" ")}
          fillRule="evenodd"
        />
        <circle
          cx={track.spawn[0]}
          cy={track.spawn[1]}
          r={6}
          className="map-start"
        />
      </g>
    </svg>
  );
}

export function TourProgress({
  detail,
  summaries,
  generation,
  onGeneration,
}: {
  detail: RunDetail;
  summaries: GenerationSummary[];
  generation: number | null;
  onGeneration: (value: number) => void;
}) {
  const perCircuit = detail.run.config.training.generationsPerCircuit;
  const population = detail.run.config.training.population;
  const rate = (summary: GenerationSummary | undefined) =>
    summary ? `${((summary.finished / population) * 100).toFixed(1)}%` : "—";
  return (
    <section
      className="tour-progress"
      aria-label="Circuit tour and transfer scores"
    >
      <div className="tour-heading">
        <strong>What carries over?</strong>
        <span>
          Cars completing {detail.run.config.training.targetLaps} laps · arrival
          → after learning
        </span>
      </div>
      <div className="tour-stages">
        {detail.stages.map((stage) => {
          const arrival = summaries.find(
            (s) => s.generation === stage.firstGeneration,
          );
          const latest = summaries
            .filter((s) => s.stageIndex === stage.index)
            .at(-1);
          const finished =
            detail.run.completedGenerations >=
            stage.firstGeneration + perCircuit;
          const watching =
            generation !== null &&
            Math.floor(generation / perCircuit) === stage.index;
          const gain =
            arrival && latest
              ? ((latest.finished - arrival.finished) / population) * 100
              : null;
          return (
            <article
              key={stage.index}
              className={`tour-stage ${watching ? "watching" : ""} ${finished ? "complete" : ""}`}
            >
              <div className="tour-stage-title">
                <span>{String(stage.index + 1).padStart(2, "0")}</span>
                <strong>{stage.circuit.name}</strong>
                <span>{finished ? "✓" : latest ? "●" : "○"}</span>
              </div>
              <CircuitMap track={stage.circuit.track} />
              <div className="transfer-scores">
                <button
                  type="button"
                  disabled={!arrival}
                  onClick={() => onGeneration(stage.firstGeneration)}
                  title="Watch before learning on this circuit"
                >
                  <span>{stage.index === 0 ? "Initial" : "Arrival"}</span>
                  <strong>{rate(arrival)}</strong>
                </button>
                <span>→</span>
                <button
                  type="button"
                  disabled={!latest}
                  onClick={() => latest && onGeneration(latest.generation)}
                  title="Watch the trained population"
                >
                  <span>{finished ? "Trained" : "Now"}</span>
                  <strong>{rate(latest)}</strong>
                </button>
              </div>
              <small>
                {gain === null || !latest
                  ? "Waiting for this circuit"
                  : `${gain >= 0 ? "+" : ""}${gain.toFixed(1)} points · ${latest.generation - stage.firstGeneration + 1}/${perCircuit} gens`}
              </small>
            </article>
          );
        })}
      </div>
    </section>
  );
}

export function RecordBoard({
  detail,
  records,
  summaries,
}: {
  detail: RunDetail;
  records: CircuitRecord[];
  summaries: GenerationSummary[];
}) {
  return (
    <section className="record-board" aria-label="All-time circuit lap records">
      <div className="tour-heading">
        <strong>Champions to beat</strong>
        <span>
          All-time laps for these vehicle settings · saved on this Mac
        </span>
      </div>
      <div className="record-grid">
        {detail.stages.map((stage) => {
          const record = records.find((r) => r.stageIndex === stage.index);
          const best = summaries
            .filter(
              (s) => s.stageIndex === stage.index && s.bestLapSeconds !== null,
            )
            .reduce<number | null>(
              (best, s) =>
                best === null
                  ? s.bestLapSeconds
                  : Math.min(best, s.bestLapSeconds ?? best),
              null,
            );
          const champion = record?.champion;
          const reference = record?.baseline;
          const delta =
            best !== null && reference ? best - reference.lapSeconds : null;
          return (
            <article className="record-card" key={stage.index}>
              <span className="stat-label">{stage.circuit.name}</span>
              <strong className="record-time">
                {lapTime(champion?.lapSeconds)}
              </strong>
              <p>
                {champion
                  ? `${champion.runName} · car ${champion.carId + 1}`
                  : "No completed lap yet"}
              </p>
              {champion && (
                <small>
                  Gen {champion.generation + 1} · lap {champion.lap} ·{" "}
                  {new Date(champion.createdAt).toLocaleDateString()}
                </small>
              )}
              <div className="record-run-best">
                This run <strong>{lapTime(best)}</strong>
              </div>
              {delta !== null && (
                <small className={delta < 0 ? "record-beaten" : ""}>
                  {delta < 0
                    ? "Beat starting champion by"
                    : "From starting champion"}{" "}
                  {Math.abs(delta).toFixed(3)}s
                </small>
              )}
              {champion?.runId === detail.run.id && (
                <span className="record-badge">Record set this run</span>
              )}
            </article>
          );
        })}
      </div>
    </section>
  );
}
