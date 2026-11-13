//! The depot lane's progress (design 14 D4, #337): one renderer for
//! `gogo depot` and for `linux-app`/`mac-app` builds that fill or repair the
//! depot cache.
//!
//! - `Json` (`--json-progress`): every [`DepotEvent`] as one NDJSON line.
//! - `Human { live: true }` (stderr a terminal, no `--no-progress`): a `gogo:`
//!   line as each phase starts and a `\r` status line under it, with bytes,
//!   speed and ETA while downloading. A spinner and the phase's elapsed time
//!   keep it moving through phases gog-warp says nothing in.
//! - `Human { live: false }` (stderr not a terminal, or `--no-progress`): the
//!   same phase lines, plus one line per tenth of hashing and downloading.
//! - `Silent` (`--quiet`, or `--json` without `--json-progress`): nothing.
//!
//! Other long steps (#420) use the same status line through
//! [`Renderer::task`]: a running count of files and bytes with a rate, or the
//! spinner alone when the total isn't known. That line is live mode only: the
//! other modes print what they printed before.

use std::collections::VecDeque;
use std::io::Write;
use std::time::Duration;

use gogo::engine::events::ProgressEvent;
use gogo::fmt::human_size;
use gogo::gog::depot::DepotEvent;

/// How the depot lane's events are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Json,
    Human { live: bool },
    Silent,
}

impl Mode {
    /// Pick from the flags and whether stderr is a terminal.
    pub fn pick(
        json_progress: bool,
        json: bool,
        quiet: bool,
        no_progress: bool,
        stderr_tty: bool,
    ) -> Self {
        if json_progress {
            Mode::Json
        } else if json || quiet {
            Mode::Silent
        } else {
            Mode::Human {
                live: stderr_tty && !no_progress,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Preparing,
    Hashing,
    Allocating,
    Downloading,
    /// A step of the caller's own ([`Renderer::task`]).
    Task,
    Done,
}

/// A count so far, and its total when known.
pub type Count = (u64, Option<u64>);

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// Samples older than this don't count towards the speed.
const SPEED_WINDOW: Duration = Duration::from_secs(5);
/// The status line is redrawn at most this often (ticks and events).
const REDRAW: Duration = Duration::from_millis(100);

/// Renders [`DepotEvent`]s into `out`. Times are durations since the run
/// started, passed in, so the output is a pure function of the events.
pub struct Renderer<W: Write> {
    mode: Mode,
    out: W,
    phase: Phase,
    /// When the first phase began (the run's own clock starts earlier).
    run_start: Option<Duration>,
    phase_start: Duration,
    /// The last fraction (hashing, allocating) or `(downloaded, total)`.
    fraction: f32,
    bytes: (u64, u64),
    /// `(time, downloaded)` within [`SPEED_WINDOW`].
    samples: VecDeque<(Duration, u64)>,
    /// The first download sample, for the phase's average speed.
    dl_first: Option<(Duration, u64)>,
    /// The tenth last reported in plain mode.
    tenth: u32,
    /// A status line is on screen (live mode).
    drawn: bool,
    last_draw: Option<Duration>,
    spin: usize,
    /// The [`Phase::Task`] step's name and its file count.
    task: String,
    task_files: Option<Count>,
    /// The task's byte count has a total.
    task_bytes_total: bool,
    /// A prompt owns the terminal: nothing is drawn until [`Renderer::resume`].
    paused: bool,
    /// What the task's count counts: "files" unless [`Renderer::counting`].
    unit: &'static str,
}

impl<W: Write> Renderer<W> {
    pub fn new(mode: Mode, out: W) -> Self {
        Self {
            mode,
            out,
            phase: Phase::Idle,
            run_start: None,
            phase_start: Duration::ZERO,
            fraction: 0.0,
            bytes: (0, 0),
            samples: VecDeque::new(),
            dl_first: None,
            tenth: 0,
            drawn: false,
            last_draw: None,
            spin: 0,
            task: String::new(),
            task_files: None,
            task_bytes_total: false,
            paused: false,
            unit: "files",
        }
    }

    /// Clears the status line and keeps it cleared, so a prompt gets the line
    /// to itself. The step stays; [`Renderer::resume`] lets it draw again.
    pub fn pause(&mut self) {
        self.paused = true;
        self.clear();
        let _ = self.out.flush();
    }

    /// The prompt is answered: a step still running draws again on the next
    /// tick, one that ended meanwhile stays gone.
    pub fn resume(&mut self) {
        self.paused = false;
        self.last_draw = None;
    }

    #[cfg(test)]
    pub fn into_inner(self) -> W {
        self.out
    }

    /// One event at `now`.
    pub fn event(&mut self, event: &DepotEvent, now: Duration) {
        use DepotEvent::*;
        match self.mode {
            Mode::Silent => return,
            Mode::Json => {
                if let Ok(line) = serde_json::to_string(event) {
                    let _ = writeln!(self.out, "{line}");
                    let _ = self.out.flush();
                }
                return;
            }
            Mode::Human { .. } => {}
        }
        match event {
            Resolved {
                title,
                version,
                generation,
                ..
            } => self.line(&format!(
                "gogo: {title} build {version} (generation {generation})"
            )),
            Plan {
                install_dir,
                download_size,
                disk_size,
                estimated_download_size,
                cache_complete,
                language,
                requested_language,
                dlcs,
                cache,
                game_files,
            } => {
                self.line(&format!(
                    "gogo: plan {} (download {}, disk {}, {}{}{})",
                    install_dir.display(),
                    if *cache_complete {
                        "none (cache complete)".to_string()
                    } else {
                        estimated_download_size.map_or_else(
                            || human_size(*download_size),
                            |e| format!("~{} (estimate: after checking the cache)", human_size(e)),
                        )
                    },
                    human_size(*disk_size),
                    language,
                    requested_language
                        .as_ref()
                        .map_or(String::new(), |r| format!(" from `{r}`")),
                    if dlcs.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " +dlc: {}",
                            dlcs.iter()
                                .map(|d| d.title.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ));
                if let Some(note) = cache {
                    self.line(&format!("gogo: {note}"));
                }
                if let Some(plan) = game_files {
                    self.line(&format!("gogo: {}", game_files_line(plan)));
                }
            }
            Note { message } => self.line(&format!("gogo: {message}")),
            Preparing => {
                self.enter(Phase::Preparing, now, "gogo: preparing".into());
                self.draw(now, true);
            }
            Verifying { fraction } => {
                if self.phase != Phase::Hashing {
                    self.enter(
                        Phase::Hashing,
                        now,
                        "gogo: hashing the files already in the cache".into(),
                    );
                }
                self.fraction = *fraction;
                self.plain_tenth(*fraction, |pct| format!("gogo: hashed {pct}%"));
                self.draw(now, false);
            }
            Allocating { fraction } => {
                if self.phase != Phase::Allocating {
                    self.enter(Phase::Allocating, now, "gogo: allocating".into());
                }
                self.fraction = *fraction;
                self.draw(now, false);
            }
            Downloading {
                downloaded,
                total_download,
                ..
            } => {
                if self.phase == Phase::Done {
                    return;
                }
                if self.phase != Phase::Downloading {
                    self.enter(
                        Phase::Downloading,
                        now,
                        format!(
                            "gogo: downloading ({} / {})",
                            human_size(*downloaded),
                            human_size(*total_download)
                        ),
                    );
                    self.samples.clear();
                    self.dl_first = Some((now, *downloaded));
                }
                self.bytes = (*downloaded, *total_download);
                self.samples.push_back((now, *downloaded));
                while self
                    .samples
                    .front()
                    .is_some_and(|(t, _)| now.saturating_sub(*t) > SPEED_WINDOW)
                    && self.samples.len() > 2
                {
                    self.samples.pop_front();
                }
                let fraction = if *total_download == 0 {
                    1.0
                } else {
                    *downloaded as f32 / *total_download as f32
                };
                let speed = self.speed();
                let (d, t) = self.bytes;
                self.plain_tenth(fraction, |pct| {
                    format!(
                        "gogo: downloaded {pct}% ({} / {}{})",
                        human_size(d),
                        human_size(t),
                        speed.map_or(String::new(), |s| format!(", {}/s", human_size(s)))
                    )
                });
                self.draw(now, false);
            }
            Finished {
                fetched_files,
                fetched_bytes,
                ..
            } => {
                let took = clock(now.saturating_sub(self.run_start.unwrap_or_default()));
                self.phase = Phase::Done;
                self.line(&if *fetched_files == 0 {
                    "gogo: nothing to fetch: every file was already in the cache".to_string()
                } else {
                    format!(
                        "gogo: fetched {fetched_files} file(s), {} (in {took})",
                        human_size(*fetched_bytes)
                    )
                });
            }
        }
    }

    /// The timer: redraws the status line so a phase without events still
    /// moves (live mode only).
    pub fn tick(&mut self, now: Duration) {
        self.draw(now, false);
    }

    /// A line of the caller's own (not a depot event), printed above the live
    /// status line like the phase lines are. Human modes only.
    pub fn note(&mut self, text: &str) {
        if matches!(self.mode, Mode::Human { .. }) {
            self.line(text);
        }
    }

    /// A step of the caller's own at `now` (#420): its name, files and bytes
    /// so far with their totals where known, or neither for the spinner
    /// alone. Draws the status line under the lines printed so far: live
    /// mode only, the other modes print nothing for it. A new `step` (or one
    /// after [`Renderer::task_done`]) starts its clock and rate afresh.
    pub fn task(&mut self, step: &str, files: Option<Count>, bytes: Option<Count>, now: Duration) {
        if !self.live() {
            return;
        }
        if self.phase != Phase::Task || self.task != step {
            self.phase = Phase::Task;
            self.phase_start = now;
            self.task = step.to_string();
            self.samples.clear();
            self.dl_first = None;
            self.bytes = (0, 0);
        }
        // A count past its total (an estimated total) reads as the total.
        let clamp = |c: Option<Count>| c.map(|(d, t)| (t.map_or(d, |t| d.min(t)), t));
        let (files, bytes) = (clamp(files), clamp(bytes));
        self.task_files = files;
        self.task_bytes_total = bytes.is_some_and(|(_, t)| t.is_some());
        if let Some((done, total)) = bytes {
            // A restart (a retried download) starts the rate afresh.
            if self.samples.back().is_some_and(|(_, d)| done < *d) {
                self.samples.clear();
                self.dl_first = None;
            }
            self.bytes = (done, total.unwrap_or(0));
            self.dl_first.get_or_insert((now, done));
            // One sample per redraw interval at most (and the last one), so
            // the window holds a bounded number however often counts come.
            let due = self
                .samples
                .back()
                .is_none_or(|(t, _)| now.saturating_sub(*t) >= REDRAW)
                || total.is_some_and(|t| done >= t);
            if due {
                self.samples.push_back((now, done));
            }
            while self
                .samples
                .front()
                .is_some_and(|(t, _)| now.saturating_sub(*t) > SPEED_WINDOW)
                && self.samples.len() > 2
            {
                self.samples.pop_front();
            }
        }
        self.draw(now, false);
    }

    /// [`Renderer::task`] with a count of `unit` (games, builds …) instead
    /// of files.
    pub fn counting(
        &mut self,
        step: &str,
        unit: &'static str,
        done: u64,
        total: u64,
        now: Duration,
    ) {
        self.unit = unit;
        self.task(step, Some((done, Some(total))), None, now);
    }

    /// The step [`Renderer::task`] showed is over: its status line goes.
    pub fn task_done(&mut self) {
        if self.phase == Phase::Task {
            self.clear();
            let _ = self.out.flush();
            self.phase = Phase::Idle;
            self.task_files = None;
            self.unit = "files";
        }
    }

    /// [`ProgressEvent::WorkProgress`], [`ProgressEvent::WorkStarted`] (the
    /// spinner alone) and [`ProgressEvent::WorkDone`] as [`Renderer::task`]
    /// and [`Renderer::task_done`]; `false` for any other event.
    pub fn work(&mut self, event: &ProgressEvent, now: Duration) -> bool {
        match event {
            ProgressEvent::WorkProgress {
                step,
                files,
                files_total,
                bytes,
                bytes_total,
            } => {
                self.task(
                    step,
                    files.map(|f| (f, *files_total)),
                    bytes.map(|b| (b, *bytes_total)),
                    now,
                );
                true
            }
            ProgressEvent::WorkStarted { step } => {
                self.task(step, None, None, now);
                true
            }
            ProgressEvent::WorkDone { .. } => {
                self.task_done();
                true
            }
            _ => false,
        }
    }

    /// Clears the status line; call once the run is over.
    pub fn finish(&mut self) {
        self.task_done();
        self.clear();
        let _ = self.out.flush();
    }

    fn live(&self) -> bool {
        self.mode == Mode::Human { live: true }
    }

    fn enter(&mut self, phase: Phase, now: Duration, line: String) {
        if self.run_start.is_none() {
            self.run_start = Some(now);
        }
        self.phase = phase;
        self.phase_start = now;
        self.fraction = 0.0;
        self.tenth = 0;
        self.line(&line);
    }

    /// A plain-mode line at each new tenth of `fraction`.
    fn plain_tenth(&mut self, fraction: f32, line: impl FnOnce(u32) -> String) {
        if self.live() {
            return;
        }
        let tenth = ((fraction.clamp(0.0, 1.0) * 10.0) as u32).min(10);
        if tenth > self.tenth {
            self.tenth = tenth;
            self.line(&line(tenth * 10));
        }
    }

    /// Bytes per second over the window, once it spans a second. Once the
    /// download is complete the window only sees the tail, so it is the
    /// phase's average instead.
    fn speed(&self) -> Option<u64> {
        let (d, t) = self.bytes;
        if t > 0 && d >= t {
            return self.average_speed();
        }
        let (t0, d0) = *self.samples.front()?;
        let (t1, d1) = *self.samples.back()?;
        let span = t1.saturating_sub(t0);
        (span >= Duration::from_secs(1))
            .then(|| (d1.saturating_sub(d0) as f64 / span.as_secs_f64()) as u64)
    }

    /// Bytes per second since the download phase's first sample.
    fn average_speed(&self) -> Option<u64> {
        let (t0, d0) = self.dl_first?;
        let (t1, d1) = *self.samples.back()?;
        let span = t1.saturating_sub(t0);
        (span >= Duration::from_secs(1))
            .then(|| (d1.saturating_sub(d0) as f64 / span.as_secs_f64()) as u64)
    }

    fn status(&self, now: Duration) -> Option<String> {
        let spin = SPINNER[self.spin % SPINNER.len()];
        let elapsed = clock(now.saturating_sub(self.phase_start));
        let pct = |f: f32| (f.clamp(0.0, 1.0) * 100.0) as u32;
        Some(match self.phase {
            Phase::Idle | Phase::Done => return None,
            Phase::Task => self.task_status(spin, &elapsed),
            Phase::Preparing => format!("{spin} preparing  {elapsed}"),
            Phase::Hashing => format!("{spin} hashing {}%  {elapsed}", pct(self.fraction)),
            Phase::Allocating => format!("{spin} allocating {}%  {elapsed}", pct(self.fraction)),
            Phase::Downloading => {
                let (d, t) = self.bytes;
                let speed = self.speed();
                let tail = match speed {
                    Some(s) if s > 0 => format!(
                        "  {}/s  eta {}",
                        human_size(s),
                        clock(Duration::from_secs(t.saturating_sub(d) / s))
                    ),
                    _ => String::new(),
                };
                format!(
                    "{spin} downloading {} / {}{tail}",
                    human_size(d),
                    human_size(t)
                )
            }
        })
    }

    /// `⠋ game files  120 / 400 files  300.0 MiB / 605.3 MiB  45.0 MiB/s  0:07`,
    /// with what is known of that.
    fn task_status(&self, spin: char, elapsed: &str) -> String {
        let mut line = format!("{spin} {}", self.task);
        match self.task_files {
            Some((d, Some(t))) => line += &format!("  {d} / {t} {}", self.unit),
            Some((d, None)) => line += &format!("  {d} {}", self.unit),
            None => {}
        }
        if self.dl_first.is_some() {
            let (d, t) = self.bytes;
            line += &if self.task_bytes_total {
                format!("  {} / {}", human_size(d), human_size(t))
            } else {
                format!("  {}", human_size(d))
            };
            if let Some(s) = self.speed().filter(|s| *s > 0) {
                line += &format!("  {}/s", human_size(s));
            }
        }
        line + "  " + elapsed
    }

    /// Redraw the status line (live mode), at most every [`REDRAW`] unless
    /// `force`.
    fn draw(&mut self, now: Duration, force: bool) {
        if !self.live() || self.paused {
            return;
        }
        if !force
            && self
                .last_draw
                .is_some_and(|t| now.saturating_sub(t) < REDRAW)
        {
            return;
        }
        let Some(status) = self.status(now) else {
            return;
        };
        self.spin += 1;
        self.last_draw = Some(now);
        let _ = write!(self.out, "\r\x1b[K{status}");
        let _ = self.out.flush();
        self.drawn = true;
    }

    fn clear(&mut self) {
        if self.drawn {
            let _ = write!(self.out, "\r\x1b[K");
            self.drawn = false;
        }
    }

    /// A whole line, above the status line (which the next draw restores).
    fn line(&mut self, text: &str) {
        self.clear();
        let _ = writeln!(self.out, "{text}");
        let _ = self.out.flush();
        self.last_draw = None;
    }
}

/// `m:ss`, or `h:mm:ss` from an hour.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The plan's Linux line: how a build turns the depot tree into the game's
/// `game/` dir, and what that costs (design 18 §6).
pub fn game_files_line(plan: &gogo::gog::materialize::MaterializePlan) -> String {
    use gogo::gog::materialize::TreeMethod;
    match plan.method {
        TreeMethod::Reflink => format!(
            "game files: reflink into {} (no extra disk)",
            plan.game_dir.display()
        ),
        TreeMethod::Copy => format!(
            "game files: copy into {} (+{} disk: no reflink between the cache and the {})",
            plan.game_dir.display(),
            human_size(plan.extra_bytes),
            if gogo::macapp::launch::in_library(&plan.game_dir) {
                "library"
            } else {
                "data dir"
            }
        ),
    }
}

/// Run the depot lane's future, rendering every event it sends on `rx` as
/// it comes; the events still queued when it returns are rendered too, in
/// order. Both `gogo depot` and the builds drive the lane through this.
pub async fn drive<T, W: Write + Send + 'static>(
    runner: impl std::future::Future<Output = T>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<DepotEvent>,
    act: &Activity<W>,
) -> T {
    Live::new(rx, act).drive(runner).await
}

/// The depot receiver and an [`Activity`], so the caller's own work can print
/// a line from inside the runner ([`Live::note`]) while [`Live::drive`] owns
/// the terminal. `gogo dlc add`'s lay runs synchronously after the download
/// and never yields, so its lines must reach the terminal as it emits them,
/// not when the runner next awaits; the activity's timer thread keeps the
/// status line moving meanwhile. The lock is never held across an await.
pub struct Live<'a, W: Write + Send + 'static> {
    rx: std::sync::Mutex<&'a mut tokio::sync::mpsc::UnboundedReceiver<DepotEvent>>,
    act: &'a Activity<W>,
}

impl<'a, W: Write + Send + 'static> Live<'a, W> {
    pub fn new(
        rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<DepotEvent>,
        act: &'a Activity<W>,
    ) -> Self {
        Self {
            rx: std::sync::Mutex::new(rx),
            act,
        }
    }

    /// Renders every depot event already queued.
    fn drain(&self) {
        let mut rx = self.rx.lock().unwrap();
        while let Ok(e) = rx.try_recv() {
            self.act.event(&e);
        }
    }

    /// Renders every depot event already queued (a download's `Finished`
    /// precedes what comes after it), then `text` on its own row: the status
    /// line is cleared first.
    pub fn note(&self, text: &str) {
        self.drain();
        self.act.note(text);
    }

    /// A [`ProgressEvent::WorkProgress`], [`ProgressEvent::WorkStarted`] or
    /// [`ProgressEvent::WorkDone`] on the status line, drawn as it comes,
    /// after the depot events queued before it. `false` for any other event.
    pub fn work(&self, event: &ProgressEvent) -> bool {
        if !event.is_live_only() {
            return false;
        }
        self.drain();
        self.act.work(event)
    }

    /// The loop of [`drive`].
    pub async fn drive<T>(&self, runner: impl std::future::Future<Output = T>) -> T {
        tokio::pin!(runner);
        let result = loop {
            tokio::select! {
                res = &mut runner => break res,
                e = std::future::poll_fn(|cx| self.rx.lock().unwrap().poll_recv(cx)) => {
                    if let Some(e) = e {
                        self.act.event(&e);
                    }
                }
            }
        };
        self.drain();
        self.act.finish();
        result
    }
}

/// The status line for a command's steps (#420), and the depot lane's
/// renderer under [`drive`]: the renderer behind a lock, and in live mode a
/// timer thread that keeps a spinner and the elapsed time moving while a step
/// blocks. Lines go through [`Activity::note`] so they land above the status
/// line. Dropping it clears the line.
pub struct Activity<W: Write + Send + 'static = Box<dyn Write + Send>> {
    shared: std::sync::Arc<std::sync::Mutex<Renderer<W>>>,
    start: std::time::Instant,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    timer: Option<std::thread::JoinHandle<()>>,
}

/// What a prompt needs of a live [`Activity`].
trait Pausable: Send + Sync {
    fn pause(&self);
    fn resume(&self);
}

impl<W: Write + Send> Pausable for std::sync::Mutex<Renderer<W>> {
    fn pause(&self) {
        if let Ok(mut r) = self.lock() {
            r.pause();
        }
    }
    fn resume(&self) {
        if let Ok(mut r) = self.lock() {
            r.resume();
        }
    }
}

/// The activities a prompt has to pause, the command's own ([`Activity::new`]).
static REGISTRY: std::sync::Mutex<Vec<std::sync::Weak<dyn Pausable>>> =
    std::sync::Mutex::new(Vec::new());

/// Run an interactive prompt (`dialoguer` or the plan's): every live status
/// line is cleared first and stays cleared while `f` runs, so a prompt never
/// shares a line with a spinner; the spinner draws again afterwards if its
/// step goes on. Use at every prompt site; it costs nothing when no activity
/// is live.
pub fn prompting<T>(f: impl FnOnce() -> T) -> T {
    let live: Vec<std::sync::Arc<dyn Pausable>> = {
        let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        reg.retain(|w| w.strong_count() > 0);
        reg.iter().filter_map(|w| w.upgrade()).collect()
    };
    for p in &live {
        p.pause();
    }
    let out = f();
    for p in &live {
        p.resume();
    }
    out
}

impl Activity {
    /// Where design 03 §4 says: NDJSON on stdout, human progress on stderr.
    pub fn new(mode: Mode) -> Self {
        let out: Box<dyn Write + Send> = match mode {
            Mode::Json => Box::new(std::io::stdout()),
            _ => Box::new(std::io::stderr()),
        };
        let act = Self::with(mode, out);
        act.register();
        act
    }
}

impl<W: Write + Send + 'static> Activity<W> {
    pub fn with(mode: Mode, out: W) -> Self {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Renderer::new(mode, out)));
        let start = std::time::Instant::now();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let timer = (mode == Mode::Human { live: true }).then(|| {
            let (shared, stop) = (shared.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    // Short sleeps, so dropping the activity doesn't wait a
                    // whole redraw interval for the thread to notice.
                    for _ in 0..10 {
                        std::thread::sleep(REDRAW / 10);
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                    }
                    if let Ok(mut r) = shared.lock() {
                        r.tick(start.elapsed());
                    }
                }
            })
        });
        Self {
            shared,
            start,
            stop,
            timer,
        }
    }

    /// Lets [`prompting`] pause this activity; [`Activity::new`] does it.
    fn register(&self) {
        let shared: std::sync::Arc<dyn Pausable> = self.shared.clone();
        REGISTRY
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(std::sync::Arc::downgrade(&shared));
    }

    /// The status line shows it; live mode only.
    pub fn live(&self) -> bool {
        self.timer.is_some()
    }

    /// A depot lane event; see [`Renderer::event`].
    pub fn event(&self, event: &DepotEvent) {
        self.shared
            .lock()
            .unwrap()
            .event(event, self.start.elapsed());
    }

    /// See [`Renderer::task`].
    pub fn task(&self, step: &str, files: Option<Count>, bytes: Option<Count>) {
        self.shared
            .lock()
            .unwrap()
            .task(step, files, bytes, self.start.elapsed());
    }

    /// The spinner alone, for a step whose total isn't known.
    pub fn working(&self, step: &str) {
        self.task(step, None, None);
    }

    /// See [`Renderer::counting`].
    pub fn counting(&self, step: &str, unit: &'static str, done: u64, total: u64) {
        self.shared
            .lock()
            .unwrap()
            .counting(step, unit, done, total, self.start.elapsed());
    }

    /// See [`Renderer::task_done`].
    pub fn done(&self) {
        self.shared.lock().unwrap().task_done();
    }

    /// See [`Renderer::work`].
    pub fn work(&self, event: &ProgressEvent) -> bool {
        self.shared
            .lock()
            .unwrap()
            .work(event, self.start.elapsed())
    }

    /// `text` on its own line above the status line, in either human mode.
    pub fn note(&self, text: &str) {
        self.shared.lock().unwrap().note(text);
    }

    /// A line the caller has always printed, whatever the mode: above the
    /// status line in a human mode, else through `plain` as before (#420).
    pub fn say(&self, text: &str, plain: &dyn Fn(&str)) {
        let mut r = self.shared.lock().unwrap();
        if matches!(r.mode, Mode::Human { .. }) {
            r.note(text);
        } else {
            plain(text);
        }
    }

    /// Run `f` with the spinner and `step` on the status line, then clear it.
    pub fn spin<T>(&self, step: &str, f: impl FnOnce() -> T) -> T {
        self.working(step);
        let out = f();
        self.done();
        out
    }

    /// [`Activity::spin`] around a future.
    pub async fn spin_async<T>(&self, step: &str, f: impl std::future::Future<Output = T>) -> T {
        self.working(step);
        let out = f.await;
        self.done();
        out
    }

    /// One timer tick, run by hand and past the redraw interval, so a test
    /// needn't wait on the timer thread.
    #[cfg(test)]
    fn tick_now(&self) {
        self.shared.lock().unwrap().draw(self.start.elapsed(), true);
    }

    /// Clears the status line.
    pub fn finish(&self) {
        self.shared.lock().unwrap().finish();
    }
}

impl<W: Write + Send + 'static> Drop for Activity<W> {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.timer.take() {
            let _ = t.join();
        }
        if let Ok(mut r) = self.shared.lock() {
            r.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn dl(downloaded: u64, total: u64) -> DepotEvent {
        DepotEvent::Downloading {
            downloaded,
            written: downloaded,
            total_download: total,
            total_size: total,
        }
    }

    fn run() -> Vec<(f64, DepotEvent)> {
        const MB: u64 = 1 << 20;
        vec![
            (
                0.0,
                DepotEvent::Resolved {
                    product_id: 1,
                    title: "Game".into(),
                    build_id: "b".into(),
                    version: "1.0".into(),
                    generation: 2,
                },
            ),
            (
                0.1,
                DepotEvent::Plan {
                    install_dir: PathBuf::from("/c/Game"),
                    download_size: 100 * MB,
                    estimated_download_size: None,
                    cache_complete: false,
                    disk_size: 200 * MB,
                    language: "en-US".into(),
                    requested_language: None,
                    dlcs: vec![],
                    cache: None,
                    game_files: None,
                },
            ),
            (0.2, DepotEvent::Preparing),
            (1.0, DepotEvent::Verifying { fraction: 0.05 }),
            (2.0, DepotEvent::Verifying { fraction: 0.55 }),
            (3.0, DepotEvent::Verifying { fraction: 1.0 }),
            (4.0, dl(10 * MB, 100 * MB)),
            (5.0, dl(20 * MB, 100 * MB)),
            (6.0, dl(60 * MB, 100 * MB)),
            (7.0, dl(100 * MB, 100 * MB)),
            (
                7.5,
                DepotEvent::Finished {
                    install_dir: PathBuf::from("/c/Game"),
                    fetched_files: 3,
                    fetched_bytes: 90 * MB,
                },
            ),
        ]
    }

    fn render(mode: Mode) -> String {
        let mut r = Renderer::new(mode, Vec::new());
        for (t, e) in run() {
            r.event(&e, secs(t));
            r.tick(secs(t + 0.5));
        }
        r.finish();
        String::from_utf8(r.into_inner()).unwrap()
    }

    #[test]
    fn json_is_every_event_unchanged() {
        let out = render(Mode::Json);
        let lines: Vec<serde_json::Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), run().len());
        assert_eq!(lines[0]["type"], "resolved");
        assert_eq!(
            lines[3],
            serde_json::json!({"type": "verifying", "fraction": 0.05})
        );
        assert_eq!(
            lines[6],
            serde_json::json!({"type": "downloading", "downloaded": 10_485_760u64,
                "written": 10_485_760u64, "totalDownload": 104_857_600u64,
                "totalSize": 104_857_600u64})
        );
        assert_eq!(
            lines[10],
            serde_json::json!({"type": "finished", "installDir": "/c/Game",
                "fetchedFiles": 3, "fetchedBytes": 94_371_840u64})
        );
    }

    #[test]
    fn silent_says_nothing() {
        assert_eq!(render(Mode::Silent), "");
    }

    #[test]
    fn plain_is_phase_lines_and_tenths() {
        assert_eq!(
            render(Mode::Human { live: false }),
            "gogo: Game build 1.0 (generation 2)\n\
             gogo: plan /c/Game (download 100.0 MiB, disk 200.0 MiB, en-US)\n\
             gogo: preparing\n\
             gogo: hashing the files already in the cache\n\
             gogo: hashed 50%\n\
             gogo: hashed 100%\n\
             gogo: downloading (10.0 MiB / 100.0 MiB)\n\
             gogo: downloaded 10% (10.0 MiB / 100.0 MiB)\n\
             gogo: downloaded 20% (20.0 MiB / 100.0 MiB, 10.0 MiB/s)\n\
             gogo: downloaded 60% (60.0 MiB / 100.0 MiB, 25.0 MiB/s)\n\
             gogo: downloaded 100% (100.0 MiB / 100.0 MiB, 30.0 MiB/s)\n\
             gogo: fetched 3 file(s), 90.0 MiB (in 0:07)\n"
        );
    }

    #[test]
    fn live_draws_a_status_line_and_clears_it() {
        let out = render(Mode::Human { live: true });
        // The phase lines are the plain ones, without the tenths.
        let lines: Vec<&str> = out
            .split('\n')
            .map(|l| l.rsplit("\r\x1b[K").next().unwrap())
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            lines,
            [
                "gogo: Game build 1.0 (generation 2)",
                "gogo: plan /c/Game (download 100.0 MiB, disk 200.0 MiB, en-US)",
                "gogo: preparing",
                "gogo: hashing the files already in the cache",
                "gogo: downloading (10.0 MiB / 100.0 MiB)",
                "gogo: fetched 3 file(s), 90.0 MiB (in 0:07)",
            ]
        );
        assert!(out.contains("\r\x1b[K⠋ preparing  0:00"), "{out:?}");
        assert!(out.contains("hashing 55%  0:01"), "{out:?}");
        assert!(
            out.contains("downloading 60.0 MiB / 100.0 MiB  25.0 MiB/s  eta 0:01"),
            "{out:?}"
        );
        // Nothing is left on the status line.
        assert!(out.ends_with("(in 0:07)\n"), "{out:?}");
    }

    #[test]
    fn a_silent_phase_still_moves() {
        let mut r = Renderer::new(Mode::Human { live: true }, Vec::new());
        r.event(&DepotEvent::Preparing, secs(0.0));
        r.tick(secs(65.0));
        r.tick(secs(65.05)); // within the redraw interval: no redraw
        r.tick(secs(3700.0));
        let out = String::from_utf8(r.into_inner()).unwrap();
        assert!(out.contains("⠙ preparing  1:05"), "{out:?}");
        assert!(out.contains("⠹ preparing  1:01:40"), "{out:?}");
        assert_eq!(out.matches("preparing  ").count(), 3);
    }

    #[test]
    fn a_whole_cache_says_nothing_was_fetched() {
        let mut r = Renderer::new(Mode::Human { live: false }, Vec::new());
        r.event(
            &DepotEvent::Finished {
                install_dir: PathBuf::from("/c"),
                fetched_files: 0,
                fetched_bytes: 0,
            },
            secs(1.0),
        );
        assert_eq!(
            String::from_utf8(r.into_inner()).unwrap(),
            "gogo: nothing to fetch: every file was already in the cache\n"
        );
    }

    #[test]
    fn modes_follow_the_flags() {
        assert_eq!(Mode::pick(true, true, true, true, true), Mode::Json);
        assert_eq!(Mode::pick(false, true, false, false, true), Mode::Silent);
        assert_eq!(Mode::pick(false, false, true, false, true), Mode::Silent);
        assert_eq!(
            Mode::pick(false, false, false, false, true),
            Mode::Human { live: true }
        );
        assert_eq!(
            Mode::pick(false, false, false, true, true),
            Mode::Human { live: false }
        );
        assert_eq!(
            Mode::pick(false, false, false, false, false),
            Mode::Human { live: false }
        );
    }

    /// The lane's events all reach the renderer, including those still
    /// queued when the lane returns, in order.
    #[tokio::test]
    async fn drive_forwards_every_event() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let runner = async move {
            for (_, e) in run() {
                tx.send(e).unwrap();
                tokio::task::yield_now().await;
            }
            // Queued after the last poll: only the drain sees these.
            tx.send(DepotEvent::Preparing).unwrap();
            tx.send(DepotEvent::Allocating { fraction: 0.5 }).unwrap();
            42
        };
        let buf = Shared::default();
        let act = Activity::with(Mode::Json, buf.clone());
        assert_eq!(drive(runner, &mut rx, &act).await, 42);
        drop(act);
        let out = buf.text();
        let types: Vec<String> = out
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            types,
            [
                "resolved",
                "plan",
                "preparing",
                "verifying",
                "verifying",
                "verifying",
                "downloading",
                "downloading",
                "downloading",
                "downloading",
                "finished",
                "preparing",
                "allocating"
            ]
        );
    }

    /// A status line (`\r\x1b[K` + spinner) is only ever followed by another
    /// clear: no text, no newline, and not the end of the output.
    fn no_text_after_a_status(out: &str) {
        let segs: Vec<&str> = out.split("\r\x1b[K").collect();
        for (i, seg) in segs.iter().enumerate().skip(1) {
            if seg.starts_with(SPINNER) {
                assert!(!seg.contains('\n') && !seg.contains("gogo:"), "{out:?}");
                assert!(i + 1 < segs.len(), "ends on a status: {out:?}");
            }
        }
    }

    /// A writer that records each write's order against marks, shared so the
    /// test can look at it from inside the runner.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
        fn push(&self, s: &str) {
            self.0.lock().unwrap().extend_from_slice(s.as_bytes());
        }
    }

    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// `gogo dlc add`: the download's `Finished` and the lay's notes through
    /// the live renderer. The lay never yields, so each note must reach the
    /// terminal as it is emitted, while the runner is still blocked.
    #[tokio::test]
    async fn notes_reach_the_terminal_while_the_runner_blocks() {
        const MB: u64 = 1 << 20;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let buf = Shared::default();
        let act = Activity::with(Mode::Human { live: true }, buf.clone());
        let live = Live::new(&mut rx, &act);
        let seen = buf.clone();
        let runner = async {
            tx.send(DepotEvent::Preparing).unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
            tx.send(dl(10 * MB, 100 * MB)).unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
            tx.send(dl(100 * MB, 100 * MB)).unwrap();
            tx.send(DepotEvent::Finished {
                install_dir: PathBuf::from("/c"),
                fetched_files: 1,
                fetched_bytes: 100 * MB,
            })
            .unwrap();
            // The lay: synchronous, no await between its lines.
            live.note("gogo: game files: adding 1 file(s)");
            std::thread::sleep(Duration::from_millis(250));
            live.note("gogo: game files: copied 1 / 2");
            let mid = String::from_utf8(seen.0.lock().unwrap().clone()).unwrap();
            // Both lines are out before the runner has returned.
            assert!(mid.contains("copied 1 / 2\n"), "{mid:?}");
        };
        live.drive(runner).await;
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        no_text_after_a_status(&out);
        let fetched = out.find("gogo: fetched 1 file(s)").expect(&out);
        let adding = out.find("gogo: game files: adding").expect(&out);
        let copied = out.find("gogo: game files: copied").expect(&out);
        assert!(fetched < adding && adding < copied, "{out:?}");
        assert!(out.ends_with("copied 1 / 2\n"), "{out:?}");
    }

    /// `--json` / `--quiet` (silent mode) print no notes.
    #[test]
    fn plan_line_marks_a_switch_estimate() {
        const MB: u64 = 1 << 20;
        let plan = |est| DepotEvent::Plan {
            install_dir: PathBuf::from("/c/Game"),
            download_size: 100 * MB,
            estimated_download_size: est,
            cache_complete: false,
            disk_size: 200 * MB,
            language: "pt-BR".into(),
            requested_language: None,
            dlcs: vec![],
            cache: None,
            game_files: None,
        };
        let line = |e: DepotEvent| {
            let mut r = Renderer::new(Mode::Human { live: false }, Vec::new());
            r.event(&e, secs(0.0));
            String::from_utf8(r.into_inner()).unwrap()
        };
        assert!(line(plan(None)).contains("(download 100.0 MiB, disk"));
        let est = line(plan(Some(5 * MB)));
        assert!(
            est.contains("download ~5.0 MiB (estimate: after checking the cache), disk"),
            "{est}"
        );
    }

    /// A complete cache plans no download (#388), with the note why.
    #[test]
    fn plan_line_of_a_complete_cache() {
        const MB: u64 = 1 << 20;
        let e = DepotEvent::Plan {
            install_dir: PathBuf::from("/c/Game"),
            download_size: 100 * MB,
            estimated_download_size: Some(0),
            cache_complete: true,
            disk_size: 200 * MB,
            language: "en-US".into(),
            requested_language: None,
            dlcs: vec![],
            cache: Some("cache holds this build, language and DLC: nothing to fetch".into()),
            game_files: None,
        };
        let mut r = Renderer::new(Mode::Human { live: false }, Vec::new());
        r.event(&e, secs(0.0));
        let out = String::from_utf8(r.into_inner()).unwrap();
        assert!(
            out.contains("(download none (cache complete), disk 200.0 MiB, en-US)"),
            "{out}"
        );
        assert!(out.contains("gogo: cache holds this build"), "{out}");
    }

    #[test]
    fn a_silent_renderer_prints_no_notes() {
        let mut r = Renderer::new(Mode::Silent, Vec::new());
        r.note("gogo: hi");
        assert!(r.into_inner().is_empty());
    }

    /// The last status draw shows the phase's average, not the tail window.
    #[test]
    fn the_final_speed_is_the_average() {
        const MB: u64 = 1 << 20;
        let mut r = Renderer::new(Mode::Human { live: true }, Vec::new());
        r.event(&dl(0, 100 * MB), secs(0.0));
        // 99 MB in the first 20 s, then a slow 1 MB tail over 10 s.
        r.event(&dl(99 * MB, 100 * MB), secs(20.0));
        r.event(&dl(100 * MB, 100 * MB), secs(30.0));
        let out = String::from_utf8(r.into_inner()).unwrap();
        let last = out.rsplit("\r\x1b[K").next().unwrap();
        // 100 MiB / 30 s = 3.3 MiB/s; the window would say 0.1 MiB/s.
        assert!(last.contains("3.3 MiB/s"), "{last:?}");
    }

    /// The status of a task, with the files, the bytes, a rate and the time.
    #[test]
    fn a_task_counts_files_and_bytes_with_a_rate() {
        const MB: u64 = 1 << 20;
        let mut r = Renderer::new(Mode::Human { live: true }, Vec::new());
        r.task(
            "game files",
            Some((0, Some(4))),
            Some((0, Some(40 * MB))),
            secs(0.0),
        );
        r.task(
            "game files",
            Some((2, Some(4))),
            Some((20 * MB, Some(40 * MB))),
            secs(2.0),
        );
        r.tick(secs(3.0));
        r.task_done();
        r.note("gogo: added");
        let out = String::from_utf8(r.into_inner()).unwrap();
        assert!(
            out.contains(
                "\r\x1b[K⠙ game files  2 / 4 files  20.0 MiB / 40.0 MiB  10.0 MiB/s  0:02"
            ),
            "{out:?}"
        );
        assert!(out.contains("0:03"), "the timer moves it: {out:?}");
        no_text_after_a_status(&out);
        assert!(out.ends_with("\r\x1b[Kgogo: added\n"), "{out:?}");
    }

    /// Unknown totals: the counts alone; neither: the spinner and the time.
    #[test]
    fn a_task_without_totals_spins() {
        let mut r = Renderer::new(Mode::Human { live: true }, Vec::new());
        r.task("game files", Some((3, None)), Some((2048, None)), secs(0.0));
        r.task("creating the Wine prefix", None, None, secs(1.0));
        r.tick(secs(66.0));
        r.finish();
        let out = String::from_utf8(r.into_inner()).unwrap();
        assert!(
            out.contains("⠋ game files  3 files  2.0 KiB  0:00"),
            "{out:?}"
        );
        assert!(out.contains("⠙ creating the Wine prefix  0:00"), "{out:?}");
        assert!(out.contains("⠹ creating the Wine prefix  1:05"), "{out:?}");
        assert!(out.ends_with("\r\x1b[K"), "cleared: {out:?}");
    }

    /// Off a terminal, in `--json` and `--quiet`, a task prints nothing: the
    /// output stays what it was before (#420).
    #[test]
    fn a_task_is_live_only() {
        for mode in [Mode::Human { live: false }, Mode::Json, Mode::Silent] {
            let mut r = Renderer::new(mode, Vec::new());
            r.task(
                "game files",
                Some((1, Some(2))),
                Some((1, Some(2))),
                secs(0.0),
            );
            r.tick(secs(1.0));
            assert!(r.work(
                &ProgressEvent::WorkStarted {
                    step: "unpacking".into()
                },
                secs(2.0)
            ));
            r.task_done();
            r.finish();
            assert!(r.into_inner().is_empty(), "{mode:?}");
        }
    }

    /// The core's work events drive the task; others are left to the caller.
    #[test]
    fn work_events_map_to_the_task() {
        let mut r = Renderer::new(Mode::Human { live: true }, Vec::new());
        let progress = ProgressEvent::WorkProgress {
            step: "downloading x.tar.gz".into(),
            files: None,
            files_total: None,
            bytes: Some(1024),
            bytes_total: Some(4096),
        };
        assert!(r.work(&progress, secs(0.0)));
        assert!(r.work(
            &ProgressEvent::WorkDone {
                step: "downloading x.tar.gz".into()
            },
            secs(0.5)
        ));
        assert!(!r.work(&ProgressEvent::PrefixStaged, secs(0.6)));
        // Done: the timer draws nothing more.
        r.tick(secs(5.0));
        let out = String::from_utf8(r.into_inner()).unwrap();
        assert!(
            out.contains("downloading x.tar.gz  1.0 KiB / 4.0 KiB  0:00"),
            "{out:?}"
        );
        assert!(out.ends_with("\r\x1b[K"), "{out:?}");
        assert_eq!(out.matches("downloading").count(), 1, "{out:?}");
    }

    /// The activity's timer thread keeps a blocked step's status moving, and
    /// the line is gone when the step returns.
    #[test]
    fn an_activity_spins_while_the_step_blocks() {
        let buf = Shared::default();
        let act = Activity::with(Mode::Human { live: true }, buf.clone());
        assert!(act.live());
        let v = act.spin("removing Game", || {
            act.tick_now();
            7
        });
        assert_eq!(v, 7);
        act.note("removed /g");
        drop(act);
        let out = buf.text();
        assert!(out.matches("removing Game  0:00").count() >= 2, "{out:?}");
        no_text_after_a_status(&out);
        assert!(out.ends_with("removed /g\n"), "{out:?}");
    }

    /// Off a terminal an activity has no timer and prints only notes.
    #[test]
    fn a_plain_activity_prints_notes_only() {
        let buf = Shared::default();
        let act = Activity::with(Mode::Human { live: false }, buf.clone());
        assert!(!act.live());
        act.spin("removing Game", || {
            std::thread::sleep(Duration::from_millis(150))
        });
        act.task("hashing a.bin", None, Some((1, Some(2))));
        act.note("  ok a.bin");
        drop(act);
        assert_eq!(buf.text(), "  ok a.bin\n");
    }

    /// `gogo dlc add`'s lay: the files' running count on the status line
    /// while the runner blocks, after the download's last line; gone before
    /// the next note.
    #[tokio::test]
    async fn the_lay_counts_while_the_runner_blocks() {
        const MB: u64 = 1 << 20;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let buf = Shared::default();
        let act = Activity::with(Mode::Human { live: true }, buf.clone());
        let live = Live::new(&mut rx, &act);
        let seen = buf.clone();
        let count = |files, bytes| ProgressEvent::WorkProgress {
            step: "game files".into(),
            files: Some(files),
            files_total: Some(2),
            bytes: Some(bytes),
            bytes_total: Some(30 * MB),
        };
        let runner = async {
            tx.send(DepotEvent::Finished {
                install_dir: PathBuf::from("/c"),
                fetched_files: 1,
                fetched_bytes: MB,
            })
            .unwrap();
            live.note("gogo: game files: adding 2 file(s) (30.0 MiB) to /g");
            assert!(live.work(&count(1, 10 * MB)));
            let mid = seen.text();
            assert!(
                mid.contains("game files  1 / 2 files  10.0 MiB / 30.0 MiB"),
                "{mid:?}"
            );
            assert!(live.work(&count(2, 30 * MB)));
            assert!(live.work(&ProgressEvent::WorkDone {
                step: "game files".into()
            }));
            assert!(!live.work(&ProgressEvent::PrefixStaged));
            live.note("gogo: snapshot 3 taken (before dlc)");
        };
        live.drive(runner).await;
        drop(act);
        let out = buf.text();
        no_text_after_a_status(&out);
        let fetched = out.find("gogo: fetched 1 file(s)").expect(&out);
        let adding = out.find("game files: adding").expect(&out);
        let counted = out.find("1 / 2 files").expect(&out);
        assert!(fetched < adding && adding < counted, "{out:?}");
        assert!(
            out.ends_with("\r\x1b[Kgogo: snapshot 3 taken (before dlc)\n"),
            "{out:?}"
        );
    }

    /// `--json`, `--json-progress` and `--quiet` keep the lines they always
    /// printed: through the plain sink, not the renderer's writer.
    #[test]
    fn a_silent_or_json_activity_still_says_its_lines() {
        for mode in [Mode::Silent, Mode::Json] {
            let buf = Shared::default();
            let act = Activity::with(mode, buf.clone());
            let said = std::cell::RefCell::new(Vec::new());
            act.say("gogo: note: x", &|l| said.borrow_mut().push(l.to_string()));
            drop(act);
            assert_eq!(said.into_inner(), ["gogo: note: x"], "{mode:?}");
            assert_eq!(buf.text(), "", "{mode:?}");
        }
        for live in [true, false] {
            let buf = Shared::default();
            let act = Activity::with(Mode::Human { live }, buf.clone());
            act.say("gogo: note: x", &|_| {
                panic!("a human mode prints it itself")
            });
            drop(act);
            assert_eq!(
                buf.text().rsplit("\r\x1b[K").next().unwrap(),
                "gogo: note: x\n"
            );
        }
    }

    /// A prompt never shares a line with a spinner: the line is cleared
    /// before it, stays clear while it runs, and the spinner is back after.
    #[test]
    fn a_prompt_clears_the_spinner_and_it_returns_after() {
        let buf = Shared::default();
        let act = Activity::with(Mode::Human { live: true }, buf.clone());
        act.register();
        act.working("planning the build");
        act.tick_now();
        let mark = buf.text().len();
        prompting(|| {
            // A tick while the prompt waits draws nothing.
            act.tick_now();
            let during = buf.text();
            let during = &during[mark..];
            assert!(during.ends_with("\r\x1b[K"), "{during:?}");
            assert!(!during.contains("planning"), "{during:?}");
            // The prompt's own line.
            buf.push("? Proceed? [y/n] y\n");
        });
        act.tick_now();
        act.done();
        drop(act);
        let out = buf.text();
        let after = &out[out.find("? Proceed?").unwrap()..];
        assert!(after.contains("planning the build"), "{out:?}");
        // The question's row held nothing else.
        let q = out.find("? Proceed?").unwrap();
        assert!(out[..q].ends_with("\r\x1b[K"), "{out:?}");
        no_text_after_a_status(&out);
    }

    /// A preflight spinner (#422): a spinner and a count on a terminal; in
    /// every other mode nothing is written, `\r` included.
    #[tokio::test]
    async fn a_preflight_spinner_prints_nothing_off_a_terminal() {
        for mode in [Mode::Silent, Mode::Json, Mode::Human { live: false }] {
            let buf = Shared::default();
            let act = Activity::with(mode, buf.clone());
            let v = act
                .spin_async("checking the library", async {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    5
                })
                .await;
            for i in 0..3 {
                act.counting("fetching game details", "games", i, 3);
            }
            act.done();
            drop(act);
            assert_eq!(v, 5);
            assert_eq!(buf.text(), "", "{mode:?}");
        }
        let buf = Shared::default();
        let act = Activity::with(Mode::Human { live: true }, buf.clone());
        act.spin_async("checking the library", async {
            tokio::time::sleep(Duration::from_millis(250)).await;
        })
        .await;
        std::thread::sleep(Duration::from_millis(120));
        act.counting("fetching game details", "games", 2, 9);
        drop(act);
        let out = buf.text();
        assert!(out.contains("checking the library  0:00"), "{out:?}");
        assert!(
            out.contains("fetching game details  2 / 9 games"),
            "{out:?}"
        );
        no_text_after_a_status(&out);
    }

    /// A line said from inside a spinning step lands on its own row, and
    /// `finish` leaves the row clear for what the command prints next.
    #[tokio::test]
    async fn a_line_said_during_a_spin_has_its_own_row() {
        let buf = Shared::default();
        let act = Activity::with(Mode::Human { live: true }, buf.clone());
        act.spin_async("checking the depot cache", async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            act.say("gogo: cache upgraded", &|_| panic!("human mode"));
            tokio::time::sleep(Duration::from_millis(150)).await;
        })
        .await;
        act.finish();
        let out = buf.text();
        assert!(out.contains("\r\x1b[Kgogo: cache upgraded\n"), "{out:?}");
        assert!(out.ends_with("\r\x1b[K") || out.ends_with('\n'), "{out:?}");
        no_text_after_a_status(&out);
    }

    /// A step that ended while the prompt was open stays gone.
    #[test]
    fn a_step_ended_during_a_prompt_does_not_come_back() {
        let buf = Shared::default();
        let act = Activity::with(Mode::Human { live: true }, buf.clone());
        act.register();
        act.working("planning the build");
        prompting(|| act.done());
        act.tick_now();
        let out = buf.text();
        assert!(out.ends_with("\r\x1b[K"), "{out:?}");
        let n = out.matches("planning").count();
        act.tick_now();
        assert_eq!(buf.text().matches("planning").count(), n);
    }

    /// A count past its known total reads as the total.
    #[test]
    fn a_task_count_never_passes_its_total() {
        let mut r = Renderer::new(Mode::Human { live: true }, Vec::new());
        r.task(
            "game files",
            Some((5, Some(4))),
            Some((5000, Some(4096))),
            secs(0.0),
        );
        let out = String::from_utf8(r.into_inner()).unwrap();
        assert!(
            out.contains("game files  4 / 4 files  4.0 KiB / 4.0 KiB"),
            "{out:?}"
        );
    }

    /// Counts sent per chunk keep one sample per redraw interval, and a
    /// restart from zero (a retry) drops the old rate.
    #[test]
    fn task_samples_stay_bounded_and_a_restart_resets_the_rate() {
        const MB: u64 = 1 << 20;
        let mut r = Renderer::new(Mode::Human { live: true }, Vec::new());
        for i in 0..10_000u64 {
            r.task(
                "dl",
                None,
                Some((i * 1024, Some(100 * MB))),
                secs(i as f64 * 0.0004),
            );
        }
        // 4 s of chunks: at most one sample per 100 ms.
        assert!(r.samples.len() <= 41, "{}", r.samples.len());
        r.task("dl", None, Some((0, Some(100 * MB))), secs(4.5));
        assert_eq!(r.samples.len(), 1);
        assert_eq!(r.dl_first, Some((secs(4.5), 0)));
    }
}
