use std::time::Instant;

#[macro_export]
macro_rules! startup_info {
    ($($arg:tt)*) => {
        #[cfg(feature = "startup-timing")]
        {
            log::info!($($arg)*);
        }
    };
}

#[cfg(feature = "startup-timing")]
pub type StartupPhases = Vec<StartupPhase>;

#[cfg(not(feature = "startup-timing"))]
pub type StartupPhases = ();

/// Field-diagnostic timings for cold start (look for `[startup]` in logs).
#[cfg(feature = "startup-timing")]
pub fn startup_log_phase(prev: &mut Instant, t0: Instant, label: &'static str) {
    let now = Instant::now();
    log::info!(
        "[startup] {:42} +{:5} ms   total {:6} ms",
        label,
        now.duration_since(*prev).as_millis(),
        now.duration_since(t0).as_millis()
    );
    // Advance after the log call so slow logger I/O is not charged to the next phase.
    *prev = Instant::now();
}

#[cfg(not(feature = "startup-timing"))]
pub fn startup_log_phase(_prev: &mut Instant, _t0: Instant, _label: &'static str) {}

#[cfg(feature = "startup-timing")]
pub struct StartupPhase {
    pub label: &'static str,
    pub delta_ms: u128,
    pub total_ms: u128,
}

#[cfg(feature = "startup-timing")]
pub fn startup_capture_phase(
    phases: &mut Vec<StartupPhase>,
    prev: &mut Instant,
    t0: Instant,
    label: &'static str,
) {
    let now = Instant::now();
    phases.push(startup_phase_at(prev, t0, label, now));
}

#[cfg(not(feature = "startup-timing"))]
#[allow(dead_code)]
pub fn startup_capture_phase(
    _phases: &mut StartupPhases,
    _prev: &mut Instant,
    _t0: Instant,
    _label: &'static str,
) {
}

#[cfg(feature = "startup-timing")]
pub fn startup_phase_at(
    prev: &mut Instant,
    t0: Instant,
    label: &'static str,
    now: Instant,
) -> StartupPhase {
    let phase = StartupPhase {
        label,
        delta_ms: now.duration_since(*prev).as_millis(),
        total_ms: now.duration_since(t0).as_millis(),
    };
    *prev = now;
    phase
}

#[cfg(not(feature = "startup-timing"))]
#[allow(dead_code)]
pub fn startup_phase_at(_prev: &mut Instant, _t0: Instant, _label: &'static str, _now: Instant) {}

#[cfg(feature = "startup-timing")]
pub fn startup_reset_after_diagnostics(prev: &mut Instant) {
    // Diagnostic log replay can be slow; never charge that I/O to the next startup phase.
    *prev = Instant::now();
}

#[cfg(not(feature = "startup-timing"))]
#[allow(dead_code)]
pub fn startup_reset_after_diagnostics(_prev: &mut Instant) {}

#[cfg(feature = "startup-timing")]
pub fn startup_log_captured_phase(phase: &StartupPhase) {
    log::info!(
        "[startup] {:42} +{:5} ms   total {:6} ms",
        phase.label,
        phase.delta_ms,
        phase.total_ms
    );
}

#[cfg(not(feature = "startup-timing"))]
#[allow(dead_code)]
pub fn startup_log_captured_phase(_phase: &()) {}

#[cfg(feature = "startup-timing")]
pub fn startup_log_captured_phases(phases: &[StartupPhase]) {
    for phase in phases {
        startup_log_captured_phase(phase);
    }
}

#[cfg(not(feature = "startup-timing"))]
pub fn startup_log_captured_phases(_phases: &StartupPhases) {}
