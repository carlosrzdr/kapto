<script lang="ts">
  import Icon from "./Icon.svelte";
  import { onMount, type Snippet, untrack } from "svelte";
  import * as THREE from "three";
  import { OrbitControls } from "three/addons/controls/OrbitControls.js";

  import {
    ApiFailure,
    getCameraFrames,
    getCameraPicture,
    getMesh,
    getReplayMesh,
    getReplayScene,
    getReplayWindow,
    getRobotScene,
    checkLayout,
    removeDefaultLayout,
    saveDefaultLayout,
  } from "./api";
  import type { LayoutInput, LiveViewCounts, RobotRole, ViewLayout, ViewPanel, VocabularyKind } from "./api-types";
  import CameraPanel from "./CameraPanel.svelte";
  import EnvironmentPanel from "./EnvironmentPanel.svelte";
  import LayoutEditor from "./LayoutEditor.svelte";
  import MarkBar from "./MarkBar.svelte";
  import PlotPanel from "./PlotPanel.svelte";
  import Timeline from "./Timeline.svelte";
  import { camerasOf, Decoder, LATE_MS, type Shown, withPicture, withSaid } from "./viewer/cameras";
  import { type Item, MarkLog } from "./viewer/marks";
  import { OffThreadDecoder } from "./viewer/pictures-off-thread";
  import { frameAt, replayCamera, type ReplayCamera, steppedTo } from "./viewer/replay-cameras";
  import {
    applyPlacement,
    browserStore,
    build,
    type EnvironmentStore,
    memoryStore,
    type Placed,
    recolor,
  } from "./viewer/environment";
  import { browserLayouts, editable, laidOut, type LayoutStore, type Offered } from "./viewer/layouts";
  import { decodeMesh } from "./viewer/mesh";
  import { type PlotStream, plotStreams, SampleBuffer } from "./viewer/plots";
  import { openedAt, playedTo, type Started } from "./viewer/play";
  import { applyPoses, buildRobot, type Robot, viewingDistance } from "./viewer/robot";
  import { openingOf, type Played, type Replaying } from "./viewer/replay";
  import { Robots } from "./viewer/robots";
  import { decodeStretch, type Stretch, STREAM_VERSION, type Watched, watchStream } from "./viewer/stream";
  import { sentence, time } from "./format";
  import { shortcut } from "./keys";
  import { token } from "./theme";

  interface Props {
    /** The port the viewer bridge streams the robot's poses on. */
    port: number;
    /** The episode replayed instead of the live robot, if one is. */
    replay?: Replaying | null;
    /** Where the replay is playing, as it moves. */
    onplayed?: (played: Played) => void;
    /** Asked to leave the replay for the live view. */
    onleave?: () => void;
    /** How the view is arranged, once known; the robot alone until then. */
    layout?: ViewLayout | null;
    /** Each role and the value that plays it, for plots that name roles. */
    roles?: RobotRole[];
    /** What a `health` panel shows: the streams' counts. */
    health?: Snippet;
    /** The robot package the view is for, which a layout is kept and saved for; none to arrange without one. */
    packageId?: string | null;
    /** Streams recorded without being decoded: nothing to plot. */
    recordedOnly?: string[];
    /** Where this browser keeps its own layouts. */
    layouts?: LayoutStore;
    /** What became of each camera's frames on their way to the live view, by stream name. */
    tallies?: Record<string, LiveViewCounts | null | undefined>;
  }

  let {
    port,
    replay = null,
    onplayed = () => {},
    onleave = () => {},
    layout = null,
    roles = [],
    health,
    packageId = null,
    recordedOnly = [],
    layouts = browserLayouts(),
    tallies = {},
  }: Props = $props();

  /** The robot alone, until the layout is known. */
  const ROBOT_ONLY: ViewLayout = {
    source: "made",
    columns: [{ width: 1, panels: [{ kind: "robot", height: 1, fields: [], missing: [] }] }],
  };
  const roleFields = $derived(
    Object.fromEntries((replay?.opening.layout ? replay.opening.roles : roles).map((each) => [each.role, each.field])),
  );
  /** Where the robot's view waits while no panel shows it. */
  let home = $state<HTMLDivElement | null>(null);

  /**
   * Shows the robot's view in a panel. It is made once and moved, never
   * made again: its 3D scene draws into the one canvas it was made with.
   */
  function adopt(slot: HTMLElement) {
    const view = container;
    if (view) {
      slot.append(view);
    }
    return () => {
      if (view && view.parentElement === slot) {
        home?.append(view);
      }
    };
  }

  /** What a panel is, in words, for its placeholder. */
  function named(panel: ViewPanel): string {
    switch (panel.kind) {
      case "camera":
        return `The camera ${panel.stream ?? ""}`;
      case "plot":
        return `The plot of ${panel.fields.join(", ")}`;
      default:
        return `The ${panel.kind} panel`;
    }
  }
  /** The replay's plots, made from its opening. */
  let replayPlots = $state.raw<PlotStream[]>([]);
  /** Whether the replay was recorded without a robot, so only its plots are shown. */
  let withoutRobot = $state(false);
  /** Whose robot is drawn: `live`, or the replayed episode's ID; empty while it loads. */
  let robotOf = $state("");
  /** Draws the robot replay `id` was recorded with, or the live one for `null`; set once the scene exists. */
  let drawRobotOf: (id: string | null) => void = () => {};

  let container = $state<HTMLDivElement | null>(null);
  let canvas = $state<HTMLCanvasElement | null>(null);
  /** What the overlay says: "Live" once frames arrive, or why there are none. */
  let said = $state("Loading the robot…");
  /** Live, the median delay to the page, in milliseconds; said beside "Live" only above `LATE_MS`. */
  let delayMs = $state<number | null>(null);
  const late = $derived(said === "Live" && delayMs !== null && delayMs > LATE_MS);
  /** Frames received, for the page's tests. */
  let frames = $state(0);
  /** Each stream's plots, made from the stream's opening message. */
  let plots = $state.raw<PlotStream[]>([]);
  /** Each camera with its newest picture, named by the stream's opening message. */
  let cameras = $state.raw<Shown[]>([]);
  /** Lets go of every camera's decoded picture. */
  function dropPictures() {
    for (const camera of cameras) {
      camera.bitmap?.close();
    }
  }
  /**
   * Paused or looking back at the live view, each camera's picture the
   * bridge kept nearest the moment shown, in place of its newest.
   */
  let keptShown = $state.raw<Shown[]>([]);
  /** The cameras whose kept picture is being asked for: one at a time each. */
  const askingKept = new Set<number>();
  /** The moment each camera's kept picture was last asked for. */
  const keptAskedAt = new Map<number, number>();

  /**
   * Asks the bridge for each camera's picture kept nearest the moment
   * shown, unless asked for that moment already or one is on its way:
   * then, once it has come, for the moment shown by then. Moving the view
   * back shows fewer pictures on a slow link, never late ones.
   */
  function showKept() {
    const at = cursorMs;
    const asking = watched;
    if (replay || !paused || at === null || !asking) {
      return;
    }
    for (const camera of cameras) {
      const stream = camera.stream;
      if (askingKept.has(stream) || keptAskedAt.get(stream) === at) {
        continue;
      }
      askingKept.add(stream);
      keptAskedAt.set(stream, at);
      asking
        .picture(stream, at)
        .then(async (kept) => {
          const bitmap = kept.bytes.length > 0 ? await createImageBitmap(new Blob([kept.bytes], { type: "image/jpeg" })) : null;
          if (replay || !paused) {
            bitmap?.close();
            return;
          }
          const live = cameras.find((each) => each.stream === stream);
          if (!live) {
            bitmap?.close();
            return;
          }
          const before = keptShown.find((each) => each.stream === stream)?.bitmap;
          const shown: Shown = bitmap
            ? { ...live, bitmap, delayMs: null, unshown: null, keptAtMs: kept.receivedMs }
            : { ...live, bitmap: null, delayMs: null, unshown: "No picture of this camera is kept from then", keptAtMs: null };
          keptShown = [...keptShown.filter((each) => each.stream !== stream), shown];
          before?.close();
        })
        // Not shown; asked for again once the moment shown moves.
        .catch(() => {})
        .finally(() => {
          askingKept.delete(stream);
          showKept();
        });
    }
  }

  /** Lets go of the kept pictures, back to each camera's newest. */
  function dropKept() {
    for (const camera of keptShown) {
      camera.bitmap?.close();
    }
    keptShown = [];
    keptAskedAt.clear();
  }


  /** A replay's cameras: each one's frame times once known, and the frame each shows. */
  let replayCameras: (ReplayCamera | undefined)[] = [];
  let replayShown = $state.raw<Shown[]>([]);
  /** The replay's cameras whose frame is being fetched: one at a time each. */
  const fetching = new Set<number>();

  /** Shows `opened`'s cameras, each from its first frame time, once the API gives them. */
  function openCameras(opened: Replaying) {
    closeCameras();
    replayShown = opened.opening.cameras.map((name, stream) => ({
      stream,
      name,
      bitmap: null,
      delayMs: null,
      unshown: null,
      pictures: 0,
      frame: null,
    }));
    opened.opening.cameras.forEach((name, index) => {
      getCameraFrames(opened.id, name)
        .then((frames) => {
          if (replay?.id === opened.id) {
            replayCameras[index] = replayCamera(frames);
            showFrames();
          }
        })
        .catch((error: unknown) => sayUnshown(index, error));
    });
  }

  function closeCameras() {
    for (const camera of replayShown) {
      camera.bitmap?.close();
    }
    replayCameras = [];
    replayShown = [];
  }

  function sayUnshown(index: number, error: unknown) {
    const why = error instanceof Error ? error.message : String(error);
    replayShown = replayShown.map((each) => (each.stream === index ? { ...each, unshown: why } : each));
  }

  /**
   * Fetches each camera's frame at the moment shown, unless it shows
   * already or one is on its way: then, once it has come, the frame at the
   * moment shown by then. Playing, a slow link shows fewer frames, never
   * late ones.
   */
  function showFrames() {
    const opened = replay;
    const at = cursorMs;
    if (!opened || at === null) {
      return;
    }
    replayCameras.forEach((camera, index) => {
      const shown = replayShown[index];
      if (!camera || !shown || fetching.has(index)) {
        return;
      }
      const n = frameAt(camera, at);
      if (n < 0 || shown.frame?.n === n) {
        return;
      }
      fetching.add(index);
      const frame = { n, of: camera.timesMs.length };
      getCameraPicture(opened.id, camera.name, n)
        .then((picture) => createImageBitmap(picture))
        .then((bitmap) => {
          if (replay?.id !== opened.id) {
            bitmap.close();
            return;
          }
          const before = replayShown[index]?.bitmap;
          replayShown = replayShown.map((each) =>
            each.stream === index ? { ...each, bitmap, unshown: null, pictures: each.pictures + 1, frame } : each,
          );
          before?.close();
        })
        .catch((error: unknown) => {
          // Not asked for again until the moment shown moves to another frame.
          replayShown = replayShown.map((each) =>
            each.stream === index ? { ...each, unshown: error instanceof Error ? error.message : String(error), frame } : each,
          );
        })
        .finally(() => {
          fetching.delete(index);
          showFrames();
        });
    });
  }

  /** Moves to camera `index`'s frame `step` frames on from the one shown, and holds there. */
  function stepCamera(index: number, step: number) {
    const camera = replayCameras[index];
    if (!camera || cursorMs === null) {
      return;
    }
    const to = steppedTo(camera, cursorMs, step);
    if (to !== null) {
      hold();
      seek(to);
    }
  }

  /** How wide each camera's picture is shown, in device pixels, by stream. */
  const shownWidths = new Map<number, number>();
  /** How wide each camera's pictures are, learnt from one decoded whole. */
  const cameraWidths = new Map<number, number>();
  /**
   * Decodes each camera's pictures as they come, by the browser's own
   * decoder off the page's main thread, scaled down to the width they are shown at: a 1080p picture
   * shown a few hundred pixels wide costs the page far less decoded that
   * small. Never scaled up: one shown wider than the camera's is decoded
   * whole, and tells the camera's width again.
   */
  const offThread = new OffThreadDecoder();
  const pictures = new Decoder(
    (picture) => {
      const shownWidth = shownWidths.get(picture.stream);
      const cameraWidth = cameraWidths.get(picture.stream);
      if (shownWidth !== undefined && cameraWidth !== undefined && shownWidth < cameraWidth) {
        return offThread.decode(picture.bytes, picture.type, shownWidth);
      }
      return offThread.decode(picture.bytes, picture.type, null).then((bitmap) => {
        cameraWidths.set(picture.stream, bitmap.width);
        return bitmap;
      });
    },
    (picture, bitmap, arrivedMs) => {
      const shown = withPicture(cameras, picture, bitmap, arrivedMs);
      cameras = shown.cameras;
      shown.replaced?.close();
    },
  );
  /** The same plots filled from the bridge's last minutes, while looking back or showing more than the page keeps. */
  let lookedBack = $state.raw<PlotStream[] | null>(null);

  /** This browser's own layout for the package, which comes before every other. */
  let own = $state.raw<LayoutInput | null>(null);
  /** The default as saved or removed from this page, newer than the one the page was given. */
  let latest = $state.raw<ViewLayout | null>(null);
  let arranging = $state(false);
  let arrangeSaid = $state("");
  let arrangeFailed = $state(false);
  /** The own layout as the server checked it, each panel with what it lacks. */
  let ownChecked = $state.raw<ViewLayout | null>(null);
  $effect(() => {
    own = packageId ? layouts.load(packageId) : null;
  });
  $effect(() => {
    const asked = own;
    ownChecked = null;
    if (asked) {
      checkLayout(asked)
        .then((checked) => {
          if (own === asked) {
            ownChecked = checked;
          }
        })
        // Unchecked, it is shown as it is, saying it lacks nothing.
        .catch(() => {});
    }
  });
  /** What the package has that a panel may show, as the page receives it. */
  const offered: Offered = $derived({
    cameras: cameras.map((camera) => camera.name),
    fields: plots.flatMap((stream) => stream.fields.map((field) => `${stream.name}.${field.name}`)),
    roles: roles.map((each) => each.role),
  });
  const liveLayout = $derived(own ? (ownChecked ?? laidOut(own)) : (latest ?? layout ?? ROBOT_ONLY));
  // A replay is arranged as its recording says, if its replay says; else
  // as the live view is.
  const arrangedBy = $derived(replay?.opening.layout ?? liveLayout);
  const columns = $derived(arrangedBy.columns);

  function arrange(next: LayoutInput) {
    own = next;
    arrangeSaid = "";
    if (packageId) {
      layouts.save(packageId, next);
    }
  }

  /** Runs a save or a reset, then follows what the API now gives, forgetting this browser's own. */
  async function settle(call: () => Promise<ViewLayout>, done: string) {
    try {
      latest = await call();
      own = null;
      if (packageId) {
        layouts.clear(packageId);
      }
      arrangeSaid = done;
      arrangeFailed = false;
    } catch (error) {
      arrangeSaid = error instanceof ApiFailure ? `${sentence(error.message)} ${error.remedy}` : String(error);
      arrangeFailed = true;
    }
  }
  /** Whether the view is held at `cursorMs` rather than following now. */
  let paused = $state(false);
  let cursorMs = $state<number | null>(null);
  /** What the camera panel shows: each camera's kept picture while looking back, its newest otherwise. */
  const liveShown = $derived(paused && replay === null ? cameras.map((camera) => keptShown.find((kept) => kept.stream === camera.stream) ?? camera) : cameras);
  /** Whether the moment shown moves forward, playing what the bridge kept. */
  let playing = $state(false);
  let speed = $state(1);
  /** Where the play started, to know where it is now. */
  let started: Started | null = null;
  /** How long a stretch the plots show. */
  let spanMs = $state(30_000);
  /** The oldest and newest moments the bridge keeps, as far as the page knows. */
  let keptMs = $state<[number, number] | null>(null);
  /** How far back the bridge keeps, from its opening message. */
  let historyMs = 0;
  let historySeconds = $state(0);
  /** The stream, to ask it for stretches. */
  let watched: Watched | null = null;
  /** The robot's pose at the moment looked back at, applied on the next frame drawn. */
  let held: Float32Array | null = null;
  /** What happened, as the bridge tells it. */
  const log = new MarkLog();
  let items = $state.raw<Item[]>([]);
  /** Each kind of event's colour, from the vocabulary. */
  let colors = $state.raw<Record<string, string>>({});

  /** What surrounds the robot, kept in this browser. */
  let placed = $state.raw<Placed[]>([]);
  let environmentShown = $state(false);
  let environmentProblem = $state("");
  /** How many of them are drawn, for the page's tests. */
  let drawn = $state(0);
  const store: EnvironmentStore = typeof indexedDB === "undefined" ? memoryStore() : browserStore();
  /** Fits the robot as it stands now in the view; set once the scene exists. */
  let frameNow: () => void = () => {};
  /** How many times the view was framed, for the page's tests. */
  let framings = $state(0);

  /** `F` frames the robot; Space pauses or plays; Esc goes back to now. */
  function viewKey(event: KeyboardEvent) {
    const key = shortcut(event);
    if (key === "f") {
      frameNow();
    } else if (key === "space") {
      event.preventDefault();
      if (!paused) {
        pause();
      } else if (playing) {
        hold();
      } else {
        play();
      }
    } else if (key === "escape" && paused) {
      backToNow();
    }
  }

  /** Draws `placed` around the robot; set once the scene exists. */
  let showEnvironment: (list: Placed[]) => void = () => {};

  function putPlaced(changed: Placed) {
    const index = placed.findIndex((each) => each.id === changed.id);
    placed = index < 0 ? [...placed, changed] : placed.map((each) => (each.id === changed.id ? changed : each));
    showEnvironment(placed);
    store.put(changed).catch((error: unknown) => (environmentProblem = `Not kept in this browser: ${String(error)}`));
  }

  function removePlaced(id: string) {
    placed = placed.filter((each) => each.id !== id);
    showEnvironment(placed);
    store.remove(id).catch((error: unknown) => (environmentProblem = `Not removed from this browser: ${String(error)}`));
  }

  function knowKinds(kinds: VocabularyKind[]) {
    colors = Object.fromEntries(kinds.flatMap((kind) => (kind.color ? [[kind.kind, kind.color]] : [])));
  }

  /** Longer than the page keeps of its own (about a minute): shown from the bridge's last minutes. */
  const PAGE_KEEPS_MS = 60_000;
  /** How many parts a stretch is merged into: about one per pixel of a plot. */
  const POINTS = 800;

  /** Whether a question is on its way, and the one to ask once it is answered. */
  let asking = false;
  let wanted: [number, number] | null = null;

  /** A stretch from the replay, or from the bridge's last minutes. */
  function ask(from: number, to: number, points: number): Promise<Stretch> {
    if (replay) {
      return getReplayWindow(replay.id, from, to, points).then(decodeStretch);
    }
    return watched ? watched.ask(from, to, points) : Promise.reject(new Error("not watching the live stream"));
  }

  /** Fills the plots, and while paused the robot, from the stretch ending at `endMs`. */
  function lookBack(endMs: number) {
    wanted = [endMs - spanMs, endMs];
    if (asking || !(replay || watched)) {
      return;
    }
    const [from, to] = wanted;
    wanted = null;
    asking = true;
    ask(from, to, POINTS)
      .then((stretch) => show(stretch))
      .catch(() => {
        // Shown as it was; the next question may be answered.
      })
      .finally(() => {
        asking = false;
        if (wanted) {
          lookBack(wanted[1]);
        }
      });
  }

  /**
   * Asks the bridge how far back it keeps, as one part spanning everything:
   * what the page has seen since it loaded may be less.
   */
  function measureKept() {
    // One question at a time, as `lookBack` asks: skipped if one is out.
    // A replay's span is its episode's.
    if (asking || !watched || replay) {
      return;
    }
    asking = true;
    watched
      .ask(0, Date.now() + 86_400_000, 1)
      .then((stretch) => {
        if (stretch.keptMs) {
          keptMs = [stretch.keptMs[0], Math.max(stretch.keptMs[1], keptMs?.[1] ?? 0)];
        }
      })
      .catch(() => {
        // Known again at the next try.
      })
      .finally(() => {
        asking = false;
        if (wanted) {
          lookBack(wanted[1]);
        }
      });
  }

  function show(stretch: Stretch) {
    if (stretch.keptMs && !replay) {
      keptMs = stretch.keptMs;
    }
    lookedBack = (replay ? replayPlots : plots).map((stream) => {
      const samples = new SampleBuffer(stream.samples.values, POINTS + 1);
      for (const part of stretch.streams.find((each) => each.stream === stream.index)?.parts ?? []) {
        samples.push(part.newestMs, part.extremes);
      }
      return { ...stream, samples };
    });
    if (paused) {
      held = stretch.poses.at(-1)?.poses ?? held;
    }
  }

  /** Opens `opened` a little before where it asks, or at its start, playing on. */
  function openReplay(opened: Replaying) {
    replayPlots = plotStreams(openingOf(opened.opening));
    const start = opened.opening.start_utc_ns / 1e6;
    const end = opened.opening.end_utc_ns / 1e6;
    keptMs = [start, end];
    drawRobotOf(opened.id);
    lookedBack = null;
    paused = true;
    playing = false;
    // A moment after its end opens it at its start.
    const at = opened.atMs === null || opened.atMs > end ? start : openedAt(opened.atMs, start);
    openCameras(opened);
    seek(at);
    play();
  }

  /** Back to the live robot once the replay is left. */
  function leaveReplay() {
    closeCameras();
    drawRobotOf(null);
    keptMs = null;
    backToNow();
    measureKept();
  }

  // A replay opened, opened again elsewhere, or left.
  let replayed = false;
  $effect(() => {
    const current = replay;
    untrack(() => {
      if (current) {
        replayed = true;
        openReplay(current);
      } else if (replayed) {
        replayed = false;
        leaveReplay();
      }
    });
  });

  // The replay's cameras follow the moment shown.
  $effect(() => {
    const at = cursorMs;
    if (replay && at !== null) {
      untrack(showFrames);
    }
  });

  // Looking back at the live view, so do its cameras.
  $effect(() => {
    const at = cursorMs;
    if (!replay && paused && at !== null) {
      untrack(showKept);
    }
  });

  // Where the replay is, for the bar above it.
  $effect(() => {
    if (replay && cursorMs !== null) {
      const end = keptMs?.[1] ?? Infinity;
      onplayed({ playing, atMs: cursorMs, over: !playing && cursorMs >= end });
    }
  });

  function pause() {
    paused = true;
    cursorMs = keptMs?.[1] ?? null;
    if (cursorMs !== null) {
      lookBack(cursorMs);
    }
  }

  function backToNow() {
    if (replay) {
      onleave();
      return;
    }
    playing = false;
    started = null;
    paused = false;
    cursorMs = null;
    held = null;
    lookedBack = null;
    dropKept();
  }

  function seek(ms: number) {
    if (!paused) {
      paused = true;
    }
    cursorMs = ms;
    if (playing) {
      started = { fromMs: ms, wallMs: performance.now() };
    }
    lookBack(ms);
  }

  /** Plays on from the moment shown. */
  function play() {
    if (!paused || cursorMs === null) {
      return;
    }
    // A replay over plays again from its start.
    if (replay && keptMs && cursorMs >= keptMs[1]) {
      cursorMs = keptMs[0];
      lookBack(cursorMs);
    }
    playing = true;
    started = { fromMs: cursorMs, wallMs: performance.now() };
  }

  /** Holds the moment shown. */
  function hold() {
    playing = false;
    started = null;
  }

  function setSpeed(chosen: number) {
    if (playing && cursorMs !== null) {
      started = { fromMs: cursorMs, wallMs: performance.now() };
    }
    speed = chosen;
  }

  /** Opens what happened at `ms` a little before it, playing on. */
  function watch(ms: number) {
    seek(openedAt(ms, keptMs?.[0] ?? ms));
    play();
  }

  function setSpan(ms: number) {
    spanMs = ms;
    if (!paused && spanMs <= PAGE_KEEPS_MS) {
      lookedBack = null;
    }
    const end = paused ? cursorMs : keptMs?.[1];
    if (end !== null && end !== undefined && (paused || spanMs > PAGE_KEEPS_MS)) {
      lookBack(end);
    }
  }

  /** How many recent delays the shown one is the median of. */
  const DELAYS = 60;

  onMount(() => {
    if (!canvas || !container) {
      return;
    }
    const renderer = new THREE.WebGLRenderer({ canvas, antialias: true });
    renderer.setPixelRatio(window.devicePixelRatio);
    const scene = new THREE.Scene();
    scene.background = new THREE.Color(token("background", "#0b0c0d"));
    scene.add(new THREE.HemisphereLight(0xffffff, 0x404040, 2));
    const sun = new THREE.DirectionalLight(0xffffff, 1.5);
    sun.position.set(2, -3, 4);
    scene.add(sun);
    // The floor: robot descriptions are Z up, three.js's grid lies in XZ.
    const grid = new THREE.GridHelper(4, 40, token("control-line", "#6b7378"), token("line", "#2a2e31"));
    grid.rotation.x = Math.PI / 2;
    scene.add(grid);
    const camera = new THREE.PerspectiveCamera(45, 1, 0.01, 100);
    camera.up.set(0, 0, 1);
    camera.position.set(1.6, -1.6, 1.2);
    const controls = new OrbitControls(camera, canvas);
    controls.target.set(0, 0, 0.4);
    controls.update();

    // What surrounds the robot: each thing built once, then only moved,
    // unless its shape or file changes.
    const surroundings = new THREE.Group();
    scene.add(surroundings);
    const built = new Map<string, { object: THREE.Object3D; from: Placed }>();
    showEnvironment = (list) => {
      const wanted = new Set(list.map((each) => each.id));
      for (const [id, { object }] of built) {
        if (!wanted.has(id)) {
          surroundings.remove(object);
          built.delete(id);
        }
      }
      for (const each of list) {
        const kept = built.get(each.id);
        if (kept && kept.from.shape === each.shape && kept.from.file === each.file) {
          applyPlacement(kept.object, each);
          recolor(kept.object, each);
          kept.from = each;
          continue;
        }
        build(each)
          .then((object) => {
            const now = placed.find((one) => one.id === each.id);
            if (!now) {
              return;
            }
            const old = built.get(each.id);
            if (old) {
              surroundings.remove(old.object);
            }
            applyPlacement(object, now);
            surroundings.add(object);
            built.set(each.id, { object, from: now });
            drawn = built.size;
            dirty = true;
          })
          .catch((error: unknown) => (environmentProblem = `${each.name} couldn't be shown: ${String(error)}`));
      }
      drawn = built.size;
      dirty = true;
    };
    store
      .load()
      .then((kept) => {
        placed = kept;
        showEnvironment(placed);
      })
      .catch((error: unknown) => (environmentProblem = `What this browser kept couldn't be read: ${String(error)}`));

    let robot: Robot | null = null;
    let pending: Float32Array | null = null;
    let framed = false;
    let dirty = true;
    const delays: number[] = [];
    let closed = false;
    controls.addEventListener("change", () => (dirty = true));

    /** Puts `sphere` in view, from the front right and a little above. */
    const frameSphere = (sphere: THREE.Sphere) => {
      if (sphere.radius <= 0 || !Number.isFinite(sphere.radius)) {
        return;
      }
      controls.target.copy(sphere.center);
      const distance = viewingDistance(sphere.radius, camera.fov, camera.aspect);
      camera.position.copy(sphere.center).add(new THREE.Vector3(1, -1, 0.7).normalize().multiplyScalar(distance));
      controls.update();
      framed = true;
      framings += 1;
      dirty = true;
    };
    /** Puts the whole robot in view as it stands now. */
    const frameRobot = (root: THREE.Object3D) =>
      frameSphere(new THREE.Box3().setFromObject(root).getBoundingSphere(new THREE.Sphere()));
    frameNow = () => {
      if (robot) {
        frameRobot(robot.root);
      }
    };

    const resize = () => {
      const { clientWidth: width, clientHeight: height } = container ?? { clientWidth: 1, clientHeight: 1 };
      renderer.setSize(width, height, false);
      camera.aspect = width / Math.max(height, 1);
      camera.updateProjectionMatrix();
      dirty = true;
    };
    const observer = new ResizeObserver(resize);
    observer.observe(container);
    resize();

    let frame = 0;
    const draw = () => {
      frame = requestAnimationFrame(draw);
      if (playing && started) {
        const to = playedTo(started, performance.now(), speed);
        const newest = keptMs?.[1];
        if (newest !== undefined && to >= newest && replay) {
          // Its last moment: held there.
          cursorMs = newest;
          lookBack(newest);
          hold();
        } else if (newest !== undefined && to >= newest) {
          // Caught up: live again.
          backToNow();
        } else {
          cursorMs = to;
          lookBack(to);
        }
      }
      if (robot && paused && held) {
        applyPoses(robot.links, held);
        held = null;
        dirty = true;
      }
      if (robot && pending && !paused) {
        applyPoses(robot.links, pending);
        pending = null;
        if (!framed) {
          frameRobot(robot.root);
        }
        dirty = true;
      }
      if (dirty) {
        renderer.render(scene, camera);
        dirty = false;
      }
    };
    draw();

    // The live robot's links, to tell when the robot Kapto runs changes.
    let links: string[] = [];
    /** A robot built, with where it can reach. */
    interface Loaded {
      built: Robot;
      reach: THREE.Sphere | null;
    }
    const robots = new Robots<Loaded>(async (id) => {
      const loaded = await (id === null ? getRobotScene() : getReplayScene(id)).catch((error: unknown) => {
        if (error instanceof ApiFailure && error.code === "viewer.replay_without_robot") {
          return null;
        }
        throw error;
      });
      if (loaded === null) {
        return null;
      }
      const mesh = (n: number) => (id === null ? getMesh(n) : getReplayMesh(id, n)).then(decodeMesh);
      const built = await buildRobot(loaded, mesh, (n) => (said = `Mesh ${n} couldn't be read`));
      const [x = 0, y = 0, z = 0] = loaded.reach?.center ?? [];
      const reach = loaded.reach ? new THREE.Sphere(new THREE.Vector3(x, y, z), loaded.reach.radius) : null;
      return { built, reach };
    });
    /** Where the robot framed last can reach: framed again only for one that reaches elsewhere. */
    let framedReach: THREE.Sphere | null = null;
    drawRobotOf = (id) => {
      // Gone at once: poses of one robot never move another.
      if (robot) {
        scene.remove(robot.root);
        robot = null;
        dirty = true;
      }
      held = null;
      robotOf = "";
      withoutRobot = false;
      robots
        .of(id)
        .then((loaded) => {
          if ((replay?.id ?? null) !== id) {
            // Another was asked for since.
            return;
          }
          robotOf = id ?? "live";
          withoutRobot = loaded === null;
          if (loaded === null) {
            return;
          }
          robot = loaded.built;
          scene.add(robot.root);
          if (id === null) {
            links = robot.links.map((link) => link.name);
          }
          // Everywhere it can reach, framed once: it stays in view as it moves.
          if (loaded.reach && !(framedReach && framedReach.equals(loaded.reach))) {
            framedReach = loaded.reach;
            frameSphere(loaded.reach);
          }
          dirty = true;
          if (paused && cursorMs !== null) {
            // Posed where the view is held.
            lookBack(cursorMs);
          }
          if (id === null) {
            said = "Waiting for the robot…";
          }
        })
        .catch((error: unknown) => (said = `The robot couldn't be loaded: ${String(error)}`));
    };
    drawRobotOf(replay?.id ?? null);

    // Showing more than the page keeps, live: the stretch asked for again
    // each second.
    let seconds = 0;
    const following = setInterval(() => {
      const newest = keptMs?.[1];
      if (!paused && spanMs > PAGE_KEEPS_MS && newest !== undefined) {
        lookBack(newest);
      }
      // How far back the bridge keeps, again every ten seconds.
      seconds += 1;
      if (seconds % 10 === 0) {
        measureKept();
      }
    }, 1000);

    // Shown four times a second: more would be unreadable.
    const shown = setInterval(() => {
      if (replay) {
        delayMs = null;
        said = withoutRobot
          ? "Replay · recorded without a robot: its values only"
          : playing
            ? `Replay · ${speed}×`
            : "Replay · paused";
        return;
      }
      if (closed || delays.length === 0) {
        return;
      }
      const sorted = [...delays].sort((a, b) => a - b);
      const median = sorted[Math.floor(sorted.length / 2)] ?? 0;
      delayMs = paused ? null : Math.max(0, Math.round(median));
      said = paused ? (playing ? `Playing back · ${speed}×` : "Paused · looking back") : "Live";
    }, 250);

    watched = watchStream(port, {
      onopening: (opening) => {
        closed = false;
        // All that is kept comes again after the opening.
        log.clear();
        items = [];
        plots = plotStreams(opening);
        dropPictures();
        cameras = camerasOf(opening);
        historyMs = opening.history_s * 1000;
        historySeconds = opening.history_s;
        // Once the opening has been read, the bridge takes questions.
        setTimeout(measureKept, 0);
        if (opening.version !== STREAM_VERSION) {
          said = `The live stream is version ${opening.version}; this page reads ${STREAM_VERSION}. Reload the page.`;
        } else if (links.length > 0 && opening.links.join("\n") !== links.join("\n")) {
          said = "The robot changed since the page loaded. Reload the page.";
        }
      },
      onframe: (received) => {
        frames += 1;
        for (const sample of received.samples) {
          plots.find((stream) => stream.index === sample.stream)?.samples.push(sample.newestMs, sample.extremes);
        }
        if (received.poses) {
          pending = received.poses;
        }
        // The bridge keeps up to its history behind the newest; its
        // answers say exactly. A replay's span is its episode's.
        if (replay) {
          return;
        }
        const oldest = keptMs ? Math.max(keptMs[0], received.receivedMs - historyMs) : received.receivedMs;
        keptMs = [oldest, Math.max(received.receivedMs, keptMs?.[1] ?? 0)];
        delays.push(Date.now() - received.receivedMs);
        if (delays.length > DELAYS) {
          delays.shift();
        }
      },
      onmarks: (marks) => {
        log.add(marks);
        items = [...log.items];
      },
      onpicture: (picture) => pictures.add(picture, Date.now()),
      oncamera: (said) => {
        cameras = withSaid(cameras, said);
      },
      onclosed: () => {
        closed = true;
        delays.length = 0;
        said = "Reconnecting to the live view…";
      },
    });

    return () => {
      watched?.stop();
      watched = null;
      offThread.stop();
      dropPictures();
      closeCameras();
      clearInterval(shown);
      clearInterval(following);
      cancelAnimationFrame(frame);
      observer.disconnect();
      controls.dispose();
      renderer.dispose();
    };
  });
</script>

<svelte:window onkeydown={viewKey} />

<div class="view">
<div class="home" hidden bind:this={home}>
<div
  class="live"
  bind:this={container}
  title="Live view of the robot"
  data-frames={frames}
  data-showing={paused && cursorMs !== null ? String(Math.round(cursorMs)) : "live"}
  data-playing={playing ? "yes" : "no"}
  data-looked-back={lookedBack === null ? "no" : "yes"}
  data-environment={drawn}
  data-framings={framings}
  data-replay={replay?.id ?? ""}
  data-robot={robotOf}
  data-delay-ms={delayMs ?? ""}
>
  <canvas bind:this={canvas}></canvas>
  <div class="tools">
    <button type="button" title="Fit the robot as it stands now in the view (F)" onclick={() => frameNow()}>
      <Icon name="frame" /> Frame robot
    </button>
    <button type="button" aria-expanded={environmentShown} onclick={() => (environmentShown = !environmentShown)}>
      <Icon name="cube" /> Environment{placed.length > 0 ? ` (${placed.length})` : ""}
    </button>
  </div>
  {#if environmentShown}
    <div class="environment">
      <EnvironmentPanel {placed} onput={putPlaced} onremove={removePlaced} problem={environmentProblem} />
    </div>
  {/if}
  <p
    class="said"
    title="{delayMs === null ? 'From' : `${delayMs} ms from`} Kapto receiving a message to this page receiving it, on this computer's clock: on another computer, any difference between the two clocks is in it too"
  >
    {said}{#if late}<span class="late">{` · ${delayMs} ms`}</span>{/if}
  </p>
</div>
</div>
<div class="columns" role="group" aria-label="The view's panels">
  {#each columns as column, place (place)}
    <div class={`column grow-${column.width}`}>
      {#each column.panels as panel, at (at)}
        <div class={`panel grow-${panel.height}`}>
          {#if panel.missing.length > 0}
            <div class="placeholder" role="note">
              <p>{named(panel)} can't be shown: {panel.missing.join("; ")}.</p>
              <p class="remedy">
                {arrangedBy.source === "own"
                  ? "Remove it, or change it, with Arrange."
                  : "Change the package's layout.toml, or what it declares."}
              </p>
            </div>
          {:else if panel.kind === "robot"}
            <div class="slot" {@attach adopt}></div>
          {:else if panel.kind === "camera"}
            {@const shown = (replay === null ? liveShown : replayShown).filter((camera) => camera.name === panel.stream)}
            {#if shown.length > 0}
              <CameraPanel
                cameras={shown}
                onstep={replay === null ? undefined : stepCamera}
                onwidth={(stream, pixels) => shownWidths.set(stream, pixels)}
                tallies={replay === null ? tallies : {}}
              />
            {:else}
              <p class="waiting">Waiting for {panel.stream}</p>
            {/if}
          {:else if panel.kind === "plot"}
            <PlotPanel
              fields={panel.fields}
              roles={roleFields}
              streams={lookedBack ?? plots}
              {spanMs}
              endMs={paused ? cursorMs : null}
            />
          {:else if panel.kind === "health"}
            {@render health?.()}
          {:else if panel.kind === "events"}
            <section class="events" aria-label="What happened">
              <ol>
                {#each (replay ? replay.items : items).slice(-50).reverse() as item, index (index)}
                  <li><time>{time(item.startMs * 1e6)}</time> {item.label}</li>
                {:else}
                  <li class="none">Nothing marked yet</li>
                {/each}
              </ol>
            </section>
          {/if}
        </div>
      {/each}
    </div>
  {/each}
</div>
<Timeline
  {paused}
  {keptMs}
  {cursorMs}
  {spanMs}
  {historySeconds}
  items={replay ? replay.items : items}
  {colors}
  onpause={pause}
  onlive={backToNow}
  onseek={seek}
  onspan={setSpan}
  {playing}
  {speed}
  onplay={play}
  onhold={hold}
  onspeed={setSpeed}
  onwatch={watch}
  replaying={replay !== null}
/>
<div class="under">
  <!-- Marks go to what is recorded now: not offered while an episode is replayed. -->
  <div hidden={replay !== null}><MarkBar onkinds={knowKinds} /></div>
  {#if packageId && replay === null}
    <button type="button" class="arrange-toggle" aria-expanded={arranging} onclick={() => (arranging = !arranging)}>
      Arrange
    </button>
  {/if}
</div>
{#if arranging && packageId && replay === null}
  <div class="arranging">
    <LayoutEditor
      layout={own ?? editable(liveLayout)}
      {offered}
      {recordedOnly}
      onchange={arrange}
      onsave={() => void settle(() => saveDefaultLayout(own ?? editable(liveLayout)), "Saved as the default: every browser shows the view this way.")}
      onreset={() => void settle(removeDefaultLayout, "Back to the package's layout, in every browser.")}
      said={arrangeSaid}
      failed={arrangeFailed}
    />
  </div>
{/if}
</div>

<style>
  .view {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .columns {
    flex: 1;
    display: flex;
    gap: 0.5rem;
    min-height: 0;
  }
  /* More panels than fit scroll rather than each being squeezed past use. */
  .column {
    display: flex;
    flex-direction: column;
    flex-basis: 0;
    gap: 0.5rem;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
  }
  .panel {
    display: flex;
    flex-direction: column;
    flex-basis: 0;
    min-width: 0;
    /* Room for a plot's key and a chart that can be read. */
    min-height: 12rem;
    overflow: hidden;
  }
  .panel > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .grow-1 {
    flex-grow: 1;
  }
  .grow-2 {
    flex-grow: 2;
  }
  .grow-3 {
    flex-grow: 3;
  }
  .grow-4 {
    flex-grow: 4;
  }
  .grow-5 {
    flex-grow: 5;
  }
  .grow-6 {
    flex-grow: 6;
  }
  .grow-7 {
    flex-grow: 7;
  }
  .grow-8 {
    flex-grow: 8;
  }
  .grow-9 {
    flex-grow: 9;
  }
  .grow-10 {
    flex-grow: 10;
  }
  .grow-11 {
    flex-grow: 11;
  }
  .grow-12 {
    flex-grow: 12;
  }
  /* Narrower, as on a tablet: the columns one under another, each one's
     panels side by side, never over the controls below. */
  @media (max-width: 70rem) {
    .columns {
      flex-direction: column;
    }
    .column {
      flex-direction: row;
    }
  }
  .slot {
    display: flex;
  }
  .placeholder,
  .waiting,
  .events {
    padding: 0.5rem 0.75rem;
    border: 1px dashed var(--line);
    border-radius: var(--radius);
    color: var(--muted);
    font-size: var(--text-sm);
    overflow-y: auto;
  }
  .placeholder p,
  .waiting {
    margin: 0 0 0.4rem;
  }
  .remedy {
    font-size: var(--text-xs);
  }
  .events {
    border-style: solid;
  }
  .events ol {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .events time {
    margin-right: 0.4rem;
    font-family: var(--font-data);
    font-size: var(--text-xs);
  }
  .live {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 0;
    border-radius: var(--radius);
    overflow: hidden;
    background: var(--viewer);
  }
  .tools {
    position: absolute;
    top: 0.5rem;
    right: 0.75rem;
    display: flex;
    gap: 0.4rem;
  }
  .tools button {
    padding: 0.1rem 0.5rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--background) 70%, transparent);
    color: var(--text);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  /* Arrange on the marks' line: their bar keeps room above it. */
  .under {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .arrange-toggle {
    padding: 0.2rem 0.6rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .arranging {
    position: absolute;
    top: 0;
    right: 0;
    z-index: 2;
    width: min(30rem, 100%);
    max-height: calc(100% - 6rem);
    overflow-y: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--panel);
  }
  .environment {
    position: absolute;
    top: 2.2rem;
    right: 0.75rem;
    max-width: min(26rem, calc(100% - 1.5rem));
    max-height: calc(100% - 3rem);
    overflow-y: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--background) 92%, transparent);
  }
  canvas {
    display: block;
    width: 100%;
    height: 100%;
  }
  .said .late {
    color: var(--warning);
  }
  .said {
    position: absolute;
    top: 0.5rem;
    left: 0.75rem;
    margin: 0;
    padding: 0.1rem 0.45rem;
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--background) 70%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
</style>
