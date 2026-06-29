//! End-to-end fixture tests: run rules over mini repos and assert the violations.

use camino::Utf8PathBuf;
use codepolicy_core::Project;
use codepolicy_match::Violation;
use codepolicy_rules::dsl;

const BUNDLED: &str = include_str!("../assets/codepolicy.rules");

fn fixture_root(name: &str) -> Utf8PathBuf {
    let manifest = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let raw = manifest.join("../..").join("fixtures").join(name);
    let canon = std::fs::canonicalize(&raw).expect("fixture dir should exist");
    Utf8PathBuf::from_path_buf(canon).expect("fixture path should be utf-8")
}

/// Compile DSL `src` and run it over `fixture`.
fn check(fixture: &str, src: &str) -> Vec<Violation> {
    let (rules, file) = dsl::load(src).expect("rules compile");
    let project = Project::new(fixture_root(fixture));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    project.check(&rules, &ctx)
}

fn count(v: &[Violation], id: &str) -> usize {
    v.iter().filter(|x| x.rule_id == id).count()
}

#[test]
fn bundled_pack_fires_expected_rules() {
    // starter_repo/apps/x.ts has a debugger, a console call, a `==`, and a TODO.
    let (rules, file) = dsl::load(BUNDLED).expect("bundled rules compile");
    let project = Project::new(fixture_root("starter_repo"));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let v = project.check(&rules, &ctx);
    assert_eq!(count(&v, "NO_DEBUGGER"), 1, "{v:#?}");
    assert_eq!(count(&v, "NO_CONSOLE"), 1, "{v:#?}");
    assert_eq!(count(&v, "NO_LOOSE_EQUALITY"), 1, "{v:#?}");
    assert_eq!(count(&v, "TODO_NEEDS_ISSUE"), 1, "{v:#?}");
}

#[test]
fn waiver_suppresses_a_rule_in_one_file() {
    // waiver_repo has a debugger and a waiver naming (NO_DEBUGGER, apps/x.ts).
    let (rules, file) = dsl::load(BUNDLED).expect("bundled rules compile");
    let project = Project::new(fixture_root("waiver_repo"));
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let v = project.check(&rules, &ctx);
    assert_eq!(count(&v, "NO_DEBUGGER"), 0, "waived: {v:#?}");
}

#[test]
fn token_single_match() {
    // The `switch` keyword lexeme — a construct the grammar names, matched by text.
    let v = check(
        "token_repo",
        r#"rule NO_SWITCH (warning) { match Token[node_kind = "switch"] message "x" }"#,
    );
    assert_eq!(v.len(), 1, "{v:#?}");
    assert_eq!(v[0].rule_id, "NO_SWITCH");
    assert_eq!(v[0].span.start_line, 2);
}

#[test]
fn exact_lexeme_and_bare_regex() {
    let v = check(
        "token_repo",
        r#"
rule EXACT (warning) { match Token[text = "switch"] message "x" }
rule REGEX (warning) { match /^sw/ message "x" }
"#,
    );
    assert_eq!(count(&v, "EXACT"), 1, "{v:#?}");
    assert_eq!(count(&v, "REGEX"), 1, "{v:#?}");
}

#[test]
fn at_class_matches_variables() {
    // class_repo: pumbaA, pumbaB are identifiers; "pumbaZ…" is a string token.
    let v = check(
        "class_repo",
        r#"rule PUMBA (warning) { match @ident & /^pumba/ message "x" }"#,
    );
    let texts: Vec<&str> = v.iter().map(|x| x.matched.text.as_str()).collect();
    assert_eq!(texts, vec!["pumbaA", "pumbaB"], "{v:#?}");
}

#[test]
fn token_sequence_in_scope() {
    // tokrel_repo/apps/x.ts: block `g` (lines 6-7) has two debugger lexemes.
    let v = check(
        "tokrel_repo",
        r#"
rule TWO_DEBUGGERS (warning) {
  sequence in scope {
    Token[node_kind = "debugger"]
    any *
    Token[node_kind = "debugger"]
    any *
  }
  message "x"
}
"#,
    );
    assert_eq!(v.len(), 1, "only block g has a pair: {v:#?}");
    assert_eq!(v[0].span.start_line, 6);
}

#[test]
fn token_where_scope() {
    // Only block `f` has both a debugger and a return.
    let v = check(
        "tokrel_repo",
        r#"
rule DBG_IN_RETURNING (warning) {
  match Token[node_kind = "debugger"]
  where scope contains Token[node_kind = "return"]
  message "x"
}
"#,
    );
    assert_eq!(v.len(), 1, "{v:#?}");
    assert_eq!(v[0].span.start_line, 2);
}

#[test]
fn token_backreference() {
    // y.ts repeats `foo`; x.ts has no repeated identifier. Sequences are per-file.
    let v = check(
        "tokrel_repo",
        r#"
rule REPEAT (warning) {
  sequence {
    Token[class = "ident"] as n = text
    any *
    Token[class = "ident", text == $n]
    any *
  }
  message "x"
}
"#,
    );
    assert_eq!(v.len(), 1, "{v:#?}");
    assert!(v[0].file.as_str().ends_with("y.ts"));
}

#[test]
fn aggregation_by_function_over_a_token_rule() {
    let v = check(
        "tokrel_repo",
        r#"
rule dbg (warning) { match Token[node_kind = "debugger"] }
rule TOO_MANY (error) {
  count dbg per function > 1
  message "x"
}
"#,
    );
    let agg: Vec<_> = v.iter().filter(|x| x.rule_id == "TOO_MANY").collect();
    assert_eq!(agg.len(), 1, "only g exceeds: {v:#?}");
    assert_eq!(agg[0].span.start_line, 6);
}

#[test]
fn compose_combines_two_lexeme_rules_by_function() {
    // class_repo has no function with both — sanity that compose runs cleanly.
    let v = check(
        "tokrel_repo",
        r#"
rule HAS_DEBUGGER (warning) { match Token[node_kind = "debugger"] }
rule HAS_RETURN (warning) { match Token[node_kind = "return"] }
rule BOTH (error) {
  compose intersection of HAS_DEBUGGER, HAS_RETURN by function
  message "x"
}
"#,
    );
    // Only function `f` has both a debugger and a return.
    let both: Vec<_> = v.iter().filter(|x| x.rule_id == "BOTH").collect();
    assert_eq!(both.len(), 1, "{v:#?}");
    assert_eq!(both[0].matched.function, "f");
}
