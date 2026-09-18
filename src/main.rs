use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "refuge", version, about = "Local-first Git repository backup")]
struct Cli {}

fn main() {
    Cli::parse();
}
