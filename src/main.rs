mod apple;
mod download;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use crate::download::Downloader;

#[derive(Debug, Parser)]
#[command(
    name = "sully",
    version,
    about = "Download the highest quality cover artwork from Apple Music."
)]
struct Cli {
    /// Apple Music album, song, or playlist pages.
    #[arg(value_name = "APPLE-MUSIC-URL", required = true)]
    urls: Vec<String>,

    /// Overwrite existing covers.
    #[arg(short, long)]
    force: bool,

    /// Write covers to <dir>.
    #[arg(short = 'o', long, value_name = "DIR", default_value = ".")]
    output_dir: PathBuf,

    /// Print progress information to stderr.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("sully: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<usize> {
    let Cli {
        urls,
        force,
        output_dir,
        verbose,
    } = cli;

    let output_dir = prepare_output_dir(&output_dir)?;
    let downloader = Downloader::new(output_dir, force, verbose)?;
    let mut failures = 0;

    for url in &urls {
        if let Err(error) = downloader.run(url) {
            eprintln!("sully: {url}: {error:#}");
            failures += 1;
        }
    }

    Ok(failures)
}

fn prepare_output_dir(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("The argument '--output-dir' requires a value but none was supplied");
    }

    std::fs::create_dir_all(path)
        .with_context(|| format!("could not open output directory {}", path.display()))?;

    Ok(path.to_path_buf())
}
