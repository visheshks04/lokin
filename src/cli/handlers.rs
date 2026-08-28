use clap::ArgMatches;

pub fn start(args: &ArgMatches) {
    let goal: &String = args.get_one::<String>("goal")
        .expect("Goal should always exist");
    let duration: &u16 = args.get_one::<u16>("duration")
    .expect("Need to have a set duration");
    println!("Great! Lokin' you in for {}", duration);
    println!("Objective: {}", goal);
}

pub fn stop() {
    println!("Time for a pause.");
}

pub fn status() {
    println!("You have x minutes left.");
}

pub fn pause() {
    println!("Pause!");
}

pub fn resume() {
    println!("Resume!!");
}
