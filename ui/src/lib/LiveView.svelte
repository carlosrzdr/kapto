<script lang="ts">
  import Icon from "./Icon.svelte";
  import { onMount, type Snippet, tick, untrack } from "svelte";
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
  } from "./api";
  import type { LayoutInput, LiveViewCounts, PanelView, RobotRole, ViewLayout, ViewPanel, VocabularyKind } from "./api-types";
  import ArrangingBox from "./ArrangingBox.svelte";
  import CameraPanel from "./CameraPanel.svelte";
  import EnvironmentPanel from "./EnvironmentPanel.svelte";
  import LayoutEditor from "./LayoutEditor.svelte";
  import LayoutList from "./LayoutList.svelte";
  import LayoutOffer from "./LayoutOffer.svelte";
  import MarkBar from "./MarkBar.svelte";
  import PanelTitle from "./PanelTitle.svelte";
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
  import { type Box, type ColumnBoxes, dragged, dropPlace, landing, shareTicks, sharesSaid, type Span } from "./viewer/dragging";
  import {
    apportioned,
    editable,
    MAX_SHARE,
    nudged,
    type Offered,
    type Place,
    moved,
    placed as droppedAt,
    removed,
    retitled,
    reshared,
    withHeights,
    withView,
    withWidths,
  } from "./viewer/layouts";
  import { PackageLayouts } from "./viewer/layout-list.svelte";
  import { offerMemory, offersEpisode } from "./viewer/layout-offer";
  import { decodeMesh } from "./viewer/mesh";
  import { ALL_MS, asked, type PlotStream, plotStreams, SampleBuffer } from "./viewer/plots";
  import { openedAt, playedTo, type Started } from "./viewer/play";
  import { applyPoses, buildRobot, drawsNow, type Robot, standingBox } from "./viewer/robot";
  import { openingOf, type Played, type Replaying } from "./viewer/replay";
  import { Robots } from "./viewer/robots";
  import { Questions } from "./viewer/questions";
  import { type Bounds, boxesOf, eased, fitted, framed as framedFrom, leftAt, placedOf, type Point, sameBoxes, widenedTo } from "./viewer/views";
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
    /** The robot package the view is for, whose layouts are offered and saved; none to arrange without one. */
    packageId?: string | null;
    /** Streams recorded without being decoded: nothing to plot. */
    recordedOnly?: string[];
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
  /** How many times the robot's views were drawn, and the frame whose poses they showed last, for the page's tests. */
  let draws = $state(0);
  let drawnFrame = $state(0);

  /** A robot panel on the page: where it is in the layout, and the view the layout gives it. */
  interface RobotSlot {
    element: HTMLElement;
    column: number;
    at: number;
    view: () => PanelView | null | undefined;
  }
  /** The robot panels shown, in the order they appeared; the scene draws into each one's place. */
  const slots: RobotSlot[] = [];
  /** Frames one panel's camera on the robot as it stands now; set once the scene exists. */
  let frameSlot: (column: number, at: number) => void = () => {};

  /** Shows the robot in a panel, from its own camera, for as long as the panel is there. */
  function robotSlot(column: number, at: number, view: () => PanelView | null | undefined) {
    return (element: HTMLElement) => {
      const slot = { element, column, at, view };
      slots.push(slot);
      return () => {
        slots.splice(slots.indexOf(slot), 1);
      };
    };
  }
  /** The robot panel that carries the view's words and buttons, and its state for the page's tests: the first shown. */
  const firstRobot = $derived.by(() => {
    for (const [column, each] of columns.entries()) {
      const at = each.panels.findIndex((panel) => panel.kind === "robot" && panel.missing.length === 0);
      if (at >= 0) {
        return { column, at };
      }
    }
    return null;
  });

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
  /** What a panel is, in a word or two, made from what it shows. */
  function madeTitle(panel: ViewPanel): string {
    switch (panel.kind) {
      case "camera":
        return panel.stream ?? "Camera";
      case "plot":
        return panel.fields.length === 1 ? panel.fields[0]! : `Plot of ${panel.fields.length}`;
      case "health":
        return "Streams' health";
      case "events":
        return "Marks";
      default:
        return "Robot";
    }
  }
  /** A panel's name on its grip: its own title, else the one made from what it shows. */
  const titled = (panel: ViewPanel) => panel.title ?? madeTitle(panel);
  /** What a panel is, in words, within a sentence. */
  const inWords = (panel: ViewPanel) => (panel.title ? `the panel ${panel.title}` : named(panel).replace(/^The /, "the "));
  /** The replay's plots, made from its opening. */
  let replayPlots = $state.raw<PlotStream[]>([]);
  /** Whether the replay was recorded without a robot, so only its plots are shown. */
  let withoutRobot = $state(false);
  /** Whose robot is drawn: `live`, or the replayed episode's ID; empty while it loads. */
  let robotOf = $state("");
  /** Draws the robot replay `id` was recorded with, or the live one for `null`; set once the scene exists. */
  let drawRobotOf: (id: string | null) => void = () => {};

  /** One canvas over the whole view, which every robot panel's camera draws into the panel's place on. */
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

  /** The package's layouts, chosen from the list beside Arrange; until it is read, the one `layout` gives. */
  let chosen = $state.raw<PackageLayouts | null>(null);
  let arranging = $state(false);
  $effect(() => {
    const layouts = packageId ? new PackageLayouts(packageId) : null;
    chosen = layouts;
    void layouts?.load();
  });
  /** What the package has that a panel may show, as the page receives it. */
  const offered: Offered = $derived({
    cameras: cameras.map((camera) => camera.name),
    fields: plots.flatMap((stream) => stream.fields.map((field) => `${stream.name}.${field.name}`)),
    roles: roles.map((each) => each.role),
  });
  const liveLayout = $derived(chosen?.view ?? layout ?? ROBOT_ONLY);
  // A replay is arranged by the layout chosen, like the live view; the one
  // its recording carries is offered, once per episode, and shown only for
  // the replay it was asked for.
  const answers = offerMemory();
  /** The episode whose layout was answered on this page, and the one whose layout is shown for its replay. */
  let answered = $state<string | null>(null);
  let usingEpisode = $state<string | null>(null);
  $effect(() => {
    if (replay?.id !== untrack(() => usingEpisode)) {
      usingEpisode = null;
    }
  });
  const episodeOffered = $derived(
    replay !== null && chosen !== null && replay.id !== answered && offersEpisode(replay.opening.layout, liveLayout, answers.answered(replay.id))
      ? replay
      : null,
  );
  const arrangedBy = $derived(replay !== null && usingEpisode === replay.id ? (replay.opening.layout ?? liveLayout) : liveLayout);
  const columns = $derived(arrangedBy.columns);

  function arrange(next: LayoutInput) {
    chosen?.arrange(next);
  }

  /** Answers the offer of episode `id`'s layout, which isn't offered for it again. */
  function answerEpisode(id: string, use: boolean) {
    answers.answer(id);
    answered = id;
    if (use) {
      usingEpisode = id;
    }
  }

  // Arranging on the view itself: a panel dragged by its grip to another
  // place, or moved by the arrow keys; an edge between two columns or two
  // panels dragged to share the room between them anew. Saved as the same
  // whole shares, set when the drag ends.
  let grid = $state<HTMLDivElement | null>(null);
  /** The panel being dragged, and where it would go if dropped now. */
  let carried = $state<{ column: number; at: number; to: Place | null } | null>(null);
  /** Shares an edge being dragged gives, shown before they are set: the columns', or one column's panels'. */
  let sharing = $state<{ column: number | null; shares: number[] } | null>(null);
  let sharingFrom: { at: number; sizes: number[]; shares: number[] | null; edge: number } | null = null;

  /** Whether the columns run one under another, as on a narrow page. */
  const stacked = () => (grid ? getComputedStyle(grid).flexDirection === "column" : false);
  const columnBoxes = (): HTMLElement[] => (grid ? [...grid.querySelectorAll<HTMLElement>(":scope > .column")] : []);
  const panelBoxes = (column: HTMLElement): HTMLElement[] => [...column.querySelectorAll<HTMLElement>(":scope > .panel")];
  const editing = () => chosen?.arranged ?? editable(liveLayout);

  function measured(): ColumnBoxes[] {
    return columnBoxes().map((column) => ({
      box: column.getBoundingClientRect(),
      panels: panelBoxes(column).map((panel) => panel.getBoundingClientRect()),
    }));
  }

  /** Where the grip was pressed, and whether it went far enough from there to be a drag rather than a click on its name. */
  let pressedAt: { x: number; y: number } | null = null;
  let draggedAway = false;

  function carry(event: PointerEvent, column: number, at: number) {
    if (event.button !== 0) {
      return;
    }
    pressedAt = { x: event.clientX, y: event.clientY };
    draggedAway = false;
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    carried = { column, at, to: null };
  }

  function carrying(event: PointerEvent) {
    if (pressedAt && Math.hypot(event.clientX - pressedAt.x, event.clientY - pressedAt.y) > 4) {
      draggedAway = true;
    }
    if (carried) {
      carried = { ...carried, to: dropPlace(measured(), event.clientX, event.clientY, stacked()) };
    }
  }

  function drop() {
    const was = carried;
    carried = null;
    if (was?.to) {
      // Dropped where it was, nothing is arranged: the package's layout stays the one shown.
      const next = droppedAt(editing(), was.column, was.at, was.to);
      if (JSON.stringify(next) !== JSON.stringify(editing())) {
        arrange(next);
      }
    }
  }

  /**
   * The panel's grip by the keyboard: the arrows move it up or down its
   * column, or to the end of the column beside it (a new one past the
   * last), its grip keeping the focus; Escape lets go of a drag.
   */
  async function step(event: KeyboardEvent, column: number, at: number) {
    if (event.key === "Escape") {
      carried = null;
      return;
    }
    const move = ({ ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" } as const)[event.key as string];
    if (move === undefined) {
      return;
    }
    event.preventDefault();
    const layout = editing();
    const next = moved(layout, column, at, move);
    if (next === layout) {
      return;
    }
    arrange(next);
    // Where it went: along its column, or last in the one beside it, which
    // is one place further left when the column it left is gone.
    const alone = layout.columns[column]!.panels.length === 1;
    const into = move === "up" || move === "down" ? column : move === "left" ? column - 1 : alone ? column : column + 1;
    const place = move === "up" ? at - 1 : move === "down" ? at + 1 : next.columns[into]!.panels.length - 1;
    await tick();
    gripOf(into, place)?.focus();
  }

  /** The panel whose title is being written, by its column and place. */
  let titling = $state<{ column: number; at: number } | null>(null);
  const gripOf = (column: number, at: number) =>
    columnBoxes()[column]?.querySelectorAll<HTMLElement>(":scope > .panel .grip:not(.remove)")[at];

  /** Its name clicked, or Enter on its grip: a field to write the panel's own title in, in place of its name. */
  function startTitling(column: number, at: number) {
    const dragged = draggedAway;
    draggedAway = false;
    pressedAt = null;
    if (!dragged) {
      titling = { column, at };
    }
  }

  /**
   * The title written saved, as every other change is saved while arranging
   * (blank, the panel's made one is back), or `null` left as it was; by a
   * key, the grip has the focus again, as it had before.
   */
  async function titleWritten(written: string | null, byKey: boolean) {
    const was = titling;
    if (!was) {
      return;
    }
    titling = null;
    const layout = editing();
    const next = written === null ? layout : retitled(layout, was.column, was.at, written);
    if (JSON.stringify(next) !== JSON.stringify(layout)) {
      arrange(next);
    }
    if (byKey) {
      await tick();
      gripOf(was.column, was.at)?.focus();
    }
  }

  /** The column dropping now would put the panel into, before panel `at`. */
  const dropsBefore = (column: number, at: number) =>
    carried?.to !== null && carried?.to !== undefined && "column" in carried.to && carried.to.column === column && carried.to.at === at;
  /** Whether dropping now would make a new column `between`. */
  const dropsBetween = (between: number) => carried?.to !== null && carried?.to !== undefined && "between" in carried.to && carried.to.between === between;

  /** Starts dragging the edge after `edge`: of the columns, or of the panels of `column`. */
  function grab(event: PointerEvent, column: number | null, edge: number) {
    if (event.button !== 0) {
      return;
    }
    const boxes = column === null ? columnBoxes() : panelBoxes(columnBoxes()[column]!);
    // Columns run across the page, panels down a column; turned on a narrow page.
    const across = (column === null) !== stacked();
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    sharingFrom = {
      at: across ? event.clientX : event.clientY,
      sizes: boxes.map((box) => (across ? box.getBoundingClientRect().width : box.getBoundingClientRect().height)),
      shares: column === null ? null : (columns[column]?.panels.map((each) => each.height) ?? []),
      edge,
    };
    sharing = { column, shares: sharesOf(sharingFrom.sizes) };
    tell({ column, edge, x: event.clientX, y: event.clientY });
  }

  /**
   * Sizes measured or dragged as shares: the columns' in proportion, the
   * largest 12; a column's panels' in the pixels a share had when grabbed,
   * a share being a size of its own there.
   */
  const sharesOf = (sizes: number[]) => (sharingFrom?.shares ? reshared(sizes, sharingFrom.shares) : apportioned(sizes));

  function pull(event: PointerEvent) {
    if (!sharing || !sharingFrom) {
      return;
    }
    const across = (sharing.column === null) !== stacked();
    const by = (across ? event.clientX : event.clientY) - sharingFrom.at;
    sharing = { ...sharing, shares: sharesOf(dragged(sharingFrom.sizes, sharingFrom.edge, by)) };
    tell({ column: sharing.column, edge: sharingFrom.edge, x: event.clientX, y: event.clientY });
  }

  /** The edge let go of without setting what it gave. */
  function cancelled() {
    sharing = null;
    sharingFrom = null;
    tell(null);
  }

  function release() {
    const was = sharing;
    sharing = null;
    sharingFrom = null;
    tell(null);
    if (was) {
      arrange(was.column === null ? withWidths(editing(), was.shares) : withHeights(editing(), was.column, was.shares));
    }
  }

  /** The edge after `edge` moved one share by the keyboard: the one before grows, the one after shrinks, as far as they can. */
  function nudge(event: KeyboardEvent, column: number | null, edge: number) {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
    if (step === undefined) {
      return;
    }
    event.preventDefault();
    const layout = editing();
    const shares = column === null ? layout.columns.map((each) => each.width) : (layout.columns[column]?.panels.map((each) => each.height) ?? []);
    const next = nudged(shares, edge, step);
    if (next !== null) {
      arrange(column === null ? withWidths(layout, next) : withHeights(layout, column, next));
    }
    // Said by the edge for a moment, as there is no pointer to say it by.
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
    tell({ column, edge, x: box.left + box.width / 2, y: box.top + box.height / 2 }, NUDGE_TOLD_MS);
  }

  /**
   * The edge whose shares are shown, and the point they are said by: the
   * pointer while it is dragged, the edge's middle after a key moved it.
   */
  let told = $state<{ column: number | null; edge: number; x: number; y: number } | null>(null);
  let toldUntil: ReturnType<typeof setTimeout> | undefined;
  /** How long the shares a key gave stay said. */
  const NUDGE_TOLD_MS = 1500;
  function tell(next: typeof told, forMs?: number) {
    clearTimeout(toldUntil);
    told = next;
    if (forMs !== undefined) {
      toldUntil = setTimeout(() => (told = null), forMs);
    }
  }
  $effect(() => () => clearTimeout(toldUntil));

  /**
   * The ruler along the two either side of the edge told, across the
   * pointer (`at`), a tick per share; measured once the page shows the
   * shares it gives, so in a column that scrolls only its part shown.
   */
  let ruler = $state<{ across: boolean; span: Span; at: number; ticks: number[]; said: string } | null>(null);
  $effect(() => {
    const at = told;
    const of = at?.column ?? null;
    const shown =
      of === null
        ? columns.map((each, place) => widthOf(place, each.width))
        : (columns[of]?.panels.map((each, place) => heightOf(of, place, each.height)) ?? []);
    const column = of === null ? null : columnBoxes()[of];
    const room = (column ?? grid)?.getBoundingClientRect();
    const boxes = (at?.column === null ? columnBoxes() : column ? panelBoxes(column) : []).map((each) => each.getBoundingClientRect());
    const before = at ? boxes[at.edge] : undefined;
    const after = at ? boxes[at.edge + 1] : undefined;
    if (!at || !room || !before || !after) {
      ruler = null;
      return;
    }
    // Columns run across the page, panels down a column; turned on a narrow page.
    const across = (at.column === null) !== stacked();
    const along = (box: DOMRect): Span => (across ? [box.left, box.right] : [box.top, box.bottom]);
    const within = along(room);
    const [low, high] = across ? [room.top, room.bottom] : [room.left, room.right];
    ruler = {
      across,
      span: [Math.max(along(before)[0], within[0]), Math.min(along(after)[1], within[1])],
      at: Math.min(Math.max(across ? at.y : at.x, low + 12), high - 12),
      ticks: shareTicks(along(before), along(after), [shown[at.edge] ?? 1, shown[at.edge + 1] ?? 1], within),
      said: sharesSaid(shown, at.edge),
    };
  });
  /** The shares said, by the point told, kept on the page; placed from here, as the page allows no style written into it. */
  let saidBox = $state<HTMLElement | null>(null);
  $effect(() => {
    if (!saidBox || !told || !ruler) {
      return;
    }
    const x = Math.min(told.x + 14, innerWidth - saidBox.offsetWidth - 4);
    const y = Math.max(told.y - saidBox.offsetHeight - 14, 4);
    saidBox.style.translate = `${x}px ${y}px`;
  });

  /** Where the panel carried would land if dropped now, as large as it is. */
  let landed = $state<Box | null>(null);
  $effect(() => {
    const was = carried;
    const column = was?.to ? columnBoxes()[was.column] : undefined;
    const own = column ? panelBoxes(column)[was!.at]?.getBoundingClientRect() : undefined;
    landed = was?.to && own ? landing(measured(), was.to, own, stacked()) : null;
  });

  /** A column's width share as shown: the one being dragged to, while it is. */
  const widthOf = (place: number, width: number) => (sharing?.column === null ? (sharing.shares[place] ?? width) : width);
  const heightOf = (place: number, at: number, height: number) => (sharing?.column === place ? (sharing.shares[at] ?? height) : height);
  /** Whether the view can be arranged on: only the live view, of a package. */
  const onView = $derived(arranging && packageId !== null && packageId !== undefined && replay === null);

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
  /** How long a stretch the plots show: all there is unless a span is chosen. */
  let spanMs = $state(ALL_MS);
  /** When the episode being recorded started, if one is, as the bridge says. */
  let recordingSince = $state<number | null>(null);
  /** Showing all there is while an episode is recorded: the episode, from its start. */
  const fromMs = $derived(replay === null && spanMs === ALL_MS ? recordingSince : null);
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
  /**
   * The live samples a stretch from the bridge takes after its own while it
   * is followed, until the next is asked for a second later: ten seconds'.
   */
  const FOLLOWED = 600;
  /** How long each part of the stretch shown from the bridge is, to merge live samples into. */
  let partMs = 1;

  /** The stretches asked for, one at a time; a late answer to what was left is dropped. */
  const questions = new Questions();

  /** A stretch from the replay, or from the bridge's last minutes. */
  function ask(from: number, to: number, points: number): Promise<Stretch> {
    if (replay) {
      return getReplayWindow(replay.id, from, to, points).then(decodeStretch);
    }
    return watched ? watched.ask(from, to, points) : Promise.reject(new Error("not watching the live stream"));
  }

  /**
   * Whether the plots show more than the page keeps of its own, from the
   * bridge's last minutes: a longer span, or all there is once that is
   * longer.
   */
  function beyondPage(): boolean {
    if (spanMs !== ALL_MS) {
      return spanMs > PAGE_KEEPS_MS;
    }
    return keptMs !== null && keptMs[1] - Math.max(keptMs[0], fromMs ?? keptMs[0]) > PAGE_KEEPS_MS;
  }

  /** Fills the plots, and while paused the robot, from the stretch ending at `endMs`. */
  function lookBack(endMs: number) {
    if (!(replay || watched)) {
      return;
    }
    const start = spanMs === ALL_MS ? Math.max(keptMs?.[0] ?? 0, fromMs ?? 0) : endMs - spanMs;
    // A tenth more before what is shown, so what slides in from the left
    // as the plot moves on was loaded already.
    const question = asked(start - (endMs - start) / 10, endMs, POINTS * 1.1);
    questions.ask(
      () => ask(question.fromMs, question.toMs, question.points),
      (stretch) => show(stretch, question.partMs),
    );
  }

  /**
   * Asks the bridge how far back it keeps, as one part spanning everything:
   * what the page has seen since it loaded may be less.
   */
  function measureKept() {
    // One question at a time, as `lookBack` asks: skipped if one is out.
    // A replay's span is its episode's.
    if (questions.busy || !watched || replay) {
      return;
    }
    const asking = watched;
    questions.ask(
      () => asking.ask(0, Date.now() + 86_400_000, 1),
      (stretch) => {
        if (stretch.keptMs) {
          keptMs = [stretch.keptMs[0], Math.max(stretch.keptMs[1], keptMs?.[1] ?? 0)];
        }
      },
    );
  }

  function show(stretch: Stretch, parts: number) {
    if (stretch.keptMs && !replay) {
      keptMs = stretch.keptMs;
    }
    partMs = parts;
    lookedBack = (replay ? replayPlots : plots).map((stream) => {
      const kept = stretch.streams.find((each) => each.stream === stream.index)?.parts ?? [];
      const samples = new SampleBuffer(stream.samples.values, kept.length + 1 + FOLLOWED);
      for (const part of kept) {
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
    // What was asked of the live view, or of another replay, isn't shown here.
    questions.forget();
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
    // A stretch asked for to follow now isn't the moment held.
    questions.forget();
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
    // What was asked while looking back, answered late, would look back
    // again: dropped.
    questions.forget();
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
    // A stretch of the span before, answered late, isn't this span's.
    questions.forget();
    spanMs = ms;
    if (!paused && !beyondPage()) {
      lookedBack = null;
    }
    const end = paused ? cursorMs : keptMs?.[1];
    if (end !== null && end !== undefined && (paused || beyondPage())) {
      lookBack(end);
    }
  }

  /** What the robot's first view says of it, for the page's tests. */
  const liveData = $derived({
    "data-frames": frames,
    "data-showing": paused && cursorMs !== null ? String(Math.round(cursorMs)) : "live",
    "data-playing": playing ? "yes" : "no",
    "data-looked-back": lookedBack === null ? "no" : "yes",
    "data-environment": drawn,
    "data-framings": framings,
    "data-draws": draws,
    "data-drawn-frame": drawnFrame,
    "data-replay": replay?.id ?? "",
    "data-robot": robotOf,
    "data-delay-ms": delayMs ?? "",
  });

  /** How many recent delays the shown one is the median of. */
  const DELAYS = 60;

  onMount(() => {
    if (!canvas) {
      return;
    }
    const renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true });
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
    /** A robot panel's own camera, turned by the pointer within the panel. */
    interface View {
      slot: RobotSlot;
      camera: THREE.PerspectiveCamera;
      controls: OrbitControls;
      /** Whether it was framed, or placed where the layout says; a new one is framed once it has a size. */
      framed: boolean;
      /** The layout's view it was last placed from, to tell when the layout changes it. */
      from: string;
      boxes: ReturnType<typeof boxesOf>;
      /** Whether it stands where a place in the layout puts it, which nothing moves by itself. */
      placed: boolean;
      /** Whether the operator turned or zoomed it, which only framing it again hands back. */
      byHand: boolean;
      /** Whether the pointer is on it: what moves it then is the operator. */
      held: boolean;
      /** How far from what it looks at it is widening to, if it is. */
      goal: number | null;
      /** How many times it widened, for the page's tests. */
      widenings: number;
    }
    const views = new Map<HTMLElement, View>();
    const viewOf = (slot: RobotSlot): View => {
      const camera = new THREE.PerspectiveCamera(45, 1, 0.01, 100);
      camera.up.set(0, 0, 1);
      const controls = new OrbitControls(camera, slot.element.querySelector<HTMLElement>(".orbit") ?? slot.element);
      const view: View = { slot, camera, controls, framed: false, from: "", boxes: null, placed: false, byHand: false, held: false, goal: null, widenings: 0 };
      place(view);
      controls.addEventListener("start", () => (view.held = true));
      controls.addEventListener("change", () => {
        dirty = true;
        if (view.held) {
          view.byHand = true;
          view.goal = null;
        }
      });
      // Where it was left while arranging is kept with the layout; only looking leaves the layout as it was.
      controls.addEventListener("end", () => {
        view.held = false;
        if (!onView) {
          return;
        }
        const left = leftAt(camera.position.toArray(), controls.target.toArray());
        // Its own change: the camera stays where it is rather than being placed again.
        view.from = JSON.stringify(left);
        view.placed = true;
        arrange(withView(editing(), slot.column, slot.at, left));
      });
      return view;
    };
    /** Puts `view`'s camera where its layout says: a place as it is, a preset framed from its side. */
    const place = (view: View) => {
      const given = view.slot.view() ?? null;
      view.from = JSON.stringify(given);
      const placed = placedOf(given);
      view.placed = placed !== null;
      view.byHand = false;
      view.goal = null;
      if (placed) {
        view.camera.position.fromArray(placed.from);
        view.controls.target.fromArray(placed.at);
        view.framed = true;
      } else {
        const { from, at } = framedFrom(given, [0, 0, 0.4], 2.4);
        view.camera.position.fromArray(from);
        view.controls.target.fromArray(at);
        view.framed = false;
      }
      view.controls.update();
      dirty = true;
    };

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
    let dirty = true;
    const delays: number[] = [];
    let closed = false;

    /** Whether the robot has been posed since it was drawn. */
    let posed = false;
    /** The box around the robot as it stands, measured again each frame it moves, into the same objects. */
    const standingNow = new THREE.Box3();
    const standing: Bounds = { low: [0, 0, 0], high: [0, 0, 0] };
    let stands = false;
    /** Where the robot can reach, if the scene says: the furthest any view widens to. */
    let limit: Bounds | null = null;
    const measure = () => {
      if (!robot || !posed) {
        stands = false;
        return;
      }
      standingBox(robot, standingNow);
      standingNow.min.toArray(standing.low);
      standingNow.max.toArray(standing.high);
      stands = !standingNow.isEmpty() && standing.low.every(Number.isFinite) && standing.high.every(Number.isFinite);
    };

    /**
     * Fills each of `which` with the robot as it stands, each from its own
     * side, by the panel's width and height alike, and lets it widen by
     * itself again; counted as one framing for the page's tests. Hidden, a
     * view keeps the aspect it had, so one framed then isn't left out of
     * sight once shown.
     */
    const frameBox = (which: View[], counted = true) => {
      if (!stands || which.length === 0) {
        return;
      }
      for (const view of which) {
        const { camera, controls } = view;
        // A place left while arranging keeps its direction, framed along it.
        const side = view.placed ? { from: camera.position.toArray(), at: controls.target.toArray() } : (view.slot.view() ?? null);
        const { from, at } = fitted(side, standing, camera.fov, camera.aspect);
        camera.position.fromArray(from);
        controls.target.fromArray(at);
        controls.update();
        view.framed = true;
        view.byHand = false;
        view.goal = null;
      }
      if (counted) {
        framings += 1;
      }
      dirty = true;
    };
    frameNow = () => frameBox([...views.values()]);
    frameSlot = (column, at) => {
      const view = [...views.values()].find(({ slot }) => slot.column === column && slot.at === at);
      if (view) {
        frameBox([view]);
      }
    };

    // Reused each frame, so keeping the robot in view makes nothing new.
    const toCamera = new THREE.Vector3();
    const along: Point = [0, 0, 0];
    const lookedAt: Point = [0, 0, 0];
    /**
     * Eases `view` out along the way it looks, about what it looks at, as
     * far as keeps the robot as it stands in it: worked out again when it or
     * the panel changed, and never nearer by itself. Not a view the operator
     * turned, nor one standing where its layout places it.
     */
    const keepInView = (view: View, changed: boolean, ms: number) => {
      const { camera, controls } = view;
      if (view.placed || view.byHand || !stands) {
        return;
      }
      const distance = toCamera.subVectors(camera.position, controls.target).length();
      if (distance === 0) {
        return;
      }
      toCamera.divideScalar(distance).toArray(along);
      if (changed) {
        const goal = widenedTo(along, controls.target.toArray(lookedAt), distance, standing, camera.fov, camera.aspect, limit);
        if (goal > (view.goal ?? distance)) {
          if (view.goal === null) {
            view.widenings += 1;
            view.slot.element.dataset.widenings = String(view.widenings);
          }
          view.goal = goal;
        }
      }
      if (view.goal === null) {
        return;
      }
      const next = eased(distance, view.goal, ms);
      camera.position.copy(controls.target).addScaledVector(toCamera, next);
      controls.update();
      if (next >= view.goal) {
        view.goal = null;
      }
      dirty = true;
    };

    const resize = () => {
      renderer.setSize(canvas?.clientWidth ?? 0, canvas?.clientHeight ?? 0, false);
      // Resized, a canvas is cleared: every view drawn again.
      for (const view of views.values()) {
        view.boxes = null;
      }
      dirty = true;
    };
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    resize();

    let frame = 0;
    // The frame whose poses the robot stands in.
    let posedFrame = 0;
    /** Whether a view was framed since the robot was drawn: the first framing of each robot is counted. */
    let framedOnce = false;
    let lastDrawMs = performance.now();
    const draw = () => {
      frame = requestAnimationFrame(draw);
      const nowMs = performance.now();
      const sinceMs = nowMs - lastDrawMs;
      lastDrawMs = nowMs;
      let robotMoved = false;
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
        robotMoved = true;
        dirty = true;
      }
      if (robot && pending && !paused) {
        applyPoses(robot.links, pending);
        pending = null;
        posedFrame = frames;
        robotMoved = true;
        dirty = true;
      }
      if (robotMoved) {
        posed = true;
        measure();
      }
      // Each panel's camera, made as the panel comes and let go as it goes.
      let moved = false;
      for (const [element, view] of views) {
        if (!slots.some((slot) => slot.element === element)) {
          view.controls.dispose();
          views.delete(element);
          moved = true;
        }
      }
      const area = canvas?.getBoundingClientRect();
      let shown = false;
      for (const slot of slots) {
        let view = views.get(slot.element);
        if (!view) {
          view = viewOf(slot);
          views.set(slot.element, view);
        } else if (JSON.stringify(slot.view() ?? null) !== view.from) {
          // Its view changed in the layout, other than by being left there.
          place(view);
        }
        const column = slot.element.closest(".column");
        const boxes =
          area && area.width > 0 && area.height > 0
            ? boxesOf(slot.element.getBoundingClientRect(), column ? [column.getBoundingClientRect()] : [], area)
            : null;
        const reshaped = !sameBoxes(boxes, view.boxes);
        if (reshaped) {
          moved = true;
          // Hidden, a view has no size: its camera keeps the aspect it had.
          if (boxes) {
            view.camera.aspect = boxes.viewport.width / boxes.viewport.height;
            view.camera.updateProjectionMatrix();
          }
        }
        view.boxes = boxes;
        shown ||= boxes !== null;
        if (boxes && !view.framed && stands) {
          // Framed as the robot first stands once it has a size; one framed then, kept in view as it moves.
          frameBox([view], !framedOnce);
          framedOnce = true;
        } else if (boxes) {
          // Narrowed or shown again, a view may need widening as much as when the robot moves.
          keepInView(view, robotMoved || reshaped, sinceMs);
        }
      }
      if (moved) {
        // What was drawn where no panel shows the robot now is cleared, and every view drawn anew.
        renderer.setScissorTest(false);
        renderer.setClearColor(0x000000, 0);
        renderer.clear();
        dirty = true;
      }
      // Posed all along, but drawn only into the panels that show it.
      if (drawsNow(dirty, shown)) {
        renderer.setScissorTest(true);
        for (const { camera, boxes } of views.values()) {
          if (boxes) {
            const { viewport, scissor } = boxes;
            renderer.setViewport(viewport.x, viewport.y, viewport.width, viewport.height);
            renderer.setScissor(scissor.x, scissor.y, scissor.width, scissor.height);
            renderer.render(scene, camera);
          }
        }
        dirty = false;
        draws += 1;
        drawnFrame = posedFrame;
      }
    };
    draw();

    // The live robot's links, to tell when the robot Kapto runs changes.
    let links: string[] = [];
    /** A robot built, with where it can reach. */
    interface Loaded {
      built: Robot;
      reach: Bounds | null;
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
      // The box around everywhere it reaches, beyond which no view widens by itself.
      const corner = (values: number[]): Point => [values[0] ?? 0, values[1] ?? 0, values[2] ?? 0];
      const reach = loaded.reach ? { low: corner(loaded.reach.low), high: corner(loaded.reach.high) } : null;
      return { built, reach };
    });
    /** Where the robot framed last can reach: framed again only for one that reaches elsewhere. */
    let framedFor: string | null = null;
    drawRobotOf = (id) => {
      // Gone at once: poses of one robot never move another.
      if (robot) {
        scene.remove(robot.root);
        robot = null;
        dirty = true;
      }
      posed = false;
      stands = false;
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
          limit = loaded.reach;
          scene.add(robot.root);
          if (id === null) {
            links = robot.links.map((link) => link.name);
          }
          // A robot that reaches elsewhere is framed again as it first stands; the
          // same one, as live and its replays are, is left as the operator has it.
          // A place left while arranging stays where it was left.
          const reaching = JSON.stringify(loaded.reach);
          if (reaching !== framedFor) {
            framedFor = reaching;
            framedOnce = false;
            for (const view of views.values()) {
              view.framed = view.placed;
            }
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
    // each second; and the page's own samples again once they are enough,
    // as when an episode starts.
    let seconds = 0;
    const following = setInterval(() => {
      const newest = keptMs?.[1];
      if (!paused && beyondPage() && newest !== undefined) {
        lookBack(newest);
      } else if (!paused && !replay && lookedBack !== null) {
        lookedBack = null;
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
        recordingSince = null;
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
        // Following now with a stretch from the bridge, it takes them too,
        // merged into its parts as the bridge merges them, so its end moves
        // with every frame and looks the same once asked for again.
        const followed = !paused && !replay ? lookedBack : null;
        for (const sample of received.samples) {
          plots.find((stream) => stream.index === sample.stream)?.samples.push(sample.newestMs, sample.extremes);
          followed?.find((stream) => stream.index === sample.stream)?.samples.pushInto(sample.newestMs, sample.extremes, partMs);
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
        recordingSince = log.recordingSinceMs;
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
      for (const view of views.values()) {
        view.controls.dispose();
      }
      renderer.dispose();
    };
  });
</script>

<svelte:window onkeydown={viewKey} />

<div class="view">
<!-- Under every panel: each robot panel is a window onto the part of it in its place. -->
<canvas class="scene" bind:this={canvas}></canvas>
{#if packageId && replay === null}
  <!-- A bar of its own above the columns, so Arrange never covers what a
       panel shows (the robot's own buttons, a plot's key, a camera's
       picture); the list of layouts goes beside it, on its left. -->
  <div class="view-bar">
    {#if chosen}<LayoutList layouts={chosen} />{/if}
    <button
      type="button"
      class="arrange-toggle"
      aria-pressed={arranging}
      aria-expanded={arranging}
      aria-controls="arranging"
      onclick={() => (arranging = !arranging)}
    >
      Arrange
    </button>
  </div>
  {#if arranging}
    <!-- Next to its button in the page's order too, so at a tablet's width
         it opens under it rather than at the page's foot. -->
    <ArrangingBox faded={carried !== null || sharing !== null}>
      <LayoutEditor
        layout={editing()}
        {offered}
        {recordedOnly}
        onchange={arrange}
      />
    </ArrangingBox>
  {/if}
{/if}
{#if chosen?.stale && replay === null}
  {@const layouts = chosen}
  <LayoutOffer
    said="The package's layout has changed since this browser last showed it."
    use="Use the new one"
    save="Save the old one as…"
    suggested={layouts.nameFor("Package's", " (old)")}
    onuse={() => layouts.useNew()}
    onsave={(name) => layouts.saveOld(name)}
    onkeep={() => layouts.keepThis()}
  />
{:else if episodeOffered?.opening.layout && chosen}
  {@const episode = episodeOffered}
  {@const carried = episodeOffered.opening.layout}
  {@const layouts = chosen}
  <LayoutOffer
    said="This episode was recorded with another layout than the one shown."
    use="Use the episode's"
    save="Save it as…"
    suggested={layouts.nameFor("Episode's", "")}
    onuse={() => answerEpisode(episode.id, true)}
    onsave={async (name) => {
      const failed = await layouts.saveAs(name, editable(carried));
      if (failed === null) {
        answerEpisode(episode.id, false);
      }
      return failed;
    }}
    onkeep={() => answerEpisode(episode.id, false)}
  />
{/if}
{#snippet robotView(first: boolean, column: number, at: number)}
  <div class="tools">
    <button type="button" title="Fit the robot as it stands now in this view (F: in every view)" onclick={() => frameSlot(column, at)}>
      <Icon name="frame" /> Frame robot
    </button>
    {#if first}
      <button type="button" aria-expanded={environmentShown} onclick={() => (environmentShown = !environmentShown)}>
        <Icon name="cube" /> Environment{placed.length > 0 ? ` (${placed.length})` : ""}
      </button>
    {/if}
  </div>
  {#if first && environmentShown}
    <div class="environment">
      <EnvironmentPanel {placed} onput={putPlaced} onremove={removePlaced} problem={environmentProblem} />
    </div>
  {/if}
  {#if first}
    <p
      class="said"
      title="{delayMs === null ? 'From' : `${delayMs} ms from`} Kapto receiving a message to this page receiving it, on this computer's clock: on another computer, any difference between the two clocks is in it too"
    >
      {said}{#if late}<span class="late">{` · ${delayMs} ms`}</span>{/if}
    </p>
  {/if}
{/snippet}
{#if firstRobot === null}
  <!-- No panel shows the robot: what the page's tests read of it stays, drawing nothing. -->
  <div class="live" hidden title="Live view of the robot" {...liveData}></div>
{/if}
<div
  class="columns"
  class:on-view={onView}
  class:carrying={carried !== null}
  class:drop-last={dropsBetween(columns.length)}
  role="group"
  aria-label="The view's panels"
  bind:this={grid}
>
  {#each columns as column, place (place)}
    {#if onView && place > 0}
      <!-- A focusable separator with a value is ARIA's window splitter, a widget of its own, which these checks don't know. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
      <div
        class="edge between-columns"
        role="separator"
        tabindex="0"
        aria-orientation="vertical"
        aria-label={`Between columns ${place} and ${place + 1}`}
        aria-valuenow={widthOf(place - 1, columns[place - 1]!.width)}
        aria-valuemin={1}
        aria-valuemax={MAX_SHARE}
        onpointerdown={(event) => grab(event, null, place - 1)}
        onpointermove={pull}
        onpointerup={release}
        onpointercancel={cancelled}
        onkeydown={(event) => nudge(event, null, place - 1)}
      ></div>
    {/if}
    <div class={`column grow-${widthOf(place, column.width)}`} class:drop-new={dropsBetween(place)} class:drop-end={dropsBefore(place, column.panels.length)}>
      {#each column.panels as panel, at (at)}
        {#if onView && at > 0}
          <!-- A focusable separator with a value is ARIA's window splitter, a widget of its own, which these checks don't know. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
          <div
            class="edge between-panels"
            role="separator"
            tabindex="0"
            aria-orientation="horizontal"
            aria-label={`Between ${inWords(column.panels[at - 1]!)} and ${inWords(panel)}`}
            aria-valuenow={heightOf(place, at - 1, column.panels[at - 1]!.height)}
            aria-valuemin={1}
            aria-valuemax={MAX_SHARE}
            onpointerdown={(event) => grab(event, place, at - 1)}
            onpointermove={pull}
            onpointerup={release}
            onpointercancel={cancelled}
            onkeydown={(event) => nudge(event, place, at - 1)}
          ></div>
        {/if}
        <div
          class={`panel grow-${heightOf(place, at, panel.height)}`}
          class:camera={panel.kind === "camera"}
          class:drop-before={dropsBefore(place, at)}
          class:carried={carried?.column === place && carried.at === at}
        >
          {#if onView}
            <div class="grip-bar">
              {#if titling?.column === place && titling.at === at}
                <PanelTitle
                  title={panel.title ?? null}
                  made={madeTitle(panel)}
                  label={`Title of ${inWords(panel)}`}
                  onsave={(written, byKey) => void titleWritten(written, byKey)}
                  oncancel={() => void titleWritten(null, true)}
                />
              {:else}
                <button
                  type="button"
                  class="grip"
                  aria-label={`Drag ${inWords(panel)} to another place`}
                  title="Drag to another place, or move it with the arrow keys; click its name, or press Enter, to give it a title"
                  onpointerdown={(event) => carry(event, place, at)}
                  onpointermove={carrying}
                  onpointerup={drop}
                  onpointercancel={() => (carried = null)}
                  onclick={() => startTitling(place, at)}
                  onkeydown={(event) => void step(event, place, at)}><Icon name="grip" /><span>{titled(panel)}</span></button
                >
              {/if}
              <button type="button" class="grip remove" aria-label={`Remove ${inWords(panel)}`} onclick={() => arrange(removed(editing(), place, at))}
                ><Icon name="bin" /></button
              >
            </div>
          {/if}
          <!-- Plots and cameras show it in their own heading; the rest above what they show. -->
          {#if panel.title && !onView && panel.missing.length === 0 && panel.kind !== "plot" && panel.kind !== "camera"}
            <p class="panel-title">{panel.title}</p>
          {/if}
          {#if panel.missing.length > 0}
            <div class="placeholder" role="note">
              <p>{named(panel)} can't be shown: {panel.missing.join("; ")}.</p>
              <p class="remedy">
                {arrangedBy.source === "package" || arrangedBy.source === "made"
                  ? "Change the package's layout.toml, or what it declares."
                  : "Remove it, or change it, with Arrange."}
              </p>
            </div>
          {:else if panel.kind === "robot"}
            {@const first = firstRobot?.column === place && firstRobot.at === at}
            {@const data = first ? liveData : {}}
            <div
              class="slot live"
              title={first ? "Live view of the robot" : "Another view of the robot"}
              {...data}
              {@attach robotSlot(place, at, () => panel.view)}
            >
              <div class="orbit"></div>
              {@render robotView(first, place, at)}
            </div>
          {:else if panel.kind === "camera"}
            {@const shown = (replay === null ? liveShown : replayShown).filter((camera) => camera.name === panel.stream)}
            {#if shown.length > 0}
              <CameraPanel
                cameras={shown}
                title={panel.title ?? undefined}
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
              title={panel.title ?? undefined}
              roles={roleFields}
              streams={lookedBack ?? plots}
              {spanMs}
              endMs={paused ? cursorMs : null}
              {fromMs}
            />
          {:else if panel.kind === "health"}
            {@render health?.()}
          {:else if panel.kind === "events"}
            <section class="events" aria-label={panel.title ?? "What happened"}>
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
<!-- Over the view while arranging, in the page's own pixels: the steps an edge dragged moves in, and where a panel carried would land. -->
{#if ruler || landed}
  <svg class="guides" aria-hidden="true">
    {#if landed}
      <rect class="landing" x={landed.left} y={landed.top} width={landed.right - landed.left} height={landed.bottom - landed.top} />
    {/if}
    {#if ruler}
      {@const [from, to] = ruler.span}
      {#if ruler.across}
        <line class="ruler" x1={from} x2={to} y1={ruler.at} y2={ruler.at} />
        {#each ruler.ticks as tick, index (index)}
          <line class="tick" x1={tick} x2={tick} y1={ruler.at - 6} y2={ruler.at + 6} />
        {/each}
      {:else}
        <line class="ruler" x1={ruler.at} x2={ruler.at} y1={from} y2={to} />
        {#each ruler.ticks as tick, index (index)}
          <line class="tick" x1={ruler.at - 6} x2={ruler.at + 6} y1={tick} y2={tick} />
        {/each}
      {/if}
    {/if}
  </svg>
{/if}
{#if ruler}
  <!-- The separator says the share before it as its value; this is the same, seen. -->
  <p class="shares-said" aria-hidden="true" bind:this={saidBox}>{ruler.said}</p>
{/if}
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
<!-- Marks go to what is recorded now: not offered while an episode is replayed. -->
<div hidden={replay !== null}><MarkBar onkinds={knowKinds} /></div>
</div>

<style>
  .view {
    position: relative;
    /* Its own layers: the robot's canvas under its panels, over what is under the view. */
    isolation: isolate;
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
  /* A height share is a size of its own: one the least room for a plot's
     key and a chart that can be read, two twice that. A column with room
     grows them in proportion; one with more than fit scrolls, each still
     as tall as its shares, so resizing two shows there too; never taller
     than the column, which would leave a panel alone in one to scroll. */
  .panel {
    display: flex;
    flex-direction: column;
    flex-basis: min(calc(var(--shares) * 12rem), 100%);
    flex-shrink: 0;
    min-width: 0;
    min-height: 12rem;
    overflow: hidden;
  }
  .panel > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .grow-1 {
    flex-grow: 1;
    --shares: 1;
  }
  .grow-2 {
    flex-grow: 2;
    --shares: 2;
  }
  .grow-3 {
    flex-grow: 3;
    --shares: 3;
  }
  .grow-4 {
    flex-grow: 4;
    --shares: 4;
  }
  .grow-5 {
    flex-grow: 5;
    --shares: 5;
  }
  .grow-6 {
    flex-grow: 6;
    --shares: 6;
  }
  .grow-7 {
    flex-grow: 7;
    --shares: 7;
  }
  .grow-8 {
    flex-grow: 8;
    --shares: 8;
  }
  .grow-9 {
    flex-grow: 9;
    --shares: 9;
  }
  .grow-10 {
    flex-grow: 10;
    --shares: 10;
  }
  .grow-11 {
    flex-grow: 11;
    --shares: 11;
  }
  .grow-12 {
    flex-grow: 12;
    --shares: 12;
  }
  /* Arranged on: the room between columns and panels is the edges to drag.
     The editor floats over them, so they keep the width they are shown at. */
  .columns.on-view,
  .on-view .column {
    gap: 0;
  }
  .on-view .panel {
    position: relative;
    outline: 1px dashed var(--line);
    outline-offset: -1px;
    border-radius: var(--radius);
  }
  .edge {
    flex: none;
    border-radius: var(--radius);
    touch-action: none;
  }
  .edge:hover,
  .edge:focus-visible {
    background: color-mix(in srgb, var(--accent) 35%, transparent);
    outline: none;
  }
  .between-columns {
    width: 0.5rem;
    cursor: col-resize;
  }
  .between-panels {
    height: 0.5rem;
    cursor: row-resize;
  }
  /* A header of its own while arranging, so the grip never covers what the
     panel shows (the robot's own buttons, a plot's key). */
  .on-view .panel > .grip-bar {
    display: flex;
    flex: none;
    gap: var(--space-1);
    justify-content: space-between;
    min-height: 0;
    border-bottom: 1px dashed var(--line);
  }
  .grip {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    margin: 0.25rem;
    padding: 0.1rem 0.45rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--background) 85%, transparent);
    color: var(--text);
    font: inherit;
    font-size: var(--text-xs);
    cursor: grab;
    touch-action: none;
  }
  .panel > .panel-title {
    flex: none;
    margin: 0;
    padding: 0.2rem 0.6rem;
    overflow: hidden;
    color: var(--text);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grip span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grip.remove {
    margin-left: 0;
    border-color: var(--danger);
    color: var(--danger);
  }
  .carrying,
  .carrying .grip {
    cursor: grabbing;
  }
  .panel.carried {
    opacity: 0.45;
  }
  /* Where the panel carried would go: before a panel, at a column's end,
     or as a new column beside one. */
  .panel.drop-before {
    box-shadow: 0 -3px 0 var(--accent);
  }
  .column.drop-end {
    box-shadow: inset 0 -3px 0 var(--accent);
  }
  .column.drop-new {
    box-shadow: -3px 0 0 var(--accent);
  }
  .columns.drop-last > .column:last-of-type {
    box-shadow: 3px 0 0 var(--accent);
  }
  /* Drawn over the view while arranging, never in the pointer's way. */
  .guides {
    position: fixed;
    inset: 0;
    z-index: 30;
    width: 100%;
    height: 100%;
    overflow: visible;
    pointer-events: none;
  }
  .guides .ruler,
  .guides .tick {
    stroke: var(--accent);
    stroke-linecap: round;
  }
  .guides .ruler {
    stroke-width: 1;
  }
  .guides .tick {
    stroke-width: 2;
  }
  .guides .landing {
    fill: color-mix(in srgb, var(--accent) 12%, transparent);
    stroke: var(--accent);
    stroke-width: 2;
    stroke-dasharray: 6 4;
    rx: var(--radius);
  }
  .shares-said {
    position: fixed;
    top: 0;
    left: 0;
    z-index: 31;
    margin: 0;
    padding: 0.1rem 0.45rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: var(--background);
    color: var(--text);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    pointer-events: none;
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
    /* Side by side, the panels share the column's width rather than scroll across. */
    .panel {
      flex-basis: 0;
    }
    .between-columns {
      width: auto;
      height: 0.5rem;
      cursor: row-resize;
    }
    .between-panels {
      width: 0.5rem;
      height: auto;
      cursor: col-resize;
    }
    .panel.drop-before {
      box-shadow: -3px 0 0 var(--accent);
    }
    .column.drop-end {
      box-shadow: inset -3px 0 0 var(--accent);
    }
    .column.drop-new {
      box-shadow: 0 -3px 0 var(--accent);
    }
    .columns.drop-last > .column:last-of-type {
      box-shadow: 0 3px 0 var(--accent);
    }
  }
  /* As on a tablet, too narrow for a camera beside plots: each camera on a
     row of its own at the column's full width, the plots on the row under
     it, the column scrolling to them; each camera as tall as the least a
     panel is, so its picture and caption show whole before any scrolling.
     Not while arranging, whose dragging reads a column's panels as one row. */
  @media (max-width: 64rem) {
    .columns:not(.on-view) .column:has(> .camera) {
      flex-wrap: wrap;
    }
    .columns:not(.on-view) .panel.camera {
      flex-basis: 100%;
      height: 12rem;
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
  }
  /* Where the pointer turns, pans and zooms the panel's camera, under its buttons. */
  .orbit {
    position: absolute;
    inset: 0;
    touch-action: none;
  }
  .tools {
    position: absolute;
    top: 0.5rem;
    right: 0.75rem;
    left: 0.75rem;
    display: flex;
    /* In a narrow panel, one under the other rather than cut off. */
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 0.4rem;
    pointer-events: none;
  }
  .tools button {
    pointer-events: auto;
    height: var(--control-height);
    padding: 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--background) 70%, transparent);
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    white-space: nowrap;
    cursor: pointer;
  }
  /* The environment's panel open under it: its button says so, as the
     header's Keys does. */
  .tools button[aria-expanded="true"] {
    border-color: var(--text);
    background: var(--raised);
  }
  /* The view's own controls, at its top right where a view's settings are
     looked for, with room on their left for more of them. */
  .view-bar {
    display: flex;
    flex: none;
    justify-content: flex-end;
    gap: var(--space-2);
    height: var(--control-height);
    margin-bottom: var(--space-1);
  }
  .arrange-toggle {
    padding: 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  /* Pressed while arranging, in the accent its editor's top edge carries
     too, so the two read as one. */
  .arrange-toggle[aria-pressed="true"] {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    color: var(--accent);
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
  /* Under the whole view rather than in its flow, so the panels never
     move it; the robot's panels show it through, the rest of it clear. */
  .scene {
    position: absolute;
    inset: 0;
    z-index: -1;
    display: block;
    width: 100%;
    height: 100%;
    pointer-events: none;
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
