import type {
  CarTrace,
  Circuit,
  CircuitRecord,
  CreateRunRequest,
  GenerationSummary,
  Health,
  ReplayManifest,
  RunDetail,
  RunSummary,
} from "./types";

export const apiUrl = (path: string) => `/api${path}`;

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(apiUrl(path), init);
  if (!response.ok) {
    const body = await response.text();
    let message = body || `Request failed (${response.status}).`;
    try {
      const value = JSON.parse(body);
      message = value.error ?? message;
    } catch {
      /* The server may return a plain-text error. */
    }
    throw new ApiError(message, response.status);
  }
  if (response.status === 202 && path.endsWith("/trace")) return null as T;
  return response.json() as Promise<T>;
}

async function binary(
  path: string,
  message: string,
  signal: AbortSignal,
): Promise<ArrayBuffer> {
  const response = await fetch(apiUrl(path), { signal });
  if (!response.ok) throw new ApiError(message, response.status);
  return response.arrayBuffer();
}

const post = <T>(path: string, body: unknown) =>
  request<T>(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
export const replayPath = (runId: string, generation: number) =>
  `/runs/${encodeURIComponent(runId)}/generations/${generation}/replay`;

export const api = {
  records: (id: string, signal?: AbortSignal) =>
    request<CircuitRecord[]>(`/records?runId=${encodeURIComponent(id)}`, {
      signal,
    }),
  ghost: (id: string, signal: AbortSignal) =>
    binary(
      `/records/${encodeURIComponent(id)}/ghost`,
      "Could not load champion ghost.",
      signal,
    ),
  health: (signal?: AbortSignal) => request<Health>("/health", { signal }),
  runs: (signal?: AbortSignal) => request<RunSummary[]>("/runs", { signal }),
  run: (id: string, signal?: AbortSignal) =>
    request<RunDetail>(`/runs/${encodeURIComponent(id)}`, { signal }),
  generations: (id: string, signal?: AbortSignal) =>
    request<GenerationSummary[]>(
      `/runs/${encodeURIComponent(id)}/generations`,
      { signal },
    ),
  circuits: (signal?: AbortSignal) =>
    request<Circuit[]>("/circuits", { signal }),
  createRun: (body: CreateRunRequest) => post<RunDetail>("/runs", body),
  stop: (id: string) =>
    post<RunDetail>(`/runs/${encodeURIComponent(id)}/stop`, {}),
  resume: (id: string) =>
    post<RunDetail>(`/runs/${encodeURIComponent(id)}/resume`, {}),
  requestReplay: (id: string, generation: number, follow: boolean) =>
    post<ReplayManifest>(replayPath(id, generation), { follow }),
  replay: (id: string, generation: number, signal?: AbortSignal) =>
    request<ReplayManifest>(replayPath(id, generation), { signal }),
  trace: (id: string, generation: number, car: number, signal?: AbortSignal) =>
    request<CarTrace | null>(
      `/runs/${encodeURIComponent(id)}/generations/${generation}/cars/${car}/trace`,
      { signal },
    ),
  chunk: (id: string, generation: number, index: number, signal: AbortSignal) =>
    binary(
      `${replayPath(id, generation)}/chunks/${index}`,
      `Could not load replay chunk ${index}.`,
      signal,
    ),
};
