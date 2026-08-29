//! The recomputed-statistics accumulators every adapter that reads actual values shares.
//!
//! Two adapters recompute statistics from the data itself — LeRobot (from Parquet cells) and HDF5
//! (from array rows) — and the `statistical.*` checks compare what they recomputed against what the
//! source stored, or judge it on its own. The accumulation has to be identical between them, or the
//! same logical dataset in two formats would produce two different verdicts, which is precisely the
//! cross-format neutrality claim Veridex makes. So it lives here once.
//!
//! Both accumulators are single-pass and hold no values: a dataset is far larger than memory, and
//! these run over every scalar in it.

use crate::cdm::{DimStats, Saturation, StreamStats};

/// Per-feature recomputed accumulators: one [`StatsAccum`] per dimension, plus a non-finite tally
/// pooled across all dimensions. Dimension 0 backs `observed_stats` (mirroring the element-0 stored
/// `stats.json` that `STATISTICAL.STATS_STALE` validates); saturation is judged across every dimension.
#[derive(Default, Clone)]
pub(crate) struct FeatureAccum {
    dims: Vec<StatsAccum>,
    /// NaN / ±inf scalars seen across all dimensions (kept out of the per-dimension stats).
    non_finite: u64,
}

impl FeatureAccum {
    /// Feed one cell's dimension-ordered scalars: finite values grow their dimension's accumulator;
    /// non-finite values are tallied and held out (a NaN would poison the summary). A `None` leaf is
    /// absent data — it is skipped, but its position is still consumed so later dimensions stay aligned.
    pub(crate) fn push_cell(&mut self, scalars: &[Option<f64>]) {
        for (dim, v) in scalars.iter().enumerate() {
            let Some(v) = *v else { continue };
            if v.is_finite() {
                if dim >= self.dims.len() {
                    self.dims.resize(dim + 1, StatsAccum::default());
                }
                self.dims[dim].push(v);
            } else {
                self.non_finite += 1;
            }
        }
    }

    /// Element-0 stats, for stored-vs-observed comparison against the element-0 stored stats.
    pub(crate) fn stats(&self) -> Option<StreamStats> {
        self.dims.first().and_then(StatsAccum::finish)
    }

    /// The saturation summary of the dimension most likely to be flagged — the non-constant dimension
    /// with the highest pinned fraction — so a saturating gripper at element 6 is caught, not just
    /// element 0. Falls back to dimension 0 (constant → the check skips it, DEGENERATE's concern), so
    /// a scalar stream reports exactly as before.
    pub(crate) fn saturation(&self) -> Option<Saturation> {
        let frac = |s: &Saturation| {
            let n = s.sample_count as f64;
            if n == 0.0 {
                0.0
            } else {
                s.at_min.max(s.at_max) as f64 / n
            }
        };
        // Tag each dimension's summary with its own index so the winning dimension is named.
        let with_dim = |(i, a): (usize, &StatsAccum)| {
            a.finish_saturation().map(|mut s| {
                s.dim = i as u64;
                s
            })
        };
        self.dims
            .iter()
            .enumerate()
            .filter_map(with_dim)
            .filter(|s| s.min != s.max)
            .max_by(|a, b| {
                frac(a)
                    .partial_cmp(&frac(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .or_else(|| self.dims.iter().enumerate().next().and_then(with_dim))
    }
}

/// Running accumulator for one dimension's recomputed statistics. Mean and variance use Welford's
/// online algorithm rather than accumulating `sum`/`sum_sq`: real robot signals often ride a large DC
/// offset (encoder counts ~1e6 with sub-unit variance), where `E[x²]−E[x]²` suffers catastrophic
/// cancellation and can clamp a real variance to 0 (a spurious `DEGENERATE`). Welford is single-pass
/// and numerically stable.
#[derive(Default, Clone, Copy)]
pub(crate) struct StatsAccum {
    count: u64,
    min: f64,
    max: f64,
    /// Running mean (Welford).
    mean: f64,
    /// Running sum of squared deviations from the mean (Welford's M2); population variance is `m2/n`.
    m2: f64,
    /// Values seen exactly equal to the running `min` / `max`. Reset to 1 whenever a new extreme
    /// appears, so at the end they count values equal to the *final* min/max in a single pass —
    /// no need to retain the values (mirrors the streaming stats above).
    at_min: u64,
    at_max: u64,
}

impl StatsAccum {
    pub(crate) fn push(&mut self, v: f64) {
        if self.count == 0 {
            self.min = v;
            self.max = v;
            self.at_min = 1;
            self.at_max = 1;
        } else {
            if v < self.min {
                self.min = v;
                self.at_min = 1;
            } else if v == self.min {
                self.at_min += 1;
            }
            if v > self.max {
                self.max = v;
                self.at_max = 1;
            } else if v == self.max {
                self.at_max += 1;
            }
        }
        self.count += 1;
        // Welford update.
        let delta = v - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = v - self.mean;
        self.m2 += delta * delta2;
    }

    /// Finalize to a [`StreamStats`] (population std), or `None` if nothing was accumulated.
    pub(crate) fn finish(&self) -> Option<StreamStats> {
        if self.count == 0 {
            return None;
        }
        let variance = (self.m2 / self.count as f64).max(0.0);
        Some(StreamStats {
            min: self.min,
            max: self.max,
            mean: self.mean,
            std: variance.sqrt(),
        })
    }

    /// Finalize the pinned-at-extreme counts, or `None` if nothing was accumulated.
    pub(crate) fn finish_saturation(&self) -> Option<Saturation> {
        if self.count == 0 {
            return None;
        }
        Some(Saturation {
            sample_count: self.count,
            at_min: self.at_min,
            at_max: self.at_max,
            min: self.min,
            max: self.max,
            dim: 0, // set by FeatureAccum::saturation, which knows the dimension index
        })
    }
}
impl FeatureAccum {
    /// The non-finite tally: values that were read and could not be summarized.
    pub(crate) fn non_finite(&self) -> u64 {
        self.non_finite
    }

    /// Count one non-finite value seen outside `push_cell` — for a caller that scans elements it does
    /// not keep per-dimension statistics for (an image array's pixels), where a NaN still matters.
    pub(crate) fn count_non_finite(&mut self) {
        self.non_finite += 1;
    }

    /// Per-dimension statistics for a multi-dimension feature, tagged with the dimension index.
    /// `None` for a feature with one dimension — its statistics are already `stats()` — so the
    /// `statistical.*` checks read exactly one of the two.
    pub(crate) fn dim_stats(&self) -> Option<Vec<DimStats>> {
        if self.dims.len() < 2 {
            return None;
        }
        let out: Vec<DimStats> = self
            .dims
            .iter()
            .enumerate()
            .filter_map(|(dim, accum)| {
                accum.finish().map(|stats| DimStats {
                    dim: dim as u64,
                    stats,
                })
            })
            .collect();
        (!out.is_empty()).then_some(out)
    }
}

/// One ROS topic's decoded values, and the joint set they belong to.
///
/// Wraps [`FeatureAccum`] with the one thing a `JointState` topic needs that an array feature does
/// not: the message guarantees only that `position[i]` belongs to `name[i]` **in that message**.
/// Nothing in the message definition says two messages order their joints alike, and a publisher
/// aggregating several sources is exactly where they might not. Accumulating positionally across a
/// reordering folds two joints into one dimension — which yields a statistic for a joint that does
/// not exist, and then names it after whichever joint was seen first. That is worse than not
/// measuring: it is a confident wrong answer.
///
/// So the joint set is fixed by the first message that names one joint per position, and a message
/// that contradicts it *refuses the whole stream* rather than being folded in or quietly dropped.
/// Refusing is bounded too — the alternative, growing an index of every joint name ever published,
/// is an allocation a file gets to choose the size of.
///
/// Shared by the MCAP and rosbag2 adapters so the two storage plugins cannot disagree about the same
/// recording, the same reason [`FeatureAccum`] itself lives here.
#[derive(Default, Clone)]
pub(crate) struct StreamValues {
    accum: FeatureAccum,
    names: Option<Vec<String>>,
    cells: u64,
    refused: bool,
}

impl StreamValues {
    /// Fold in one `JointState` message: its joint names and the positions they belong to.
    pub(crate) fn push_joint_state(&mut self, names: Vec<String>, positions: Vec<f64>) {
        if self.refused {
            return;
        }
        // A message that names one joint per position establishes — or must match — the joint set.
        // One that does not (an empty `name[]`, or a count that disagrees) names nothing this can
        // be checked against, so it is accumulated positionally, as it was before names were read.
        if names.len() == positions.len() && !names.is_empty() {
            match &self.names {
                Some(established) if *established != names => {
                    self.refused = true;
                    return;
                }
                Some(_) => {}
                None => self.names = Some(names),
            }
        }
        let cell: Vec<Option<f64>> = positions.into_iter().map(Some).collect();
        self.accum.push_cell(&cell);
        self.cells += 1;
    }

    /// Fold in one message whose dimensions are fixed by the message definition itself (an `Imu`),
    /// where there is no publisher-chosen ordering to contradict.
    pub(crate) fn push_fixed(&mut self, values: &[Option<f64>], names: &[&str]) {
        if self.refused {
            return;
        }
        if self.names.is_none() {
            self.names = Some(names.iter().map(|n| n.to_string()).collect());
        }
        self.accum.push_cell(values);
        self.cells += 1;
    }

    /// Why this topic's values were not measured, if they were not — the sentence the caller
    /// discloses as a coverage hole, because a stream that silently reports nothing reads as a
    /// stream that had nothing to report.
    pub(crate) fn refusal(&self) -> Option<&'static str> {
        self.refused.then_some(
            "this topic publishes `JointState` messages under more than one joint set, and \
             `position[i]` belongs to `name[i]` only within a single message — so no dimension of \
             this stream is one joint, and its values were not summarized",
        )
    }

    /// The measured statistics, or `None` where nothing was measured or the stream was refused.
    pub(crate) fn finish(self) -> Option<(FeatureAccum, Option<Vec<String>>)> {
        (!self.refused && self.cells > 0).then_some((self.accum, self.names))
    }
}
