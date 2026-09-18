import type { QueryClient } from "@tanstack/react-query";
import { api } from "./api";
import type {
  CircuitRecord,
  CreateRunRequest,
  GenerationSummary,
  ReplayManifest,
  ReplayProgress,
  RunDetail,
  RunSummary,
} from "./types";

const byCreated = (a: RunSummary, b: RunSummary) =>
  b.createdAt.localeCompare(a.createdAt);

function mergeGenerations(
  fetched: GenerationSummary[],
  current: GenerationSummary[] = [],
): GenerationSummary[] {
  const generations = new Map(fetched.map((item) => [item.generation, item]));
  for (const item of current) generations.set(item.generation, item);
  return [...generations.values()].sort((a, b) => a.generation - b.generation);
}

/** Reconciles HTTP snapshots with events received while requests are in flight. */
export class RunData {
  private revision = 0;
  private recordRevision = 0;
  private runs = new Map<string, { revision: number; value: RunSummary }>();
  private replays = new Map<
    string,
    { revision: number; value: ReplayProgress }
  >();

  constructor(private client: QueryClient) {}

  applyRun(update: RunSummary): void {
    const previous =
      this.runs.get(update.id)?.value ??
      this.client.getQueryData<RunDetail>(["run", update.id])?.run;
    if (previous?.status !== update.status) this.recordChanged();
    this.runs.set(update.id, { revision: ++this.revision, value: update });
    this.client.setQueryData<RunSummary[]>(["runs"], (current) =>
      [update, ...(current ?? []).filter((item) => item.id !== update.id)].sort(
        byCreated,
      ),
    );
    this.client.setQueryData<RunDetail>(["run", update.id], (current) =>
      current ? { ...current, run: update } : undefined,
    );
  }

  applyGeneration(runId: string, summary: GenerationSummary): void {
    this.client.setQueryData<GenerationSummary[]>(
      ["generations", runId],
      (current) => mergeGenerations([summary], current),
    );
  }

  applyReplay(update: ReplayProgress): void {
    this.replays.set(`${update.runId}:${update.generation}`, {
      revision: ++this.revision,
      value: update,
    });
    this.client.setQueryData<ReplayManifest>(
      ["replay", update.runId, update.generation],
      (current) => (current ? { ...current, ...update } : undefined),
    );
  }

  async fetchRuns(signal?: AbortSignal): Promise<RunSummary[]> {
    const started = this.revision;
    const fetched = await api.runs(signal);
    const runs = new Map(fetched.map((run) => [run.id, run]));
    for (const [id, update] of this.runs) {
      if (update.revision > started) runs.set(id, update.value);
    }
    return [...runs.values()].sort(byCreated);
  }

  async fetchGenerations(
    id: string,
    signal?: AbortSignal,
  ): Promise<GenerationSummary[]> {
    const fetched = await api.generations(id, signal);
    return mergeGenerations(
      fetched,
      this.client.getQueryData<GenerationSummary[]>(["generations", id]),
    );
  }

  fetchRun(id: string, signal?: AbortSignal): Promise<RunDetail> {
    return this.runResponse(() => api.run(id, signal));
  }

  createRun(body: CreateRunRequest): Promise<RunDetail> {
    return this.mutationResponse(() => api.createRun(body));
  }

  stop(id: string): Promise<RunDetail> {
    return this.mutationResponse(() => api.stop(id));
  }

  resume(id: string): Promise<RunDetail> {
    return this.mutationResponse(() => api.resume(id));
  }

  fetchReplay(
    id: string,
    generation: number,
    signal?: AbortSignal,
  ): Promise<ReplayManifest> {
    return this.replayResponse(() => api.replay(id, generation, signal));
  }

  requestReplay(
    id: string,
    generation: number,
    follow: boolean,
  ): Promise<ReplayManifest> {
    return this.replayResponse(() => api.requestReplay(id, generation, follow));
  }

  async fetchRecords(
    id: string,
    signal?: AbortSignal,
  ): Promise<CircuitRecord[]> {
    for (;;) {
      const started = this.recordRevision;
      const records = await api.records(id, signal);
      signal?.throwIfAborted();
      if (started === this.recordRevision) return records;
    }
  }

  recordChanged(): void {
    this.recordRevision++;
    void this.client.invalidateQueries({ queryKey: ["records"] });
  }

  refresh(): void {
    for (const key of ["runs", "run", "generations", "replay", "records"])
      void this.client.invalidateQueries({ queryKey: [key] });
  }

  reset(): void {
    this.runs.clear();
    this.replays.clear();
    this.refresh();
  }

  private async runResponse(
    request: () => Promise<RunDetail>,
  ): Promise<RunDetail> {
    const started = this.revision;
    const fetched = await request();
    const update = this.runs.get(fetched.run.id);
    return update && update.revision > started
      ? { ...fetched, run: update.value }
      : fetched;
  }

  private async mutationResponse(
    request: () => Promise<RunDetail>,
  ): Promise<RunDetail> {
    const detail = await this.runResponse(request);
    this.applyRun(detail.run);
    this.client.setQueryData(["run", detail.run.id], detail);
    return detail;
  }

  private async replayResponse(
    request: () => Promise<ReplayManifest>,
  ): Promise<ReplayManifest> {
    const started = this.revision;
    const fetched = await request();
    const update = this.replays.get(`${fetched.runId}:${fetched.generation}`);
    return update && update.revision > started
      ? { ...fetched, ...update.value }
      : fetched;
  }
}
