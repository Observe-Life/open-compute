//! One-shot P003/P004 forensic: walk `WorkflowController::reconcile` read-only.
fn main() {
    let control = std::env::args().nth(1).expect("usage: p003_repro <control.sqlite> <scheduler.sqlite>");
    let scheduler = std::env::args().nth(2).expect("usage: p003_repro <control.sqlite> <scheduler.sqlite>");
    let report = open_compute_storage::scheduler::inspect_workflow_reconcile_failures(
        std::path::Path::new(&control),
        std::path::Path::new(&scheduler),
        0,
    );
    match report {
        Ok(value) => println!("{}", serde_json::to_string(&value).unwrap_or_default()),
        Err(error) => {
            eprintln!("diagnosis error: {}", error.code().as_str());
            std::process::exit(2);
        }
    }
}
