use rhythm_lib::run;
use tracing::error;

fn main() {
    if let Err(e) = run() {
        error!("Application failed to run: {}", e);
    }
}
