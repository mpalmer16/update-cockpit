fn main() {
    if let Err(error) = upgrade_cockpit::run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
