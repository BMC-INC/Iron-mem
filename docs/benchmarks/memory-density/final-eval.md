# IronMem Evaluation Report

- generated_at: `2026-09-07T23:23:36.620755+00:00`
- command: `ironmem eval`
- commit: `0c66eac`
- model: `gemini-2.5-flash`
- result: `72/72 passed`

| Case | Result | Detail |
| --- | --- | --- |
| graph_relation_recall | pass | expected memory 1, got Some(1) |
| temporal_update_recall | pass | current=["approved"]; past=["draft"] |
| procedural_ranking | pass | expected procedural 5, got Some(5) |
| multi_hop_bridge_recall | pass | expected 6 and 7 in top-3, got [6, 7] |
| multi_hop_chain_second_hop | pass | expected bridged memory 7 in top-3, got [6, 7] |
| multi_hop_dependency_recall | pass | expected 8 first, got [8] |
| multi_hop_shared_entity_expansion | pass | expected 10 and 11 in top-2, got [10, 11] |
| multi_hop_route_detection | pass | positives=[true, true, true] negatives=[false, false] |
| multi_hop_direct_still_first | pass | expected direct memory 6 first, got [6] |
| temporal_event_time_recall | pass | expected 12 in top-2, got [12, 13] |
| temporal_year_ordering | pass | rome_rank=Some(0) paris_rank=Some(1) hits=[12, 13] |
| temporal_validity_boundary_inclusive | pass | edges at valid_from date: ["team lead"] |
| temporal_validity_before_start_excluded | pass | edges before valid_from: [] |
| temporal_when_question_recall | pass | expected 13 first, got [13, 12] |
| open_domain_keyword_recall | pass | expected 15 first, got [15] |
| open_domain_partial_token_recall | pass | expected 16 in top-3, got [16] |
| open_domain_wide_pool_surfacing | pass | expected 29 first among 13 memories, got [29] |
| open_domain_fact_retrievable_beside_narrative | pass | expected fact 31 and narrative 30 in top-2, got [31, 30] |
| open_domain_phrase_recall | pass | expected 32 in top-3, got [32] |
| knowledge_update_current_state_supersedes | pass | active edges: ["active"] |
| knowledge_update_history_preserved | pass | historical edges: ["active", "onboarding"] |
| knowledge_update_fresh_fact_retrievable | pass | expected fresh fact 36 in top-2, got [35, 36] |
| knowledge_update_negative_feedback_decays | pass | reinforcement_multiplier(-1.0, 0) = 0.88 |
| knowledge_update_positive_feedback_reinforces | pass | reinforcement_multiplier(1.5, 3) = 1.12 |
| knowledge_update_feedback_roundtrip | pass | feedback signals: ["correction"] |
| abstention_empty_project | pass | expected no hits from empty project, got [] |
| abstention_no_match_returns_empty | pass | expected no hits for gibberish query, got [] |
| abstention_tombstoned_excluded | pass | before=[38] deleted=true after=[] |
| abstention_expired_excluded | pass | expected expired 39 absent, got [] |
| governance_parity_ranking | pass | plain=["The retry budget for webhook delivery is three attempts"] governed=["The retry budget for webhook delivery is three attempts"] |
| governance_pii_fails_closed | pass | write without consent returned Err(Pii memory requires consent_state=granted before it can be stored) |
| governance_pii_with_consent_recall | pass | expected consented memory 46 retrievable, got [46] |
| governance_ledger_written | pass | first has 1 entries, second has 1 |
| governance_ledger_chain_linked | pass | chain_ok=true latest_ok=true latest=Some("a71bc12b4c525b818c948ec7e0744af0c66b58d370a3cfd945209e82bd7fe38c") |
| governance_namespace_isolation | pass | default=[] tenant=[49] |
| governance_delete_leaves_audit_trail | pass | ledger ops: ["remember", "forget"] |
| influence_allow | pass | decision=Allow |
| influence_permissive_relevance_delta | pass | hit-rate delta=0.000 |
| influence_deny_task | pass | decision=Deny |
| influence_deny_risk | pass | decision=Deny |
| influence_reasoning_only | pass | decision=AllowReasoningOnly |
| influence_reasoning_only_consumer_capability | pass | decision=Deny |
| influence_source_required | pass | decision=RequireOriginalSource |
| influence_confirmation_required | pass | decision=RequireHumanConfirmation |
| influence_derivation_depth | pass | decision=Deny |
| influence_contradiction_annotation | pass | sets=["eval-set"] |
| evidence_root_deduplication | pass | independent_roots=1 |
| influence_unattested_cannot_downgrade_risk | pass | authority=SelfDeclared |
| influence_attestation_scope_and_replay | pass | replay_rejected=true |
| influence_confirmation_scope_and_replay | pass | replay_rejected=true |
| revocation_no_stale_injection | pass | denied=[51] |
| influence_all_egress_surfaces_block_content | pass | shared egress returned identifiers only |
| influence_unauthorized_rate_zero | pass | unauthorized releases=0 |
| influence_decision_reproduction | pass | reproducible=true |
| entity_index_recall | pass | expected 52 via entity index, got [52] |
| entity_multiple_memories | pass | expected 52 and 53, got [52, 53] |
| entity_delete_removes_mapping | pass | after delete expected only 53, got [53] |
| chunk_skim_written_on_remember | pass | remember() produced 2 chunk(s) |
| chunk_fetch_by_id | pass | chunk id lookup roundtrip for Some("mem:54:overview") |
| chunk_replace_reflected | pass | chunks after replace: Some(1) |
| lever_chunk_recall_surfaces_parent | pass | expected chunk parent 55 in top-3, got [55] |
| lever_maturity_set_clamps_tiers | pass | clamped_core=true clamped_draft=true |
| lever_maturity_promotion_flow | pass | after_injections_stable=true after_feedback_core=true |
| lever_maturity_multiplier_monotone | pass | core=1.3 stable=1.15 draft=1 |
| compliance_chain_verifies_honest_history | pass | valid=true entries=2 |
| compliance_chain_detects_tampering | pass | valid=false first_broken_id=Some(20) |
| compliance_lineage_traces_write_and_action | pass | writer=Some("ironmem:remember") ledger_ops=["remember"] injections=1 |
| compliance_report_generates | pass | chains=4 inventory_rows=5 (tampered namespace must surface as failed) |
| observer_lines_parse_and_persist | pass | parsed=3 stored=3 |
| observer_detail_survives_and_recalls | pass | hits=["Caroline joined the LGBTQ support group", "Narrative summary of the planning session"] |
| observer_kind_importance_and_parent_lineage | pass | kind=observation importance=0.85 writer=Some("ironmem:observer") parent_chain=[61] |
| observer_event_time_stamped | pass | 2022-dated memory ids: [64] |
