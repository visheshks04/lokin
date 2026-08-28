use clap::{Arg, ArgGroup, Command};

pub mod handlers;

pub fn build() -> Command {
    Command::new("lokin")
        .about("A task-agnostic command-line focus timer.")
        .version(env!("CARGO_PKG_VERSION"))
        .arg_required_else_help(true)
        .after_help(
            "Examples:\n  lokin start \"Build authentication API\" --duration 45\n  lokin pause --note \"Schema complete\"\n  lokin status\n  lokin history",
        )
        .subcommand(start_command())
        .subcommand(Command::new("status").about("Show the latest session status."))
        .subcommand(
            Command::new("pause")
                .about("Pause the running session without extending its deadline.")
                .arg(
                    Arg::new("note")
                        .long("note")
                        .help("Optionally add a checkpoint with the pause."),
                ),
        )
        .subcommand(
            Command::new("checkpoint")
                .about("Add a checkpoint without changing lifecycle state.")
                .arg(
                    Arg::new("note")
                        .long("note")
                        .help("Checkpoint note.")
                        .required(true),
                ),
        )
        .subcommand(Command::new("resume").about("Resume the paused session."))
        .subcommand(Command::new("stop").about("Stop the current session early."))
        .subcommand(plan_command())
        .subcommand(history_command())
}

fn start_command() -> Command {
    Command::new("start")
        .about("Start a new lock-in session.")
        .arg(
            Arg::new("goal")
                .help("What do you want to achieve?")
                .required(true),
        )
        .arg(
            Arg::new("duration")
                .short('d')
                .long("duration")
                .help("Hard wall-clock budget in minutes.")
                .value_parser(clap::value_parser!(u16))
                .default_value("25"),
        )
        .arg(
            Arg::new("plan")
                .long("plan")
                .action(clap::ArgAction::SetTrue)
                .help("Interactively generate and confirm an LLM plan before starting."),
        )
        .arg(
            Arg::new("context")
                .long("context")
                .requires("plan")
                .help("Optional context for LLM planning."),
        )
}

fn plan_command() -> Command {
    Command::new("plan")
        .about("Manage the current session's plan.")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(Command::new("show").about("Show the latest plan snapshot."))
        .subcommand(
            Command::new("revise")
                .about("Interactively generate and confirm an LLM plan revision.")
                .arg(
                    Arg::new("context")
                        .long("context")
                        .help("Optional context for the LLM revision."),
                ),
        )
        .subcommand(
            Command::new("add")
                .about("Add a pending plan item.")
                .arg(
                    Arg::new("description")
                        .help("Plan item description.")
                        .required(true),
                )
                .arg(
                    Arg::new("duration")
                        .short('d')
                        .long("duration")
                        .help("Estimated item duration in minutes.")
                        .value_parser(clap::value_parser!(u16))
                        .required(true),
                ),
        )
        .subcommand(
            Command::new("update")
                .about("Update one plan item and save a full snapshot.")
                .arg(
                    Arg::new("item_number")
                        .help("One-based item number.")
                        .value_parser(clap::value_parser!(usize))
                        .required(true),
                )
                .arg(
                    Arg::new("description")
                        .long("description")
                        .help("Replacement description."),
                )
                .arg(
                    Arg::new("duration")
                        .short('d')
                        .long("duration")
                        .help("Replacement duration in minutes.")
                        .value_parser(clap::value_parser!(u16)),
                )
                .arg(
                    Arg::new("status")
                        .long("status")
                        .help("Replacement status.")
                        .value_parser(["pending", "active", "completed", "skipped"]),
                )
                .group(
                    ArgGroup::new("changes")
                        .args(["description", "duration", "status"])
                        .required(true)
                        .multiple(true),
                ),
        )
        .subcommand(
            Command::new("remove")
                .about("Remove one plan item and save a full snapshot.")
                .arg(
                    Arg::new("item_number")
                        .help("One-based item number.")
                        .value_parser(clap::value_parser!(usize))
                        .required(true),
                ),
        )
}

fn history_command() -> Command {
    Command::new("history")
        .about("List or inspect persisted sessions.")
        .arg(
            Arg::new("search")
                .long("search")
                .help("Filter by goal text or session-ID prefix."),
        )
        .subcommand(
            Command::new("show")
                .about("Show one session and its event timeline.")
                .arg(
                    Arg::new("session_id")
                        .help("Full session ID or an unambiguous prefix.")
                        .required(true),
                ),
        )
}
