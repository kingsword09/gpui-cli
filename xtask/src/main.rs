mod check_design_docs;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
struct Command {
    #[command(subcommand)]
    sub: SubCommand,
}

#[derive(Debug, Subcommand)]
enum SubCommand {
    #[command(about = "Check design-document links, indexes and examples.")]
    CheckDesignDocs,
}

fn main() {
    let command = Command::parse();

    let result = match command.sub {
        SubCommand::CheckDesignDocs => check_design_docs::run(),
    };

    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
