//! Rule matching over the lexeme (token) stream.
//!
//! Single `match` rules scan tokens; `sequence` rules run a finite-state pass,
//! optionally anchored to one `{}` block (paired via the lexer's `jmp` links);
//! `compose`/`count` rules run as a post-pass over the produced violations.

use camino::Utf8PathBuf;
use codepolicy_token::{Interner, Span, Token, TokenRef, TokenStream, NO_JMP};
use codepolicy_rules::{
    AttrPred, Bindings, ClauseMatcher, CompiledCompose, CompiledCount, CompiledRule,
    CompiledSequence, CompiledWhereScope, CountScope, KeyPart, Quant, Severity, SetOp,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// A structured waiver: a file-scoped exception for one rule.
#[derive(Debug, Clone)]
pub struct WaiverRecord {
    pub rule: String,
    pub file: String,
}

/// An ADR: a repository-wide decision, matched by topic.
#[derive(Debug, Clone)]
pub struct AdrRecord {
    pub topic: String,
    pub accepted: bool,
}

/// The escape-hatch context consulted by `unless` guards.
#[derive(Debug, Default)]
pub struct MatchContext {
    pub waivers: Vec<WaiverRecord>,
    pub adrs: Vec<AdrRecord>,
}

impl MatchContext {
    /// Whether a structured waiver covers `rule` at `file` (a global escape hatch).
    pub fn waiver_covers(&self, rule: &str, file: &str) -> bool {
        self.waivers.iter().any(|w| w.rule == rule && w.file == file)
    }

    fn adr_accepts(&self, topic: &str) -> bool {
        self.adrs.iter().any(|a| a.accepted && a.topic == topic)
    }
}

/// The lexeme a violation matched, for reporting.
#[derive(Debug, Clone, Serialize)]
pub struct Matched {
    pub node_kind: String,
    pub class: String,
    pub text: String,
    pub named: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub function: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Violation {
    pub rule_id: String,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub file: Utf8PathBuf,
    pub span: Span,
    pub matched: Matched,
}

fn suppressed(rule: &CompiledRule, file: &str, ctx: &MatchContext) -> bool {
    let Some(unless) = &rule.unless else {
        return false;
    };
    if let Some(globs) = &unless.path_matches {
        if globs.is_match(file) {
            return true;
        }
    }
    if let Some(waiver_rule) = &unless.waiver_rule {
        if ctx.waiver_covers(waiver_rule, file) {
            return true;
        }
    }
    if let Some(topic) = &unless.adr_topic {
        if ctx.adr_accepts(topic) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Single-token field access (no-alloc fast path for plain `match` rules)
// ---------------------------------------------------------------------------

enum TokenField<'a> {
    Str(&'a str),
    Num(u64),
}

fn token_field<'a>(token: &Token, key: &str, it: &'a Interner) -> Option<TokenField<'a>> {
    Some(match key {
        "node_kind" => TokenField::Str(it.resolve(token.kind)),
        "class" => TokenField::Str(it.resolve(token.class)),
        "text" => TokenField::Str(it.resolve(token.text)),
        "function" => TokenField::Str(it.resolve(token.func)),
        "named" => TokenField::Str(if token.named { "true" } else { "false" }),
        "curly_depth" => TokenField::Num(token.curly as u64),
        "round_depth" => TokenField::Num(token.round as u64),
        "bracket_depth" => TokenField::Num(token.bracket as u64),
        "range_lines" => TokenField::Num((token.end_line - token.start_line + 1) as u64),
        "text_len" => TokenField::Num((token.end_byte - token.start_byte) as u64),
        _ => return None,
    })
}

fn token_pred(pred: &AttrPred, token: &Token, it: &Interner) -> bool {
    let field = token_field(token, pred.attr_name(), it);
    match pred {
        AttrPred::Eq { value, .. } => match field {
            Some(TokenField::Str(s)) => s == value.as_str(),
            Some(TokenField::Num(n)) => n.to_string() == *value,
            None => false,
        },
        AttrPred::AnyOf { values, .. } => match field {
            Some(TokenField::Str(s)) => values.iter().any(|v| v.as_str() == s),
            Some(TokenField::Num(n)) => values.contains(&n.to_string()),
            None => false,
        },
        AttrPred::Regex { re, .. } => match field {
            Some(TokenField::Str(s)) => re.is_match(s),
            Some(TokenField::Num(n)) => re.is_match(&n.to_string()),
            None => false,
        },
        AttrPred::NotRegex { re, .. } => match field {
            Some(TokenField::Str(s)) => !re.is_match(s),
            Some(TokenField::Num(n)) => !re.is_match(&n.to_string()),
            None => true,
        },
        AttrPred::Cmp { op, n, .. } => match field {
            Some(TokenField::Num(x)) => op.test(x as f64, *n),
            Some(TokenField::Str(s)) => s.parse::<f64>().map(|x| op.test(x, *n)).unwrap_or(false),
            None => false,
        },
        // Backreferences need a binding environment — single-token rules have none.
        AttrPred::EqRef { .. } | AttrPred::NeRef { .. } => false,
    }
}

fn token_matches(preds: &[AttrPred], token: &Token, it: &Interner) -> bool {
    preds.iter().all(|p| token_pred(p, token, it))
}

fn violation_from_token(rule: &CompiledRule, ts: &TokenStream, token: &Token) -> Violation {
    Violation {
        rule_id: rule.id.clone(),
        severity: rule.severity,
        description: rule.description.clone(),
        message: rule.message.clone(),
        file: (*ts.file).clone(),
        span: ts.span_of(token),
        matched: Matched {
            node_kind: ts.interner.resolve(token.kind).to_string(),
            class: ts.interner.resolve(token.class).to_string(),
            text: ts.interner.resolve(token.text).to_string(),
            named: token.named,
            function: ts.interner.resolve(token.func).to_string(),
        },
    }
}

/// A borrowed, matchable view of every token in a stream.
fn token_refs(ts: &TokenStream) -> Vec<TokenRef<'_>> {
    ts.tokens
        .iter()
        .map(|t| TokenRef {
            token: t,
            interner: &ts.interner,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Top-level
// ---------------------------------------------------------------------------

/// Run all rules over the token streams, returning violations.
pub fn run(rules: &[CompiledRule], tokens: &[TokenStream], ctx: &MatchContext) -> Vec<Violation> {
    let mut out = Vec::new();
    // Pass 1: primary (non-aggregate) rules.
    for rule in rules {
        if rule.is_aggregate() {
            continue;
        }
        run_rule(rule, tokens, ctx, &mut out);
    }
    // Pass 2: aggregation rules (compose/count) over the primary violations.
    let primary = out.clone();
    for rule in rules {
        if let Some(comp) = &rule.compose {
            run_compose(rule, comp, &primary, ctx, &mut out);
        } else if let Some(cnt) = &rule.count {
            run_count(rule, cnt, &primary, ctx, &mut out);
        }
    }
    out.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.span.start_line.cmp(&b.span.start_line))
            .then(a.span.start_col.cmp(&b.span.start_col))
            .then(a.rule_id.cmp(&b.rule_id))
    });
    out
}

fn run_rule(rule: &CompiledRule, streams: &[TokenStream], ctx: &MatchContext, out: &mut Vec<Violation>) {
    for ts in streams {
        if !rule.applies_to_language(ts.language) {
            continue;
        }
        let file = ts.file.as_str();
        if !rule.path_in_scope(file) {
            continue;
        }
        if ctx.waiver_covers(&rule.id, file) || suppressed(rule, file, ctx) {
            continue;
        }

        if let Some(seq) = &rule.sequence {
            run_sequence(rule, seq, ts, out);
        } else if let Some(ws) = &rule.where_scope {
            let refs = token_refs(ts);
            for (i, token) in ts.tokens.iter().enumerate() {
                if token_matches(&rule.preds, token, &ts.interner)
                    && where_scope_holds(ws, &refs, i)
                {
                    out.push(violation_from_token(rule, ts, token));
                }
            }
        } else {
            // Plain single-token match (fast path, no binding env).
            for token in &ts.tokens {
                if token_matches(&rule.preds, token, &ts.interner) {
                    out.push(violation_from_token(rule, ts, token));
                }
            }
        }
    }
}

fn run_sequence(rule: &CompiledRule, seq: &CompiledSequence, ts: &TokenStream, out: &mut Vec<Violation>) {
    let refs = token_refs(ts);
    let regions: Vec<&[TokenRef]> = if seq.within_scope {
        scope_regions(&refs)
    } else {
        vec![refs.as_slice()]
    };
    // A token inside several nested scopes is reported at most once per rule.
    let mut reported: HashSet<usize> = HashSet::new();
    for region in regions {
        for anchor_idx in match_sequence(seq, region) {
            let anchor = region[anchor_idx];
            if reported.insert(anchor.token.start_byte as usize) {
                out.push(violation_from_token(rule, ts, anchor.token));
            }
        }
    }
}

/// One slice per `{}` block: the tokens strictly between each `{` and its `jmp`
/// partner `}` (the lexer's matching-delimiter link).
fn scope_regions<'a>(refs: &'a [TokenRef<'a>]) -> Vec<&'a [TokenRef<'a>]> {
    let mut regions = Vec::new();
    for (i, r) in refs.iter().enumerate() {
        if r.interner.resolve(r.token.kind) == "{" && r.token.jmp != NO_JMP {
            let j = r.token.jmp as usize;
            if j > i && j <= refs.len() {
                regions.push(&refs[i + 1..j]);
            }
        }
    }
    regions
}

/// Evaluate a `where_scope` clause against the token at `target_idx`, using its
/// innermost enclosing `{}` block (the whole stream when nothing encloses it).
fn where_scope_holds(ws: &CompiledWhereScope, refs: &[TokenRef], target_idx: usize) -> bool {
    let mut best: Option<(usize, usize)> = None;
    for (i, r) in refs.iter().enumerate() {
        if i >= target_idx {
            break;
        }
        if r.interner.resolve(r.token.kind) == "{" && r.token.jmp != NO_JMP {
            let j = r.token.jmp as usize;
            if target_idx < j && best.is_none_or(|(bi, _)| i > bi) {
                best = Some((i, j));
            }
        }
    }
    let (lo, hi) = match best {
        Some((bi, bj)) => (Some(bi), bj),
        None => (None, refs.len()),
    };
    let in_scope = |k: usize| k != target_idx && lo.is_none_or(|l| k > l) && k < hi;
    let any = |m: &ClauseMatcher| {
        refs.iter()
            .enumerate()
            .any(|(k, r)| in_scope(k) && m.matches(r))
    };
    if let Some(m) = &ws.contains {
        if !any(m) {
            return false;
        }
    }
    if let Some(m) = &ws.not_contains {
        if any(m) {
            return false;
        }
    }
    if let Some(m) = &ws.followed_by {
        let found = refs
            .iter()
            .enumerate()
            .any(|(k, r)| in_scope(k) && k > target_idx && m.matches(r));
        if !found {
            return false;
        }
    }
    true
}

/// Every anchor index (within `region`) at which the sequence matches. A match
/// must consume the whole region from its start position onward (end-anchored),
/// so a trailing negated run means "none in the rest of the scope". Matching is
/// attempted from each position; every distinct occurrence is reported.
fn match_sequence(seq: &CompiledSequence, region: &[TokenRef]) -> Vec<usize> {
    let mut failed: HashSet<(usize, usize)> = HashSet::new();
    let no_caps = !seq.has_captures;
    let mut anchors = Vec::new();
    for p in 0..region.len() {
        if match_here(&seq.steps, 0, region, p, &Bindings::new(), no_caps, &mut failed) {
            anchors.push(p);
        }
    }
    anchors
}

/// Whether `steps[si..]` can consume `region[ei..]` exactly to the end.
fn match_here(
    steps: &[codepolicy_rules::CompiledStep],
    si: usize,
    region: &[TokenRef],
    ei: usize,
    binds: &Bindings,
    no_caps: bool,
    failed: &mut HashSet<(usize, usize)>,
) -> bool {
    if si == steps.len() {
        return ei == region.len();
    }
    if no_caps && failed.contains(&(si, ei)) {
        return false;
    }

    let step = &steps[si];

    let result = (|| {
        match step.quant {
            Quant::One => {
                if ei < region.len() && step.accepts(&region[ei], binds) {
                    let b2 = step.apply_bind(&region[ei], binds);
                    return match_here(steps, si + 1, region, ei + 1, &b2, no_caps, failed);
                }
                false
            }
            Quant::Optional => {
                if ei < region.len() && step.accepts(&region[ei], binds) {
                    let b2 = step.apply_bind(&region[ei], binds);
                    if match_here(steps, si + 1, region, ei + 1, &b2, no_caps, failed) {
                        return true;
                    }
                }
                match_here(steps, si + 1, region, ei, binds, no_caps, failed)
            }
            Quant::ZeroOrMore => {
                if match_here(steps, si + 1, region, ei, binds, no_caps, failed) {
                    return true;
                }
                if ei < region.len() && step.accepts(&region[ei], binds) {
                    let b2 = step.apply_bind(&region[ei], binds);
                    return match_here(steps, si, region, ei + 1, &b2, no_caps, failed);
                }
                false
            }
        }
    })();

    if !result && no_caps {
        failed.insert((si, ei));
    }
    result
}

// ---------------------------------------------------------------------------
// Aggregation rules (compose / count) — a post-pass over violations.
// ---------------------------------------------------------------------------

fn func_of(v: &Violation) -> String {
    v.matched.function.clone()
}

fn violation_key(v: &Violation, key: &[KeyPart]) -> Vec<String> {
    key.iter()
        .map(|p| match p {
            KeyPart::File => v.file.to_string(),
            KeyPart::Function => func_of(v),
        })
        .collect()
}

fn aggregate_violation(rule: &CompiledRule, rep: &Violation) -> Violation {
    Violation {
        rule_id: rule.id.clone(),
        severity: rule.severity,
        description: rule.description.clone(),
        message: rule.message.clone(),
        file: rep.file.clone(),
        span: rep.span.clone(),
        matched: rep.matched.clone(),
    }
}

fn emit_aggregate(rule: &CompiledRule, rep: &Violation, ctx: &MatchContext, out: &mut Vec<Violation>) {
    let file = rep.file.as_str();
    if ctx.waiver_covers(&rule.id, file) || suppressed(rule, file, ctx) {
        return;
    }
    out.push(aggregate_violation(rule, rep));
}

fn run_compose(
    rule: &CompiledRule,
    comp: &CompiledCompose,
    primary: &[Violation],
    ctx: &MatchContext,
    out: &mut Vec<Violation>,
) {
    let maps: Vec<BTreeMap<Vec<String>, &Violation>> = comp
        .of
        .iter()
        .map(|id| {
            let mut m: BTreeMap<Vec<String>, &Violation> = BTreeMap::new();
            for v in primary.iter().filter(|v| &v.rule_id == id) {
                m.entry(violation_key(v, &comp.key)).or_insert(v);
            }
            m
        })
        .collect();
    if maps.is_empty() {
        return;
    }

    let mut result: BTreeMap<Vec<String>, &Violation> = BTreeMap::new();
    match comp.op {
        SetOp::Union => {
            for m in &maps {
                for (k, v) in m {
                    result.entry(k.clone()).or_insert(*v);
                }
            }
        }
        SetOp::Intersection => {
            for (k, v) in &maps[0] {
                if maps[1..].iter().all(|m| m.contains_key(k)) {
                    result.insert(k.clone(), *v);
                }
            }
        }
        SetOp::Difference => {
            for (k, v) in &maps[0] {
                if maps[1..].iter().all(|m| !m.contains_key(k)) {
                    result.insert(k.clone(), *v);
                }
            }
        }
    }
    for rep in result.into_values() {
        emit_aggregate(rule, rep, ctx, out);
    }
}

fn run_count(
    rule: &CompiledRule,
    cnt: &CompiledCount,
    primary: &[Violation],
    ctx: &MatchContext,
    out: &mut Vec<Violation>,
) {
    let mut groups: BTreeMap<Vec<String>, (u64, &Violation)> = BTreeMap::new();
    for v in primary.iter().filter(|v| v.rule_id == cnt.rule) {
        let key = match cnt.scope {
            CountScope::File => vec![v.file.to_string()],
            CountScope::Function => vec![v.file.to_string(), func_of(v)],
        };
        let entry = groups.entry(key).or_insert((0, v));
        entry.0 += 1;
    }
    for (count, rep) in groups.values() {
        if cnt.op.test(*count, cnt.n) {
            emit_aggregate(rule, rep, ctx, out);
        }
    }
}

/// Count of errors and warnings in a set of violations.
pub fn summarize(violations: &[Violation]) -> (usize, usize) {
    let errors = violations
        .iter()
        .filter(|v| v.severity == Severity::Error)
        .count();
    (errors, violations.len() - errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codepolicy_token::Language;
    use codepolicy_rules::dsl;
    use std::sync::Arc;

    /// Build a token stream from `(node_kind, class, text, function, start)` rows.
    fn stream(file: &str, rows: &[(&str, &str, &str, &str, u32)]) -> TokenStream {
        let mut interner = Interner::default();
        let tokens = rows
            .iter()
            .map(|(nk, cls, txt, func, start)| Token {
                kind: interner.intern(nk),
                text: interner.intern(txt),
                func: interner.intern(func),
                class: interner.intern(cls),
                named: true,
                start_byte: *start,
                end_byte: start + 1,
                start_line: start + 1,
                start_col: 1,
                end_line: start + 1,
                end_col: 2,
                curly: 0,
                round: 0,
                bracket: 0,
                jmp: NO_JMP,
            })
            .collect();
        TokenStream {
            file: Arc::new(Utf8PathBuf::from(file)),
            language: Language::Typescript,
            interner,
            tokens,
        }
    }

    #[test]
    fn single_token_match() {
        let (rules, _) = dsl::load(r#"rule D (error) { match debugger }"#).unwrap();
        let ts = stream(
            "a.ts",
            &[("debugger", "symbol", "debugger", "f", 0), ("identifier", "ident", "x", "f", 10)],
        );
        let v = run(&rules, &[ts], &MatchContext::default());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "D");
        assert_eq!(v[0].matched.node_kind, "debugger");
    }

    #[test]
    fn token_sequence_backreference() {
        // The same identifier text appears twice -> fires.
        let src = r#"
            rule REPEAT (warning) {
              sequence {
                n:@ident
                any *
                :n
                any *
              }
            }
        "#;
        let (rules, _) = dsl::load(src).unwrap();
        let dup = stream(
            "a.ts",
            &[("identifier", "ident", "foo", "f", 0), ("identifier", "ident", "bar", "f", 10), ("identifier", "ident", "foo", "f", 20)],
        );
        assert_eq!(run(&rules, &[dup], &MatchContext::default()).len(), 1);

        let uniq = stream(
            "b.ts",
            &[("identifier", "ident", "foo", "f", 0), ("identifier", "ident", "bar", "f", 10)],
        );
        assert!(run(&rules, &[uniq], &MatchContext::default()).is_empty());
    }

    #[test]
    fn compose_intersection_by_function() {
        let src = r#"
            rule A (warning) { match acquire }
            rule B (warning) { match risky }
            rule BOTH (error) { compose intersection of A, B by file, function }
        "#;
        let (rules, _) = dsl::load(src).unwrap();
        let ts = stream(
            "a.ts",
            &[
                ("identifier", "ident", "acquire", "f", 0),
                ("identifier", "ident", "risky", "f", 10),
                ("identifier", "ident", "acquire", "g", 20),
            ],
        );
        let v = run(&rules, &[ts], &MatchContext::default());
        let both: Vec<_> = v.iter().filter(|x| x.rule_id == "BOTH").collect();
        assert_eq!(both.len(), 1, "only f does both: {v:?}");
        assert_eq!(both[0].matched.function, "f");
    }

    #[test]
    fn count_per_file_threshold() {
        let src = r#"
            rule DBG (warning) { match debugger }
            rule TOO_MANY (error) { count DBG per file > 2 message "too many" }
        "#;
        let (rules, _) = dsl::load(src).unwrap();
        let many = stream(
            "a.ts",
            &[("debugger", "symbol", "debugger", "f", 0), ("debugger", "symbol", "debugger", "f", 10), ("debugger", "symbol", "debugger", "f", 20)],
        );
        let few = stream("b.ts", &[("debugger", "symbol", "debugger", "g", 0)]);
        let v = run(&rules, &[many, few], &MatchContext::default());
        let agg: Vec<_> = v.iter().filter(|x| x.rule_id == "TOO_MANY").collect();
        assert_eq!(agg.len(), 1, "only a.ts (3 > 2): {v:?}");
        assert_eq!(agg[0].file.as_str(), "a.ts");
    }
}
