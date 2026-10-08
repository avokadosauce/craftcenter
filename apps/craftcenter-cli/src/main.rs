//! CraftCenter on the command line.
//!
//! The same core as the desktop app, so the two cannot disagree about what "installed" or
//! "up to date" means — and the only front end that can be driven on a machine with no display.
//!
//! Arguments are parsed by hand. The surface is eleven verbs and five flags, which is less code
//! than a parser dependency and one fewer thing that can change under the program.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use craftcenter_core::{Center, Error, Row, Status};

const USAGE: &str = "\
craftcenter-cli — install and update the Crafting Apps

USAGE
    craftcenter-cli <command> [app] [options]

COMMANDS
    list                 the catalogue, with what is installed and what is available
    check [app]          look up the latest release (every app when none is named)
    install <app>        download, verify and install for the current user
    update [app]         update one app, or every app that has an update
    launch <app>         start an installed app
    remove <app>         uninstall an app
    verify <app>         re-hash what is installed and compare it with what was recorded
    config               show the current settings
    config install-dir <path>     choose where new installs go
    config install-dir --default  go back to the per-user default
    move [app]           move an app, or every misplaced app, into the current install dir
    self-update          replace CraftCenter with a newer build of itself
    paths                where CraftCenter keeps things
    help                 this text

OPTIONS
    --force              ignore the cached release check and look again
    --json               machine-readable output (list only)
    --platform <id>      resolve assets for another platform, e.g. windows-arm64
    --install-dir <path> install into this folder for this run only, without saving the choice
    --default            reset a chosen setting to its default (config install-dir only)

NOTES
    Installs are per-user and never ask for administrator rights.
    Every download is checked against the release's SHA256SUMS.txt before it is installed.
    The only hosts contacted are github.com and api.github.com. There is no telemetry.
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "craftcenter: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    command: String,
    app: Option<String>,
    /// The third positional argument, e.g. the path in `config install-dir <path>`.
    value: Option<String>,
    force: bool,
    json: bool,
    /// Reset a setting to its default; currently only `config install-dir --default` reads this.
    default: bool,
    platform: Option<String>,
    /// Where to install for this run only. `config install-dir` is how a choice is kept.
    install_dir: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut command = None;
    let mut app = None;
    let mut value = None;
    let mut force = false;
    let mut json = false;
    let mut default = false;
    let mut platform = None;
    let mut install_dir = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--force" => force = true,
            "--json" => json = true,
            "--default" => default = true,
            "--platform" => platform = Some(args.next().ok_or("--platform needs a value, for example windows-arm64")?),
            "--install-dir" => install_dir = Some(args.next().ok_or("--install-dir needs a path")?),
            "-h" | "--help" => command = Some("help".to_owned()),
            "-V" | "--version" => command = Some("version".to_owned()),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other if command.is_none() => command = Some(other.to_owned()),
            other if app.is_none() => app = Some(other.to_owned()),
            other if value.is_none() => value = Some(other.to_owned()),
            other => return Err(format!("unexpected argument {other}")),
        }
    }

    Ok(Args { command: command.unwrap_or_else(|| "help".to_owned()), app, value, force, json, default, platform, install_dir })
}

fn run() -> Result<ExitCode, String> {
    let args = parse_args()?;

    match args.command.as_str() {
        "help" => {
            print!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        "version" => {
            println!("craftcenter-cli {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    let mut center = Center::open().map_err(|e| e.to_string())?;
    if let Some(label) = &args.platform {
        let platform = craftcenter_select::Platform::parse(label).ok_or_else(|| format!("{label:?} is not a platform CraftCenter installs for"))?;
        center = center.with_platform(platform);
    }
    if let Some(dir) = &args.install_dir {
        center = center.with_install_dir(std::path::Path::new(dir)).map_err(|e| e.to_string())?;
    }

    match args.command.as_str() {
        "list" => {
            if args.json {
                print_json(&center.rows())
            } else {
                print_table(&center.rows())
            }
            Ok(ExitCode::SUCCESS)
        }
        "check" => cmd_check(&center, args.app.as_deref(), args.force),
        "install" => cmd_install(&center, &need_app(args.app)?),
        "update" => cmd_update(&center, args.app.as_deref()),
        "launch" => {
            let slug = need_app(args.app)?;
            center.launch(&slug).map_err(|e| e.to_string())?;
            println!("started {slug}");
            Ok(ExitCode::SUCCESS)
        }
        "remove" => {
            let slug = need_app(args.app)?;
            center.remove(&slug).map_err(|e| e.to_string())?;
            println!("removed {slug}");
            Ok(ExitCode::SUCCESS)
        }
        "verify" => {
            let slug = need_app(args.app)?;
            match center.verify(&slug) {
                Ok(()) => {
                    println!("{slug}: matches the digest recorded at install time");
                    Ok(ExitCode::SUCCESS)
                }
                Err(error) => {
                    eprintln!("{slug}: {error}");
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        "config" => cmd_config(&mut center, args.app.as_deref(), args.value.as_deref(), args.default),
        "move" => cmd_move(&center, args.app.as_deref()),
        "self-update" => cmd_self_update(&center),
        "paths" => {
            let paths = center.paths();
            println!("apps      {}", paths.apps.display());
            println!("launchers {}", paths.bin.display());
            println!("state     {}", paths.state.display());
            println!("cache     {}", paths.cache.display());
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other:?}; `craftcenter-cli help` lists them")),
    }
}

fn need_app(app: Option<String>) -> Result<String, String> {
    app.ok_or_else(|| "this command needs an app, for example `photocraft`; `craftcenter-cli list` shows them".to_owned())
}

fn cmd_check(center: &Center, slug: Option<&str>, force: bool) -> Result<ExitCode, String> {
    match slug {
        Some(slug) => {
            let row = center.check(slug, force).map_err(|e| e.to_string())?;
            println!("{:<13} {}", row.app.slug, row.status.label());
            Ok(ExitCode::SUCCESS)
        }
        None => {
            let mut failed = false;
            for (slug, result) in center.check_all(force) {
                match result {
                    Ok(row) => println!("{:<13} {}", slug, row.status.label()),
                    Err(error) => {
                        failed = true;
                        println!("{slug:<13} {error}");
                    }
                }
            }
            Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
        }
    }
}

fn cmd_install(center: &Center, slug: &str) -> Result<ExitCode, String> {
    let mut progress = Reporter::new(slug);
    let installed = center.install(slug, &mut |done, total| progress.update(done, total)).map_err(|e| e.to_string())?;
    progress.finish();
    println!("installed {} {} from {}", slug, installed.version, installed.asset);
    if let Some(note) = center.row_for(slug).ok().and_then(|r| r.choice.and_then(|c| c.note)) {
        println!("note: {note}");
    }
    println!("run it with: {}", installed.launcher);
    Ok(ExitCode::SUCCESS)
}

fn cmd_update(center: &Center, slug: Option<&str>) -> Result<ExitCode, String> {
    if let Some(slug) = slug {
        center.check(slug, true).map_err(|e| e.to_string())?;
        return cmd_install(center, slug);
    }
    center.check_all(false);
    let results = center.update_all(&mut |_, _| {});
    if results.is_empty() {
        println!("everything installed is up to date");
        return Ok(ExitCode::SUCCESS);
    }
    let mut failed = false;
    for (slug, result) in results {
        match result {
            Ok(installed) => println!("{slug:<13} updated to {}", installed.version),
            Err(error) => {
                failed = true;
                println!("{slug:<13} {error}");
            }
        }
    }
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn cmd_config(center: &mut Center, setting: Option<&str>, value: Option<&str>, use_default: bool) -> Result<ExitCode, String> {
    match setting {
        None => {
            print_config(center);
            Ok(ExitCode::SUCCESS)
        }
        Some("install-dir") => cmd_config_install_dir(center, value, use_default),
        Some(other) => Err(format!("unknown config setting {other:?}")),
    }
}

fn cmd_config_install_dir(center: &mut Center, value: Option<&str>, use_default: bool) -> Result<ExitCode, String> {
    if use_default {
        center.set_install_dir(None).map_err(|e| e.to_string())?;
        println!("install dir reset to {}", center.install_dir().display());
        return Ok(ExitCode::SUCCESS);
    }
    let path = value.ok_or_else(|| "config install-dir needs a path, or --default to reset it".to_owned())?;
    center.set_install_dir(Some(std::path::Path::new(path))).map_err(|e| e.to_string())?;
    println!("install dir set to {}", center.install_dir().display());
    let misplaced = center.misplaced();
    if !misplaced.is_empty() {
        let (count, noun, pronoun) = if misplaced.len() == 1 { (1, "app is", "it") } else { (misplaced.len(), "apps are", "them") };
        println!("{count} {noun} still in the old place; `craftcenter-cli move` will move {pronoun}");
    }
    Ok(ExitCode::SUCCESS)
}

fn print_config(center: &Center) {
    let settings = center.settings();
    println!("{:<20} {}h", "check interval", settings.check_interval_hours);
    println!("{:<20} {}", "keep previous", settings.keep_previous);
    println!("{:<20} {}", "theme", settings.theme);
    let install_dir = center.install_dir();
    println!("{:<20} {}", "install dir", install_dir.display());
    let default_dir = center.default_install_dir();
    if install_dir != default_dir {
        println!("{:<20} {}", "default install dir", default_dir.display());
    }
}

fn cmd_move(center: &Center, slug: Option<&str>) -> Result<ExitCode, String> {
    let slugs = match slug {
        Some(slug) => vec![slug.to_owned()],
        None => center.misplaced(),
    };
    if slugs.is_empty() {
        println!("nothing to move; every installed app is already in the current install dir");
        return Ok(ExitCode::SUCCESS);
    }

    let mut failed = false;
    for slug in slugs {
        let mut progress = Reporter::new(&slug);
        let result = center.move_app(&slug, &mut |done, total| progress.update(done, total));
        progress.finish();
        match result {
            Ok(installed) => println!("moved {slug} to {}", installed.dir),
            Err(error) => {
                failed = true;
                println!("{slug}: {error}");
            }
        }
    }
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn cmd_self_update(center: &Center) -> Result<ExitCode, String> {
    let mut progress = Reporter::new("craftcenter");
    let result = center.self_update(&mut |done, total| progress.update(done, total));
    progress.finish();
    match result {
        Ok(update) if !update.restart_required => {
            println!("craftcenter {} is the newest build", update.from);
            Ok(ExitCode::SUCCESS)
        }
        Ok(update) => {
            println!("craftcenter {} -> {}; restart it to run the new build", update.from, update.to);
            Ok(ExitCode::SUCCESS)
        }
        Err(Error::NoRelease { .. }) => {
            println!("craftcenter has published no release yet, so there is nothing to update to");
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => Err(error.to_string()),
    }
}

/// A one-line download progress report, and only when someone is watching: piped output stays
/// clean for scripts.
struct Reporter {
    label: String,
    interactive: bool,
    last: u64,
    any: bool,
}

impl Reporter {
    fn new(label: &str) -> Self {
        Self { label: label.to_owned(), interactive: std::io::stderr().is_terminal(), last: 0, any: false }
    }

    fn update(&mut self, done: u64, total: Option<u64>) {
        self.any = true;
        if !self.interactive {
            return;
        }
        // Redraw at most once per mebibyte, so a fast download does not become a write storm.
        if done.saturating_sub(self.last) < 1_048_576 && Some(done) != total {
            return;
        }
        self.last = done;
        let mut stderr = std::io::stderr();
        let _ = match total {
            Some(total) if total > 0 => {
                let percent = done.saturating_mul(100) / total;
                write!(stderr, "\r{}: {percent:>3}%  {:.1} of {:.1} MiB   ", self.label, mib(done), mib(total))
            }
            _ => write!(stderr, "\r{}: {:.1} MiB   ", self.label, mib(done)),
        };
        let _ = stderr.flush();
    }

    fn finish(&mut self) {
        if self.interactive && self.any {
            let _ = writeln!(std::io::stderr());
        }
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn print_table(rows: &[Row]) {
    println!("{:<13} {:<34} {:<10} {:<10} STATUS", "APP", "WHAT IT IS FOR", "INSTALLED", "LATEST");
    for row in rows {
        let installed = row.installed.as_ref().map(|i| i.version.clone()).unwrap_or_else(|| "-".to_owned());
        let latest = row.latest.as_ref().map(|r| r.version.clone()).unwrap_or_else(|| "-".to_owned());
        println!("{:<13} {:<34} {:<10} {:<10} {}", row.app.slug, truncate(&row.app.tagline, 34), installed, latest, row.status.label());
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let keep = width.saturating_sub(1);
    text.chars().take(keep).collect::<String>() + "\u{2026}"
}

/// Deliberately hand-written rather than pulled through a serialiser: the shape is this program's
/// contract with whatever reads it, so it should be visible here.
fn print_json(rows: &[Row]) {
    println!("[");
    for (index, row) in rows.iter().enumerate() {
        let comma = if index + 1 == rows.len() { "" } else { "," };
        println!(
            "  {{\"slug\": {}, \"name\": {}, \"installed\": {}, \"latest\": {}, \"status\": {}, \"asset\": {}}}{comma}",
            json_string(&row.app.slug),
            json_string(&row.app.name),
            row.installed.as_ref().map(|i| json_string(&i.version)).unwrap_or_else(|| "null".to_owned()),
            row.latest.as_ref().map(|r| json_string(&r.version)).unwrap_or_else(|| "null".to_owned()),
            json_string(status_id(&row.status)),
            row.choice.as_ref().map(|c| json_string(&c.asset)).unwrap_or_else(|| "null".to_owned()),
        );
    }
    println!("]");
}

fn status_id(status: &Status) -> &'static str {
    match status {
        Status::NoRelease => "no-release",
        Status::Available => "available",
        Status::UpToDate => "up-to-date",
        Status::UpdateAvailable { .. } => "update-available",
        Status::Unavailable { .. } => "unavailable",
        Status::Unchecked => "unchecked",
    }
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("line\nbreak"), "\"line\\nbreak\"");
        assert_eq!(json_string("bell\u{7}"), "\"bell\\u0007\"");
    }

    #[test]
    fn taglines_are_truncated_with_an_ellipsis_not_clipped_mid_character() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("0123456789", 5), "0123\u{2026}");
        // Multi-byte input must not be cut inside a character.
        assert_eq!(truncate("\u{e9}\u{e9}\u{e9}\u{e9}", 2), "\u{e9}\u{2026}");
    }

    #[test]
    fn megabytes_are_reported_in_mebibytes() {
        assert!((mib(1_048_576) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_usage_text_names_every_command_the_parser_accepts() {
        for command in ["list", "check", "install", "update", "launch", "remove", "verify", "config", "move", "self-update", "paths", "help"] {
            assert!(USAGE.contains(command), "{command} is not documented");
        }
    }

    #[test]
    fn the_usage_text_says_what_the_program_will_not_do() {
        assert!(USAGE.contains("never ask for administrator rights"));
        assert!(USAGE.contains("SHA256SUMS.txt"));
        assert!(USAGE.contains("no telemetry"));
    }
}
