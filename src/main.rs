use clap::{ArgMatches, Command};
mod cli;

fn main() {
    let mut command: Command = cli::build();
    let matches: ArgMatches = command.clone().get_matches();

    match matches.subcommand() {
        Some(("start", args)) => cli::handlers::start(args),
        Some(("stop",_)) => cli::handlers::stop(),
        Some(("pause", _)) => cli::handlers::pause(),
        Some(("resume", _)) => cli::handlers::resume(),
        Some(("status", _)) => cli::handlers::status(),
        _ => {
            command.print_help().unwrap();
        }
    }
}
