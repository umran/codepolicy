//! Violation reporting in three formats.

use codepolicy_match::{summarize, Matched, Violation};
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

/// Format the matched lexeme as evidence.
fn matched_summary(m: &Matched) -> String {
    let mut parts = vec![
        format!("node_kind={:?}", m.node_kind),
        format!("class={:?}", m.class),
        format!("text={:?}", m.text),
    ];
    if !m.function.is_empty() {
        parts.push(format!("function={:?}", m.function));
    }
    parts.join(" ")
}

fn render_human(violations: &[Violation]) -> String {
    let mut out = String::new();
    for v in violations {
        let _ = writeln!(out, "{} {}", v.severity.label(), v.rule_id);
        let _ = writeln!(out, "{}:{}:{}", v.file, v.span.start_line, v.span.start_col);
        if let Some(desc) = &v.description {
            let _ = writeln!(out, "\n{desc}");
        }
        let _ = writeln!(out, "\nMatched:\n  {}", matched_summary(&v.matched));
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

/// Compiler-like output tuned for an LLM coding agent.
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
        let _ = writeln!(out, "  matched: {}", matched_summary(&v.matched));
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
    use codepolicy_token::Span;
    use codepolicy_rules::Severity;

    fn sample() -> Vec<Violation> {
        vec![Violation {
            rule_id: "NO_DEBUGGER".into(),
            severity: Severity::Error,
            description: Some("Remove debugger statements.".into()),
            message: Some("Delete it.".into()),
            file: Utf8PathBuf::from("apps/admin/src/Member.tsx"),
            span: Span {
                start_byte: 0,
                end_byte: 0,
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 9,
            },
            matched: Matched {
                node_kind: "debugger".into(),
                class: "symbol".into(),
                text: "debugger".into(),
                named: false,
                function: "f".into(),
            },
        }]
    }

    #[test]
    fn human_mentions_rule_and_location() {
        let s = render(&sample(), Format::Human);
        assert!(s.contains("ERROR NO_DEBUGGER"));
        assert!(s.contains("apps/admin/src/Member.tsx:1:1"));
        assert!(s.contains("node_kind=\"debugger\""));
    }

    #[test]
    fn json_is_valid_and_has_summary() {
        let s = render(&sample(), Format::Json);
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["summary"]["errors"], 1);
        assert_eq!(parsed["violations"][0]["rule_id"], "NO_DEBUGGER");
    }

    #[test]
    fn agent_is_terse() {
        let s = render(&sample(), Format::Agent);
        assert!(s.contains("fix:"));
        assert!(s.contains("ERROR NO_DEBUGGER"));
    }
}
