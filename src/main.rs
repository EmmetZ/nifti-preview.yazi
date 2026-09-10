use clap::{Parser, Subcommand};
use std::path::PathBuf;
use yazi_nifti_preview::{probe_named, render_named};

#[derive(Debug, Parser)]
#[command(
    name = "yazi-nifti-preview",
    version,
    about = "Render NIfTI previews for Yazi"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Probe {
        #[arg(long)]
        input: PathBuf,
        #[arg(long, value_name = "LOGICAL_FILENAME")]
        name: Option<String>,
    },
    Render {
        #[arg(long)]
        input: PathBuf,
        #[arg(long, value_name = "LOGICAL_FILENAME")]
        name: Option<String>,
        #[arg(long, value_name = "ZERO_BASED_INDEX")]
        slice: Option<usize>,
    },
}

fn run() -> yazi_nifti_preview::Result<()> {
    let result = match Cli::parse().command {
        Command::Probe { input, name } => {
            serde_json::to_string(&probe_named(&input, name.as_deref())?)?
        }
        Command::Render { input, name, slice } => {
            serde_json::to_string(&render_named(&input, name.as_deref(), slice)?)?
        }
    };
    println!("{result}");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("yazi-nifti-preview: {error}");
        std::process::exit(1);
    }
}
