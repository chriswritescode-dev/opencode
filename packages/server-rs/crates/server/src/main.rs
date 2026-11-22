const VERSION: &str = match option_env!("OPENCODE_VERSION") {
    Some(version) => version,
    None => "local",
};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        println!("opencode-server v{VERSION}");
        return std::process::ExitCode::SUCCESS;
    }
    eprintln!("usage: opencode-server --version");
    std::process::ExitCode::from(2)
}
