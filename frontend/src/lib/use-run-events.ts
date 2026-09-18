"use client";

import { useEffect, useState } from "react";
import { apiUrl } from "./api";
import type { RunData } from "./run-data";
import type {
  GenerationSummary,
  PreviewSnapshot,
  ReplayProgress,
  RunSummary,
} from "./types";

export function useRunEvents(
  data: RunData,
  onPreview: (preview: PreviewSnapshot) => void,
) {
  const [connection, setConnection] = useState<
    "connecting" | "connected" | "disconnected"
  >("connecting");
  useEffect(() => {
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
      data.applyRun(update);
    };
    const generation = (event: MessageEvent) => {
      if (!accept(event)) return;
      const update = JSON.parse(event.data) as {
        runId: string;
        summary: GenerationSummary;
      };
      data.applyGeneration(update.runId, update.summary);
    };
    const replay = (event: MessageEvent) => {
      if (!accept(event)) return;
      const update = JSON.parse(event.data) as ReplayProgress;
      data.applyReplay(update);
    };
    const preview = (event: MessageEvent) =>
      onPreview(JSON.parse(event.data) as PreviewSnapshot);
    const reset = (event: MessageEvent) => {
      lastApplied = BigInt(event.lastEventId);
      sessionStorage.setItem("race-event-cursor", event.lastEventId);
      data.reset();
    };
    source.addEventListener("record", (event) => {
      if (accept(event as MessageEvent)) data.recordChanged();
    });
    source.addEventListener("run", run);
    source.addEventListener("generation", generation);
    source.addEventListener("replay", replay);
    source.addEventListener("preview", preview);
    source.addEventListener("reset", reset);
    source.onopen = () => {
      setConnection("connected");
      data.refresh();
    };
    source.onerror = () => setConnection("disconnected");
    return () => source.close();
  }, [data, onPreview]);
  return connection;
}
