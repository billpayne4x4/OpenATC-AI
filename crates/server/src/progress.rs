//! Machine-readable startup events for the local dashboard.
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
static ENABLED: AtomicBool = AtomicBool::new(false);
pub fn enable(value: bool) {
    ENABLED.store(value, Ordering::Relaxed);
}
// Callers build a fresh value for each event.
#[allow(clippy::needless_pass_by_value)]
pub fn emit(value: serde_json::Value) {
    if ENABLED.load(Ordering::Relaxed) {
        let mut output = std::io::stdout().lock();
        let _ = writeln!(output, "OPENATC_STATUS {value}");
        let _ = output.flush();
    }
}
