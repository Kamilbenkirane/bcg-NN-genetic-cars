import { api } from "./api";
import {
  completedLapsAt,
  decodeGhost,
  decodeReplayChunk,
  ghostPoseAt,
  interpolateHeading,
  lapClock,
  ReplayCache,
  sampleWindow,
  terminalTick,
  traceFrameAt,
} from "./replay-buffer";
import type {
  CarStatus,
  CarTrace,
  LapRecord,
  PreviewSnapshot,
  ReplayManifest,
  Track,
} from "./types";

export const CAR_COLORS: Record<CarStatus, string> = {
  running: "#d5fbe6",
  crashed: "#c38a7f",
  finished: "#f0cc71",
  timed_out: "#adabc5",
};
export interface PlaybackState {
  step: number;
  lastStep: number;
  playing: boolean;
  buffering: boolean;
  ended: boolean;
  cameraMode: "overview" | "follow" | "free";
  focusCar: number | null;
  completedLaps: number;
  ghostRecord: LapRecord | null;
  selected: { x: number; y: number; heading: number; status: CarStatus } | null;
}
export const EMPTY_PLAYBACK: PlaybackState = {
  step: 0,
  lastStep: 0,
  playing: false,
  buffering: false,
  ended: false,
  cameraMode: "overview",
  focusCar: null,
  completedLaps: 0,
  ghostRecord: null,
  selected: null,
};
const sensorAngles = [Math.PI / 2, Math.PI / 4, 0, -Math.PI / 4, -Math.PI / 2];
const cache = new ReplayCache();

export function fitCamera(bounds: number[], width: number, height: number) {
  const [xmin, ymin, xmax, ymax] = bounds;
  return {
    x: (xmin + xmax) / 2,
    y: (ymin + ymax) / 2,
    scale: Math.min(
      Math.max(1, width - 100) / Math.max(1, xmax - xmin),
      Math.max(1, height - 130) / Math.max(1, ymax - ymin),
    ),
  };
}

export class RacePlayer {
  private context: CanvasRenderingContext2D;
  private resizeObserver: ResizeObserver;
  private raf = 0;
  private destroyed = false;
  private dirty = true;
  private lastTime = 0;
  private lastPublish = 0;
  private pendingPublish = false;
  private width = 1;
  private height = 1;
  private dpr = 1;
  private track: Track | null = null;
  private road = new Path2D();
  private centerline = new Path2D();
  private tracePath = new Path2D();
  private bounds = [0, 0, 1, 1];
  private camera = { x: 0, y: 0, scale: 1 };
  private manifest: ReplayManifest | null = null;
  private preview: PreviewSnapshot | null = null;
  private trace: CarTrace | null = null;
  private scratch = new Float64Array(0);
  private hasFrame = false;
  private sampledStep = 0;
  private requests = new Map<string, AbortController>();
  private failures = new Set<string>();
  private step = 0;
  private playing = true;
  private buffering = false;
  private ended = false;
  private speed = 1;
  private selected: number | null = null;
  private cameraMode: PlaybackState["cameraMode"] = "overview";
  private bestCar: number | null = null;
  private ghostRecord: LapRecord | null = null;
  private ghost: Float32Array | null = null;
  private overlays = { centerline: false, sensors: false, trajectory: false };
  private drag: { x: number; y: number; moved: boolean } | null = null;
  private cleanup: (() => void)[] = [];

  constructor(
    private canvas: HTMLCanvasElement,
    private onState: (state: PlaybackState) => void,
    private onSelect: (car: number | null) => void,
    private onEndedChange: (ended: boolean) => void,
    private onError: (message: string) => void,
  ) {
    const context = canvas.getContext("2d", { alpha: false });
    if (!context) throw new Error("Canvas2D is unavailable.");
    this.context = context;
    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(canvas);
    this.bindPointer();
    const visibility = () => {
      this.lastTime = 0;
      this.dirty = true;
    };
    document.addEventListener("visibilitychange", visibility);
    this.cleanup.push(() =>
      document.removeEventListener("visibilitychange", visibility),
    );
    this.resize();
    this.raf = requestAnimationFrame(this.frame);
  }

  destroy(): void {
    this.destroyed = true;
    cancelAnimationFrame(this.raf);
    this.resizeObserver.disconnect();
    this.abortRequests();
    for (const dispose of this.cleanup) dispose();
    cache.pin([]);
  }

  setTrack(track: Track | null): void {
    if (this.track === track) return;
    const sameCourse =
      this.track &&
      track &&
      this.track.width === track.width &&
      this.track.points.length === track.points.length &&
      this.track.points.every(
        (p, i) => p[0] === track.points[i][0] && p[1] === track.points[i][1],
      );
    this.track = track;
    if (sameCourse) return;
    this.road = new Path2D();
    this.centerline = new Path2D();
    if (track) {
      const outline = [...track.left, ...track.right.toReversed()];
      for (const boundary of [track.left, track.right.toReversed()]) {
        boundary.forEach(([x, y], i) => {
          if (i) this.road.lineTo(x, y);
          else this.road.moveTo(x, y);
        });
        this.road.closePath();
      }
      track.points.forEach(([x, y], i) => {
        if (i) this.centerline.lineTo(x, y);
        else this.centerline.moveTo(x, y);
      });
      this.bounds = [
        Math.min(...outline.map((p) => p[0])),
        Math.min(...outline.map((p) => p[1])),
        Math.max(...outline.map((p) => p[0])),
        Math.max(...outline.map((p) => p[1])),
      ];
      this.fit();
    }
    this.dirty = true;
  }

  setReplay(manifest: ReplayManifest | null): void {
    const changed =
      this.manifest?.runId !== manifest?.runId ||
      this.manifest?.generation !== manifest?.generation;
    if (changed) {
      this.abortRequests();
      this.ghostRecord = null;
      this.ghost = null;
      this.failures.clear();
      this.step = 0;
      this.ended = false;
      this.onEndedChange(false);
      this.playing = true;
      this.hasFrame = false;
      this.sampledStep = 0;
      this.buffering = !!manifest;
      this.lastTime = 0;
      this.scratch = new Float64Array((manifest?.carCount ?? 0) * 3);
    }
    this.manifest = manifest;
    if (changed) this.bestCar = manifest?.bestCarId ?? null;
    if (manifest?.status === "failed")
      this.onError(manifest.error ?? "Replay failed.");
    this.dirty = true;
    this.ensureWindow();
    this.publish();
  }

  setChampion(record: LapRecord | null): void {
    if (!record || this.ghostRecord || !this.manifest) return;
    this.ghostRecord = record;
    this.loadGhost(record);
    this.publish();
  }

  private loadGhost(record: LapRecord): void {
    const key = `ghost:${record.id}`;
    const controller = new AbortController();
    this.requests.set(key, controller);
    api
      .ghost(record.id, controller.signal)
      .then((buffer) => {
        if (controller.signal.aborted || this.destroyed) return;
        this.ghost = decodeGhost(buffer, record.ghostFrames);
        this.dirty = true;
      })
      .catch((error) => {
        if (!controller.signal.aborted)
          this.onError(
            error instanceof Error
              ? error.message
              : "Could not load champion ghost.",
          );
      })
      .finally(() => {
        if (this.requests.get(key) === controller) this.requests.delete(key);
      });
  }

  setPreview(preview: PreviewSnapshot | null): void {
    this.preview = preview;
    this.dirty = true;
  }
  setTrace(trace: CarTrace | null): void {
    this.trace = trace;
    this.tracePath = new Path2D();
    trace?.frames.forEach((frame, i) => {
      if (i) this.tracePath.lineTo(frame.x, frame.y);
      else this.tracePath.moveTo(frame.x, frame.y);
    });
    this.dirty = true;
  }
  select(car: number | null): void {
    this.selected = car;
    this.dirty = true;
    this.publish();
  }
  setFollowCar(enabled: boolean): void {
    if (!enabled) {
      this.fit();
      return;
    }
    this.cameraMode = "follow";
    if (this.track)
      this.camera.scale = Math.max(
        fitCamera(this.bounds, this.width, this.height).scale,
        (Math.min(this.width, this.height) * 0.12) / this.track.width,
      );
    this.dirty = true;
    this.publish();
  }
  setOverlays(overlays: typeof this.overlays): void {
    this.overlays = overlays;
    this.dirty = true;
  }
  setSpeed(speed: number): void {
    this.speed = speed;
    this.lastTime = 0;
  }
  toggle(): void {
    if (this.ended) {
      this.step = 0;
      this.ended = false;
      this.onEndedChange(false);
    }
    this.playing = !this.playing;
    this.lastTime = 0;
    this.dirty = true;
    this.publish();
  }
  seek(step: number): void {
    this.step = Math.max(0, Math.min(this.lastStep, step));
    this.ended = this.step >= this.lastStep;
    if (this.ended) this.playing = false;
    else this.onEndedChange(false);
    this.lastTime = 0;
    this.abortRequests();
    this.ensureWindow();
    this.dirty = true;
    this.publish();
  }
  retry(): void {
    if (this.ghostRecord && !this.ghost) this.loadGhost(this.ghostRecord);
    this.failures.clear();
    this.ensureWindow();
    this.dirty = true;
  }
  fit(): void {
    this.camera = fitCamera(this.bounds, this.width, this.height);
    this.cameraMode = "overview";
    this.dirty = true;
    this.publish();
  }
  zoom(factor: number): void {
    this.camera.scale = Math.max(
      0.001,
      Math.min(1000, this.camera.scale * factor),
    );
    if (this.cameraMode === "overview") this.cameraMode = "free";
    this.dirty = true;
    this.publish();
  }
  private get lastStep(): number {
    return this.manifest?.lastStep ?? 0;
  }
  private get identity(): string {
    return `${this.manifest?.runId}:${this.manifest?.generation}`;
  }
  private key(index: number): string {
    return `${this.identity}:${index}`;
  }
  private abortRequests(): void {
    for (const controller of this.requests.values()) controller.abort();
    this.requests.clear();
  }

  private ensureWindow(): void {
    const manifest = this.manifest;
    if (!manifest) {
      cache.pin([]);
      return;
    }
    const index = Math.floor(
      sampleWindow(manifest, this.step).before / manifest.framesPerChunk,
    );
    const indices = [index - 1, index, index + 1].filter(
      (i) => i >= 0 && i < manifest.chunks,
    );
    cache.pin(indices.map((i) => this.key(i)));
    for (const i of [index, index + 1, index - 1]) {
      if (i >= 0 && i < manifest.chunks && manifest.availableChunks.includes(i))
        this.loadChunk(i);
    }
  }

  private loadChunk(index: number): void {
    const manifest = this.manifest;
    if (!manifest) return;
    const key = this.key(index);
    if (cache.get(key) || this.requests.has(key) || this.failures.has(key))
      return;
    const controller = new AbortController();
    this.requests.set(key, controller);
    api
      .chunk(manifest.runId, manifest.generation, index, controller.signal)
      .then((buffer) => {
        if (controller.signal.aborted || this.destroyed) return;
        const chunk = decodeReplayChunk(buffer);
        if (
          chunk.carCount !== manifest.carCount ||
          chunk.firstFrame !== index * 32 ||
          chunk.frameCount !==
            Math.min(32, manifest.totalFrames - index * 32) ||
          chunk.firstFrame + chunk.frameCount > manifest.totalFrames
        )
          throw new Error("Replay chunk does not match its manifest.");
        cache.set(key, chunk);
        this.dirty = true;
        this.lastTime = 0;
      })
      .catch((error) => {
        if (controller.signal.aborted) return;
        this.failures.add(key);
        this.onError(
          error instanceof Error ? error.message : "Replay download failed.",
        );
      })
      .finally(() => {
        if (this.requests.get(key) === controller) this.requests.delete(key);
      });
  }

  private frame = (now: number): void => {
    if (this.destroyed) return;
    const elapsed = this.lastTime ? (now - this.lastTime) / 1000 : 0;
    this.lastTime = now;
    if (!document.hidden) {
      if (this.manifest && this.playing && !this.ended && !this.buffering) {
        this.step = Math.min(
          this.lastStep,
          this.step + elapsed * this.manifest.simulationHz * this.speed,
        );
        this.dirty = true;
      }
      if (this.dirty) {
        this.draw();
        this.dirty = false;
        this.pendingPublish = true;
      }
      if (
        now - this.lastPublish >= 100 &&
        (this.playing || this.buffering || this.pendingPublish)
      ) {
        this.publish();
        this.lastPublish = now;
        this.pendingPublish = false;
      }
      if (
        this.manifest &&
        !this.buffering &&
        this.step >= this.lastStep &&
        !this.ended
      ) {
        this.ended = true;
        this.playing = false;
        this.publish();
        this.onEndedChange(true);
      }
    }
    this.raf = requestAnimationFrame(this.frame);
  };

  private resize(): void {
    const rect = this.canvas.getBoundingClientRect();
    this.width = Math.max(1, rect.width);
    this.height = Math.max(1, rect.height);
    this.dpr = window.devicePixelRatio || 1;
    this.canvas.width = Math.round(this.width * this.dpr);
    this.canvas.height = Math.round(this.height * this.dpr);
    if (this.cameraMode === "overview" && this.track) this.fit();
    this.dirty = true;
  }

  private sample(): boolean {
    const manifest = this.manifest;
    if (!manifest) return false;
    const { before, after, fraction } = sampleWindow(manifest, this.step);
    const a = cache.get(this.key(Math.floor(before / 32)));
    const b = cache.get(this.key(Math.floor(after / 32)));
    this.ensureWindow();
    if (!a || !b) return false;
    const aOffset = (before - a.firstFrame) * manifest.carCount * 3;
    const bOffset = (after - b.firstFrame) * manifest.carCount * 3;
    for (let i = 0; i < manifest.carCount * 3; i += 3) {
      const outcome = manifest.outcomes[i / 3];
      if (this.step >= terminalTick(outcome)) {
        this.scratch[i] = outcome.x;
        this.scratch[i + 1] = outcome.y;
        this.scratch[i + 2] = outcome.heading;
        continue;
      }
      const endStep = Math.min(
        after * manifest.sampleStride,
        manifest.lastStep,
        terminalTick(outcome),
      );
      const startStep = before * manifest.sampleStride;
      const carFraction =
        endStep < Math.min(after * manifest.sampleStride, manifest.lastStep)
          ? Math.min(
              1,
              (this.step - startStep) / Math.max(1, endStep - startStep),
            )
          : fraction;
      this.scratch[i] =
        a.poses[aOffset + i] +
        (b.poses[bOffset + i] - a.poses[aOffset + i]) * carFraction;
      this.scratch[i + 1] =
        a.poses[aOffset + i + 1] +
        (b.poses[bOffset + i + 1] - a.poses[aOffset + i + 1]) * carFraction;
      this.scratch[i + 2] = interpolateHeading(
        a.poses[aOffset + i + 2],
        b.poses[bOffset + i + 2],
        carFraction,
      );
    }
    this.hasFrame = true;
    this.sampledStep = this.step;
    return true;
  }

  private status(car: number): CarStatus {
    const outcome = this.manifest?.outcomes[car];
    return outcome && this.sampledStep >= terminalTick(outcome)
      ? outcome.status
      : "running";
  }

  private draw(): void {
    const ctx = this.context;
    const sampled = this.sample();
    this.buffering = !!this.manifest && !sampled;
    const focusCar = this.selected ?? this.bestCar;
    if (
      sampled &&
      this.cameraMode === "follow" &&
      focusCar !== null &&
      focusCar < this.scratch.length / 3
    ) {
      this.camera.x = this.scratch[focusCar * 3];
      this.camera.y = this.scratch[focusCar * 3 + 1];
    }
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.fillStyle = "#17332d";
    ctx.fillRect(0, 0, this.width, this.height);
    const { x, y, scale } = this.camera;
    ctx.setTransform(
      this.dpr * scale,
      0,
      0,
      -this.dpr * scale,
      this.dpr * (this.width / 2 - x * scale),
      this.dpr * (this.height / 2 + y * scale),
    );
    if (!this.track) return;
    ctx.fillStyle = "#354440";
    ctx.fill(this.road, "evenodd");
    ctx.lineWidth = 4 / scale;
    ctx.strokeStyle = "#b5bdac";
    ctx.stroke(this.road);
    ctx.lineWidth = 2 / scale;
    ctx.strokeStyle = "#788d7b";
    ctx.setLineDash([8 / scale, 8 / scale]);
    ctx.stroke(this.road);
    ctx.setLineDash([]);
    if (this.overlays.centerline) {
      ctx.strokeStyle = "#9ba69b66";
      ctx.lineWidth = 1 / scale;
      ctx.setLineDash([7 / scale, 7 / scale]);
      ctx.stroke(this.centerline);
      ctx.setLineDash([]);
    }
    const left = this.track.left,
      right = this.track.right;
    if (left.length > 1 && right.length > 1) {
      ctx.beginPath();
      ctx.moveTo((left[0][0] + left[1][0]) / 2, (left[0][1] + left[1][1]) / 2);
      ctx.lineTo(
        (right[0][0] + right[1][0]) / 2,
        (right[0][1] + right[1][1]) / 2,
      );
      ctx.strokeStyle = "#e6c76e";
      ctx.lineWidth = 5 / scale;
      ctx.stroke();
      ctx.strokeStyle = "#17332d";
      ctx.setLineDash([3 / scale, 3 / scale]);
      ctx.stroke();
      ctx.setLineDash([]);
    }
    const [sx, sy] = this.track.spawn;
    ctx.beginPath();
    ctx.arc(sx, sy, 5 / scale, 0, Math.PI * 2);
    ctx.strokeStyle = "#bbd2c5";
    ctx.lineWidth = 1.5 / scale;
    ctx.stroke();
    if (this.overlays.trajectory && this.currentTrace) {
      ctx.strokeStyle = "#e6c76e88";
      ctx.lineWidth = 1.5 / scale;
      ctx.stroke(this.tracePath);
    }
    if (this.hasFrame && this.manifest) {
      for (const status of [
        "crashed",
        "timed_out",
        "finished",
        "running",
      ] as CarStatus[]) {
        ctx.beginPath();
        for (let car = 0; car < this.manifest.carCount; car++) {
          if (this.status(car) !== status) continue;
          const cx = this.scratch[car * 3],
            cy = this.scratch[car * 3 + 1];
          if (
            Math.abs((cx - x) * scale) > this.width / 2 + 12 ||
            Math.abs((cy - y) * scale) > this.height / 2 + 12
          )
            continue;
          this.carPath(
            cx,
            cy,
            this.scratch[car * 3 + 2],
            Math.max(3.5, Math.min(7, this.track.width * scale * 0.13)) / scale,
          );
        }
        ctx.fillStyle = CAR_COLORS[status];
        ctx.globalAlpha =
          status === "crashed" || status === "timed_out" ? 0.32 : 0.95;
        ctx.fill();
        ctx.globalAlpha = 1;
      }
      if (
        focusCar !== null &&
        focusCar < this.manifest.carCount &&
        (this.selected !== null || this.cameraMode === "follow")
      ) {
        ctx.beginPath();
        this.carPath(
          this.scratch[focusCar * 3],
          this.scratch[focusCar * 3 + 1],
          this.scratch[focusCar * 3 + 2],
          8 / scale,
        );
        ctx.fillStyle = "#f0cc71";
        ctx.fill();
        ctx.strokeStyle = "#17332d";
        ctx.lineWidth = 1.5 / scale;
        ctx.stroke();
      }
      if (
        this.cameraMode === "follow" &&
        focusCar !== null &&
        focusCar < this.manifest.carCount &&
        focusCar !== this.selected
      ) {
        ctx.beginPath();
        ctx.arc(
          this.scratch[focusCar * 3],
          this.scratch[focusCar * 3 + 1],
          12 / scale,
          0,
          Math.PI * 2,
        );
        ctx.strokeStyle = "#f0cc71";
        ctx.lineWidth = 2 / scale;
        ctx.stroke();
      }
      const comparison =
        this.manifest.outcomes[this.selected ?? this.bestCar ?? -1];
      if (this.ghost && comparison) {
        const [gx, gy, gh] = ghostPoseAt(
          this.ghost,
          lapClock(comparison, this.step).elapsed,
        );
        ctx.beginPath();
        this.carPath(gx, gy, gh, 7 / scale);
        ctx.fillStyle = "#69e2eb40";
        ctx.fill();
        ctx.strokeStyle = "#83eff4";
        ctx.lineWidth = 2 / scale;
        ctx.stroke();
      }
      if (this.selected !== null && this.selected < this.manifest.carCount)
        this.drawSelection(scale);
    } else if (
      this.preview &&
      this.preview.runId === this.manifest?.runId &&
      this.preview.generation === this.manifest.generation
    ) {
      for (const car of this.preview.cars) {
        ctx.beginPath();
        ctx.arc(car.x, car.y, 3 / scale, 0, Math.PI * 2);
        ctx.fillStyle = CAR_COLORS[car.status];
        ctx.fill();
      }
    }
    this.drawMap();
  }

  private carPath(x: number, y: number, heading: number, size: number): void {
    const ctx = this.context,
      dx = Math.cos(heading) * size,
      dy = Math.sin(heading) * size;
    ctx.moveTo(x + dx * 1.4 - dy * 0.55, y + dy * 1.4 + dx * 0.55);
    ctx.lineTo(x - dx - dy * 0.7, y - dy + dx * 0.7);
    ctx.lineTo(x - dx + dy * 0.7, y - dy - dx * 0.7);
    ctx.lineTo(x + dx * 1.4 + dy * 0.55, y + dy * 1.4 - dx * 0.55);
    ctx.closePath();
  }

  private drawMap(): void {
    if (!this.track) return;
    const ctx = this.context;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    const screenPoint = (p: number[]) => [
      this.width / 2 + (p[0] - this.camera.x) * this.camera.scale,
      this.height / 2 - (p[1] - this.camera.y) * this.camera.scale,
    ];
    ctx.font = "600 10px system-ui";
    ctx.textAlign = "center";
    for (const [label, point] of [
      ["START / FINISH", this.track.spawn],
    ] as const) {
      const [x, y] = screenPoint(point);
      if (x < 25 || x > this.width - 25 || y < 70 || y > this.height - 25)
        continue;
      ctx.fillStyle = "#10251fe6";
      ctx.fillRect(x - 47, y + 12, 94, 19);
      ctx.fillStyle = "#f0cc71";
      ctx.fillText(label, x, y + 25);
    }
    if (this.cameraMode === "overview" || this.width < 480) return;
    const w = 172,
      h = 120,
      left = this.width - w - 18,
      top = this.height - h - 42;
    ctx.save();
    ctx.beginPath();
    ctx.roundRect(left, top, w, h, 10);
    ctx.fillStyle = "#10251fee";
    ctx.fill();
    ctx.strokeStyle = "#567064";
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.clip();
    const [xmin, ymin, xmax, ymax] = this.bounds;
    const scale = Math.min((w - 24) / (xmax - xmin), (h - 24) / (ymax - ymin));
    const cx = (xmin + xmax) / 2,
      cy = (ymin + ymax) / 2;
    ctx.translate(left + w / 2, top + h / 2);
    ctx.scale(scale, -scale);
    ctx.translate(-cx, -cy);
    ctx.fillStyle = "#8ba898";
    ctx.fill(this.road, "evenodd");
    ctx.strokeStyle = "#e2f2d980";
    ctx.lineWidth = 1 / scale;
    const vw = this.width / this.camera.scale,
      vh = this.height / this.camera.scale;
    ctx.strokeRect(this.camera.x - vw / 2, this.camera.y - vh / 2, vw, vh);
    const target = this.selected ?? this.bestCar;
    if (this.hasFrame && target !== null && target < this.scratch.length / 3) {
      ctx.beginPath();
      ctx.arc(
        this.scratch[target * 3],
        this.scratch[target * 3 + 1],
        4 / scale,
        0,
        Math.PI * 2,
      );
      ctx.fillStyle = "#f0cc71";
      ctx.fill();
    }
    ctx.restore();
  }

  private drawSelection(scale: number): void {
    if (this.selected === null) return;
    const ctx = this.context,
      offset = this.selected * 3;
    const x = this.scratch[offset],
      y = this.scratch[offset + 1],
      heading = this.scratch[offset + 2];
    ctx.beginPath();
    ctx.arc(x, y, 10 / scale, 0, Math.PI * 2);
    ctx.lineWidth = 1.5 / scale;
    ctx.strokeStyle = "#f3d578";
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(x, y);
    ctx.lineTo(
      x + (Math.cos(heading) * 20) / scale,
      y + (Math.sin(heading) * 20) / scale,
    );
    ctx.stroke();
    const reading = traceFrameAt(this.currentTrace, this.sampledStep);
    if (
      !this.overlays.sensors ||
      !reading ||
      this.status(this.selected) !== "running"
    )
      return;
    ctx.strokeStyle = "#e6c76e88";
    ctx.lineWidth = 1 / scale;
    for (let i = 0; i < 5; i++) {
      const angle = reading.heading + sensorAngles[i];
      const ex = reading.x + Math.cos(angle) * reading.sensors[i],
        ey = reading.y + Math.sin(angle) * reading.sensors[i];
      ctx.beginPath();
      ctx.moveTo(reading.x, reading.y);
      ctx.lineTo(ex, ey);
      ctx.stroke();
      ctx.beginPath();
      ctx.arc(ex, ey, 2 / scale, 0, Math.PI * 2);
      ctx.fillStyle = "#e6c76e";
      ctx.fill();
    }
  }

  private get currentTrace(): CarTrace | null {
    return this.trace?.runId === this.manifest?.runId &&
      this.trace?.generation === this.manifest?.generation &&
      this.trace?.carId === this.selected
      ? this.trace
      : null;
  }

  private publish(): void {
    let selected: PlaybackState["selected"] = null;
    if (
      this.selected !== null &&
      this.manifest &&
      this.selected < this.manifest.carCount &&
      this.hasFrame
    ) {
      const i = this.selected * 3;
      selected = {
        x: this.scratch[i],
        y: this.scratch[i + 1],
        heading: this.scratch[i + 2],
        status: this.status(this.selected),
      };
    }
    this.onState({
      step: this.step,
      lastStep: this.lastStep,
      playing: this.playing,
      buffering: this.buffering,
      ended: this.ended,
      cameraMode: this.cameraMode,
      focusCar: this.selected ?? this.bestCar,
      ghostRecord: this.ghostRecord,
      completedLaps: completedLapsAt(
        this.manifest?.outcomes[this.selected ?? this.bestCar ?? -1]?.lapEnds ??
          [],
        this.step,
      ),
      selected,
    });
  }

  private bindPointer(): void {
    const down = (event: PointerEvent) => {
      this.canvas.focus();
      this.canvas.setPointerCapture(event.pointerId);
      this.drag = { x: event.clientX, y: event.clientY, moved: false };
    };
    const move = (event: PointerEvent) => {
      if (!this.drag) return;
      const dx = event.clientX - this.drag.x,
        dy = event.clientY - this.drag.y;
      if (Math.abs(dx) + Math.abs(dy) > 2) this.drag.moved = true;
      this.camera.x -= dx / this.camera.scale;
      this.camera.y += dy / this.camera.scale;
      this.drag.x = event.clientX;
      this.drag.y = event.clientY;
      if (this.drag.moved && this.cameraMode !== "free") {
        this.cameraMode = "free";
        this.publish();
      }
      this.dirty = true;
    };
    const up = (event: PointerEvent) => {
      if (this.drag && !this.drag.moved && this.manifest && !this.buffering) {
        const rect = this.canvas.getBoundingClientRect();
        const px = event.clientX - rect.left - this.width / 2,
          py = event.clientY - rect.top - this.height / 2;
        let nearest: number | null = null,
          distance = 14 * 14;
        for (let car = 0; car < this.manifest.carCount; car++) {
          const dx =
            (this.scratch[car * 3] - this.camera.x) * this.camera.scale - px;
          const dy =
            -(this.scratch[car * 3 + 1] - this.camera.y) * this.camera.scale -
            py;
          if (dx * dx + dy * dy < distance) {
            distance = dx * dx + dy * dy;
            nearest = car;
          }
        }
        this.onSelect(nearest);
      }
      this.drag = null;
    };
    const wheel = (event: WheelEvent) => {
      event.preventDefault();
      const rect = this.canvas.getBoundingClientRect(),
        px = event.clientX - rect.left - this.width / 2,
        py = event.clientY - rect.top - this.height / 2;
      const old = this.camera.scale,
        scale = Math.max(
          0.001,
          Math.min(1000, old * Math.exp(-event.deltaY * 0.001)),
        );
      this.camera.x += px / old - px / scale;
      this.camera.y -= py / old - py / scale;
      this.camera.scale = scale;
      if (this.cameraMode === "overview") this.cameraMode = "free";
      this.publish();
      this.dirty = true;
    };
    const key = (event: KeyboardEvent) => {
      if (event.code === "Space") {
        event.preventDefault();
        this.toggle();
      }
      if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
        event.preventDefault();
        this.seek(
          this.step +
            (event.key === "ArrowLeft" ? -1 : 1) * (event.shiftKey ? 10 : 1),
        );
      }
      if (event.key.toLowerCase() === "f") this.fit();
    };
    this.canvas.addEventListener("pointerdown", down);
    this.canvas.addEventListener("pointermove", move);
    this.canvas.addEventListener("pointerup", up);
    this.canvas.addEventListener("pointercancel", up);
    this.canvas.addEventListener("wheel", wheel, { passive: false });
    this.canvas.addEventListener("keydown", key);
    this.cleanup.push(() => {
      this.canvas.removeEventListener("pointerdown", down);
      this.canvas.removeEventListener("pointermove", move);
      this.canvas.removeEventListener("pointerup", up);
      this.canvas.removeEventListener("pointercancel", up);
      this.canvas.removeEventListener("wheel", wheel);
      this.canvas.removeEventListener("keydown", key);
    });
  }
}
