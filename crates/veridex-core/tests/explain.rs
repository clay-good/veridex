//! `explain`: what one string in a report means.
//!
//! A CI log shows a finding code and nothing else. The reader's next question is which check raised
//! it and whether it is a measurement or an abstention — "I could not measure" and "I measured and
//! it is sound" produce the same silence everywhere else, which is why the catalog marks the
//! difference. These tests hold the two properties that make the answer trustworthy: every code any
//! report can carry has an answer, and a string that is not a code is told so rather than guessed
//! at.

use veridex_core::engine::ENGINE_EMITTED_CODES;
use veridex_core::{explain, Explained};

fn catalog() -> Vec<veridex_core::engine::CheckInfo> {
    veridex_core::checks::default_engine()
        .expect("the standard catalog")
        .catalog()
}

#[test]
fn every_code_a_report_can_carry_has_an_explanation() {
    // The property worth having: a reader who pastes any code from any Veridex output gets an
    // answer. The catalog's codes come from the checks; the rest come from the engine's own table,
    // and a code in neither is a code nothing can explain.
    let catalog = catalog();
    for check in &catalog {
        for code in check.finding_codes {
            match explain(&catalog, code) {
                Explained::Code {
                    code: found,
                    check: owner,
                } => {
                    assert_eq!(found, *code);
                    assert_eq!(owner.id, check.id, "`{code}` is explained by its own check");
                }
                other => panic!("`{code}` is a catalog code: {other:?}"),
            }
        }
        // And the check id itself resolves, which is how a reader goes from a code to its family.
        assert!(
            matches!(explain(&catalog, check.id), Explained::Check(_)),
            "`{}` is a check id",
            check.id
        );
    }
    for (code, check_id, _) in ENGINE_EMITTED_CODES {
        match explain(&catalog, code) {
            Explained::Engine {
                code: found,
                check_id: owner,
                why,
            } => {
                assert_eq!(found, *code);
                assert_eq!(owner, *check_id);
                assert!(!why.is_empty(), "`{code}` must say why no check owns it");
            }
            other => panic!("`{code}` is emitted by the engine, not by a check: {other:?}"),
        }
    }
}

#[test]
fn the_engine_table_holds_exactly_the_codes_the_catalog_does_not() {
    // Two lists, and the drift between them is what would leave a code unexplainable. A code in
    // both would be answered twice, differently; a code in neither is the gap this file exists to
    // close. The suite already asserts that every code is documented in `docs/checks.md`; this
    // asserts that every code is *attributable*.
    let catalog = catalog();
    let catalog_codes: std::collections::BTreeSet<&str> = catalog
        .iter()
        .flat_map(|c| c.finding_codes.iter().copied())
        .collect();
    for (code, _, _) in ENGINE_EMITTED_CODES {
        assert!(
            !catalog_codes.contains(code),
            "`{code}` is in the engine's table *and* in the catalog: a reader would be told two \
             different things about it"
        );
    }
    // The engine's table is the complete set of codes outside the catalog, which the doc guard in
    // `tests/checks.rs` enumerates independently. If that list grows and this one does not, a code
    // reaches a report that `explain` cannot answer — so the two counts are pinned together.
    assert_eq!(
        ENGINE_EMITTED_CODES.len(),
        11,
        "a code was added to or removed from the engine's own disclosures; update \
         `tests/checks.rs`'s `engine_emitted` list and this count together"
    );
}

#[test]
fn an_abstention_code_says_it_could_not_measure() {
    let catalog = catalog();
    // One of each, so the two renderings are both exercised.
    let abstention = explain(&catalog, "SEMANTIC.NO_ANNOTATIONS");
    let measurement = explain(&catalog, "TEMPORAL.CLOCK_SKEW");
    let text = veridex_core::render_explain(&abstention);
    assert!(text.contains("could not measure"), "{text}");
    let text = veridex_core::render_explain(&measurement);
    assert!(
        text.contains("measured the data"),
        "a measurement must not read as an abstention: {text}"
    );
    // And the machine form carries the same distinction as a field rather than as prose.
    let json: serde_json::Value =
        serde_json::from_str(&veridex_core::render_explain_json(&abstention)).expect("json");
    assert_eq!(json["abstention"], serde_json::json!(true));
    assert_eq!(json["kind"], serde_json::json!("finding_code"));
    let json: serde_json::Value =
        serde_json::from_str(&veridex_core::render_explain_json(&measurement)).expect("json");
    assert_eq!(json["abstention"], serde_json::json!(false));
}

#[test]
fn a_string_that_is_not_a_code_is_told_so_and_offered_the_near_misses() {
    let catalog = catalog();
    match explain(&catalog, "clock") {
        Explained::Unknown { query, near } => {
            assert_eq!(query, "clock");
            assert!(
                near.iter().any(|c| c == "TEMPORAL.CLOCK_SKEW"),
                "a substring must offer what it is a substring of: {near:?}"
            );
            assert!(near.len() <= 8, "a hint is readable or it is not a hint");
        }
        other => panic!("`clock` is not a code: {other:?}"),
    }
    // Nothing at all matches: the answer says where the whole list is rather than offering nothing.
    match explain(&catalog, "zzzz") {
        Explained::Unknown { near, .. } => assert!(near.is_empty()),
        other => panic!("{other:?}"),
    }
    let text = veridex_core::render_explain(&explain(&catalog, "zzzz"));
    assert!(text.contains("veridex checks"), "{text}");
    // An empty query is unknown rather than a match on everything.
    assert!(matches!(explain(&catalog, "  "), Explained::Unknown { .. }));
}

#[test]
fn case_does_not_decide_whether_a_code_is_found() {
    // A reader pastes a code from a log in whatever case it appeared in.
    let catalog = catalog();
    for spelling in [
        "temporal.clock_skew",
        "TEMPORAL.CLOCK_SKEW",
        "Temporal.Clock_Skew",
    ] {
        assert!(
            matches!(explain(&catalog, spelling), Explained::Code { .. }),
            "`{spelling}` names the same code"
        );
    }
    assert!(matches!(
        explain(&catalog, "TEMPORAL.CLOCK-SKEW"),
        Explained::Check(_)
    ));
}
