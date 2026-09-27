"use client";

import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { apiUrl } from "./api";
import type { GenerationSummary, PreviewSnapshot, RunSummary } from "./types";

export function mergeGenerations(
  fetched: GenerationSummary[],
  current: GenerationSummary[] = [],
): GenerationSummary[] {
  const generations = new Map(fetched.map((item) => [item.generation, item]));
  for (const item of current) generations.set(item.generation, item);
  return [...generations.values()].sort((a, b) => a.generation - b.generation);
}

// Invalidation cancels any in-flight fetch that started before the event, so stale snapshots never win.
export function useRunEvents(onPreview: (preview: PreviewSnapshot) => void) {
  const client = useQueryClient();
  const [connection, setConnection] = useState<
    "connecting" | "connected" | "disconnected"
  >("connecting");
  useEffect(() => {
    const invalidate = (...keys: unknown[][]) => {
      for (const queryKey of keys) void client.invalidateQueries({ queryKey });
    };
    const refresh = () =>
      invalidate(["runs"], ["run"], ["generations"], ["replay"], ["records"]);
    const cursor = sessionStorage.getItem("race-event-cursor");
    let lastApplied = cursor && /^\d+$/.test(cursor) ? BigInt(cursor) : 0n;
    const source = new EventSource(
      apiUrl(`/events${cursor ? `?after=${encodeURIComponent(cursor)}` : ""}`),
    );
    const accept = (event: MessageEvent): boolean => {
      const id = BigInt(event.lastEventId);
      if (id <= lastApplied) return false;
      lastApplied = id;
      sessionStorage.setItem("race-event-cursor", event.lastEventId);
      return true;
    };
    const run = (event: MessageEvent) => {
      if (!accept(event)) return;
      const update = JSON.parse(event.data) as RunSummary;
      invalidate(["runs"], ["run", update.id], ["records"]);
    };
    const generation = (event: MessageEvent) => {
      if (!accept(event)) return;
      const update = JSON.parse(event.data) as {
        runId: string;
        summary: GenerationSummary;
      };
      client.setQueryData<GenerationSummary[]>(
        ["generations", update.runId],
        (current) => mergeGenerations([update.summary], current),
      );
    };
    const preview = (event: MessageEvent) =>
      onPreview(JSON.parse(event.data) as PreviewSnapshot);
    const reset = (event: MessageEvent) => {
      lastApplied = BigInt(event.lastEventId);
      sessionStorage.setItem("race-event-cursor", event.lastEventId);
      refresh();
    };
    source.addEventListener("record", (event) => {
      if (accept(event as MessageEvent)) invalidate(["records"]);
    });
    source.addEventListener("run", run);
    source.addEventListener("generation", generation);
    source.addEventListener("preview", preview);
    source.addEventListener("reset", reset);
    source.onopen = () => {
      setConnection("connected");
      refresh();
    };
    source.onerror = () => setConnection("disconnected");
    return () => source.close();
  }, [client, onPreview]);
  return connection;
}
