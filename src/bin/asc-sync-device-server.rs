fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return;
    }
    if !args.is_empty() {
        eprintln!("error: unexpected arguments: {}", args.join(" "));
        eprintln!("run with --help to see the supported environment variables");
        std::process::exit(2);
    }

    if let Err(error) = asc_sync::device_server::run_from_env() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!("Shared Apple device registration server");
    println!();
    println!("This binary is configured only through environment variables.");
    println!();
    println!("Required:");
    println!("  ASC_DEVICE_SERVER_PUBLIC_URL");
    println!();
    println!("Optional:");
    println!("  ASC_DEVICE_SERVER_LISTEN (default: 0.0.0.0:3000)");
}
