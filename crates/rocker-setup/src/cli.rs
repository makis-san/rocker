//! The management CLI the `rocker` binary dispatches through before it opens a
//! window. Hand-rolled parsing keeps the dependency surface at zero — the verb
//! set is tiny and fixed.

use crate::{
    ChangeVerb, Diagnosis, InstallOptions, Report, Scope, UninstallOptions, BIN_NAME, VERSION,
};

/// What [`run`] decided.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A management verb ran; the process should exit with this code.
    Handled(i32),
    /// Not a management verb — carry on and open the GUI.
    LaunchGui,
}

const HELP: &str = "\
rocker — a native desktop client for the Docker Engine API

usage:
  rocker                     launch the app
  rocker install [options]   install the binary, extension host, and desktop integration
  rocker uninstall [options] remove everything `install` added
  rocker self-update [--check] [--tag vX.Y.Z]
  rocker doctor              report install health and available updates
  rocker --version | --help

install options:
  --system         install into the shared prefix instead of ~/.local (may need sudo)
  --modify-path    add the binary directory to your shell PATH
  --bin-dir <dir>  override where the binary is placed

uninstall options:
  --system         operate on a --system install
  --purge          also delete your config and history
";

/// Inspect `std::env::args()` and, if the first argument is a management verb,
/// run it. Otherwise return [`Outcome::LaunchGui`].
pub fn run() -> anyhow::Result<Outcome> {
    run_args(std::env::args().skip(1))
}

fn run_args<I>(args: I) -> anyhow::Result<Outcome>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    let Some(verb) = args.first().map(String::as_str) else {
        return Ok(Outcome::LaunchGui);
    };
    let rest = &args[1..];

    match verb {
        "install" => {
            let report = crate::install(&parse_install(rest)?)?;
            print_report("install", &report);
            Ok(Outcome::Handled(0))
        }
        "uninstall" => {
            let report = crate::uninstall(&parse_uninstall(rest)?)?;
            print_report("uninstall", &report);
            Ok(Outcome::Handled(0))
        }
        "self-update" | "update" => self_update(rest),
        "doctor" => {
            print_diagnosis(&crate::doctor()?);
            Ok(Outcome::Handled(0))
        }
        "--version" | "-V" | "version" => {
            println!("{BIN_NAME} {VERSION} ({})", env!("ROCKER_TARGET"));
            Ok(Outcome::Handled(0))
        }
        "--help" | "-h" | "help" => {
            print!("{HELP}");
            Ok(Outcome::Handled(0))
        }
        // A URL (docker://…), a file path, or an unknown flag: leave it for the GUI.
        _ => Ok(Outcome::LaunchGui),
    }
}

fn parse_install(args: &[String]) -> anyhow::Result<InstallOptions> {
    let mut opts = InstallOptions::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--system" => opts.scope = Scope::System,
            "--modify-path" => opts.modify_path = true,
            "--bin-dir" => {
                let dir = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--bin-dir needs a path"))?;
                opts.bin_dir = Some(dir.into());
            }
            other => anyhow::bail!("unknown option for `install`: {other}\n\n{HELP}"),
        }
    }
    Ok(opts)
}

fn parse_uninstall(args: &[String]) -> anyhow::Result<UninstallOptions> {
    let mut opts = UninstallOptions::default();
    for arg in args {
        match arg.as_str() {
            "--system" => opts.scope = Scope::System,
            "--purge" => opts.purge = true,
            other => anyhow::bail!("unknown option for `uninstall`: {other}\n\n{HELP}"),
        }
    }
    Ok(opts)
}

#[cfg(feature = "self-update")]
fn self_update(args: &[String]) -> anyhow::Result<Outcome> {
    use crate::update::{run as run_update, UpdateOptions, UpdateOutcome};

    let mut opts = UpdateOptions::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--check" => opts.check_only = true,
            "--tag" => {
                opts.tag = Some(
                    it.next()
                        .ok_or_else(|| anyhow::anyhow!("--tag needs a version"))?
                        .clone(),
                );
            }
            other => anyhow::bail!("unknown option for `self-update`: {other}"),
        }
    }

    match run_update(&opts)? {
        UpdateOutcome::UpToDate { version } => {
            println!("rocker {version} is the latest release.");
            Ok(Outcome::Handled(0))
        }
        UpdateOutcome::UpdateAvailable { current, latest } => {
            println!("update available: {current} -> {latest}");
            println!("run `rocker self-update` to install it.");
            Ok(Outcome::Handled(0))
        }
        UpdateOutcome::Updated { from, to } => {
            println!("updated rocker {from} -> {to}. restart the app to use it.");
            Ok(Outcome::Handled(0))
        }
    }
}

#[cfg(not(feature = "self-update"))]
fn self_update(_args: &[String]) -> anyhow::Result<Outcome> {
    anyhow::bail!("this build was compiled without `self-update`; use the install script")
}

fn symbol(verb: ChangeVerb) -> char {
    match verb {
        ChangeVerb::Created => '+',
        ChangeVerb::Updated => '~',
        ChangeVerb::Removed => '-',
        ChangeVerb::Skipped => '=',
        ChangeVerb::Hook => '>',
        ChangeVerb::Warning => '!',
    }
}

fn print_report(verb: &str, report: &Report) {
    for change in &report.changes {
        print!("  {} {}", symbol(change.verb), change.target);
        if let Some(note) = &change.note {
            print!("  ({note})");
        }
        println!();
    }
    if report.changes.is_empty() {
        println!("  nothing to do");
    } else if verb == "install" && !report.made_changes() {
        println!("\nalready installed and up to date.");
    } else {
        println!("\n{verb} complete.");
    }
}

fn print_diagnosis(d: &Diagnosis) {
    println!("rocker {}  [{}]", d.version, d.target_triple);
    if let Some(exe) = &d.running_exe {
        println!("running from: {}", exe.display());
    }
    println!("\nintegration:");
    for (label, path, present) in &d.artifacts {
        println!(
            "  [{}] {label:<20} {}",
            if *present { 'x' } else { ' ' },
            path.display()
        );
    }
    if !d.notes.is_empty() {
        println!("\nnotes:");
        for note in &d.notes {
            println!("  - {note}");
        }
    }
    match &d.latest_release {
        Some((tag, true)) => println!("\nupdate available: {} -> {tag}", d.version),
        Some((_, false)) => println!("\nup to date."),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{run_args, Outcome};

    #[test]
    fn no_arguments_launches_the_gui() {
        assert_eq!(run_args(Vec::<String>::new()).unwrap(), Outcome::LaunchGui);
    }

    #[test]
    fn management_help_is_handled_before_the_gui() {
        assert_eq!(
            run_args([String::from("--help")]).unwrap(),
            Outcome::Handled(0)
        );
    }

    #[test]
    fn management_version_is_handled_before_the_gui() {
        assert_eq!(
            run_args([String::from("--version")]).unwrap(),
            Outcome::Handled(0)
        );
    }
}
