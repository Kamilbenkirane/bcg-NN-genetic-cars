"use client";

import {
  QueryClient,
  QueryClientProvider,
  skipToken,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, api } from "@/lib/api";
import { DEFAULT_CONFIG, nextTourGeneration } from "@/lib/circuits";
import { RunData } from "@/lib/run-data";
import type { CreateRunRequest, PreviewSnapshot, RunStatus } from "@/lib/types";
import { useRunEvents } from "@/lib/use-run-events";
import Charts from "./charts";
import Configuration from "./configuration";
import RaceViewport from "./race-viewport";
import Statistics from "./statistics";
import { CircuitMap, RecordBoard, TourProgress } from "./tour-progress";

const statuses: Record<RunStatus, string> = {
  queued: "Queued",
  running: "Training",
  stopping: "Stopping",
  stopped: "Stopped",
  completed: "Complete",
  failed: "Failed",
};
const integerParam = (value: string | null): number | null =>
  value !== null && /^\d+$/.test(value) && Number.isSafeInteger(Number(value))
    ? Number(value)
    : null;
const message = (error: unknown) =>
  error instanceof Error
    ? error.message
    : "The request could not be completed.";

function locationState(
  run: string | null,
  generation: number | null,
  car: number | null,
  push = false,
) {
  const params = new URLSearchParams();
  if (run) params.set("run", run);
  if (generation !== null) params.set("generation", String(generation));
  if (car !== null) params.set("car", String(car));
  const url = params.size ? `/?${params}` : "/";
  if (push) window.history.pushState(null, "", url);
  else window.history.replaceState(null, "", url);
}

export default function Workspace() {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            staleTime: Infinity,
            retry: (attempt, error) =>
              !(error instanceof ApiError && error.status < 500) && attempt < 2,
          },
          mutations: { retry: false },
        },
      }),
  );
  return (
    <QueryClientProvider client={client}>
      <ExperimentWorkspace />
    </QueryClientProvider>
  );
}

function ExperimentWorkspace() {
  const client = useQueryClient();
  const [runData] = useState(() => new RunData(client));
  const params = useSearchParams();
  const [runId, setRunId] = useState<string | null>(params.get("run"));
  const [generation, setGeneration] = useState<number | null>(
    integerParam(params.get("generation")),
  );
  const [selected, setSelected] = useState<number | null>(
    integerParam(params.get("car")),
  );
  const [config, setConfig] = useState(DEFAULT_CONFIG);
  const [name, setName] = useState("");
  const [sidebar, setSidebar] = useState<"setup" | "history">("setup");
  const [followLatest, setFollowLatest] = useState(
    params.get("generation") === null,
  );
  const [playbackEnded, setPlaybackEnded] = useState(false);
  const [preview, setPreview] = useState<PreviewSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const submission = useRef<CreateRunRequest | null>(null);
  const viewRevision = useRef(0);
  const viewedId = useRef(runId);
  const [createdRun, setCreatedRun] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const progressPanel = useRef<HTMLDialogElement>(null);
  const workspaceMain = useRef<HTMLElement>(null);
  const [showProgress, setShowProgress] = useState(false);
  const replayStarted = useRef(new Set<string>());
  const receivePreview = useCallback(
    (snapshot: PreviewSnapshot) => setPreview(snapshot),
    [],
  );
  const connection = useRunEvents(runData, receivePreview);

  const health = useQuery({
    queryKey: ["health"],
    queryFn: ({ signal }) => api.health(signal),
    refetchInterval: 15000,
  });
  const runs = useQuery({
    queryKey: ["runs"],
    queryFn: ({ signal }) => runData.fetchRuns(signal),
  });
  const detail = useQuery({
    queryKey: ["run", runId],
    queryFn: runId
      ? ({ signal }) => runData.fetchRun(runId, signal)
      : skipToken,
    enabled: !!runId,
  });
  const summaries = useQuery({
    queryKey: ["generations", runId],
    queryFn: runId
      ? ({ signal }) => runData.fetchGenerations(runId, signal)
      : skipToken,
    enabled: !!runId,
  });
  const circuits = useQuery({
    queryKey: ["circuits", health.data?.engineVersion],
    enabled: health.data?.status === "ready",
    queryFn: ({ signal }) => api.circuits(signal),
  });
  const records = useQuery({
    queryKey: ["records", runId],
    queryFn: runId
      ? ({ signal }) => runData.fetchRecords(runId, signal)
      : skipToken,
    enabled: !!runId,
  });
  const run = detail.data?.run;
  const engineCompatible =
    !run || !health.data || run.engineVersion === health.data.engineVersion;
  const latest = summaries.data?.at(-1)?.generation ?? null;
  const hasGeneration =
    engineCompatible &&
    runId !== null &&
    generation !== null &&
    (run?.completedGenerations ?? 0) > generation;
  const replay = useQuery({
    queryKey: ["replay", runId, generation],
    queryFn: async ({ signal }) => {
      if (runId === null || generation === null)
        throw new Error("Select a completed generation first.");
      const key = `${runId}:${generation}`;
      if (!replayStarted.current.has(key)) {
        replayStarted.current.add(key);
        try {
          return await runData.requestReplay(runId, generation, followLatest);
        } catch (requestError) {
          replayStarted.current.delete(key);
          throw requestError;
        }
      }
      return runData.fetchReplay(runId, generation, signal);
    },
    enabled: hasGeneration,
    refetchInterval: (query) =>
      query.state.data &&
      ["queued", "running"].includes(query.state.data.status)
        ? 1000
        : false,
  });
  const trace = useQuery({
    queryKey: ["trace", runId, generation, selected],
    queryFn:
      runId !== null && generation !== null && selected !== null
        ? ({ signal }) => api.trace(runId, generation, selected, signal)
        : skipToken,
    enabled:
      hasGeneration &&
      selected !== null &&
      selected < (run?.config.training.population ?? 0),
    refetchInterval: (query) => (query.state.data === null ? 500 : false),
  });

  useEffect(() => {
    const id = params.get("run");
    if (viewedId.current !== id) viewRevision.current++;
    viewedId.current = id;
    setRunId(id);
    setGeneration(integerParam(params.get("generation")));
    setSelected(integerParam(params.get("car")));
  }, [params]);
  useEffect(() => {
    if (generation === null && latest !== null) {
      setGeneration(0);
      locationState(runId, 0, selected);
    }
  }, [generation, latest, runId, selected]);
  useEffect(() => {
    if (
      followLatest &&
      playbackEnded &&
      latest !== null &&
      generation !== null &&
      latest > generation
    ) {
      const next = nextTourGeneration(
        generation,
        latest,
        run?.config.training.generationsPerCircuit ?? 50,
      );
      setGeneration(next);
      setPlaybackEnded(false);
      setSelected(null);
      locationState(runId, next, null);
    }
  }, [
    followLatest,
    playbackEnded,
    latest,
    generation,
    runId,
    run?.config.training.generationsPerCircuit,
  ]);

  const chooseRun = useCallback((id: string | null) => {
    viewRevision.current++;
    viewedId.current = id;
    setRunId(id);
    setGeneration(null);
    setSelected(null);
    setFollowLatest(true);
    setPlaybackEnded(false);
    setError(null);
    locationState(id, null, null, true);
  }, []);
  const chooseGeneration = (value: number) => {
    setGeneration(value);
    setSelected(null);
    setFollowLatest(false);
    setPlaybackEnded(false);
    setError(null);
    locationState(runId, value, null, true);
  };
  const chooseCar = useCallback(
    (car: number | null) => {
      setSelected(car);
      locationState(runId, generation, car);
    },
    [runId, generation],
  );
  const ended = useCallback((value: boolean) => setPlaybackEnded(value), []);
  const create = useMutation({
    mutationFn: (submitted: {
      request: CreateRunRequest;
      revision: number;
      hadViewedRun: boolean;
    }) => runData.createRun(submitted.request),
    onSuccess: (data, submitted) => {
      if (submission.current?.requestId === submitted.request.requestId)
        submission.current = null;
      setCreatedRun({ id: data.run.id, name: data.run.name });
      if (
        !submitted.hadViewedRun &&
        viewRevision.current === submitted.revision
      ) {
        chooseRun(data.run.id);
        if (window.matchMedia("(max-width: 760px)").matches)
          requestAnimationFrame(() =>
            workspaceMain.current
              ?.querySelector(".race-card")
              ?.scrollIntoView({ block: "start" }),
          );
      }
    },
    onError: (requestError) => setError(message(requestError)),
  });
  const stop = useMutation({
    mutationFn: (id: string) => runData.stop(id),
    onError: (requestError) => setError(message(requestError)),
  });
  const resume = useMutation({
    mutationFn: (id: string) => runData.resume(id),
    onError: (requestError) => setError(message(requestError)),
  });
  const stageIndex = Math.floor(
    (generation ?? 0) / (run?.config.training.generationsPerCircuit ?? 50),
  );
  const stage = detail.data?.stages[stageIndex];
  const track = engineCompatible ? (stage?.circuit.track ?? null) : null;
  const circuit = stage?.circuit;
  const summary = summaries.data?.find(
    (item) => item.generation === generation,
  );
  const stageRecord = records.data?.find(
    (item) => item.stageIndex === stageIndex,
  );
  const champion = stageRecord?.baseline ?? stageRecord?.champion ?? null;
  const trainingStage =
    detail.data?.stages[
      Math.min(
        detail.data.stages.length - 1,
        Math.floor(
          (run?.completedGenerations ?? 0) /
            (run?.config.training.generationsPerCircuit ?? 50),
        ),
      )
    ];
  const active = runs.data?.find(
    (item) => item.status === "running" || item.status === "stopping",
  );
  const pending =
    runs.data?.filter((item) => item.status === "queued").length ?? 0;
  const completed = run?.completedGenerations ?? 0;
  const failure =
    error ??
    (!engineCompatible
      ? "Replay and resume require the engine and device recorded by this run."
      : null) ??
    (detail.error
      ? message(detail.error)
      : replay.error
        ? message(replay.error)
        : run?.lastError) ??
    (circuits.error ? message(circuits.error) : null);

  return (
    <div className="workspace">
      <aside className="sidebar">
        <a
          className="brand"
          href="/"
          onClick={(event) => {
            event.preventDefault();
            chooseRun(null);
            setSidebar("setup");
          }}
        >
          <span className="brand-symbol">↝</span>
          <span>
            genetic<span className="brand-light">cars</span>
            <small>RACE LAB</small>
          </span>
        </a>
        <div className="sidebar-tabs">
          <button
            type="button"
            className={sidebar === "setup" ? "active" : ""}
            onClick={() => setSidebar("setup")}
          >
            Drive
          </button>
          <button
            type="button"
            className={sidebar === "history" ? "active" : ""}
            onClick={() => setSidebar("history")}
          >
            Experiments <span>{runs.data?.length ?? 0}</span>
          </button>
        </div>
        <div className="sidebar-body">
          {sidebar === "setup" ? (
            <form
              id="experiment-config"
              onSubmit={(event) => {
                event.preventDefault();
                if (event.currentTarget.querySelector("dialog[open]")) return;
                setError(null);
                if (create.isPending || !circuits.data?.length) return;
                submission.current ??= {
                  requestId: crypto.randomUUID(),
                  name:
                    name.trim() ||
                    `Circuit tour · ${config.training.population.toLocaleString()} cars · ${config.training.targetLaps} laps`,
                  config: {
                    ...structuredClone(config),
                    seed:
                      config.seed ||
                      crypto
                        .getRandomValues(new BigUint64Array(1))[0]
                        .toString(),
                  },
                };
                create.mutate({
                  request: submission.current,
                  revision: viewRevision.current,
                  hadViewedRun: runId !== null,
                });
              }}
            >
              {runId && (
                <div className="next-run-heading">
                  <div>
                    <strong>Next run</strong>
                    <p>Changes apply to the next drive.</p>
                  </div>
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => chooseRun(null)}
                  >
                    + New
                  </button>
                </div>
              )}
              <Configuration
                config={config}
                circuitCount={circuits.data?.length ?? 0}
                onChange={(value) => {
                  setConfig(value);
                  submission.current = null;
                }}
                name={name}
                onName={(value) => {
                  setName(value);
                  submission.current = null;
                }}
              />
              <div className="drive-actions">
                <button
                  type="submit"
                  className="start-button"
                  disabled={
                    create.isPending ||
                    health.data?.status !== "ready" ||
                    !circuits.data?.length
                  }
                >
                  {create.isPending
                    ? "Starting…"
                    : active
                      ? "Queue next drive"
                      : "Start driving"}
                  <span>↗</span>
                </button>
                <p className="start-note">
                  {active
                    ? "Starts after the current experiment."
                    : "Watch the cars improve, generation by generation."}
                </p>
                {createdRun && createdRun.id !== runId && (
                  <div className="run-confirmation" role="status">
                    <span>
                      <strong>Drive saved</strong>
                      {createdRun.name}
                    </span>
                    <button
                      type="button"
                      className="text-button"
                      onClick={() => chooseRun(createdRun.id)}
                    >
                      View run →
                    </button>
                  </div>
                )}
              </div>
              {circuits.error && (
                <button
                  type="button"
                  className="text-button"
                  onClick={() => void circuits.refetch()}
                >
                  Retry loading circuits
                </button>
              )}
            </form>
          ) : (
            <div className="history">
              <div className="history-heading">
                <span className="eyebrow">SAVED EXPERIMENTS</span>
                <button
                  type="button"
                  className="text-button"
                  onClick={() => {
                    chooseRun(null);
                    setSidebar("setup");
                  }}
                >
                  + New
                </button>
              </div>
              {runs.isPending && <p className="muted">Loading experiments…</p>}
              {runs.error && (
                <button
                  type="button"
                  className="text-button"
                  onClick={() => void runs.refetch()}
                >
                  Retry loading experiments
                </button>
              )}
              {!runs.data?.length && !runs.isPending && (
                <p className="muted">
                  Your circuit tours will appear here. Start driving to begin.
                </p>
              )}
              {runs.data?.map((item) => (
                <button
                  type="button"
                  className={`history-item ${item.id === runId ? "active" : ""}`}
                  key={item.id}
                  onClick={() => chooseRun(item.id)}
                >
                  <div>
                    <strong>{item.name}</strong>
                    <span className={`status-dot ${item.status}`} />
                  </div>
                  <p>
                    {item.config.training.population.toLocaleString()} cars ·{" "}
                    {item.config.training.targetLaps} laps ·{" "}
                    {item.completedGenerations}/{item.totalGenerations}{" "}
                    generations
                  </p>
                  <footer>
                    <span>{statuses[item.status]}</span>
                    <span>
                      {new Date(item.createdAt).toLocaleDateString(undefined, {
                        month: "short",
                        day: "numeric",
                      })}
                    </span>
                  </footer>
                </button>
              ))}
            </div>
          )}
        </div>
        <div className="engine-status">
          <span className={`engine-indicator ${health.data ? "online" : ""}`} />
          <div>
            <strong>
              {health.data ? "Saved on this Mac" : "Connecting to Race Lab"}
            </strong>
            <span>
              {health.data
                ? "Training continues when you close this tab."
                : "Start the local server to connect."}
            </span>
          </div>
        </div>
      </aside>
      <main className="main-workspace" ref={workspaceMain}>
        <header className="workspace-header">
          <div>
            <span className="eyebrow">
              {run
                ? "YOUR EXPERIMENT"
                : "SIX CIRCUITS. ONE EVOLVING POPULATION."}
            </span>
            <h1>{run?.name ?? "Learn the line. Take it further."}</h1>
            <p>
              {run
                ? `${completed} / ${run.totalGenerations} generations learned${run.status === "running" ? ` · Training on ${trainingStage?.circuit.name}` : ""} · ${run.config.training.targetLaps} laps per car`
                : "Train on each circuit, then discover how much the cars learned for the next one."}
            </p>
          </div>
          <div className="header-actions">
            <span className={`connection ${connection}`}>
              <i />
              {connection === "connected"
                ? "Connected"
                : connection === "connecting"
                  ? "Connecting"
                  : "Reconnecting"}
            </span>
            {pending > 0 && (
              <span className="queue-count">{pending} queued</span>
            )}
            {run && (
              <span className={`status-badge ${run.status}`}>
                {statuses[run.status]}
              </span>
            )}
            {run && ["queued", "running", "stopping"].includes(run.status) && (
              <button
                type="button"
                className="secondary-button"
                disabled={stop.isPending || run.status === "stopping"}
                onClick={() => stop.mutate(run.id)}
              >
                {stop.isPending || run.status === "stopping"
                  ? "Stopping…"
                  : "Stop training"}
              </button>
            )}
            {run && ["stopped", "failed"].includes(run.status) && (
              <button
                type="button"
                className="secondary-button"
                disabled={resume.isPending || !engineCompatible}
                onClick={() => resume.mutate(run.id)}
              >
                {resume.isPending ? "Resuming…" : "Resume training"}
              </button>
            )}
          </div>
        </header>
        {(failure || health.error) && (
          <div className="error-banner" role="alert">
            <span>
              {failure ??
                "The local server is unavailable. Saved experiments will reconnect when it returns."}
            </span>
            {error && (
              <button
                type="button"
                aria-label="Dismiss error"
                onClick={() => setError(null)}
              >
                ×
              </button>
            )}
          </div>
        )}
        {detail.data && (
          <TourProgress
            detail={detail.data}
            summaries={summaries.data ?? []}
            generation={generation}
            onGeneration={chooseGeneration}
          />
        )}
        {run && track && (
          <Statistics
            summary={summary}
            summaries={summaries.data ?? []}
            track={track}
            population={run.config.training.population}
            targetLaps={run.config.training.targetLaps}
          />
        )}
        <section className="race-card" aria-label="Race and playback">
          <div className="race-card-heading">
            {run ? (
              <div className="generation-controls">
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Previous generation"
                  disabled={generation === null || generation === 0}
                  onClick={() =>
                    generation !== null && chooseGeneration(generation - 1)
                  }
                >
                  ←
                </button>
                <label>
                  <span className="sr-only">Playback generation</span>
                  <select
                    value={generation ?? ""}
                    disabled={!summaries.data?.length}
                    onChange={(event) =>
                      chooseGeneration(Number(event.target.value))
                    }
                  >
                    {!summaries.data?.length && (
                      <option value="">Preparing first generation…</option>
                    )}
                    {summaries.data?.map((item) => (
                      <option key={item.generation} value={item.generation}>
                        Gen {item.generation + 1} ·{" "}
                        {detail.data?.stages[item.stageIndex]?.circuit.name}
                      </option>
                    ))}
                  </select>
                </label>
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Next generation"
                  disabled={
                    generation === null ||
                    latest === null ||
                    generation >= latest
                  }
                  onClick={() =>
                    generation !== null && chooseGeneration(generation + 1)
                  }
                >
                  →
                </button>
                <button
                  type="button"
                  className={`follow-latest ${followLatest ? "active" : ""}`}
                  aria-pressed={followLatest}
                  onClick={() => {
                    setFollowLatest((value) => !value);
                  }}
                >
                  Auto-advance
                </button>
              </div>
            ) : (
              <span className="course-preview-label">
                Your next circuit tour{" "}
                <span>
                  Random order · {config.training.targetLaps} laps per car
                </span>
              </span>
            )}
            {run && (
              <button
                type="button"
                className="text-button"
                onClick={() => {
                  setShowProgress(true);
                  progressPanel.current?.showModal();
                }}
              >
                Learning progress ↗
              </button>
            )}
          </div>
          {run && (
            <p className="watching-label">
              Watching {circuit?.name ?? "first circuit"} · generation{" "}
              {(generation ?? 0) + 1}
              {summary && stage && summary.generation === stage.firstGeneration
                ? stage.index === 0
                  ? " · initial population"
                  : " · arrival, before learning here"
                : ""}
            </p>
          )}
          {run ? (
            <RaceViewport
              champion={champion}
              track={track}
              targetLaps={
                run?.config.training.targetLaps ?? config.training.targetLaps
              }
              unavailable={!engineCompatible}
              manifest={hasGeneration ? (replay.data ?? null) : null}
              trace={trace.data ?? null}
              preview={preview}
              selected={selected}
              onSelect={chooseCar}
              onEndedChange={ended}
            />
          ) : (
            <div className="tour-welcome">
              <span className="eyebrow">FROM ONE CIRCUIT TO THE NEXT</span>
              <h2>Can a great driver handle a new road?</h2>
              <p>
                Watch the same cars arrive on each circuit, then learn its
                corners. Every completed lap has a chance to become the
                champion.
              </p>
              <div className="course-grid tour-catalog welcome-circuits">
                {circuits.data?.map((circuit) => (
                  <div
                    className="course-choice"
                    key={circuit.id}
                    title={circuit.description}
                  >
                    <CircuitMap track={circuit.track} />
                    <strong>{circuit.name}</strong>
                  </div>
                ))}
              </div>
              <div className="welcome-steps">
                <span>Train</span>
                <i>→</i>
                <span>Transfer</span>
                <i>→</i>
                <span>Beat the record</span>
              </div>
            </div>
          )}
        </section>
        {detail.data && (
          <RecordBoard
            detail={detail.data}
            records={records.data ?? []}
            summaries={summaries.data ?? []}
          />
        )}
        {records.error && (
          <button
            type="button"
            className="text-button"
            onClick={() => void records.refetch()}
          >
            Retry loading lap records
          </button>
        )}
        <div className="workspace-caption">
          <span>
            {run
              ? "Training is saved automatically. Playback runs at your pace."
              : "Set cars, laps and generations per circuit. The tour is handled for you."}
          </span>
          <span>Drag to pan · Scroll to zoom · Click a car to inspect</span>
        </div>
      </main>
      <dialog
        ref={progressPanel}
        className="progress-dialog"
        aria-labelledby="progress-title"
        onClose={() => setShowProgress(false)}
      >
        <div className="dialog-heading">
          <div>
            <span className="eyebrow">{run?.name}</span>
            <h2 id="progress-title">Learning progress</h2>
          </div>
          <button
            type="button"
            className="close-button"
            aria-label="Close learning progress"
            onClick={() => progressPanel.current?.close()}
          >
            ×
          </button>
        </div>
        <p className="muted">
          {completed} generations learned. Distance shows how far cars reach
          toward the full lap target.
        </p>
        {showProgress && (
          <Charts
            summaries={runId ? (summaries.data ?? []) : []}
            stages={detail.data?.stages ?? []}
            targetLaps={
              run?.config.training.targetLaps ?? config.training.targetLaps
            }
          />
        )}
      </dialog>
    </div>
  );
}
