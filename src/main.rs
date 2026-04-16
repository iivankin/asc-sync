fn main() {
    if let Err(error) = asc_sync::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
