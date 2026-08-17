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

impl Profile {
    /// Lay this profile's thresholds over the ones the run was already configured with.
    ///
    /// A profile is built as `Tolerances { clock_skew_ns: 20ms, ..Tolerances::default() }`, so every
    /// field it does not deliberately name holds a *default* rather than an absence of opinion.
    /// Assigning the whole struct therefore did more than tighten: a `veridex.toml` setting
    /// `ego_max_speed_mps = 1.0`, `outlier_z = 2.0`, `gap_factor = 1.5` had all three silently
    /// reverted to 100.0 / 10.0 / 3.0 by `--profile world-model-ready`, making the run *looser* than
    /// the operator asked for — and the "Tolerances (non-default)" line then said nothing, because
    /// the reverted values were once again exactly the defaults.
    ///
    /// The rule the profile's own rationale implies is narrower than replacement: the thresholds a
    /// profile *names* win, because a readiness judgement is only meaningful at those; the rest are
    /// none of its business. A field the profile leaves at the default is treated as unnamed, which
    /// is indistinguishable from naming it *as* the default — and in that case both readings give
    /// the same answer anyway, since the config's value is the one with an opinion behind it.
    ///
    /// Among the fields it *does* name, the profile can only **tighten**. `docs/profiles.md` sells
    /// `world-model-ready` as one that "tightens cross-sensor sync … stricter than the 50 ms
    /// default", but keeping its value unconditionally moved thresholds in both directions: an
    /// operator asking for `clock_skew_ms = 5.0` had it *loosened* to the profile's 20 ms, so a
    /// 10 ms drift that failed their run passed once they added the flag that advertises strictness.
    /// Taking the stricter of the two keeps the profile's guarantee — the operator's threshold is
    /// tighter than the one the readiness criterion requires, so the criterion still holds — while
    /// never relaxing a limit the operator set deliberately.
    ///
    /// Every tolerance is an upper bound on tolerated deviation, so for all twelve "stricter" is
    /// simply the smaller value. That includes `saturation_min_samples`, which is the sample count
    /// below which the check abstains: a smaller one abstains on fewer streams.
    pub fn apply_tolerances(&self, base: Tolerances) -> Tolerances {
        let d = Tolerances::default();
        let p = self.tolerances;
        /// The stricter of two thresholds. A non-finite configured value yields the other operand
        /// rather than panicking; `finite_or_default` sanitizes those before they reach a report.
        fn stricter<T: PartialOrd>(profile: T, base: T) -> T {
            if profile < base {
                profile
            } else {
                base
            }
        }
        // `pick` consults the profile only where it departs from the default, and even then only to
        // tighten.
        macro_rules! pick {
            ($($field:ident),+ $(,)?) => {
                Tolerances {
                    $($field: if p.$field == d.$field {
                        base.$field
                    } else {
                        stricter(p.$field, base.$field)
                    },)+
                }
            };
        }
        pick!(
            clock_skew_ns,
            start_offset_ns,
            end_offset_ns,
            rate_deviation,
            gap_factor,
            jitter_cv,
            episode_duration_factor,
            saturation_fraction,
            saturation_min_samples,
            outlier_z,
            sequence_drop_fraction,
            ego_max_speed_mps,
        )
    }
}

/// The `world-model-ready` criteria: the autonomy checks and the guarantee each attests.
///
/// Every autonomy check that can fail a rig belongs here. A check missing from this list is a check
/// the profile does not judge, so a defect that moves from a listed check to an unlisted one becomes
/// invisible to `ready` while still failing the verdict — a certificate reading `status: fail` beside
/// `ready: true`. Adding an autonomy check to the catalog means adding it here.
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
    (
        "autonomy.sensor-frame-resolution",
        "every sensor's own frame resolves through the tree to a camera",
    ),
];

/// The `world-model-ready` profile: tightens cross-sensor sync to 20 ms and bundles the autonomy
/// criteria a world-model training set needs (rig sync, sequence completeness, ego-pose continuity,
/// calibration completeness, and per-sensor frame resolution).
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
/// Being a sensor rig is not enough. Several criteria — calibration completeness, per-sensor frame
/// resolution, and ego-pose continuity — abstain when the dataset has no spatial sensor and no ego
/// trajectory, so a
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
