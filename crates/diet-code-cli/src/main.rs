mod commands;
mod output;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "diet-code", version, about = "Put your AI coding agent on a diet.")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Analyze the repository and report dead-code candidates.
    Analyze(commands::analyze::AnalyzeArgs),
    /// Explain a finding with deterministic evidence.
    Explain(commands::explain::ExplainArgs),
    /// Show or apply deterministic cleanup of CERTAIN/HIGH findings.
    Clean(commands::clean::CleanArgs),
    /// Run the agent diet benchmark (BASE vs DIET).
    Benchmark(commands::benchmark::BenchmarkArgs),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Analyze(args) => commands::analyze::run(args),
        Commands::Explain(args) => commands::explain::run(args),
        Commands::Clean(args) => commands::clean::run(args),
        Commands::Benchmark(args) => commands::benchmark::run(args),
    }
}
