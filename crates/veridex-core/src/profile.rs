//! Policy profiles: named bundles of thresholds and pass/fail criteria (design A4).
//!
//! A profile does **not** change which checks run — the full catalog still runs and scores the
//! dataset. It sets the run's tolerances and names the subset of checks whose results constitute a
//! *readiness* verdict, which the certificate reports per-criterion (see
//! [`ReadinessReport`](crate::certificate::ReadinessReport)). The only profile today is
//! `world-model-ready`, which bundles the autonomy sync / sequence / ego-pose / calibration criteria.

use crate::engine::Tolerances;

/// A named readiness profile.
pub struct Profile {
    /// The profile name (e.g. `world-model-ready`), recorded on the certificate.
    pub name: &'static str,
    /// The run tolerances the profile applies (tighter than the defaults where readiness demands it).
    pub tolerances: Tolerances,
    /// The readiness criteria: `(check id, human-readable threshold)` pairs. A dataset is *ready* when
    /// every criterion's check produced no findings — and, for an autonomy profile, when it is actually
    /// a sensor rig (an empty criterion set can't be vacuously satisfied).
    pub criteria: &'static [(&'static str, &'static str)],
}

/// The `world-model-ready` criteria: the four autonomy checks and the guarantee each attests.
const WORLD_MODEL_READY_CRITERIA: &[(&str, &str)] = &[
    (
        "autonomy.rig-sync",
        "rig sensors within a 20 ms cross-sensor span drift",
    ),
    (
        "autonomy.sequence-complete",
        "no rig sensor dropping more than 5% of its frames",
    ),
    (
        "autonomy.ego-pose-continuity",
        "ego trajectory continuous (no step above 100 m/s implied speed)",
    ),
    (
        "autonomy.calibration-completeness",
        "connected transform (TF) tree and camera intrinsics present",
    ),
];

/// The `world-model-ready` profile: tightens cross-sensor sync to 20 ms and bundles the four autonomy
/// criteria a world-model training set needs (rig sync, sequence completeness, ego-pose continuity,
/// calibration completeness).
pub fn world_model_ready() -> Profile {
    Profile {
        name: "world-model-ready",
        tolerances: Tolerances {
            clock_skew_ns: 20_000_000, // 20 ms — stricter than the 50 ms default
            ..Tolerances::default()
        },
        criteria: WORLD_MODEL_READY_CRITERIA,
    }
}

/// Resolve a profile by name, or `None` for an unknown name.
pub fn by_name(name: &str) -> Option<Profile> {
    match name {
        "world-model-ready" => Some(world_model_ready()),
        _ => None,
    }
}
