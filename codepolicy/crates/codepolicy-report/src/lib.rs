//! Violation reporting in three formats (proposal §10.4, §17 Phase 7).

use codepolicy_match::{summarize, MatchedEvent, Violation};
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
    Agent,
}

/// Render a set of violations in the requested format.
pub fn render(violations: &[Violation], format: Format) -> String {
    match format {
        Format::Human => render_human(violations),
        Format::Json => render_json(violations),
        Format::Agent => render_agent(violations),
    }
}

/// Format the matched event's attributes as `key=value` evidence.
fn matched_summary(m: &MatchedEvent) -> String {
    let mut parts = vec![format!("{:?}", m.kind)];
    for (k, v) in &m.attrs {
        // Skip empty arrays/strings to keep the line readable.
        if v.is_null() {
            continue;
        }
        if let Some(arr) = v.as_array() {
            if arr.is_empty() {
                continue;
            }
        }
        if let Some(s) = v.as_str() {
            if s.is_empty() {
                continue;
            }
        }
        parts.push(format!("{k}={}", compact_json(v)));
    }
    parts.join(" ")
}

fn compact_json(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "?".to_string())
}

fn render_human(violations: &[Violation]) -> String {
    let mut out = String::new();
    for v in violations {
        let _ = writeln!(out, "{} {}", v.severity.label(), v.rule_id);
        let _ = writeln!(
            out,
            "{}:{}:{}",
            v.file, v.span.start_line, v.span.start_col
        );
        if let Some(desc) = &v.description {
            let _ = writeln!(out, "\n{desc}");
        }
        let _ = writeln!(out, "\nMatched:\n  {}", matched_summary(&v.matched_event));
        if let Some(msg) = &v.message {
            let _ = writeln!(out, "\nRemediation:\n  {msg}");
        }
        let _ = writeln!(out);
    }
    let (errors, warnings) = summarize(violations);
    let _ = writeln!(
        out,
        "{} violation(s): {} error(s), {} warning(s)",
        violations.len(),
        errors,
        warnings
    );
    out
}

#[derive(Serialize)]
struct JsonReport<'a> {
    violations: &'a [Violation],
    summary: Summary,
}

#[derive(Serialize)]
struct Summary {
    errors: usize,
    warnings: usize,
    total: usize,
}

fn render_json(violations: &[Violation]) -> String {
    let (errors, warnings) = summarize(violations);
    let report = JsonReport {
        violations,
        summary: Summary {
            errors,
            warnings,
            total: violations.len(),
        },
    };
    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
}

/// Compiler-like output tuned for an LLM coding agent (proposal §13, §17).
fn render_agent(violations: &[Violation]) -> String {
    let mut out = String::new();
    if violations.is_empty() {
        out.push_str("codepolicy: no violations.\n");
        return out;
    }
    for v in violations {
        let _ = writeln!(
            out,
            "{} {} at {}:{}:{}",
            v.severity.label(),
            v.rule_id,
            v.file,
            v.span.start_line,
            v.span.start_col
        );
        let _ = writeln!(out, "  matched: {}", matched_summary(&v.matched_event));
        if let Some(desc) = &v.description {
            let _ = writeln!(out, "  why: {desc}");
        }
        if let Some(msg) = &v.message {
            let _ = writeln!(out, "  fix: {msg}");
        }
        let _ = writeln!(out);
    }
    let (errors, warnings) = summarize(violations);
    let _ = writeln!(
        out,
        "codepolicy: {} error(s), {} warning(s). Fix the errors above before committing.",
        errors, warnings
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use codepolicy_events::{EventKind, Span};
    use codepolicy_match::MatchedEvent;
    use codepolicy_rules::Severity;
    use std::collections::BTreeMap;

    fn sample() -> Vec<Violation> {
        let mut attrs = BTreeMap::new();
        attrs.insert("source".to_string(), serde_json::json!("@apollo/client"));
        attrs.insert("symbols".to_string(), serde_json::json!(["useQuery"]));
        vec![Violation {
            rule_id: "NO_DIRECT_GRAPHQL_CLIENT".into(),
            severity: Severity::Error,
            description: Some("Feature code must use the approved GraphQL access layer.".into()),
            message: Some("Use @app/graphql generated hooks.".into()),
            file: Utf8PathBuf::from("apps/admin/src/Member.tsx"),
            span: Span {
                start_byte: 0,
                end_byte: 0,
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 43,
            },
            matched_event: MatchedEvent {
                kind: EventKind::Import,
                attrs,
            },
        }]
    }

    #[test]
    fn human_mentions_rule_and_location() {
        let s = render(&sample(), Format::Human);
        assert!(s.contains("ERROR NO_DIRECT_GRAPHQL_CLIENT"));
        assert!(s.contains("apps/admin/src/Member.tsx:1:1"));
        assert!(s.contains("source=\"@apollo/client\""));
    }

    #[test]
    fn json_is_valid_and_has_summary() {
        let s = render(&sample(), Format::Json);
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["summary"]["errors"], 1);
        assert_eq!(parsed["violations"][0]["rule_id"], "NO_DIRECT_GRAPHQL_CLIENT");
    }

    #[test]
    fn agent_is_terse() {
        let s = render(&sample(), Format::Agent);
        assert!(s.contains("fix:"));
        assert!(s.contains("ERROR NO_DIRECT_GRAPHQL_CLIENT"));
    }
}
