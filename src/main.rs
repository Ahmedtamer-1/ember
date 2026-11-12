#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    match ember::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ember::ui::show_fatal_error("Ember Startup Error", &err.to_string());
            ExitCode::FAILURE
        }
    }
}
