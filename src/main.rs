use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use plexfilerenamer::config::{Collision, Config, DEFAULT_TOML};
use plexfilerenamer::db;
use plexfilerenamer::exec::{self, Mode};
use plexfilerenamer::plan::{self, Filter, Select};
use plexfilerenamer::script::{self, Shell};
use plexfilerenamer::template::Engine;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Rename and reorganise media files from the metadata already matched in Plex.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Config file (default: ./plexfilerenamer.toml if present, else built-in defaults).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// More logs (-v, -vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Common {
    /// Path to com.plexapp.plugins.library.db (opened read-only).
    db: PathBuf,
    /// Force a profile instead of matching by library.
    #[arg(long)]
    profile: Option<String>,
    /// Restrict to libraries (name or id). Repeatable.
    #[arg(long)]
    library: Vec<String>,
    /// Filter: year<2000, title~^Star, audio.lang=fr, resolution=576p. Repeatable (AND).
    #[arg(long)]
    filter: Vec<Filter>,
    /// Plex id (movie, episode or show) or title substring.
    #[arg(long)]
    item: Option<String>,
    /// Stop after N items.
    #[arg(long)]
    limit: Option<usize>,
    /// Override the config's collision strategy.
    #[arg(long, value_enum)]
    collision: Option<Collision>,
    /// Open the DB with immutable=1 (for a copy). Automatic when there is no -wal file.
    #[arg(long)]
    immutable: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a commented config with every default.
    Init {
        #[arg(short, long, default_value = "plexfilerenamer.toml")]
        output: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// List Plex libraries.
    Libraries {
        db: PathBuf,
        #[arg(long)]
        immutable: bool,
    },
    /// Dump every template variable of an item as JSON.
    Vars {
        #[command(flatten)]
        c: Common,
    },
    /// Render a template (or the profile's full target path) for matching items.
    Render {
        #[command(flatten)]
        c: Common,
        #[arg(long)]
        template: Option<String>,
    },
    /// Dry run: before/after for every file.
    Plan {
        #[command(flatten)]
        c: Common,
        /// Hide files already at the right place.
        #[arg(long)]
        only_changed: bool,
        /// Machine-readable plan.
        #[arg(long)]
        json: bool,
    },
    /// Write a shell script performing the moves.
    Script {
        #[command(flatten)]
        c: Common,
        #[arg(long, value_enum, default_value = "bash")]
        shell: Shell,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Perform the plan (asks for confirmation).
    Apply {
        #[command(flatten)]
        c: Common,
        #[arg(long, value_enum, default_value = "move")]
        mode: Mode,
        /// Don't ask for confirmation.
        #[arg(long)]
        yes: bool,
        /// Journal path (default: ./plexfilerenamer-journal-<timestamp>.jsonl).
        #[arg(long)]
        journal: Option<PathBuf>,
        /// Remove source folders left empty (overrides config).
        #[arg(long)]
        clean_empty_dirs: bool,
    },
    /// Revert an apply from its journal.
    Undo {
        journal: PathBuf,
        #[arg(long)]
        yes: bool,
    },
}

fn confirm(q: &str) -> Result<bool> {
    eprint!("{q} [y/N] ");
    std::io::stderr().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    Ok(matches!(s.trim(), "y" | "Y" | "yes" | "oui" | "o"))
}

struct Ctx {
    cfg: Config,
    engine: Engine,
    loaded: Vec<plan::Loaded>,
}

fn load(cli_cfg: Option<&Path>, c: &Common) -> Result<Ctx> {
    let mut cfg = Config::load(cli_cfg)?;
    if let Some(s) = c.collision {
        cfg.collision = s;
    }
    let engine = Engine::new(&cfg);
    let db = db::open(&c.db, c.immutable)?;
    let sel = Select {
        profile: c.profile.clone(),
        libraries: c.library.clone(),
        filters: c.filter.clone(),
        limit: c.limit,
        item: c.item.clone(),
    };
    let loaded = plan::load(&db, &cfg, &engine, &sel)?;
    Ok(Ctx {
        cfg,
        engine,
        loaded,
    })
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => tracing::Level::WARN,
        1 => tracing::Level::INFO,
        _ => tracing::Level::DEBUG,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .init();
    let cfgp = cli.config.as_deref();
    match cli.cmd {
        Cmd::Init { output, force } => {
            if output.exists() && !force {
                bail!("{} exists (use --force to overwrite)", output.display());
            }
            std::fs::write(&output, DEFAULT_TOML)?;
            println!("wrote {}", output.display());
        }
        Cmd::Libraries { db, immutable } => {
            for l in db::open(&db, immutable)?.libraries()? {
                let kind = match l.section_type {
                    1 => "movies",
                    2 => "shows",
                    8 => "music",
                    13 => "photos",
                    _ => "other",
                };
                println!(
                    "{:>4}  {:<7} {}  [{}]",
                    l.id,
                    kind,
                    l.name,
                    l.roots.join(", ")
                );
            }
        }
        Cmd::Vars { c } => {
            if c.item.is_none() {
                bail!("--item <id|title> is required");
            }
            let x = load(cfgp, &c)?;
            let all: Vec<_> = x.loaded.iter().map(|l| &l.ctx).collect();
            match all.as_slice() {
                [] => bail!("no item matches"),
                [one] => println!("{}", serde_json::to_string_pretty(one)?),
                many => println!("{}", serde_json::to_string_pretty(many)?),
            }
        }
        Cmd::Render { c, template } => {
            let x = load(cfgp, &c)?;
            for l in &x.loaded {
                let out = match (&template, &l.profile) {
                    (Some(t), _) => x.engine.render_raw(t, &l.ctx).map(|r| {
                        r.split('/')
                            .map(|s| x.engine.finish_segment(s))
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join("/")
                    }),
                    (None, Some(p)) => x
                        .engine
                        .render_path(&x.cfg.profiles[p], &l.ctx, &l.item.ext)
                        .map(|r| {
                            let mut v = r.dirs;
                            v.push(if l.item.ext.is_empty() {
                                r.stem
                            } else {
                                format!("{}.{}", r.stem, l.item.ext)
                            });
                            v.join("/")
                        }),
                    (None, None) => Err(anyhow::anyhow!(
                        "no profile for library {:?}",
                        l.item.library
                    )),
                };
                match out {
                    Ok(s) => println!("{s}"),
                    Err(e) => println!("[{}] error: {e:#}", l.item.ids.plex),
                }
            }
        }
        Cmd::Plan {
            c,
            only_changed,
            json,
        } => {
            let x = load(cfgp, &c)?;
            let mut p = plan::build(&x.loaded, &x.cfg, &x.engine);
            if json {
                if only_changed {
                    p.ops.retain(|o| o.status != plan::Status::Unchanged);
                }
                println!("{}", serde_json::to_string_pretty(&p)?);
            } else {
                plan::print(&p, only_changed);
            }
        }
        Cmd::Script { c, shell, output } => {
            let x = load(cfgp, &c)?;
            let p = plan::build(&x.loaded, &x.cfg, &x.engine);
            if p.conflicts() > 0 {
                plan::print(&p, true);
                bail!(
                    "{} conflicts: fix them or set collision = \"skip\" / \"suffix\"",
                    p.conflicts()
                );
            }
            if output.exists() {
                bail!("{} exists, not overwriting", output.display());
            }
            std::fs::write(&output, script::generate(&p, shell))?;
            #[cfg(unix)]
            if shell == Shell::Bash {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o755))?;
            }
            println!(
                "wrote {} ({} operations)",
                output.display(),
                p.count(plan::Status::Rename)
            );
        }
        Cmd::Apply {
            c,
            mode,
            yes,
            journal,
            clean_empty_dirs,
        } => {
            let x = load(cfgp, &c)?;
            let p = plan::build(&x.loaded, &x.cfg, &x.engine);
            plan::print(&p, true);
            if p.conflicts() > 0 {
                bail!(
                    "{} conflicts: nothing done. Fix them or set collision = \"skip\" / \"suffix\"",
                    p.conflicts()
                );
            }
            let n = p.count(plan::Status::Rename);
            if n == 0 {
                println!("nothing to do");
                return Ok(());
            }
            if !yes && !confirm(&format!("Apply {n} operations ({mode:?})?"))? {
                bail!("aborted");
            }
            let jp = journal.unwrap_or_else(|| {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                PathBuf::from(format!("plexfilerenamer-journal-{ts}.jsonl"))
            });
            let mut j = exec::Journal::create(&jp)?;
            let rep = exec::apply(&p, mode, &mut j, clean_empty_dirs || x.cfg.clean_empty_dirs);
            println!(
                "{} done, {} errors. Journal: {}",
                rep.done,
                rep.errors.len(),
                jp.display()
            );
            for e in &rep.errors {
                eprintln!("  {e}");
            }
            if !rep.errors.is_empty() {
                std::process::exit(2);
            }
        }
        Cmd::Undo { journal, yes } => {
            let entries = exec::read_journal(&journal)?;
            if !yes
                && !confirm(&format!(
                    "Revert {} journal entries from {}?",
                    entries.len(),
                    journal.display()
                ))?
            {
                bail!("aborted");
            }
            let rep = exec::undo(&entries);
            println!("{} reverted, {} errors", rep.done, rep.errors.len());
            for e in &rep.errors {
                eprintln!("  {e}");
            }
            if !rep.errors.is_empty() {
                std::process::exit(2);
            }
        }
    }
    Ok(())
}
