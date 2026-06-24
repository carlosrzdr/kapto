<script lang="ts">
  import Icon from "./lib/Icon.svelte";
  import { onMount } from "svelte";

  import {
    ApiFailure,
    askReplay,
    checkVersion,
    getEpisode,
    getEpisodeQuality,
    getEpisodeEvents,
    getEpisodeStatistics,
    getEpisodeUploads,
    getEventKinds,
    getEpisodes,
    getProcesses,
    getProfiles,
    getReplay,
    getReplayOpening,
    getRobot,
    getSettings,
    getStorage,
    getStatus,
    getTasks,
    getUploads,
    type QualityFilter,
    retryUploads,
    runPreflight,
    type SessionChoice,
    startEpisode,
    stopEpisode,
    testDestination,
    watchEvents,
  } from "./lib/api";
  import type {
    EpisodeDetail,
    EpisodeQuality,
    EpisodeEvents,
    EpisodeStatistics,
    EpisodeUploads,
    VocabularyKind,
    EpisodeSummary,
    ProcessStatus,
    CheckRange,
    ProfilesOverview,
    ReplayStatus,
    RobotOverview,
    SettingsOverview,
    Status,
    Storage,
    TasksOverview,
    UploadsOverview,
  } from "./lib/api-types";
  import EpisodeList from "./lib/EpisodeList.svelte";
  import LiveView from "./lib/LiveView.svelte";
  import Logo from "./lib/Logo.svelte";
  import PreflightPanel from "./lib/PreflightPanel.svelte";
  import { duration, plain, rate, sentence, time } from "./lib/format";
  import ProcessTable from "./lib/ProcessTable.svelte";
  import RecordButton from "./lib/RecordButton.svelte";
  import ReplayBar from "./lib/ReplayBar.svelte";
  import RobotPanel from "./lib/RobotPanel.svelte";
  import SessionPicker from "./lib/SessionPicker.svelte";
  import SettingsPanel from "./lib/SettingsPanel.svelte";
  import StorageSummary from "./lib/StorageSummary.svelte";
  import StreamTable from "./lib/StreamTable.svelte";
  import { type Measured, measure } from "./lib/uploads";
  import UploadsPanel from "./lib/UploadsPanel.svelte";
  import { type Playback, type Played, type Replaying, replayItems } from "./lib/viewer/replay";
  import { oneAtATime } from "./lib/poll";
  import Tabs from "./lib/Tabs.svelte";
  import { shortcut, SHORTCUTS } from "./lib/keys";

  /** Where the selected tab is remembered, in this browser only. */
  const TAB_KEY = "kapto.tab";

  /** The tabs, in order; a remembered tab no longer among them is ignored. */
  const TAB_IDS = ["live", "episodes", "uploads", "robot", "system"];

  function rememberedTab(): string {
    try {
      const remembered = localStorage.getItem(TAB_KEY);
      return remembered !== null && TAB_IDS.includes(remembered) ? remembered : "live";
    } catch {
      return "live";
    }
  }

  let tab = $state(rememberedTab());

  /** Where the profile and task chosen for the next episode are remembered. */
  const SESSION_KEY = "kapto.session";

  function rememberedChoice(): SessionChoice {
    try {
      const remembered = JSON.parse(localStorage.getItem(SESSION_KEY) ?? "{}") as Partial<SessionChoice>;
      return {
        profile: typeof remembered.profile === "string" ? remembered.profile : null,
        task: typeof remembered.task === "string" ? remembered.task : null,
      };
    } catch {
      return { profile: null, task: null };
    }
  }

  let profiles = $state<ProfilesOverview | null>(null);
  let tasks = $state<TasksOverview | null>(null);
  let choice = $state<SessionChoice>(rememberedChoice());

  function choose(next: SessionChoice) {
    choice = next;
    try {
      localStorage.setItem(SESSION_KEY, JSON.stringify(next));
    } catch {
      // Not remembered, then; the choice still holds for this page.
    }
  }

  /** Reads the saved profiles and tasks again, and forgets a choice no longer among them. */
  async function refreshSessions() {
    try {
      [profiles, tasks] = await Promise.all([getProfiles(), getTasks()]);
      const usable = profiles.profiles.some((profile) => profile.id === choice.profile && profile.available);
      const known = tasks.tasks.some((task) => task.id === choice.task);
      if ((choice.profile !== null && !usable) || (choice.task !== null && !known)) {
        choose({ profile: usable ? choice.profile : null, task: known ? choice.task : null });
      }
    } catch (error) {
      problem = failure(error);
    }
  }

  function select(id: string) {
    tab = id;
    try {
      localStorage.setItem(TAB_KEY, id);
    } catch {
      // Not remembered, then; nothing else depends on it.
    }
  }

  /** How often the status is asked for. */
  const POLL_MS = 1000;

  let kapto = $state("");
  /** A problem that stops the page working at all. */
  let fatal = $state<ApiFailure | null>(null);
  /** The last request's problem, shown until the next one succeeds. */
  let problem = $state<ApiFailure | null>(null);
  /**
   * Why the last order (record, stop) was refused, e.g. a disk too full to
   * record on: kept until the next order or dismissed, not cleared by the
   * next status answer.
   */
  let refused = $state<ApiFailure | null>(null);
  let status = $state<Status | null>(null);
  let rates = $state<Record<string, number>>({});
  let episodes = $state<EpisodeSummary[]>([]);
  let openEpisode = $state<EpisodeDetail | null>(null);
  let openQuality = $state<EpisodeQuality | null>(null);
  let openStatistics = $state<EpisodeStatistics | null>(null);
  let openEvents = $state<EpisodeEvents | null>(null);
  let openUploads = $state<EpisodeUploads | null>(null);
  /** The kinds of event that may be sent, to find episodes by. */
  let eventKinds = $state<VocabularyKind[]>([]);
  /** Only episodes with an event of this kind are listed, if set. */
  let eventFilter = $state<string | null>(null);
  let episodeFilter = $state<QualityFilter>("all");
  let processes = $state<ProcessStatus[]>([]);
  let storage = $state<Storage | null>(null);
  /** What serve was started with; it doesn't change while the page is open. */
  let robot = $state<RobotOverview | null>(null);
  /** The settings serve started with; they don't change while it runs. */
  let settings = $state<SettingsOverview | null>(null);
  let uploads = $state<UploadsOverview | null>(null);
  /** How fast each upload goes, from the progress it reported. */
  let uploadRates = $state.raw(new Map<string, Measured>());
  /** Bytes written per second while recording, from two storage readings. */
  let writing = $state<number | null>(null);
  let previousStorage: { at: number; staging: number } | null = null;
  let busy = $state(false);
  /** Pre-flight was asked to run again and hasn't answered yet. */
  let preflightRunning = $state(false);
  /** Whether the list of keys is shown. */
  let keysShown = $state(false);

  /** R records or stops; ? shows the keys, Esc hides them. */
  function appKey(event: KeyboardEvent) {
    const key = shortcut(event);
    if (key === "?") {
      keysShown = !keysShown;
    } else if (key === "escape" && keysShown) {
      keysShown = false;
    } else if (key === "r" && status !== null && !busy && !preflightRunning) {
      event.preventDefault();
      void act(recording ? stopEpisode : () => startEpisode(choice));
    }
  }
  /**
   * The episode whose replay was asked for, and where it is: the live view
   * replays it once made, until "Back to live". `null` shows the live robot.
   */
  let replay = $state<ReplayStatus | null>(null);
  /** When the replayed episode was recorded, to name it. */
  let replayLabel = $state("");
  /**
   * Where the replay opens: a few seconds before this moment, in UTC
   * nanoseconds, if one was asked for when it was opened.
   */
  let replayAt = $state<number | null>(null);
  /** Changed to open the replay shown again, where `replayAt` says. */
  let replayAsked = $state(0);
  /** What the made replay shows and what happened in it, once read. */
  let replaying = $state.raw<Replaying | null>(null);
  /** Where the shown replay is playing, as the live view last said. */
  let playback = $state<Playback | null>(null);

  function backToLive() {
    replay = null;
    replayAt = null;
    replaying = null;
    playback = null;
  }

  /** Takes where the live view's replay is, counted from its start. */
  function played(where: Played) {
    if (!replaying) {
      return;
    }
    const start = replaying.opening.start_utc_ns / 1e6;
    const length = (replaying.opening.end_utc_ns / 1e6 - start) / 1000;
    playback = { playing: where.playing, at: (where.atMs - start) / 1000, length, over: where.over };
  }

  /** Opens the replay shown again, a few seconds before `atNs` or at its start. */
  function reopen(atNs: number | null) {
    replayAt = atNs;
    replayAsked += 1;
    if (replaying) {
      replaying = { ...replaying, atMs: atNs === null ? null : atNs / 1e6, asked: replayAsked };
    }
  }

  /** Once the replay is made, reads what it shows and what happened in it, then shows it. */
  async function showReplay(made: ReplayStatus) {
    const id = made.episode;
    try {
      const [opening, events, quality, episode] = await Promise.all([
        getReplayOpening(id),
        getEpisodeEvents(id).catch(() => null),
        getEpisodeQuality(id).catch(() => null),
        getEpisode(id).catch(() => null),
      ]);
      if (replay?.episode === id) {
        const atMs = replayAt === null ? null : replayAt / 1e6;
        const span: [number, number] = [opening.start_utc_ns, opening.end_utc_ns];
        const items = replayItems(events, quality, episode, span);
        replaying = { id, opening, items, atMs, asked: replayAsked };
      }
    } catch (error) {
      problem = failure(error);
    }
  }

  /**
   * How many status readings each stream's rate is measured over. One
   * reply arriving late would otherwise make a healthy stream look slow.
   */
  const RATE_READINGS = 5;
  /** The latest readings, oldest first. */
  let readings: { at: number; received: Map<string, number> }[] = [];

  const recording = $derived(status?.capture.episode != null);

  /** What the page's top bar says by its colour: recording, something wrong, or live. */
  const shellState = $derived(recording ? "recording" : status === null || problem !== null ? "degraded" : "live");

  const tabs = $derived([
    {
      id: "live",
      label: "Live",
      alert:
        status?.capture.preflight?.passed === false ||
        (status?.capture.streams ?? []).some(
          (s) => s.dropped_by_us > 0 || s.lost_upstream > 0 || (s.received > 0 && rates[s.stream] === 0),
        ),
    },
    { id: "episodes", label: episodes.length > 0 ? `Episodes (${episodes.length})` : "Episodes" },
    {
      id: "uploads",
      label: "Uploads",
      alert: (uploads?.destinations ?? []).some((destination) => destination.failed > 0),
    },
    { id: "robot", label: "Robot", alert: (robot?.warnings.length ?? 0) > 0 },
    { id: "system", label: "System", alert: processes.some((p) => p.state === "restarting") },
  ]);

  function failure(error: unknown): ApiFailure {
    return error instanceof ApiFailure
      ? error
      : new ApiFailure({ code: "ui.error", message: String(error), remedy: "Reload the page." });
  }

  async function refresh() {
    try {
      // Timed when asked rather than when answered: an answer the page is
      // too busy to take at once would otherwise look like a slower stream.
      const asked = performance.now();
      const next = await getStatus();
      const reading = {
        at: asked,
        received: new Map(next.capture.streams.map((s) => [s.stream, s.received])),
      };
      const last = readings.at(-1);
      // Counts start again from zero when an episode starts or stops, here
      // or elsewhere: the readings before that measure nothing now.
      const restarted =
        last !== undefined &&
        [...reading.received].some(([stream, count]) => count < (last.received.get(stream) ?? 0));
      readings = [...(restarted ? [] : readings), reading].slice(-RATE_READINGS);
      const oldest = readings[0];
      if (oldest && oldest !== reading) {
        const seconds = (reading.at - oldest.at) / 1000;
        rates = Object.fromEntries(
          next.capture.streams.map((s) => [s.stream, rate(oldest.received.get(s.stream) ?? 0, s.received, seconds)]),
        );
      }
      // An episode that ended is listed even if its announcement was missed,
      // the event stream reconnecting meanwhile: the status says it ended.
      const ended = status?.capture.episode;
      status = next;
      problem = null;
      if (ended != null && next.capture.episode !== ended) {
        void refreshEpisodes();
      }
    } catch (error) {
      problem = failure(error);
    }
  }

  /** Asked for on its own, so it still shows while capture doesn't answer. */
  async function refreshProcesses() {
    try {
      processes = await getProcesses();
    } catch {
      // The status request reports why Kapto doesn't answer.
    }
  }

  async function refreshStorage() {
    try {
      const next = await getStorage();
      const now = performance.now();
      writing =
        recording && previousStorage
          ? rate(previousStorage.staging, next.staging_bytes, (now - previousStorage.at) / 1000)
          : null;
      previousStorage = { at: now, staging: next.staging_bytes };
      storage = next;
    } catch {
      // Shown as not known; the status request reports why Kapto doesn't answer.
      storage = null;
    }
  }

  async function refreshUploads() {
    try {
      const next = await getUploads();
      uploadRates = measure(uploadRates, next.uploads);
      uploads = next;
    } catch {
      // Shown as last known; the status request reports why Kapto doesn't answer.
    }
    // The open episode's, as they go up.
    const id = openEpisode?.id;
    if (id) {
      try {
        const episodeUploads = await getEpisodeUploads(id);
        if (openEpisode?.id === id) {
          openUploads = episodeUploads;
        }
      } catch {
        // Asked again on the next round.
      }
    }
  }

  /** Tries an episode's failed uploads again, then shows them queued. */
  async function retry(episode: string) {
    await retryUploads(episode);
    await refreshUploads();
  }

  async function showEpisode(id: string | null) {
    if (id === null) {
      openEpisode = null;
      openQuality = null;
      openStatistics = null;
      openEvents = null;
      openUploads = null;
      return;
    }
    try {
      [openEpisode, openQuality, openStatistics, openEvents, openUploads] = await Promise.all([
        getEpisode(id),
        getEpisodeQuality(id),
        getEpisodeStatistics(id),
        getEpisodeEvents(id),
        getEpisodeUploads(id),
      ]);
      problem = null;
    } catch (error) {
      problem = failure(error);
    }
  }

  async function refreshEpisodes() {
    try {
      episodes = await getEpisodes(20, episodeFilter, eventFilter);
    } catch (error) {
      problem = failure(error);
    }
  }

  function filterEpisodes(filter: QualityFilter) {
    episodeFilter = filter;
    void refreshEpisodes();
  }

  function filterByEvent(kind: string | null) {
    eventFilter = kind;
    void refreshEpisodes();
  }

  /** How long after an episode ends the page waits for it to be checked. */
  const CHECKED_WITHIN_NS = 10 * 60 * 1e9;

  /** Whether `episode` ended recently enough to be checked any moment. */
  const awaitingCheck = (episode: { ended_utc_ns: number }) =>
    Date.now() * 1e6 - episode.ended_utc_ns < CHECKED_WITHIN_NS;

  /**
   * The worker checks each episode and reads its events a moment after it
   * is recorded, and nothing announces it: while a listed or open episode
   * that ended recently isn't checked or read yet, its results are asked
   * for again. Older
   * ones, e.g. recorded before the worker ran, aren't waited for.
   */
  async function refreshUnchecked() {
    // Filtered by an event, a new episode is only listed once its events
    // are read, so the list is asked for again until then.
    const unread = episodes.some((episode) => (episode.accepted == null || episode.events == null) && awaitingCheck(episode));
    if (unread || eventFilter !== null) {
      await refreshEpisodes();
    }
    const id = openEpisode?.id;
    if (id && openEpisode && awaitingCheck(openEpisode) && openQuality?.accepted == null) {
      try {
        // Worked out together, so asked for together.
        const [quality, statistics] = await Promise.all([getEpisodeQuality(id), getEpisodeStatistics(id)]);
        // Only if the same episode is still open.
        if (openEpisode?.id === id) {
          openQuality = quality;
          openStatistics = statistics;
        }
      } catch {
        // Asked again on the next round; the episode's own request reports why.
      }
    }
    if (id && openEpisode && awaitingCheck(openEpisode) && openEvents?.read === false) {
      try {
        const events = await getEpisodeEvents(id);
        if (openEpisode?.id === id) {
          openEvents = events;
        }
      } catch {
        // Asked again on the next round.
      }
    }
  }

  /**
   * Asks for an episode's replay; the viewer switches to it once made, a
   * few seconds before `moment` if one is given. The replay already shown
   * just moves there.
   */
  async function replayEpisode(id: string, moment: CheckRange | null = null) {
    if (replay?.episode === id && replay.state === "ready" && moment !== null) {
      reopen(moment.start_utc_ns);
      return;
    }
    const known = episodes.find((episode) => episode.id === id) ?? (openEpisode?.id === id ? openEpisode : null);
    replayLabel = known === null ? id : time(known.started_utc_ns);
    replaying = null;
    playback = null;
    replayAt = moment?.start_utc_ns ?? null;
    try {
      replay = await askReplay(id);
      problem = null;
      if (replay.state === "ready") {
        await showReplay(replay);
      }
    } catch (error) {
      problem = failure(error);
    }
  }

  /** While a replay is being made, asks how far it got. */
  async function refreshReplay() {
    const waiting = replay;
    if (waiting === null || waiting.state === "ready" || waiting.state === "failed") {
      return;
    }
    try {
      // Removed from the cache meanwhile, it is asked for again.
      const next =
        waiting.state === "not_made" ? await askReplay(waiting.episode) : await getReplay(waiting.episode);
      // Only if it is still the one asked for.
      if (replay?.episode === next.episode) {
        replay = next;
        if (next.state === "ready") {
          await showReplay(next);
        }
      }
    } catch {
      // Asked again on the next round; the status request reports why Kapto doesn't answer.
    }
  }

  async function act(order: () => Promise<unknown>) {
    busy = true;
    refused = null;
    try {
      await order();
      // A new phase starts counting from zero.
      readings = [];
      await refresh();
      await refreshEpisodes();
    } catch (error) {
      refused = failure(error);
      // What failed, with what to do and "Record anyway", is on the live tab.
      if (refused.code === "capture.preflight_failed") {
        select("live");
      }
    } finally {
      busy = false;
    }
  }

  /** Runs pre-flight again; the status then carries the new run. */
  async function preflightAgain() {
    preflightRunning = true;
    refused = null;
    try {
      await runPreflight();
      await refresh();
    } catch (error) {
      refused = failure(error);
    } finally {
      preflightRunning = false;
    }
  }

  onMount(() => {
    let timer: ReturnType<typeof setInterval> | undefined;
    // An episode may start or end from elsewhere: `kapto episode`, the API,
    // another browser, or be recovered when capture starts again. The list
    // follows at once.
    const unwatch = watchEvents((event) => {
      if (event.event === "episode_ended" || event.event === "episode_recovered") {
        void refreshEpisodes();
        getEventKinds()
          .then((vocabulary) => (eventKinds = vocabulary.kinds))
          .catch(() => {
            // Episodes are still found by the kinds they hold.
          });
      }
    });
    checkVersion()
      .then((version) => {
        kapto = version.kapto;
        void refresh();
        void refreshProcesses();
        void refreshStorage();
        void refreshEpisodes();
        void refreshSessions();
        void refreshUploads();
        getRobot()
          .then((loaded) => (robot = loaded))
          .catch((error) => (problem = failure(error)));
        getSettings()
          .then((loaded) => (settings = loaded))
          .catch((error) => (problem = failure(error)));
        const polls = [refresh, refreshProcesses, refreshStorage, refreshUploads, refreshUnchecked, refreshReplay].map(oneAtATime);
        timer = setInterval(() => polls.forEach((poll) => poll()), POLL_MS);
      })
      .catch((error) => (fatal = failure(error)));
    return () => {
      clearInterval(timer);
      unwatch();
    };
  });
</script>

<!-- The streams' counts: in the side panel, and in the view's health panels. -->
{#snippet streamTable()}
  <StreamTable
    streams={status?.capture.streams ?? []}
    known={status !== null}
    {rates}
    {recording}
    declared={Object.fromEntries((robot?.streams ?? []).map((s) => [s.name, s.rate_hz]))}
    kept={Object.fromEntries((robot?.streams ?? []).map((s) => [s.name, s.kept]))}
  />
{/snippet}

<svelte:window onkeydown={appKey} />

{#if fatal}
  <main class="fatal">
    <h1><Logo />Kapto can't show this page</h1>
    <p>{fatal.message}</p>
    <p class="remedy">{fatal.remedy}</p>
    <p class="code">{fatal.code}</p>
  </main>
{:else}
  <header data-state={shellState}>
    <h1><Logo />Kapto</h1>
    {#if replay}
      <!-- Always here while the viewer isn't live, whatever the page shows. -->
      <button type="button" class="back" onclick={backToLive}>Back to live</button>
    {/if}
    <span class="state" class:live={recording}>
      {#if status === null}
        {problem ? "Capture not reachable" : "Connecting…"}
      {:else if recording}
        Recording · {duration(status.capture.seconds)}
      {:else}
        Live, not recording
      {/if}
    </span>
    <SessionPicker
      {profiles}
      {tasks}
      {choice}
      disabled={recording || busy}
      onchange={choose}
      onopen={() => void refreshSessions()}
    />
    <button
      type="button"
      class="keys-toggle"
      aria-expanded={keysShown}
      title="The keys for what you do most (?)"
      onclick={() => (keysShown = !keysShown)}><Icon name="keyboard" /> Keys</button
    >
    <RecordButton
      {recording}
      {busy}
      held={status === null
        ? "Kapto can't reach the capture process, so nothing can be recorded"
        : preflightRunning
          ? "Pre-flight is running; recording can start once it ends"
          : null}
      onstart={() => act(() => startEpisode(choice))}
      onstop={() => act(stopEpisode)}
    />
  </header>
  {#if keysShown}
    <section class="keys" aria-label="Keyboard shortcuts">
      <h2>Keys</h2>
      <dl>
        {#each SHORTCUTS as each (each.keys)}
          <dt><kbd>{each.keys}</kbd></dt>
          <dd>{each.does}</dd>
        {/each}
      </dl>
      <button type="button" onclick={() => (keysShown = false)}>Close</button>
    </section>
  {/if}

  {#if refused}
    <p class="problem" role="alert">
      {sentence(refused.message)} <span class="remedy">{refused.remedy}</span>
      <button type="button" class="dismiss" onclick={() => (refused = null)}>Dismiss</button>
    </p>
  {/if}
  {#if problem}
    <p class="problem" role="alert" title={problem.message}>
      {plain(problem.message)} <span class="remedy">{problem.remedy}</span>
    </p>
  {/if}

  <main>
    <section class="viewer">
      {#if replay}
        <ReplayBar
          {replay}
          label={replayLabel}
          {playback}
          onplayagain={() => reopen(null)}
        />
      {/if}
      {#if status}
        <LiveView
          port={status.viewer.stream_port}
          replay={replaying}
          onplayed={played}
          onleave={backToLive}
          layout={robot?.layout ?? null}
          roles={robot?.roles ?? []}
          health={streamTable}
          packageId={robot?.package ?? null}
          recordedOnly={(robot?.streams ?? []).filter((each) => !each.decoder).map((each) => each.name)}
        />
      {:else}
        <!-- Where the robot would be: what keeps it away and what to do. -->
        <div class="no-view" role="status">
          {#if problem}
            <!-- The banner above says what is wrong and what to do: said once. -->
            <h2>No live view</h2>
            <p>Nothing to show until Kapto can ask the capture process what it receives: the message above says why, and what to do.</p>
            <p class="remedy">The page tries again by itself every few seconds.</p>
          {:else}
            <p>Connecting to Kapto…</p>
          {/if}
        </div>
      {/if}
    </section>
    <aside>
      <Tabs {tabs} selected={tab} onselect={select} />
      <div role="tabpanel" id="panel-{tab}" aria-labelledby="tab-{tab}">
        {#if tab === "episodes"}
          <h2>Episodes</h2>
          <EpisodeList
            {episodes}
            open={openEpisode}
            quality={openQuality}
            statistics={openStatistics}
            events={openEvents}
            uploads={openUploads}
            destinations={uploads?.destinations ?? []}
            kinds={eventKinds}
            {eventFilter}
            onevent={filterByEvent}
            filter={episodeFilter}
            onselect={(id) => void showEpisode(id)}
            onfilter={filterEpisodes}
            {replay}
            onreplay={(id, moment) => void replayEpisode(id, moment)}
          />
        {:else if tab === "uploads"}
          <h2>Uploads</h2>
          <UploadsPanel
            overview={uploads}
            measured={uploadRates}
            marker={settings?.settings.find((setting) => setting.key === "upload.marker")?.value !== "false"}
            onretry={retry}
            ontest={testDestination}
          />
        {:else if tab === "robot"}
          {#if robot}
            <RobotPanel {robot} values={status?.capture.values ?? []} />
          {:else}
            <p class="unknown">Not known yet.</p>
          {/if}
        {:else if tab === "system"}
          <h2>Processes</h2>
          <ProcessTable {processes} />
          {#if settings}
            <h2>Settings</h2>
            <SettingsPanel {settings} />
          {/if}
        {:else}
          <h2>Pre-flight</h2>
          <PreflightPanel
            run={status?.capture.preflight ?? null}
            {recording}
            running={preflightRunning}
            busy={busy || status === null}
            onrun={() => void preflightAgain()}
            onrecordanyway={(waive) => act(() => startEpisode(choice, waive))}
          />
          <h2>Streams</h2>
          {@render streamTable()}
          <h2>Storage</h2>
          <StorageSummary {storage} {writing} />
        {/if}
      </div>
    </aside>
  </main>

  <footer>Kapto {kapto}</footer>
{/if}

<style>
  header {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    padding: var(--space-3) var(--space-4);
    border-top: 3px solid var(--accent);
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }
  /* The capture's state at a glance, from across the room. */
  header[data-state="recording"] {
    border-top-color: var(--danger-fill);
    background: var(--danger-background);
  }
  header[data-state="degraded"] {
    border-top-color: var(--warning);
  }
  h1 {
    display: flex;
    align-items: center;
    gap: 0.4em;
    margin: 0;
    font-size: var(--text-xl);
  }
  .back {
    padding: 0.35rem 0.9rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .state {
    margin-left: auto;
    color: var(--muted);
  }
  .state.live {
    color: var(--danger);
    font-weight: 600;
  }
  main {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(20rem, 1fr);
    gap: 1rem;
    flex: 1 1 0;
    min-height: 0;
    padding: 1rem;
  }
  /* On a smaller screen (a tablet beside the robot), the panel gets the
     room its tables need. */
  @media (max-width: 80rem) {
    main {
      grid-template-columns: minmax(0, 3fr) minmax(24rem, 2fr);
    }
  }
  .viewer {
    display: flex;
    flex-direction: column;
    min-height: 20rem;
  }
  .no-view {
    display: flex;
    flex: 1;
    flex-direction: column;
    justify-content: center;
    align-items: flex-start;
    gap: var(--space-2);
    max-width: 36rem;
    margin: 0 auto;
    padding: var(--space-6);
  }
  .no-view h2 {
    color: var(--text);
    font-size: var(--text-lg);
  }
  .no-view p {
    margin: 0;
  }
  aside {
    overflow-y: auto;
  }
  /* Room on its right, so nothing ends against the panel's edge or under
     its scrollbar. */
  [role="tabpanel"] {
    padding-right: var(--space-2);
  }
  .unknown {
    color: var(--muted);
  }
  .keys-toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
  }
  .keys {
    position: fixed;
    top: 4.5rem;
    right: var(--space-4);
    z-index: 10;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    width: 22rem;
    padding: var(--space-4);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--raised);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.6);
  }
  .keys dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: var(--space-2) var(--space-3);
    margin: 0;
  }
  .keys dd {
    margin: 0;
  }
  .keys kbd {
    padding: 0.05rem 0.4rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
  }
  .keys button {
    align-self: flex-end;
    padding: 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
  }
  .problem {
    margin: 0;
    padding: 0.5rem 1rem;
    background: var(--warning-background);
    color: var(--text);
  }
  .remedy {
    color: var(--muted);
  }
  .dismiss {
    margin-left: 0.75rem;
    padding: 0.1rem 0.5rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: none;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .fatal {
    display: block;
    max-width: 40rem;
    margin: 4rem auto;
    height: auto;
  }
  .code {
    color: var(--muted);
    font-family: var(--font-data);
  }
  footer {
    padding: 0 1rem;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  @media (max-width: 50rem) {
    main {
      grid-template-columns: 1fr;
      flex: none;
    }
    .viewer {
      height: 60vh;
    }
  }
</style>
