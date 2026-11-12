//! `gogo linux-app snapshot <gogid> list|create|delete|auto` (design 20 §8):
//! the flags, the question and the output; the work is
//! `gogo::linuxapp::snapshots`. Refusals exit 7 with nothing touched, a bad id
//! or snapshot number exits 2, and a change without `--yes` and without a
//! terminal prints its plan and exits 2 (design 03 §3, §5).

use gogo::linuxapp::snapshots::{self as snap, Backends};

use crate::{AppError, cli};

/// Dispatch `gogo linux-app snapshot <gogid> <cmd>`.
pub fn run(gogid: String, cmd: cli::LinuxSnapshotCmd) -> Result<i32, AppError> {
    if let Err(e) = gogo::macapp::compat::validate_gogid(&gogid) {
        eprintln!("gogo: {e}");
        return Ok(cli::exit::USAGE);
    }
    let keep_auto = crate::load_config_from_env()?.snapshots.keep_auto;
    let backends = Backends::real();
    match cmd {
        cli::LinuxSnapshotCmd::List { no_space, json } => {
            let v = snap::list(&gogid, &backends, keep_auto, !no_space)?;
            if json {
                println!("{}", serde_json::to_string(&v).unwrap());
            } else {
                print!("{}", snap::render_list(&v));
            }
            Ok(cli::exit::OK)
        }
        cli::LinuxSnapshotCmd::Create {
            label,
            yes: _,
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
            let out = snap::create(&gogid, &backends, label.as_deref())?;
            if json {
                println!("{}", serde_json::to_string(&out).unwrap());
                return Ok(cli::exit::OK);
            }
            for l in &out.recovered {
                println!("{l}");
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
                let confirmed = dialoguer::Confirm::new()
                    .with_prompt(plan.question())
                    .default(false)
                    .interact()
                    .unwrap_or(false);
                if !confirmed {
                    eprintln!("gogo: aborted");
                    return Ok(cli::exit::OK);
                }
            }
            let out = snap::delete(&gogid, &backends, &ids)?;
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
        cli::LinuxSnapshotCmd::Auto {
            state,
            yes: _,
            json,
        } => {
            let out = match state {
                None => snap::auto(&gogid, &backends, keep_auto)?,
                Some(s) => {
                    let on = s == cli::OnOff::On;
                    snap::plan_auto(&gogid, &backends, on)?;
                    snap::set_auto(&gogid, &backends, on, keep_auto)?
                }
            };
            if json {
                println!("{}", serde_json::to_string(&out).unwrap());
                return Ok(cli::exit::OK);
            }
            for l in &out.recovered {
                println!("{l}");
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

/// A question can be asked: stdin and stderr are terminals.
fn interactive() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr())
}
