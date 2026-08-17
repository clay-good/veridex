use std::path::PathBuf;
use veridex_core::adapter::{default_registry, IngestOptions, Source};

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("SCRATCH_FX").unwrap())
}

fn hashes(name: &str, ep: u64, stream: &str) -> Vec<String> {
    let ing = default_registry()
        .ingest(&Source::Local(dir().join(name)), &IngestOptions::default())
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    ing.dataset
        .episodes
        .iter()
        .find(|e| e.index == ep)
        .unwrap_or_else(|| panic!("{name}: no episode {ep}"))
        .streams
        .iter()
        .find(|s| s.name == stream)
        .unwrap_or_else(|| panic!("{name}: no stream {stream}"))
        .frames
        .iter()
        .map(|f| {
            f.value_ref
                .content_hash
                .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>())
                .unwrap_or_default()
        })
        .collect()
}

fn expected(key: &str) -> Vec<String> {
    let txt = std::fs::read_to_string(dir().join("../expected.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn t_fillvalue() {
    assert_eq!(hashes("fillvalue.h5", 0, "actions"), expected("fillvalue"));
}
#[test]
fn t_partialrows() {
    assert_eq!(hashes("partialrows.h5", 0, "actions"), expected("partialrows"));
}
#[test]
fn t_manychunks() {
    assert_eq!(hashes("manychunks.h5", 0, "actions"), expected("manychunks"));
}
#[test]
fn t_be() {
    assert_eq!(hashes("be.h5", 0, "actions"), expected("be_actions"));
    assert_eq!(hashes("be.h5", 0, "odd"), expected("be_odd"));
}
#[test]
fn t_i16() {
    assert_eq!(hashes("misctypes.h5", 0, "i16"), expected("i16"));
}
#[test]
fn t_manylinks() {
    let ing = default_registry()
        .ingest(&Source::Local(dir().join("manylinks.h5")), &IngestOptions::default())
        .unwrap_or_else(|e| panic!("manylinks: {e:?}"));
    assert_eq!(ing.dataset.episodes.len(), 300);
}
#[test]
fn t_manyattrs() {
    let ing = default_registry()
        .ingest(&Source::Local(dir().join("manyattrs.h5")), &IngestOptions::default())
        .unwrap_or_else(|e| panic!("manyattrs: {e:?}"));
    assert_eq!(ing.dataset.episodes[0].declared_frame_count, Some(2));
    let n = ing.dataset.metadata.len();
    println!("metadata entries {n}");
}
#[test]
fn t_scaleoffset() {
    let r = default_registry()
        .ingest(&Source::Local(dir().join("scaleoffset.h5")), &IngestOptions::default());
    println!("scaleoffset -> {r:?}");
    assert!(r.is_err());
}
#[test]
fn t_bigtime_budget() {
    let r = default_registry().ingest(
        &Source::Local(dir().join("bigtime.h5")),
        &IngestOptions { max_frames: Some(10), ..IngestOptions::default() },
    );
    println!("bigtime with max_frames=10 -> {:?}", r.as_ref().map(|_| "ok"));
    assert!(r.is_err(), "a 3M-row timeline should be refused by a 10-frame budget");
}
