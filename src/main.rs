mod cli;
mod error;
mod llm;
mod session;
mod storage;

fn main() {
    let matches = cli::build().get_matches();
    if let Err(error) = cli::handlers::dispatch(&matches) {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}
