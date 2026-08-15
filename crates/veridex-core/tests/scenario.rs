//! Tests for the descriptive scenario-dimension coverage report (A3/A6).

use veridex_core::cdm::{Dataset, Episode, Label};
use veridex_core::scenario;

fn episode_with(index: u64, labels: &[(&str, &str)]) -> Episode {
    Episode {
        index,
        start_ts: None,
        end_ts: None,
        streams: vec![],
        task: None,
        labels: labels
            .iter()
            .map(|(k, v)| Label {
                key: (*k).into(),
                value: (*v).into(),
                ts: None,
            })
            .collect(),
        ego_poses: None,
        declared_frame_count: None,
    }
}

fn dataset(episodes: Vec<Episode>) -> Dataset {
    Dataset {
        id: "d".into(),
        metadata: vec![],
        provenance: vec![],
        episodes,
        calibration: None,
    }
}

#[test]
fn scenario_coverage_reports_the_distribution() {
    let d = dataset(vec![
        episode_with(0, &[("weather", "rain")]),
        episode_with(1, &[("weather", "rain")]),
        episode_with(2, &[("weather", "sunny")]),
    ]);
    let cov = scenario::coverage(&d);
    assert_eq!(cov.len(), 1);
    assert_eq!(cov[0].dimension, "weather");
    assert_eq!(cov[0].episodes_covered, 3);
    // Most frequent first.
    assert_eq!(
        cov[0].values,
        vec![("rain".to_string(), 2), ("sunny".to_string(), 1)]
    );
}

#[test]
fn a_rare_scenario_value_is_marked_sparse() {
    // 20 episodes, one rainy: rain is under 10% of covered episodes → sparse (descriptive, no finding).
    let mut eps = vec![episode_with(0, &[("weather", "rain")])];
    for i in 1..20 {
        eps.push(episode_with(i, &[("weather", "sunny")]));
    }
    let text = scenario::render_coverage(&dataset(eps));
    assert!(text.contains("rain (1, sparse)"), "rendered: {text}");
    assert!(text.contains("sunny (19)"));
}

#[test]
fn no_scenario_labels_yields_no_report() {
    let d = dataset(vec![episode_with(0, &[("language", "pick up the block")])]);
    assert!(scenario::coverage(&d).is_empty());
    assert!(scenario::render_coverage(&d).is_empty());
}

#[test]
fn scenario_dimension_spellings_are_recognized() {
    assert_eq!(
        scenario::scenario_dim_for("weather_condition"),
        Some("weather")
    );
    assert_eq!(scenario::scenario_dim_for("tod"), Some("time_of_day"));
    assert_eq!(scenario::scenario_dim_for("road_type"), Some("environment"));
    assert_eq!(scenario::scenario_dim_for("nonsense"), None);
}
