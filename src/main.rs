use rhythm_lib::{error_details, report_fatal, run};

fn main() {
    if let Err(e) = run(false) {
        report_fatal("Rhythm failed", &format!("{e}"), &error_details(&e));
    }
}
