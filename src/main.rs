use std::process::ExitCode;

fn main() -> ExitCode {
    match wt_janitor::cli::run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code() as u8)
        }
    }
}
