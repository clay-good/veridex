# Spec coverage

Every requirement in [`openspec/specs`](../openspec/specs), the code that implements it, and a test
that fails if it stops working.

This table is generated from the rows in `crates/veridex-core/tests/spec_coverage.rs`, and three
guards there keep it honest: every requirement in every spec must have a row, every row must name a
requirement that still exists, and every path and test name must exist in this repo. A requirement
added to a spec fails the build until someone decides what implements it.

What the guards cannot check is whether the named test *proves* the requirement — that is a reader's
judgement, which is why each row names the single most direct piece of evidence rather than counting
tests. Where a requirement is partly a stated limit rather than code, the spec itself carries the
reasoning in an `Implemented in v0.1` note beside it.

| Spec | Requirement | Implementation | Test |
|---|---|---|---|
| `autonomy-sensor-data` | Multi-sensor rig model | `crates/veridex-core/src/cdm.rs` | `autonomy_cdm::autonomy_dataset_round_trips_through_json` |
| `autonomy-sensor-data` | AV-native format adapters | `crates/veridex-core/src/adapter/mcap.rs` | `mcap_adapter::av_rig_message_types_map_to_the_autonomy_modalities` |
| `autonomy-sensor-data` | Rig-wide time synchronization checks | `crates/veridex-core/src/checks/autonomy.rs` | `mcap_adapter::an_injected_single_sensor_sync_drift_is_flagged_on_an_av_rig` |
| `autonomy-sensor-data` | Spatial calibration consistency checks | `crates/veridex-core/src/checks/autonomy.rs` | `checks::a_quaternion_that_is_not_unit_is_a_rotation_with_a_scale_in_it` |
| `autonomy-sensor-data` | Ego-motion and pose consistency checks | `crates/veridex-core/src/checks/autonomy.rs` | `checks::a_teleporting_ego_trajectory_is_flagged` |
| `autonomy-sensor-data` | Sequence completeness for world-model training | `crates/veridex-core/src/checks/autonomy.rs` | `checks::a_complete_rig_sequence_is_clean` |
| `autonomy-sensor-data` | Autonomy provenance extensions | `crates/veridex-core/src/adapter/mod.rs` | `mcap_adapter::metadata_records_and_attachments_become_provenance` |
| `autonomy-sensor-data` | Scenario and coverage metadata | `crates/veridex-core/src/scenario.rs` | `scenario::scenario_coverage_reports_the_distribution` |
| `autonomy-sensor-data` | World-model readiness profile | `crates/veridex-core/src/profile.rs` | `profile::a_healthy_rig_is_world_model_ready` |
| `checks-catalog` | Structural integrity checks | `crates/veridex-core/src/checks/structural.rs` | `checks::duplicate_episode_index_is_a_boundary_error` |
| `checks-catalog` | Temporal and synchronization checks | `crates/veridex-core/src/checks/temporal.rs` | `boundaries::a_start_offset_exactly_at_the_allowance_is_not_reported` |
| `checks-catalog` | Statistical checks | `crates/veridex-core/src/checks/statistical.rs` | `checks::stream_pinned_at_its_max_is_saturated` |
| `checks-catalog` | Semantic checks | `crates/veridex-core/src/checks/semantic.rs` | `boundaries::a_conflicting_annotation_list_names_four_and_counts_the_rest` |
| `checks-catalog` | Video and media checks | `crates/veridex-core/src/checks/video.rs` | `boundaries::a_frame_gap_of_exactly_the_factor_is_reported` |
| `checks-catalog` | Provenance completeness checks | `crates/veridex-core/src/checks/provenance.rs` | `checks::missing_license_and_sensor_are_surfaced` |
| `checks-catalog` | Duplicate and near-duplicate detection | `crates/veridex-core/src/checks/structural.rs` | `checks::an_episode_re_uploaded_with_its_tail_trimmed_is_a_near_duplicate` |
| `checks-catalog` | Privacy and safety checks | `crates/veridex-core/src/checks/provenance.rs` | `checks::a_recorder_path_under_a_home_directory_is_reported` |
| `checks-catalog` | Every check documents its rationale and remedy | `crates/veridex-core/src/check.rs` | `checks::every_registered_check_is_documented_in_docs_checks_md` |
| `cli` | Command surface | `crates/veridex-cli/src/main.rs` | `cli::every_command_accepts_help` |
| `cli` | Config discovery and reproducibility flags | `crates/veridex-cli/src/main.rs` | `cli::print_config_names_the_layer_every_value_came_from` |
| `cli` | Format autodetection with override | `crates/veridex-core/src/adapter/mod.rs` | `adapter_registry::a_file_no_adapter_recognizes_gets_no_hint` |
| `cli` | CI-friendly exit codes and output | `crates/veridex-cli/src/main.rs` | `cli::check_on_a_real_dataset_reports_and_exits_on_findings` |
| `cli` | Non-mutating by contract | `crates/veridex-cli/src/main.rs` | `cli::certify_refuses_to_default_its_output_into_the_dataset` |
| `cli` | Parity with the Python API | `crates/veridex-py/src/lib.rs` | `cli::the_python_bindings_expose_every_cli_operation` |
| `configuration` | Config file and precedence | `crates/veridex-core/src/config.rs` | `config::parses_full_config` |
| `configuration` | Policy profiles | `crates/veridex-core/src/profile.rs` | `profile::a_desynced_rig_is_not_ready` |
| `configuration` | Tolerances and thresholds are explicit | `crates/veridex-core/src/config.rs` | `config::tolerances_parse_resolve_and_validate` |
| `configuration` | Configuration validation | `crates/veridex-core/src/config.rs` | `config::a_threshold_exactly_at_the_edge_of_its_range_is_accepted_or_refused_as_documented` |
| `dataset-ingestion` | Canonical Dataset Model | `crates/veridex-core/src/cdm.rs` | `canonical_golden::the_canonical_encoding_has_not_changed_without_a_version_bump` |
| `dataset-ingestion` | Format adapters | `crates/veridex-core/src/adapter/mod.rs` | `adapter_registry::supported_formats_reflects_registrations` |
| `dataset-ingestion` | Streaming and scale | `crates/veridex-core/src/adapter/mod.rs` | `adapter_registry::an_ingest_refuses_to_materialize_more_frames_than_its_budget` |
| `dataset-ingestion` | Fidelity and adapter self-declaration | `crates/veridex-core/src/adapter/mod.rs` | `metadata_only::the_verdict_says_it_read_no_data` |
| `dataset-ingestion` | Deterministic ingestion | `crates/veridex-core/src/canonical.rs` | `dataset_id::every_spelling_of_one_path_yields_one_id_and_one_hash` |
| `dataset-ingestion` | Spatial and calibration metadata | `crates/veridex-core/src/cdm.rs` | `autonomy_cdm::a_changed_intrinsic_changes_the_hash` |
| `dataset-ingestion` | Sampling-based ingestion | `crates/veridex-core/src/adapter/mod.rs` | `sampling::the_same_fraction_and_seed_always_draw_the_same_episodes` |
| `extensibility` | Stable plugin API | `crates/veridex-core/src/engine.rs` | `check_packs::a_pack_built_for_another_plugin_api_is_refused` |
| `extensibility` | Custom checks are first-class | `crates/veridex-core/src/engine.rs` | `check_packs::a_packs_finding_is_attributable_to_it` |
| `extensibility` | Custom adapters and extractors | `crates/veridex-core/src/adapter/mod.rs` | `adapter_registry::recognized_source_ingests` |
| `extensibility` | Check-pack distribution and provenance | `crates/veridex-core/src/engine.rs` | `check_packs::a_pack_loaded_through_the_pipeline_runs_beside_the_catalog` |
| `extensibility` | Untrusted plugins cannot forge trust | `crates/veridex-core/src/certificate/signing.rs` | `check_packs::two_packs_cannot_register_the_same_id` |
| `provenance-lineage` | Provenance model | `crates/veridex-core/src/cdm.rs` | `scoring::coverage_counts_known_and_asserted_separately` |
| `provenance-lineage` | Known / asserted / unknown separation | `crates/veridex-core/src/certificate/coverage.rs` | `scoring::placeholder_value_does_not_count_toward_coverage` |
| `provenance-lineage` | Lineage graph | `crates/veridex-core/src/emit.rs` | `emit::prov_attributes_and_derives_from_known_elements` |
| `provenance-lineage` | Standard emit (Croissant and PROV) | `crates/veridex-core/src/emit.rs` | `emit::croissant_is_well_formed_and_binds_content_hash` |
| `provenance-lineage` | Producer attestation | `crates/veridex-core/src/certificate/attestation.rs` | `certificate::an_attestation_may_repeat_only_the_keys_that_are_lists` |
| `provenance-lineage` | License compatibility for derived datasets | `crates/veridex-core/src/checks/provenance.rs` | `checks::a_noncommercial_source_pooled_with_a_permissive_one_is_a_conflict` |
| `provenance-lineage` | Provenance is part of the signed attestation | `crates/veridex-core/src/certificate/document.rs` | `certificate::certificate_binds_content_and_states_coverage` |
| `provenance-lineage` | Provenance never fabricated | `crates/veridex-core/src/emit.rs` | `emit::croissant_preserves_provenance_classes_and_does_not_fabricate` |
| `registry` | Publish and look up certificates | `crates/veridex-core/src/registry.rs` | `registry::a_published_certificate_is_found_by_the_hash_it_is_bound_to` |
| `registry` | Public verification endpoint | `crates/veridex-core/src/registry.rs` | `registry::verifying_through_the_registry_is_the_same_computation_as_offline` |
| `registry` | Revocation and supersession | `crates/veridex-core/src/registry.rs` | `registry::revocation_is_the_issuers_word_and_not_a_withdrawal_of_the_document` |
| `registry` | Badges | `crates/veridex-core/src/registry.rs` | `registry::a_badge_reflects_the_current_status_and_not_the_grade_alone` |
| `registry` | Neutrality and offline independence preserved | `crates/veridex-core/src/registry.rs` | `registry::nothing_in_the_pipeline_needs_a_registry` |
| `reporting` | Multiple output formats | `crates/veridex-core/src/report.rs` | `report::sarif_is_valid_2_1_0_and_maps_findings` |
| `reporting` | Rollup summaries | `crates/veridex-core/src/report.rs` | `report::terminal_report_ranks_worst_episode_first` |
| `reporting` | Machine-readable output is stable | `crates/veridex-core/src/report.rs` | `report::json_report_is_versioned_and_parseable` |
| `reporting` | Verdict diffing | `crates/veridex-core/src/diff.rs` | `diff::a_new_finding_is_introduced_and_an_old_one_resolved` |
| `reporting` | Reports never leak beyond what is shared | `crates/veridex-core/src/redact.rs` | `redact::an_identifier_exactly_at_the_floor_is_redacted_and_one_shorter_is_not` |
| `reporting` | Remediation guidance | `crates/veridex-core/src/check.rs` | `variant_findings::every_finding_names_a_risk_and_a_remedy` |
| `reporting` | Coverage disclosure | `crates/veridex-core/src/engine.rs` | `metadata_only::the_verdict_says_it_read_no_data` |
| `security` | Key management | `crates/veridex-core/src/certificate/signing.rs` | `cli::full_keygen_certify_verify_flow` |
| `security` | Signing trust model | `crates/veridex-core/src/certificate/signing.rs` | `certificate::wrong_issuer_key_is_rejected` |
| `security` | Guarantees and non-guarantees are explicit | `crates/veridex-core/src/certificate/label.rs` | `cli::a_label_carries_its_own_caveats` |
| `security` | Tamper evidence and audit | `crates/veridex-core/src/audit.rs` | `audit::altering_a_prior_entry_is_detected_at_that_line` |
| `security` | Threat model | `crates/veridex-core/src/certificate/attestation.rs` | `certificate::tampering_with_content_is_rejected` |
| `security` | Safe handling of untrusted dataset content | `crates/veridex-core/src/adapter/mod.rs` | `corrupted_inputs::no_damaged_kitti_drive_takes_the_process_down` |
| `trust-certificate` | Certificate content | `crates/veridex-core/src/certificate/document.rs` | `certificate::certificate_binds_content_and_states_coverage` |
| `trust-certificate` | Binding to dataset content | `crates/veridex-core/src/certificate/signing.rs` | `certificate::transplanting_onto_a_different_dataset_is_rejected` |
| `trust-certificate` | Signing and offline verification | `crates/veridex-core/src/certificate/signing.rs` | `certificate::valid_certificate_verifies_offline` |
| `trust-certificate` | Trust score and grade | `crates/veridex-core/src/certificate/score.rs` | `scoring::clean_data_with_full_provenance_scores_a` |
| `trust-certificate` | Provenance coverage in the certificate | `crates/veridex-core/src/certificate/coverage.rs` | `scoring::coverage_counts_known_and_asserted_separately` |
| `trust-certificate` | Certificate is a statement of fact, not approval | `crates/veridex-core/src/certificate/label.rs` | `cli::a_label_says_what_the_certificate_says_and_nothing_more` |
| `trust-certificate` | Certificate schema versioning | `crates/veridex-core/src/certificate/document.rs` | `certificate::a_certificate_from_a_future_schema_is_refused` |
| `trust-certificate` | Revocation and supersession identity | `crates/veridex-core/src/certificate/supersession.rs` | `cli::a_later_certificate_supersedes_an_earlier_one_for_the_same_dataset` |
| `trust-certificate` | Human-readable nutrition label | `crates/veridex-core/src/certificate/label.rs` | `cli::a_label_says_what_the_certificate_says_and_nothing_more` |
| `validation-engine` | Check registry and plugin surface | `crates/veridex-core/src/engine.rs` | `engine::duplicate_check_id_is_rejected` |
| `validation-engine` | Severities and categories | `crates/veridex-core/src/engine.rs` | `engine::error_finding_drives_fail_status` |
| `validation-engine` | Selective and configurable runs | `crates/veridex-core/src/engine.rs` | `engine::category_selection_scopes_the_run` |
| `validation-engine` | Deterministic, ordered verdicts | `crates/veridex-core/src/engine.rs` | `engine::same_cdm_yields_byte_identical_verdict_and_shared_hash` |
| `validation-engine` | Findings are precise and actionable | `crates/veridex-core/src/check.rs` | `engine::findings_are_sorted_deterministically_regardless_of_registration_order` |
| `validation-engine` | Fault isolation | `crates/veridex-core/src/engine.rs` | `engine::crashing_check_is_isolated_and_reported_separately` |
| `validation-engine` | Run metadata for reproducibility | `crates/veridex-core/src/engine.rs` | `certificate::certification_is_reproducible` |
| `validation-engine` | Incremental and cached validation | `crates/veridex-core/src/cdm_cache.rs` | `cdm_cache::a_cached_run_is_the_full_runs_verdict_plus_the_fact_that_it_was_cached` |
| `validation-engine` | Progress and cancellation | `crates/veridex-core/src/engine.rs` | `run_control::a_cancelled_run_keeps_what_it_found_and_refuses_to_look_complete` |
