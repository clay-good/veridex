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
    /// every criterion's check ran cleanly and produced no findings — and when the profile applies at
    /// all (an empty or vacuous criterion set can't be satisfied by default).
    pub criteria: &'static [(&'static str, &'static str)],
    /// Whether this profile applies to a dataset. It must demand the data the criteria are *about*:
    /// a criterion whose subject is absent abstains rather than fails, so a profile that applied too
    /// broadly would certify a dataset on criteria that never had anything to judge.
    pub applies_to: fn(&crate::cdm::Dataset) -> bool,
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
        applies_to: is_world_model_candidate,
    }
}

/// Whether `world-model-ready` has anything to say about a dataset.
///
/// Being a sensor rig is not enough. Two of the four criteria — calibration completeness and
/// ego-pose continuity — abstain when the dataset has no spatial sensor and no ego trajectory, so a
/// bus-only measurement (a CAN or MF4 log, which is a "rig" by sensor count alone) would satisfy them
/// with nothing examined. The profile therefore applies only to a rig that actually carries what a
/// world model is built from: a perception sensor **and** an ego trajectory.
fn is_world_model_candidate(dataset: &crate::cdm::Dataset) -> bool {
    use crate::cdm::Modality;
    dataset.episodes.iter().any(|ep| {
        crate::checks::autonomy::is_rig_episode(ep)
            && ep
                .streams
                .iter()
                .any(|s| matches!(s.modality, Modality::PointCloud | Modality::Video))
            && ep.ego_poses.as_ref().is_some_and(|p| !p.is_empty())
    })
}

/// Resolve a profile by name, or `None` for an unknown name.
pub fn by_name(name: &str) -> Option<Profile> {
    match name {
        "world-model-ready" => Some(world_model_ready()),
        _ => None,
    }
}
