//! Policy profiles: named bundles of thresholds and pass/fail criteria (design A4).
//!
//! A profile does **not** change which checks run — the full catalog still runs and scores the
//! dataset. It sets the run's tolerances and names the subset of checks whose results constitute a
//! *readiness* verdict, which the certificate reports per-criterion (see
//! [`ReadinessReport`](crate::certificate::ReadinessReport)). The only profile today is
//! `world-model-ready`, which bundles the autonomy sync / sequence / ego-pose / calibration criteria.

use crate::engine::Tolerances;

/// What a profile claims.
///
/// The distinction exists because `--profile` came to mean two things. A **readiness** profile names
/// criteria and produces a per-criterion verdict a certificate signs; a **threshold** profile only
/// moves the thresholds the run measures at, and has no readiness opinion at all. Rendering an
/// empty readiness block for the second kind would print `NOT READY` about criteria it never had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// Bundles thresholds only: no criteria, no readiness verdict.
    Thresholds,
    /// Names criteria and produces a readiness verdict.
    Readiness,
}

/// A named policy profile.
pub struct Profile {
    /// The profile name (e.g. `world-model-ready`), recorded on the certificate.
    pub name: &'static str,
    /// Whether this profile makes a readiness claim, or only sets thresholds.
    pub kind: ProfileKind,
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
    /// Every tolerance is an upper bound on tolerated deviation, so for all thirteen "stricter" is
    /// simply the smaller value. That includes `saturation_min_samples`, which is the sample count
    /// below which the check abstains: a smaller one abstains on fewer streams, and
    /// `near_duplicate_fraction`, which is the overlap at which a pair is reported: a smaller one
    /// reports more pairs.
    /// Whether this profile produces a readiness verdict at all.
    pub fn judges_readiness(&self) -> bool {
        self.kind == ProfileKind::Readiness
    }

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
            near_duplicate_fraction,
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
        "connected transform (TF) tree and camera intrinsics present, and arithmetically usable",
    ),
    (
        "autonomy.sensor-frame-resolution",
        "every sensor's own frame resolves through the tree to a camera",
    ),
    (
        "autonomy.gnss-plausibility",
        "every satellite fix is a possible place, and the receiver actually had one",
    ),
];

/// The `world-model-ready` profile: tightens cross-sensor sync to 20 ms and bundles the autonomy
/// criteria a world-model training set needs (rig sync, sequence completeness, ego-pose continuity,
/// calibration completeness, per-sensor frame resolution, and GNSS plausibility — a drive whose fix
/// is impossible or never acquired cannot be aligned to a map or to another drive, which is what a
/// world model built from more than one of them requires).
pub fn world_model_ready() -> Profile {
    Profile {
        name: "world-model-ready",
        kind: ProfileKind::Readiness,
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

/// The `standard` profile: Veridex's built-in thresholds, named.
///
/// It changes nothing, which is the point. A pipeline that says `--profile standard` records in its
/// verdict *which* policy it ran under, and a later run under `strict` is then a visible change
/// rather than an undocumented one. Naming the default also gives the other profiles something to
/// be named against.
pub fn standard() -> Profile {
    Profile {
        name: "standard",
        kind: ProfileKind::Thresholds,
        tolerances: Tolerances::default(),
        criteria: &[],
        applies_to: |_| false,
    }
}

/// The `strict` profile: the same catalog, measured harder.
///
/// Every threshold it names is tighter than the default, so — like every profile — it can only
/// tighten, never relax what a `veridex.toml` already set ([`Profile::apply_tolerances`]). That is
/// what keeps it usable with `--min-score`: measuring the data *harder* than the catalog asks can
/// only lower a score, so it is not a narrowing and does not disqualify a gate.
///
/// The numbers are one step in from the defaults rather than a different philosophy: 20 ms of
/// cross-stream drift instead of 50, 5% rate deviation instead of 10, a 2x gap instead of 3x, and an
/// outlier at 6σ instead of 10σ (a Chebyshev tail of ~2.8% instead of 1%).
pub fn strict() -> Profile {
    Profile {
        name: "strict",
        kind: ProfileKind::Thresholds,
        tolerances: Tolerances {
            clock_skew_ns: 20_000_000,
            start_offset_ns: 20_000_000,
            end_offset_ns: 20_000_000,
            rate_deviation: 0.05,
            gap_factor: 2.0,
            jitter_cv: 0.3,
            outlier_z: 6.0,
            sequence_drop_fraction: 0.01,
            ..Tolerances::default()
        },
        criteria: &[],
        applies_to: |_| false,
    }
}

/// Resolve a profile by name, or `None` for an unknown name.
pub fn by_name(name: &str) -> Option<Profile> {
    match name {
        "world-model-ready" => Some(world_model_ready()),
        "strict" => Some(strict()),
        "standard" => Some(standard()),
        _ => None,
    }
}

/// The profile names that exist, for error messages and documentation.
pub const KNOWN_PROFILES: &[&str] = &["standard", "strict", "world-model-ready"];

/// Why a name that looks like a profile is not one.
///
/// `lenient` is the case worth explaining rather than dismissing as a typo. A profile that *loosens*
/// thresholds is a narrowing of the run — the checks still run, measure the defect, and pass it —
/// and this tool refuses to let a narrowing hide behind a name. Loosened thresholds belong in a
/// `veridex.toml`, where `SCOPE.NARROWED` names each one and to what, `--min-score` refuses to gate
/// the result, and a certificate carries the disclosure. Bundling them under a reassuring word would
/// launder exactly the thing the disclosure exists to surface.
pub fn refusal_reason(name: &str) -> Option<&'static str> {
    match name {
        "lenient" | "relaxed" | "permissive" => Some(
            "a profile may only tighten a threshold, never loosen one — a loosened run is a \
             narrowed run, and Veridex discloses it per threshold (`SCOPE.NARROWED`) rather than \
             hiding it behind a name. Set the thresholds you want in `veridex.toml`: the run will \
             say which ones moved, and `--min-score` will refuse to gate it.",
        ),
        _ => None,
    }
}
