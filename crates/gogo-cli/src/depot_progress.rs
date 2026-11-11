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

use std::collections::VecDeque;
use std::io::Write;
use std::time::Duration;

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

    /// From the process's flags, writing where design 03 §4 says: NDJSON on
    /// stdout, human progress on stderr.
    pub fn renderer(self) -> Renderer<Box<dyn Write + Send>> {
        let out: Box<dyn Write + Send> = match self {
            Mode::Json => Box::new(std::io::stdout()),
            _ => Box::new(std::io::stderr()),
        };
        Renderer::new(self, out)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Preparing,
    Hashing,
    Allocating,
    Downloading,
    Done,
}

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
    /// The tenth last reported in plain mode.
    tenth: u32,
    /// A status line is on screen (live mode).
    drawn: bool,
    last_draw: Option<Duration>,
    spin: usize,
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
            tenth: 0,
            drawn: false,
            last_draw: None,
            spin: 0,
        }
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
                language,
                dlcs,
                game_files,
            } => {
                self.line(&format!(
                    "gogo: plan {} (download {}, disk {}, {}{})",
                    install_dir.display(),
                    human_size(*download_size),
                    human_size(*disk_size),
                    language,
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
                if let Some(plan) = game_files {
                    self.line(&format!("gogo: {}", game_files_line(plan)));
                }
            }
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

    /// Clears the status line; call once the run is over.
    pub fn finish(&mut self) {
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

    /// Bytes per second over the window, once it spans a second.
    fn speed(&self) -> Option<u64> {
        let (t0, d0) = *self.samples.front()?;
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

    /// Redraw the status line (live mode), at most every [`REDRAW`] unless
    /// `force`.
    fn draw(&mut self, now: Duration, force: bool) {
        if !self.live() {
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
/// it comes, and the timer in between; the events still queued when it
/// returns are rendered too, in order. Both `gogo depot` and the builds
/// drive the lane through this.
pub async fn drive<T, W: Write>(
    runner: impl std::future::Future<Output = T>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<DepotEvent>,
    renderer: &mut Renderer<W>,
) -> T {
    let start = tokio::time::Instant::now();
    let mut timer = tokio::time::interval(REDRAW);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::pin!(runner);
    let result = loop {
        tokio::select! {
            res = &mut runner => break res,
            Some(e) = rx.recv() => renderer.event(&e, start.elapsed()),
            _ = timer.tick() => renderer.tick(start.elapsed()),
        }
    };
    while let Ok(e) = rx.try_recv() {
        renderer.event(&e, start.elapsed());
    }
    renderer.finish();
    result
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
                    disk_size: 200 * MB,
                    language: "en-US".into(),
                    dlcs: vec![],
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
        let mut r = Renderer::new(Mode::Json, Vec::new());
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
        assert_eq!(drive(runner, &mut rx, &mut r).await, 42);
        let out = String::from_utf8(r.into_inner()).unwrap();
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
}
