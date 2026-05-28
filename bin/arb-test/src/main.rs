mod arb1_header;
mod arb1_state_convert;
mod fixture;
mod genesis_capture;
mod sepolia_import;
mod state_dump;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "arb-test",
    version,
    about = "Unified arbreth testing CLI.",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Operate on execution fixtures: record, verify, compare, promote, triage.
    #[command(subcommand)]
    Fixture(fixture::FixtureCommand),

    /// Capture an Arbitrum genesis state into a geth-format JSON file.
    GenesisCapture(genesis_capture::GenesisCaptureArgs),

    /// Sepolia archive helpers.
    #[command(subcommand)]
    SepoliaImport(sepolia_import::SepoliaImportCommand),

    /// Export an archive node's state at a block into reth `init-state` JSONL.
    StateDump(state_dump::StateDumpArgs),

    /// Derive + verify a migrated chain's genesis header and write its spec.
    Arb1Header(arb1_header::Arb1HeaderArgs),

    /// Convert a classic `arb_exportState` `accounts.json` to reth init-state JSONL.
    Arb1StateConvert(arb1_state_convert::Arb1StateConvertArgs),
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    match args.command {
        Command::Fixture(cmd) => fixture::run(cmd),
        Command::GenesisCapture(a) => genesis_capture::run(a),
        Command::SepoliaImport(cmd) => sepolia_import::run(cmd),
        Command::StateDump(a) => state_dump::run(a),
        Command::Arb1Header(a) => arb1_header::run(a),
        Command::Arb1StateConvert(a) => arb1_state_convert::run(a),
    }
}
