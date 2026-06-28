//! End-to-end fixture tests: run the bundled policy pack over mini repos and
//! assert the exact set of violations, including exempt paths and escape hatches.

use camino::Utf8PathBuf;
use codepolicy_core::Project;
use codepolicy_rules::load;
use std::collections::BTreeSet;

const RULES: &str = include_str!("../assets/codepolicy.yaml");

fn fixture_root(name: &str) -> Utf8PathBuf {
    let manifest = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let raw = manifest.join("../..").join("fixtures").join(name);
    let canon = std::fs::canonicalize(&raw).expect("fixture dir should exist");
    Utf8PathBuf::from_path_buf(canon).expect("fixture path should be utf-8")
}

fn run(name: &str) -> Vec<(String, String)> {
    let (rules, file) = load(RULES).expect("bundled rules should compile");
    let project = Project::new(fixture_root(name));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let mut pairs: Vec<(String, String)> = project
        .check(&rules, &ctx)
        .into_iter()
        .map(|v| (v.rule_id, v.file.to_string()))
        .collect();
    pairs.sort();
    pairs
}

#[test]
fn repo_flags_exactly_the_expected_violations() {
    let pairs = run("repo");
    let set: BTreeSet<(String, String)> = pairs.iter().cloned().collect();

    let member = "apps/admin/src/features/members/Member.tsx";
    let expect = |rule: &str, file: &str| {
        assert!(
            set.contains(&(rule.to_string(), file.to_string())),
            "expected {rule} at {file}; got {pairs:#?}"
        );
    };

    expect("NO_DIRECT_GRAPHQL_CLIENT", member);
    expect("NO_RAW_GRAPHQL_FETCH", member);
    expect("NO_MANUAL_GRAPHQL_OPERATION_TYPES", member);
    expect("NO_DIRECT_ZUSTAND_OUTSIDE_STATE_PACKAGE", member);
    expect("NO_DIRECT_ENV_ACCESS", "apps/admin/src/handler.ts");
    expect("NO_PROVIDER_SDK_OUTSIDE_INFRA", "apps/admin/src/payments.ts");
    expect("NO_TODO_WITHOUT_ISSUE", "apps/admin/src/notes.ts");
    expect("NO_UNAPPROVED_STATE_LIBRARY", "package.json");

    // Exempt by path-exclude / approved-wrapper / infra-package:
    let touched: BTreeSet<&str> = set.iter().map(|(_, f)| f.as_str()).collect();
    assert!(
        !touched.contains("apps/admin/src/config/env.ts"),
        "env access in the config layer must be exempt; got {pairs:#?}"
    );
    assert!(
        !set.contains(&(
            "NO_DIRECT_GRAPHQL_CLIENT".to_string(),
            "packages/graphql/client.ts".to_string()
        )),
        "the approved GraphQL wrapper must be exempt; got {pairs:#?}"
    );
    assert!(
        !set.iter().any(|(r, f)| r == "NO_PROVIDER_SDK_OUTSIDE_INFRA"
            && f == "apps/admin/src/infrastructure/aws.ts"),
        "provider SDK use inside an infra package must be exempt; got {pairs:#?}"
    );

    // The TODO that references an issue must not be flagged (only one TODO fires).
    let todo_count = set
        .iter()
        .filter(|(r, _)| r == "NO_TODO_WITHOUT_ISSUE")
        .count();
    assert_eq!(todo_count, 1, "only the issue-less TODO should fire");

    assert_eq!(set.len(), 8, "unexpected violation set: {pairs:#?}");
}

const SEQUENCE_RULES: &str = r##"
rules:
  - id: TXN_WITHOUT_COMMIT_OR_ROLLBACK
    severity: warning
    applies_to: { languages: [typescript, javascript] }
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - { event: Call, attrs: { name.regex: "^(begin|beginTransaction)$" } }
        - alt:
            - [ { event: Call, attrs: { name: "commit" } } ]
            - [ { event: Call, attrs: { name: "rollback" } } ]
          negate: true
          quant: zero_or_more
    message: "Transaction opened with begin() has no commit()/rollback() in this scope."
"##;

#[test]
fn sequence_rule_fires_through_the_real_frontend() {
    // End-to-end: the TS/JS frontend must emit paired ScopeStart/ScopeEnd, and
    // the sequence matcher must flag only the function that never commits.
    let (rules, file) = codepolicy_rules::load(SEQUENCE_RULES).expect("rules compile");
    let project = Project::new(fixture_root("seq_repo"));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let violations = project.check(&rules, &ctx);

    assert_eq!(
        violations.len(),
        1,
        "exactly the uncommitted transaction should fire: {violations:#?}"
    );
    let v = &violations[0];
    assert_eq!(v.rule_id, "TXN_WITHOUT_COMMIT_OR_ROLLBACK");
    assert_eq!(v.file.as_str(), "apps/db.ts");
    // The anchor should land on the begin() call inside bad() (line 11), not good().
    assert!(
        v.span.start_line > 8,
        "anchor should be inside bad(), got line {}",
        v.span.start_line
    );
}

const STRUCT_RULES: &str = r##"
rules:
  - id: TOP_LEVEL_CALL
    severity: warning
    applies_to: { languages: [typescript, javascript] }
    match:
      event: Call
      attrs: { curly_depth: 0 }
    message: "Call at module top level."
  - id: LONG_FN
    severity: warning
    applies_to: { languages: [typescript, javascript] }
    match:
      event: FunctionDecl
      attrs: { range_lines.gt: 2 }
    message: "Function longer than 2 lines."
"##;

#[test]
fn structural_fields_drive_depth_and_comparison_predicates() {
    // End-to-end: the frontend must emit curly_depth and range_lines so the
    // §8.8 predicates (exact depth, numeric comparison) actually match.
    let (rules, file) = codepolicy_rules::load(STRUCT_RULES).expect("rules compile");
    let project = Project::new(fixture_root("struct_repo"));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let v = project.check(&rules, &ctx);
    let ids: Vec<&str> = v.iter().map(|x| x.rule_id.as_str()).collect();

    // Only the top-level call has curly_depth 0; the two nested calls are at 1.
    assert_eq!(
        ids.iter().filter(|&&r| r == "TOP_LEVEL_CALL").count(),
        1,
        "exactly the top-level call should match curly_depth:0: {v:#?}"
    );
    // The multi-line function trips range_lines.gt: 2.
    assert_eq!(
        ids.iter().filter(|&&r| r == "LONG_FN").count(),
        1,
        "the 4-line function should trip range_lines.gt:2: {v:#?}"
    );
    let tl = v.iter().find(|x| x.rule_id == "TOP_LEVEL_CALL").unwrap();
    assert_eq!(tl.span.start_line, 1, "top-level call is on line 1");
}

const SCOPE_RULES: &str = r##"
rules:
  - id: ACQUIRE_WITHOUT_RELEASE_IN_SCOPE
    severity: warning
    applies_to: { languages: [typescript, javascript] }
    match:
      event: Call
      attrs: { name: "acquire" }
    where_scope:
      not_contains:
        event: Call
        attrs: { name: "release" }
    message: "acquire() in this scope has no matching release()."
"##;

#[test]
fn where_scope_uses_the_enclosing_scope() {
    // End-to-end: `safe()` releases in the same function scope (ok); `leaky()`
    // does not -> only its acquire() should fire.
    let (rules, file) = codepolicy_rules::load(SCOPE_RULES).expect("rules compile");
    let project = Project::new(fixture_root("scope_repo"));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let v = project.check(&rules, &ctx);

    assert_eq!(v.len(), 1, "only the unreleased acquire should fire: {v:#?}");
    assert_eq!(v[0].rule_id, "ACQUIRE_WITHOUT_RELEASE_IN_SCOPE");
    assert!(
        v[0].span.start_line > 6,
        "the flagged acquire is inside leaky(), got line {}",
        v[0].span.start_line
    );
}

const COUNT_RULES: &str = r##"
rules:
  - id: ENV
    severity: warning
    applies_to: { languages: [typescript, javascript] }
    match: { event: EnvAccess }
    message: "direct env read"
  - id: TOO_MANY_ENV_READS
    severity: error
    count: { rule: ENV, scope: file, op: gt, n: 2 }
    message: "more than 2 direct env reads in one file; centralize in config."
"##;

#[test]
fn count_aggregates_over_a_real_file() {
    // End-to-end: 3 env reads in one file -> 3 ENV warnings plus one
    // TOO_MANY_ENV_READS error from the count post-pass.
    let (rules, file) = codepolicy_rules::load(COUNT_RULES).expect("rules compile");
    let project = Project::new(fixture_root("count_repo"));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let v = project.check(&rules, &ctx);

    assert_eq!(
        v.iter().filter(|x| x.rule_id == "ENV").count(),
        3,
        "three direct env reads: {v:#?}"
    );
    let too_many: Vec<_> = v.iter().filter(|x| x.rule_id == "TOO_MANY_ENV_READS").collect();
    assert_eq!(too_many.len(), 1, "the file trips the count threshold: {v:#?}");
    assert_eq!(too_many[0].file.as_str(), "apps/env.ts");
}

const LISTENER_RULES: &str = r##"
rules:
  - id: EVENT_LISTENER_LEAK
    severity: warning
    applies_to: { languages: [typescript, javascript] }
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - event: Call
          attrs: { name: "addEventListener" }
          bind: { obj: receiver, ev: string_args }
        - event: Call
          attrs:
            name: "removeEventListener"
            receiver.eq_ref: obj
            string_args.eq_ref: ev
          negate: true
          quant: zero_or_more
    message: "addEventListener with no matching removeEventListener (same object + event) in scope."
"##;

#[test]
fn listener_leak_keys_on_object_and_event_and_reports_all() {
    // End-to-end: receiver capture + multi-bind + all-occurrence matching.
    // leaky() leaks both "scroll" (never removed) and "resize" (removed on the
    // wrong object); setup()'s "click" is paired and must not fire.
    let (rules, file) = codepolicy_rules::load(LISTENER_RULES).expect("rules compile");
    let project = Project::new(fixture_root("listener_repo"));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let v = project.check(&rules, &ctx);

    assert_eq!(v.len(), 2, "both distinct leaks should fire: {v:#?}");
    let names: Vec<String> = v
        .iter()
        .map(|x| {
            x.matched_event
                .attrs
                .get("string_args")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(names.contains(&"scroll".to_string()));
    assert!(names.contains(&"resize".to_string()));
    assert!(!names.contains(&"click".to_string()), "paired click must not fire");
    assert!(v.iter().all(|x| x.file.as_str() == "apps/widget.ts"));
}

#[test]
fn dsl_and_yaml_produce_identical_results() {
    // The textual DSL is a front-end over the same compiler, so the same rule
    // written in YAML and in the DSL must flag exactly the same loci.
    const YAML: &str = r#"
rules:
  - id: NO_MANUAL_GRAPHQL_OPERATION_TYPES
    severity: error
    applies_to:
      languages: [typescript]
      paths:
        include: ["apps/*/src/**/*.{ts,tsx}"]
        exclude: ["**/generated/**", "**/*.generated.ts"]
    match:
      event: TypeDecl
      attrs:
        name.regex: ".*(Query|Mutation|Subscription)(Variables)?$"
    message: "Use generated types."
"#;
    const DSL: &str = r#"
rule NO_MANUAL_GRAPHQL_OPERATION_TYPES (error) {
  lang typescript
  in "apps/*/src/**/*.{ts,tsx}"
  not in "**/generated/**", "**/*.generated.ts"
  match TypeDecl[name ~ /.*(Query|Mutation|Subscription)(Variables)?$/]
  message "Use generated types."
}
"#;
    let root = fixture_root("repo");
    let collect = |rules: &[codepolicy_rules::CompiledRule]| -> Vec<(String, String, usize)> {
        let project = Project::new(root.clone());
        let ctx = project.load_context(None, None);
        let mut out: Vec<_> = project
            .check(rules, &ctx)
            .into_iter()
            .map(|v| (v.rule_id, v.file.to_string(), v.span.start_line))
            .collect();
        out.sort();
        out
    };
    let (yaml_rules, _) = codepolicy_rules::load(YAML).expect("yaml compiles");
    let (dsl_rules, _) = codepolicy_rules::dsl::load(DSL).expect("dsl compiles");
    let yaml_hits = collect(&yaml_rules);
    assert!(!yaml_hits.is_empty(), "rule should fire on the fixture");
    assert_eq!(yaml_hits, collect(&dsl_rules), "DSL and YAML must agree");
}

#[test]
fn positional_arg_kind_matching_end_to_end() {
    // `foo(a, $x: string, b)` must match only the call whose 2nd argument is a
    // string literal — not the one passing an identifier.
    const RULES: &str = r#"
rule FOO_STRING_ARG (warning) {
  lang typescript, javascript
  match foo(a, $x: string, b)
  message "second arg must not be an inline string"
}
"#;
    let (rules, _) = codepolicy_rules::dsl::load(RULES).expect("dsl compiles");
    let project = Project::new(fixture_root("argkind_repo"));
    let ctx = project.load_context(None, None);
    let v = project.check(&rules, &ctx);
    assert_eq!(v.len(), 1, "only the string-literal call should match: {v:#?}");
    assert_eq!(v[0].span.start_line, 1, "line 1 has the string arg");
    assert_eq!(
        v[0].matched_event.attrs.get("arg1").and_then(|x| x.as_str()),
        Some("evt")
    );
}

#[test]
fn bundled_packs_compile_and_define_the_same_rules() {
    const YAML: &str = include_str!("../assets/codepolicy.yaml");
    const DSL: &str = include_str!("../assets/codepolicy.rules");
    let (yaml_rules, _) = codepolicy_rules::load(YAML).expect("yaml bundle compiles");
    let (dsl_rules, _) = codepolicy_rules::dsl::load(DSL).expect("dsl bundle compiles");
    let yids: BTreeSet<&str> = yaml_rules.iter().map(|r| r.id.as_str()).collect();
    let dids: BTreeSet<&str> = dsl_rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(yids, dids, "the YAML and DSL starter packs must define the same rules");
    assert!(yids.contains("EVENT_LISTENER_LEAK"), "listener rule must be bundled");
}

#[test]
fn bundled_dsl_listener_rule_fires() {
    const DSL: &str = include_str!("../assets/codepolicy.rules");
    let (rules, file) = codepolicy_rules::dsl::load(DSL).expect("dsl bundle compiles");
    let project = Project::new(fixture_root("listener_repo"));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let leaks = project
        .check(&rules, &ctx)
        .into_iter()
        .filter(|v| v.rule_id == "EVENT_LISTENER_LEAK")
        .count();
    assert_eq!(leaks, 2, "bundled listener rule should flag both leaks in leaky()");
}

#[test]
fn token_layer_matches_language_specific_constructs() {
    // Cobra-style: match a construct the canonical vocabulary has no kind for.
    const RULES: &str = r#"
rule NO_SWITCH (warning) {
  lang typescript, javascript
  match Token[node_kind = "switch_statement"]
  message "switch statements are discouraged here."
}
"#;
    let (rules, _) = codepolicy_rules::dsl::load(RULES).expect("dsl compiles");
    let project = Project::new(fixture_root("token_repo"));
    let ctx = project.load_context(None, None);
    let v = project.check(&rules, &ctx);
    assert_eq!(v.len(), 1, "the switch_statement should match: {v:#?}");
    assert_eq!(v[0].rule_id, "NO_SWITCH");
    assert_eq!(v[0].span.start_line, 2);
}

#[test]
fn token_layer_matches_bare_symbol_literals() {
    // A bare keyword (anonymous token) — not a named construct — is matchable.
    const RULES: &str = r#"
rule NO_DEBUGGER (error) {
  lang typescript, javascript
  match Token[node_kind = "debugger"]
  message "remove debugger statements."
}
"#;
    let (rules, _) = codepolicy_rules::dsl::load(RULES).expect("dsl compiles");
    let project = Project::new(fixture_root("token_repo"));
    let ctx = project.load_context(None, None);
    let v = project.check(&rules, &ctx);
    assert_eq!(v.len(), 1, "the `debugger` symbol should match: {v:#?}");
    assert_eq!(v[0].rule_id, "NO_DEBUGGER");
}

#[test]
fn waivers_and_adrs_suppress_violations() {
    let pairs = run("repo_waived");
    assert!(
        !pairs.iter().any(|(r, _)| r == "NO_UNAPPROVED_STATE_LIBRARY"),
        "an accepted ADR should suppress the jotai dependency; got {pairs:#?}"
    );
    assert!(
        !pairs
            .iter()
            .any(|(r, _)| r == "NO_DIRECT_ZUSTAND_OUTSIDE_STATE_PACKAGE"),
        "a structured waiver should suppress the zustand import; got {pairs:#?}"
    );
}
