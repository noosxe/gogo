//! `gogo linux-app snapshot <gogid> list|create|rollback|delete|auto` (design 20 §8):
//! the flags, the questions (a plain dir's conversion, §2.1) and the output; the work is
//! `gogo::linuxapp::snapshots`. Refusals exit 7 with nothing touched, a bad id
//! or snapshot number exits 2, and a change without `--yes` and without a
//! terminal prints its plan and exits 2 (design 03 §3, §5).

use gogo::linuxapp::rollback::{self as rb, Planned, RollbackOptions};
use gogo::linuxapp::snapshots::{self as snap, Backends};

use crate::depot_progress::Activity;
use crate::{AppError, cli};

/// Dispatch `gogo linux-app snapshot <gogid> <cmd>`.
pub fn run(gogid: String, cmd: cli::LinuxSnapshotCmd) -> Result<i32, AppError> {
    if let Err(e) = gogo::macapp::compat::validate_gogid(&gogid) {
        eprintln!("gogo: {e}");
        return Ok(cli::exit::USAGE);
    }
    let keep_auto = crate::load_config_from_env()?.snapshots.keep_auto;
    let backends = Backends::real();
    // A rollback that stopped is settled before anything else (design 20
    // §5.3.1); `list` does it itself and notes it.
    if !matches!(cmd, cli::LinuxSnapshotCmd::List { .. }) {
        for l in rb::settle_pending(&gogid, &backends)? {
            eprintln!("{l}");
        }
    }
    match cmd {
        cli::LinuxSnapshotCmd::List { no_space, json } => {
            // Measuring walks the extents of the game and every snapshot.
            let v = if no_space {
                snap::list(&gogid, &backends, keep_auto, false)?
            } else {
                Activity::new(crate::depot_mode(false, json))
                    .spin("measuring the snapshots' space", || {
                        snap::list(&gogid, &backends, keep_auto, true)
                    })?
            };
            if json {
                println!("{}", serde_json::to_string(&v).unwrap());
            } else {
                print!("{}", snap::render_list(&v));
            }
            Ok(cli::exit::OK)
        }
        cli::LinuxSnapshotCmd::Create {
            label,
            yes,
            dry_run,
            json,
        } => {
            let plan = snap::plan_create(&gogid, &backends, label.as_deref())?;
            if dry_run {
                if json {
                    println!("{}", serde_json::to_string(&plan).unwrap());
                } else {
                    println!("would take a manual snapshot:");
                    for l in plan.lines() {
                        println!("  {l}");
                    }
                }
                return Ok(cli::exit::OK);
            }
            if plan.convert.is_some()
                && let Some(code) = ask(
                    &plan.lines(),
                    yes,
                    &format!("convert {} and take the snapshot?", plan.game_dir.display()),
                )
            {
                return Ok(code);
            }
            let act = Activity::new(crate::depot_mode(false, json));
            let progress = progress(json, &act);
            let conv = snap::Convert {
                progress: &progress,
                ..Default::default()
            };
            let out = snap::create_with(
                &gogid,
                &backends,
                label.as_deref(),
                plan.convert.is_some().then_some(&conv),
            )?;
            if json {
                println!("{}", serde_json::to_string(&out).unwrap());
                return Ok(cli::exit::OK);
            }
            for l in &out.recovered {
                println!("{l}");
            }
            if let Some(c) = &out.conversion {
                print_converted(c);
            }
            println!(
                "snapshot {} of {gogid} taken ({})",
                out.snapshot.id,
                out.snapshot.label.as_deref().unwrap_or("manual")
            );
            Ok(cli::exit::OK)
        }
        cli::LinuxSnapshotCmd::Delete {
            ids,
            yes,
            dry_run,
            json,
        } => {
            let plan = snap::plan_delete(&gogid, &backends, &ids)?;
            if !plan.unknown.is_empty() {
                let ns: Vec<String> = plan.unknown.iter().map(u32::to_string).collect();
                eprintln!(
                    "gogo: {gogid} has no snapshot {} (see `gogo linux-app snapshot {gogid} list`)",
                    ns.join(", ")
                );
                return Ok(cli::exit::USAGE);
            }
            if dry_run {
                if json {
                    println!("{}", serde_json::to_string(&plan).unwrap());
                } else {
                    println!("would delete:");
                    for l in plan.lines() {
                        println!("  {l}");
                    }
                }
                return Ok(cli::exit::OK);
            }
            if !yes {
                eprintln!("to delete:");
                for l in plan.lines() {
                    eprintln!("  {l}");
                }
                if !interactive() {
                    eprintln!("gogo: non-interactive without --yes — aborting (plan above)");
                    return Ok(cli::exit::USAGE);
                }
                let confirmed = crate::depot_progress::prompting(|| {
                    dialoguer::Confirm::new()
                        .with_prompt(plan.question())
                        .default(false)
                        .interact()
                })
                .unwrap_or(false);
                if !confirmed {
                    eprintln!("gogo: aborted");
                    return Ok(cli::exit::OK);
                }
            }
            let act = Activity::new(crate::depot_mode(false, json));
            let out = act.spin(&format!("deleting {} snapshot(s)", ids.len()), || {
                snap::delete(&gogid, &backends, &ids)
            });
            drop(act);
            let out = out?;
            if json {
                println!("{}", serde_json::to_string(&out).unwrap());
                return Ok(cli::exit::OK);
            }
            for l in &out.recovered {
                println!("{l}");
            }
            for n in &out.deleted {
                println!("deleted snapshot {n} of {gogid}");
            }
            Ok(cli::exit::OK)
        }
        cli::LinuxSnapshotCmd::Rollback {
            id,
            drop_saves,
            keep_override,
            yes,
            dry_run,
            verbose,
            json,
        } => rollback(
            &gogid,
            &backends,
            id,
            RollbackOptions {
                drop_saves,
                keep_override,
                keep_auto,
                ..RollbackOptions::default()
            },
            Flags {
                yes,
                dry_run,
                verbose,
                json,
            },
        ),
        cli::LinuxSnapshotCmd::Auto {
            state,
            yes,
            dry_run,
            json,
        } => {
            let out = match state {
                None => snap::auto(&gogid, &backends, keep_auto)?,
                Some(s) => {
                    let on = s == cli::OnOff::On;
                    let plan = snap::plan_auto(&gogid, &backends, on)?;
                    if dry_run {
                        if json {
                            println!("{}", serde_json::to_string(&plan).unwrap());
                        } else {
                            println!("would turn:");
                            for l in plan.lines() {
                                println!("  {l}");
                            }
                        }
                        return Ok(cli::exit::OK);
                    }
                    if plan.convert.is_some()
                        && let Some(code) = ask(
                            &plan.lines(),
                            yes,
                            &format!(
                                "convert {} and turn autosnapshot on?",
                                plan.game_dir.display()
                            ),
                        )
                    {
                        return Ok(code);
                    }
                    let act = Activity::new(crate::depot_mode(false, json));
                    let progress = progress(json, &act);
                    let conv = snap::Convert {
                        progress: &progress,
                        ..Default::default()
                    };
                    snap::set_auto_with(
                        &gogid,
                        &backends,
                        on,
                        keep_auto,
                        plan.convert.is_some().then_some(&conv),
                    )?
                }
            };
            if json {
                println!("{}", serde_json::to_string(&out).unwrap());
                return Ok(cli::exit::OK);
            }
            for l in &out.recovered {
                println!("{l}");
            }
            if let Some(c) = &out.conversion {
                print_converted(c);
            }
            if out.auto {
                println!("autosnapshot: on, keeping {} automatic", out.keep_auto);
            } else {
                println!("autosnapshot: off");
            }
            Ok(cli::exit::OK)
        }
    }
}

/// `rollback`'s output flags.
struct Flags {
    yes: bool,
    dry_run: bool,
    verbose: bool,
    json: bool,
}

/// `snapshot <gogid> rollback <n>` (design 20 §5.3, §6, §8): the plan first,
/// then the question, then the rollback; its last line names the snapshot
/// taken before and where the newer saves went.
fn rollback(
    gogid: &str,
    backends: &Backends,
    n: u32,
    mut opts: RollbackOptions,
    f: Flags,
) -> Result<i32, AppError> {
    let act = Activity::new(crate::depot_mode(false, f.json));
    let planned = act.spin("planning the rollback", || {
        if let Some(want) = rb::depot_fetch_wanted(gogid) {
            opts.build_files = gogo::macapp::lutris::block_on_detached(fetch_build_files(want));
        }
        rb::plan(gogid, backends, n, &opts)
    });
    let plan = match planned? {
        Planned::Plan(p) => p,
        Planned::Unknown(why) => {
            eprintln!("gogo: {gogid}: {why} (see `gogo linux-app snapshot {gogid} list`)");
            return Ok(cli::exit::USAGE);
        }
    };
    if f.dry_run {
        if f.json {
            println!("{}", serde_json::to_string(&plan).unwrap());
        } else {
            println!("would do:");
            for l in plan.lines(f.verbose) {
                println!("{l}");
            }
        }
        return Ok(cli::exit::OK);
    }
    if !f.yes {
        for l in plan.lines(f.verbose) {
            eprintln!("{l}");
        }
        if !interactive() {
            eprintln!("gogo: non-interactive without --yes — aborting (plan above)");
            return Ok(cli::exit::USAGE);
        }
        let confirmed = crate::depot_progress::prompting(|| {
            dialoguer::Confirm::new()
                .with_prompt(plan.question())
                .default(false)
                .interact()
        })
        .unwrap_or(false);
        if !confirmed {
            eprintln!("gogo: aborted");
            return Ok(cli::exit::OK);
        }
    } else if !f.json {
        for l in plan.lines(f.verbose) {
            println!("{l}");
        }
    }
    let out = act.spin(&format!("rolling back to snapshot {n}"), || {
        rb::rollback(gogid, backends, n, &plan, &opts)
    });
    drop(act);
    let out = out?;
    if f.json {
        println!("{}", serde_json::to_string(&out).unwrap());
        return Ok(cli::exit::OK);
    }
    for l in &out.recovered {
        println!("{l}");
    }
    println!(
        "snapshot {} taken ({})",
        out.pre,
        gogo::snapshot::labels::rollback(n)
    );
    if let Some(dir) = &out.saves_copied_to {
        println!(
            "{} newer save file(s) copied to {}",
            out.saves_newer.len(),
            dir.display()
        );
    }
    if out.override_restored {
        for c in &out.override_changes {
            println!("override restored: {}", c.line());
        }
    } else if !out.override_changes.is_empty() {
        println!("override kept as it is (--keep-override): the next launch runs setup");
    }
    println!("launcher and menu entry written");
    for p in &out.pruned {
        println!("pruned snapshot {p}");
    }
    for w in &out.warnings {
        eprintln!("warning: {w}");
    }
    println!("{}", out.last_line());
    Ok(cli::exit::OK)
}

/// The build's file list from its depot manifests (design 20 §6.1), for a
/// game whose depot cache doesn't hold its build. `None` (the plan then says
/// it checked without a list) when gogo isn't signed in, GOG can't be reached
/// or no longer lists the build.
async fn fetch_build_files(want: rb::FetchWant) -> Option<gogo::snapshot::saves::BuildFiles> {
    use gogo::gog::{depot, dlc_filter};
    let ctx = crate::session_ctx().await.ok()?;
    let core = ctx.session.as_ref().and_then(|p| p.gog_warp_core())?;
    let depot::InstalledBuild {
        listed: Some((_, _, manifest)),
        ..
    } = depot::installed_build(core, want.product_id, Some(&want.build_id))
        .await
        .ok()?
    else {
        return None;
    };
    let depots = dlc_filter::fetch_depots(
        core.reqwest_client(),
        &manifest,
        Some((&want.products, std::slice::from_ref(&want.language))),
    )
    .await
    .ok()?;
    Some(rb::depots_list(
        &depots,
        &manifest.install_directory(),
        &want.language,
        &want.products,
    ))
}

/// Show `lines` (a plan) and ask `question`, unless `yes`. `None`: go on;
/// `Some(code)`: stop with that exit code (2 without a terminal, 0 on a no).
pub(crate) fn ask(lines: &[String], yes: bool, question: &str) -> Option<i32> {
    if yes {
        return None;
    }
    eprintln!("plan:");
    for l in lines {
        eprintln!("  {l}");
    }
    if !interactive() {
        eprintln!("gogo: non-interactive without --yes — aborting (plan above)");
        return Some(cli::exit::USAGE);
    }
    let confirmed = crate::depot_progress::prompting(|| {
        dialoguer::Confirm::new()
            .with_prompt(question)
            .default(false)
            .interact()
    })
    .unwrap_or(false);
    if confirmed {
        None
    } else {
        eprintln!("gogo: aborted");
        Some(cli::exit::OK)
    }
}

/// A conversion's progress on stderr: on a terminal the live status line
/// with the bytes and a rate (#420), else a line at most once a tenth per
/// phase; nothing with `--json`.
fn progress(json: bool, act: &Activity) -> impl Fn(snap::ConvertProgress) + '_ {
    let shown = std::cell::Cell::new((0u8, 0u64));
    move |p| {
        if json {
            return;
        }
        let (phase, done, total, what) = match p {
            snap::ConvertProgress::Copying { done, total } => (1, done, total, "copying"),
            snap::ConvertProgress::Verifying { done, total } => (2, done, total, "checking"),
        };
        if act.live() {
            act.task(
                &format!("converting: {what}"),
                None,
                Some((done, Some(total))),
            );
            return;
        }
        let tenth = (done.min(total) as u128 * 10 / u128::from(total.max(1))) as u64;
        let (last_phase, last) = shown.get();
        if phase != last_phase || tenth > last {
            shown.set((phase, tenth));
            eprintln!("converting: {what} {}%", tenth * 10);
        }
    }
}

fn print_converted(c: &snap::Converted) {
    for w in &c.warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "converted {} to a btrfs subvolume in {:.1} s ({} files reflinked, {} copied)",
        c.game_dir.display(),
        c.seconds,
        c.reflinked_files,
        c.copied_files
    );
}

/// A question can be asked: stdin and stderr are terminals.
fn interactive() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use gogo::snapshot::fake::Fake;

    const ID: &str = "1999999902";

    /// A game dir as `snapshot` commands find it, under a scratch data root,
    /// with one snapshot. Each nextest test is its own process, so setting
    /// `GOGO_HOME` touches no other test.
    fn world() -> (tempfile::TempDir, Backends) {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: nextest runs each test in its own process.
        unsafe { std::env::set_var("GOGO_HOME", tmp.path()) };
        let root = tmp.path().join("games").join(ID);
        std::fs::create_dir_all(root.join("prefix/drive_c/users/steamuser/Saved Games")).unwrap();
        std::fs::write(root.join("gogo-launch.toml"), "").unwrap();
        let b = Backends::fixed(Arc::new(Fake::new()));
        assert_eq!(snap::create(ID, &b, None).unwrap().snapshot.id, 1);
        std::fs::write(
            root.join("prefix/drive_c/users/steamuser/Saved Games/new.sav"),
            "x",
        )
        .unwrap();
        (tmp, b)
    }

    #[test]
    fn a_rollback_without_a_terminal_or_yes_prints_the_plan_and_exits_2() {
        let (tmp, b) = world();
        let flags = || Flags {
            yes: false,
            dry_run: false,
            verbose: false,
            json: false,
        };
        // Tests run without a terminal on stdin.
        assert!(!interactive());
        let code = rollback(ID, &b, 1, RollbackOptions::default(), flags())
            .unwrap_or_else(|_| panic!("the command failed"));
        assert_eq!(code, cli::exit::USAGE);
        // Nothing happened: no journal, no snapshot before the rollback.
        let meta = tmp.path().join("snapshots").join(ID);
        assert!(!meta.join("rollback.json").exists());
        assert!(!meta.join("2.toml").exists());
        assert!(
            tmp.path()
                .join("games")
                .join(ID)
                .join("prefix/drive_c/users/steamuser/Saved Games/new.sav")
                .is_file()
        );
        // An unknown number is a usage error too, before any plan.
        let code = rollback(ID, &b, 9, RollbackOptions::default(), flags())
            .unwrap_or_else(|_| panic!("the command failed"));
        assert_eq!(code, cli::exit::USAGE);
        // --dry-run prints the plan and stops, exit 0.
        let code = rollback(
            ID,
            &b,
            1,
            RollbackOptions::default(),
            Flags {
                dry_run: true,
                ..flags()
            },
        )
        .unwrap_or_else(|_| panic!("the command failed"));
        assert_eq!(code, cli::exit::OK);
        assert!(!meta.join("2.toml").exists());
    }

    #[test]
    fn a_conversion_without_yes_and_without_a_terminal_exits_2() {
        let plan = ["a plan line".to_string()];
        // Tests run with stdin not a terminal.
        if !interactive() {
            assert_eq!(ask(&plan, false, "convert?"), Some(cli::exit::USAGE));
        }
        assert_eq!(ask(&plan, true, "convert?"), None);
    }
}
