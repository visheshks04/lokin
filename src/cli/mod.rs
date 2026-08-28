use clap::{Arg, Command};
pub mod handlers;

pub fn build() -> Command {
    let command: Command = Command::new("lokin")
        .about("a command line utility to keep you focused.")
        .version(env!("CARGO_PKG_VERSION"))
        .arg_required_else_help(true)
        .after_help(
            "Examples:\n  lokin start \"Build authentication API\"\n  lokin status\n  lokin stop",
        )
        .subcommand(
            Command::new("start")
                .about("Start the lokin session.")
                .arg(
                    Arg::new("goal")
                        .help("What do you wanna achieve next?")
                        .required(true),
                )
                .arg(
                    Arg::new("duration")
                        .short('d')
                        .long("duration")
                        .help("Duration in minutes")
                        .value_parser(clap::value_parser!(u16))
                        .default_value("25"),
                ),
        )
        .subcommand(Command::new("stop").about("Stop the lokin session."))
        .subcommand(Command::new("status").about("Get the status of the lokin session."))
        .subcommand(
            Command::new("pause")
                .about("Pause the current running lokin session.")
                .arg(
                    Arg::new("duration")
                        .short('d')
                        .long("duration")
                        .help("Duration in minutes")
                        .value_parser(clap::value_parser!(u16))
                        .default_value("5"),
                ),
        )
        .subcommand(Command::new("resume").about("Resume the paused lokin session"));
    command
}
