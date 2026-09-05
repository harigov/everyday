//! `everyday` -- a command line front end to a journal vault.
//!
//! It exists for three reasons: scripted capture (`echo ... | everyday new`),
//! export and inspection without launching a GUI, and as an end-to-end
//! exercise of the whole stack on machines where the desktop shell cannot be
//! built.

mod app;

use clap::Parser;

fn main() -> std::process::ExitCode {
    let cli = app::Cli::parse();
    match app::run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            std::process::ExitCode::FAILURE
        }
    }
}
