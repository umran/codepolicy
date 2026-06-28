//! Event indexing and rule matching (proposal §9).
//!
//! Single-event rules scan events of their kind (avoiding the naive
//! `rules × all_events` scan, §9.2). Sequence rules (§8.6) run a finite-state
//! pass, optionally anchored to one pre-paired `ScopeStart`/`ScopeEnd` region.

use camino::Utf8PathBuf;
use codepolicy_events::{Event, EventKind, Interner, Span, Token, TokenStream};
use codepolicy_rules::{
    AttrPred, Bindings, CompiledCompose, CompiledCount, CompiledRule, CompiledSequence,
    CompiledWhereScope, CountScope, KeyPart, Quant, Severity, SetOp,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// An index over a slice of events, grouped by kind and by file.
pub struct EventIndex<'a> {
    by_kind: HashMap<EventKind, Vec<&'a Event>>,
    /// Per-file event lists, sorted by start position (for sequence matching).
    by_file: BTreeMap<&'a str, Vec<&'a Event>>,
}

impl<'a> EventIndex<'a> {
    pub fn build(events: &'a [Event]) -> Self {
        let mut by_kind: HashMap<EventKind, Vec<&'a Event>> = HashMap::new();
        let mut by_file: BTreeMap<&'a str, Vec<&'a Event>> = BTreeMap::new();
        for ev in events {
            by_kind.entry(ev.kind).or_default().push(ev);
            by_file.entry(ev.file.as_str()).or_default().push(ev);
        }
        for list in by_file.values_mut() {
            list.sort_by_key(|e| (e.span.start_byte, e.span.end_byte));
        }
        EventIndex { by_kind, by_file }
    }

    pub fn of_kind(&self, kind: EventKind) -> &[&'a Event] {
        self.by_kind.get(&kind).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// All events of one file, sorted by position.
    pub fn file_events(&self, file: &str) -> &[&'a Event] {
        self.by_file.get(file).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// A structured waiver (proposal §14): a file-scoped exception for one rule.
#[derive(Debug, Clone)]
pub struct WaiverRecord {
    pub rule: String,
    pub file: String,
}

/// An ADR (proposal §14): a repository-wide decision, matched by topic.
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
    /// Whether a structured waiver covers `rule` at `file` (a global escape
    /// hatch, proposal §14).
    pub fn waiver_covers(&self, rule: &str, file: &str) -> bool {
        self.waivers.iter().any(|w| w.rule == rule && w.file == file)
    }

    fn adr_accepts(&self, topic: &str) -> bool {
        self.adrs.iter().any(|a| a.accepted && a.topic == topic)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedEvent {
    pub kind: EventKind,
    pub attrs: BTreeMap<String, serde_json::Value>,
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
    pub matched_event: MatchedEvent,
}

fn attrs_match(rule: &CompiledRule, event: &Event) -> bool {
    // Single-event predicates evaluate with an empty capture environment.
    let empty = Bindings::new();
    rule.preds
        .iter()
        .all(|pred| pred.eval(&event.attr_strings(pred.attr_name()), &empty))
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

fn violation_from(rule: &CompiledRule, event: &Event) -> Violation {
    Violation {
        rule_id: rule.id.clone(),
        severity: rule.severity,
        description: rule.description.clone(),
        message: rule.message.clone(),
        file: event.file.as_ref().clone(),
        span: event.span.clone(),
        matched_event: MatchedEvent {
            kind: event.kind,
            attrs: event.attrs.clone(),
        },
    }
}

// ---------------------------------------------------------------------------
// Token matching (single-event `match Token[...]` over the compact stream)
// ---------------------------------------------------------------------------

/// A token's value for one attribute key, without allocating for string fields.
enum TokenField<'a> {
    Str(&'a str),
    Num(u64),
}

fn token_field<'a>(token: &Token, key: &str, it: &'a Interner) -> Option<TokenField<'a>> {
    Some(match key {
        "node_kind" => TokenField::Str(it.resolve(token.kind)),
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
            Some(TokenField::Num(n)) => {
                let ns = n.to_string();
                values.contains(&ns)
            }
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
        // Backreferences require a binding environment, which single-event token
        // rules don't have.
        AttrPred::EqRef { .. } | AttrPred::NeRef { .. } => false,
    }
}

fn token_matches(preds: &[AttrPred], token: &Token, it: &Interner) -> bool {
    preds.iter().all(|p| token_pred(p, token, it))
}

fn violation_from_token(rule: &CompiledRule, ts: &TokenStream, token: &Token) -> Violation {
    let mut attrs = BTreeMap::new();
    attrs.insert(
        "node_kind".to_string(),
        serde_json::json!(ts.interner.resolve(token.kind)),
    );
    attrs.insert(
        "text".to_string(),
        serde_json::json!(ts.interner.resolve(token.text)),
    );
    attrs.insert("named".to_string(), serde_json::json!(token.named));
    Violation {
        rule_id: rule.id.clone(),
        severity: rule.severity,
        description: rule.description.clone(),
        message: rule.message.clone(),
        file: (*ts.file).clone(),
        span: ts.span_of(token),
        matched_event: MatchedEvent {
            kind: EventKind::Token,
            attrs,
        },
    }
}

fn run_token_rule(
    rule: &CompiledRule,
    streams: &[TokenStream],
    ctx: &MatchContext,
    out: &mut Vec<Violation>,
) {
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
        for token in &ts.tokens {
            if token_matches(&rule.preds, token, &ts.interner) {
                out.push(violation_from_token(rule, ts, token));
            }
        }
    }
}

/// Run all rules against the indexed events, returning violations.
pub fn run(
    rules: &[CompiledRule],
    index: &EventIndex,
    tokens: &[TokenStream],
    ctx: &MatchContext,
) -> Vec<Violation> {
    let mut out = Vec::new();
    // Pass 1: primary rules. Token rules match the compact token stream; all
    // others match the canonical event index.
    for rule in rules {
        if rule.is_aggregate() {
            continue;
        }
        if rule.references_kind(EventKind::Token) {
            run_token_rule(rule, tokens, ctx, &mut out);
        } else {
            match &rule.sequence {
                Some(seq) => run_sequence(rule, seq, index, ctx, &mut out),
                None => run_single(rule, index, ctx, &mut out),
            }
        }
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

fn run_single(rule: &CompiledRule, index: &EventIndex, ctx: &MatchContext, out: &mut Vec<Violation>) {
    for event in index.of_kind(rule.event) {
        if !rule.applies_to_language(event.language) {
            continue;
        }
        let file = event.file.as_str();
        if !rule.path_in_scope(file) {
            continue;
        }
        if !attrs_match(rule, event) {
            continue;
        }
        if let Some(ws) = &rule.where_scope {
            if !where_scope_holds(ws, index.file_events(file), event) {
                continue;
            }
        }
        if ctx.waiver_covers(&rule.id, file) || suppressed(rule, file, ctx) {
            continue;
        }
        out.push(violation_from(rule, event));
    }
}

fn run_sequence(
    rule: &CompiledRule,
    seq: &CompiledSequence,
    index: &EventIndex,
    ctx: &MatchContext,
    out: &mut Vec<Violation>,
) {
    for (file, events) in &index.by_file {
        let Some(first) = events.first() else { continue };
        if !rule.applies_to_language(first.language) {
            continue;
        }
        if !rule.path_in_scope(file) {
            continue;
        }
        if ctx.waiver_covers(&rule.id, file) || suppressed(rule, file, ctx) {
            continue;
        }

        let regions: Vec<&[&Event]> = if seq.within_scope {
            scope_regions(events)
        } else {
            vec![events.as_slice()]
        };

        // A given anchor (e.g. one malloc) may sit inside several nested
        // scopes; report it at most once per rule per file.
        let mut reported: HashSet<usize> = HashSet::new();
        for region in regions {
            for anchor_idx in match_sequence(seq, region) {
                let anchor = region[anchor_idx];
                if reported.insert(anchor.span.start_byte) {
                    out.push(violation_from(rule, anchor));
                }
            }
        }
    }
}

fn scope_id(ev: &Event) -> Option<String> {
    ev.attr_strings("scope_id").into_iter().next()
}

/// Partition a file's (position-sorted) events into one slice per scope: the
/// events strictly between each `ScopeStart` and its matching `ScopeEnd`.
fn scope_regions<'a>(events: &'a [&'a Event]) -> Vec<&'a [&'a Event]> {
    let mut open: HashMap<String, usize> = HashMap::new();
    let mut regions = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        match ev.kind {
            EventKind::ScopeStart => {
                if let Some(id) = scope_id(ev) {
                    open.insert(id, i);
                }
            }
            EventKind::ScopeEnd => {
                if let Some(id) = scope_id(ev) {
                    if let Some(a) = open.remove(&id) {
                        if a < i {
                            regions.push(&events[a + 1..i]);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    regions
}

/// The byte range `(lo, hi)` of the innermost `ScopeStart`/`ScopeEnd` pair
/// enclosing `event`, or the whole file when nothing encloses it (§8.9).
fn enclosing_range(file_events: &[&Event], event: &Event) -> (usize, usize) {
    let target = event.span.start_byte;
    let mut open: HashMap<String, usize> = HashMap::new();
    let mut best: Option<(usize, usize)> = None;
    for ev in file_events {
        match ev.kind {
            EventKind::ScopeStart => {
                if let Some(id) = scope_id(ev) {
                    open.insert(id, ev.span.start_byte);
                }
            }
            EventKind::ScopeEnd => {
                if let Some(id) = scope_id(ev) {
                    if let Some(lo) = open.remove(&id) {
                        let hi = ev.span.start_byte;
                        if lo < target && target < hi && best.is_none_or(|(blo, _)| lo > blo) {
                            best = Some((lo, hi)); // innermost = latest opening
                        }
                    }
                }
            }
            _ => {}
        }
    }
    best.unwrap_or((usize::MIN, usize::MAX))
}

/// Evaluate a `where_scope` clause against the matched event's enclosing scope.
fn where_scope_holds(ws: &CompiledWhereScope, file_events: &[&Event], event: &Event) -> bool {
    let (lo, hi) = enclosing_range(file_events, event);
    let target = event.span.start_byte;
    let in_scope = |e: &&Event| {
        let b = e.span.start_byte;
        b > lo && b < hi && !std::ptr::eq(*e, event)
    };
    if let Some(m) = &ws.contains {
        if !file_events.iter().copied().filter(in_scope).any(|e| m.matches(e)) {
            return false;
        }
    }
    if let Some(m) = &ws.not_contains {
        if file_events.iter().copied().filter(in_scope).any(|e| m.matches(e)) {
            return false;
        }
    }
    if let Some(m) = &ws.followed_by {
        let found = file_events
            .iter()
            .copied()
            .filter(in_scope)
            .any(|e| e.span.start_byte > target && m.matches(e));
        if !found {
            return false;
        }
    }
    true
}

/// Every anchor index (within `region`) at which the sequence matches. A match
/// must consume the whole region from its start position onward (end-anchored),
/// which is what makes a trailing negated run mean "none in the rest of the
/// scope" (proposal §8.6). Matching is attempted from each position, so the
/// first step may begin anywhere and every distinct occurrence is reported.
fn match_sequence(seq: &CompiledSequence, region: &[&Event]) -> Vec<usize> {
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
    region: &[&Event],
    ei: usize,
    binds: &Bindings,
    no_caps: bool,
    failed: &mut HashSet<(usize, usize)>,
) -> bool {
    if si == steps.len() {
        // The whole region must be consumed (end-anchored).
        return ei == region.len();
    }
    if no_caps && failed.contains(&(si, ei)) {
        return false;
    }

    let step = &steps[si];

    let result = (|| {
        match step.quant {
            Quant::One => {
                if ei < region.len() && step.accepts(region[ei], binds) {
                    let b2 = step.apply_bind(region[ei], binds);
                    return match_here(steps, si + 1, region, ei + 1, &b2, no_caps, failed);
                }
                false
            }
            Quant::Optional => {
                if ei < region.len() && step.accepts(region[ei], binds) {
                    let b2 = step.apply_bind(region[ei], binds);
                    if match_here(steps, si + 1, region, ei + 1, &b2, no_caps, failed) {
                        return true;
                    }
                }
                match_here(steps, si + 1, region, ei, binds, no_caps, failed)
            }
            Quant::ZeroOrMore => {
                // Try consuming zero (move on) first, then consume one and stay.
                if match_here(steps, si + 1, region, ei, binds, no_caps, failed) {
                    return true;
                }
                if ei < region.len() && step.accepts(region[ei], binds) {
                    let b2 = step.apply_bind(region[ei], binds);
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
// Aggregation rules (compose / count, §8.10) — a post-pass over violations.
// ---------------------------------------------------------------------------

fn func_of(v: &Violation) -> String {
    v.matched_event
        .attrs
        .get("function")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn violation_key(v: &Violation, key: &[KeyPart]) -> Vec<String> {
    key.iter()
        .map(|p| match p {
            KeyPart::File => v.file.to_string(),
            KeyPart::Function => func_of(v),
        })
        .collect()
}

/// Build an aggregate violation from a representative source violation, carrying
/// the aggregate rule's own id/severity/message but the source's file/span.
fn aggregate_violation(rule: &CompiledRule, rep: &Violation) -> Violation {
    Violation {
        rule_id: rule.id.clone(),
        severity: rule.severity,
        description: rule.description.clone(),
        message: rule.message.clone(),
        file: rep.file.clone(),
        span: rep.span.clone(),
        matched_event: rep.matched_event.clone(),
    }
}

fn emit_aggregate(
    rule: &CompiledRule,
    rep: &Violation,
    ctx: &MatchContext,
    out: &mut Vec<Violation>,
) {
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
    // Per referenced rule: locus key -> first violation at that locus.
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
    use codepolicy_events::Language;
    use codepolicy_rules::load;
    use serde_json::json;

    fn ev_at(kind: EventKind, file: &str, start: usize, attrs: serde_json::Value) -> Event {
        let mut e = Event::new(
            kind,
            Language::Typescript,
            Utf8PathBuf::from(file),
            Span {
                start_byte: start,
                end_byte: start + 1,
                start_line: start + 1,
                start_col: 1,
                end_line: start + 1,
                end_col: 2,
            },
        );
        if let serde_json::Value::Object(map) = attrs {
            for (k, v) in map {
                e.attrs.insert(k, v);
            }
        }
        e
    }

    #[test]
    fn single_event_rule_still_works() {
        let yaml = r##"
rules:
  - id: NO_JOTAI
    severity: error
    match:
      event: Import
      attrs: { source: "jotai" }
"##;
        let (rules, _) = load(yaml).unwrap();
        let events = vec![
            ev_at(EventKind::Import, "a.ts", 0, json!({"source": "jotai"})),
            ev_at(EventKind::Import, "a.ts", 10, json!({"source": "react"})),
        ];
        let index = EventIndex::build(&events);
        let v = run(&rules, &index, &[], &MatchContext::default());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "NO_JOTAI");
    }

    fn scope_pair(file: &str, start: usize, end: usize, id: &str) -> Vec<Event> {
        vec![
            ev_at(EventKind::ScopeStart, file, start, json!({ "scope_id": id })),
            ev_at(EventKind::ScopeEnd, file, end, json!({ "scope_id": id })),
        ]
    }

    fn alloc_free_rule() -> Vec<CompiledRule> {
        let yaml = r##"
rules:
  - id: ALLOC_WITHOUT_FREE
    severity: warning
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - { event: Call, attrs: { name: "malloc" } }
        - { event: Call, attrs: { name: "free" }, negate: true, quant: zero_or_more }
"##;
        load(yaml).unwrap().0
    }

    #[test]
    fn sequence_fires_when_free_absent() {
        let mut events = scope_pair("a.ts", 0, 100, "s0");
        events.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "malloc" })));
        events.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "log" })));
        let index = EventIndex::build(&events);
        let v = run(&alloc_free_rule(), &index, &[], &MatchContext::default());
        assert_eq!(v.len(), 1, "malloc with no free should fire: {v:?}");
        assert_eq!(v[0].span.start_byte, 10, "anchor should be the malloc");
    }

    #[test]
    fn sequence_suppressed_when_free_present() {
        let mut events = scope_pair("a.ts", 0, 100, "s0");
        events.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "malloc" })));
        events.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "free" })));
        let index = EventIndex::build(&events);
        let v = run(&alloc_free_rule(), &index, &[], &MatchContext::default());
        assert!(v.is_empty(), "free present should suppress: {v:?}");
    }

    #[test]
    fn capture_backreference_matches_same_object_only() {
        // Lock on `a`, unlock on `b` -> the lock on `a` is never released.
        let yaml = r##"
rules:
  - id: LOCK_WITHOUT_UNLOCK
    severity: warning
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - event: Call
          attrs: { name: "lock" }
          bind: { obj: receiver }
        - event: Call
          attrs: { name: "unlock", receiver.eq_ref: obj }
          negate: true
          quant: zero_or_more
"##;
        let (rules, _) = load(yaml).unwrap();

        // unlock on a different object -> still a violation.
        let mut bad = scope_pair("a.ts", 0, 100, "s0");
        bad.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "lock", "receiver": "a" })));
        bad.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "unlock", "receiver": "b" })));
        let v = run(&rules, &EventIndex::build(&bad), &[], &MatchContext::default());
        assert_eq!(v.len(), 1, "unlock on wrong object should fire: {v:?}");

        // unlock on the same object -> no violation.
        let mut good = scope_pair("a.ts", 0, 100, "s0");
        good.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "lock", "receiver": "a" })));
        good.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "unlock", "receiver": "a" })));
        let v = run(&rules, &EventIndex::build(&good), &[], &MatchContext::default());
        assert!(v.is_empty(), "unlock on same object should suppress: {v:?}");
    }

    #[test]
    fn alternation_treats_either_closer() {
        let yaml = r##"
rules:
  - id: TXN_WITHOUT_CLOSE
    severity: warning
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - { event: Call, attrs: { name: "begin" } }
        - alt:
            - [ { event: Call, attrs: { name: "commit" } } ]
            - [ { event: Call, attrs: { name: "rollback" } } ]
          negate: true
          quant: zero_or_more
"##;
        let (rules, _) = load(yaml).unwrap();

        let mut open = scope_pair("a.ts", 0, 100, "s0");
        open.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "begin" })));
        open.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "work" })));
        assert_eq!(
            run(&rules, &EventIndex::build(&open), &[], &MatchContext::default()).len(),
            1
        );

        let mut closed = scope_pair("a.ts", 0, 100, "s0");
        closed.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "begin" })));
        closed.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "rollback" })));
        assert!(run(&rules, &EventIndex::build(&closed), &[], &MatchContext::default()).is_empty());
    }

    #[test]
    fn where_scope_not_contains() {
        let yaml = r##"
rules:
  - id: ACQUIRE_WITHOUT_RELEASE
    severity: warning
    match:
      event: Call
      attrs: { name: "acquire" }
    where_scope:
      not_contains:
        event: Call
        attrs: { name: "release" }
"##;
        let (rules, _) = load(yaml).unwrap();

        // acquire + release in the same scope -> ok.
        let mut ok = scope_pair("a.ts", 0, 100, "s0");
        ok.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "acquire" })));
        ok.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "release" })));
        assert!(run(&rules, &EventIndex::build(&ok), &[], &MatchContext::default()).is_empty());

        // acquire with no release in scope -> fires on the acquire.
        let mut bad = scope_pair("a.ts", 0, 100, "s0");
        bad.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "acquire" })));
        let v = run(&rules, &EventIndex::build(&bad), &[], &MatchContext::default());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].span.start_byte, 10);
    }

    #[test]
    fn where_scope_followed_by_respects_order() {
        let yaml = r##"
rules:
  - id: OPEN_THEN_USE
    severity: warning
    match:
      event: Call
      attrs: { name: "open" }
    where_scope:
      followed_by:
        event: Call
        attrs: { name: "use" }
"##;
        let (rules, _) = load(yaml).unwrap();

        // `use` after `open` -> followed_by holds.
        let mut after = scope_pair("a.ts", 0, 100, "s0");
        after.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "open" })));
        after.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "use" })));
        assert_eq!(
            run(&rules, &EventIndex::build(&after), &[], &MatchContext::default()).len(),
            1
        );

        // `use` before `open` -> not "followed by".
        let mut before = scope_pair("a.ts", 0, 100, "s0");
        before.push(ev_at(EventKind::Call, "a.ts", 10, json!({ "name": "use" })));
        before.push(ev_at(EventKind::Call, "a.ts", 20, json!({ "name": "open" })));
        assert!(run(&rules, &EventIndex::build(&before), &[], &MatchContext::default()).is_empty());
    }

    #[test]
    fn count_fires_only_above_threshold_per_file() {
        let yaml = r##"
rules:
  - id: ENV
    severity: warning
    match: { event: EnvAccess }
  - id: TOO_MANY_ENV
    severity: error
    count: { rule: ENV, scope: file, op: gt, n: 2 }
    message: "too many env reads"
"##;
        let (rules, _) = load(yaml).unwrap();
        let events = vec![
            ev_at(EventKind::EnvAccess, "a.ts", 1, json!({ "name": "A" })),
            ev_at(EventKind::EnvAccess, "a.ts", 2, json!({ "name": "B" })),
            ev_at(EventKind::EnvAccess, "a.ts", 3, json!({ "name": "C" })),
            ev_at(EventKind::EnvAccess, "b.ts", 1, json!({ "name": "A" })),
        ];
        let v = run(&rules, &EventIndex::build(&events), &[], &MatchContext::default());
        let too_many: Vec<_> = v.iter().filter(|x| x.rule_id == "TOO_MANY_ENV").collect();
        assert_eq!(too_many.len(), 1, "only a.ts (3 > 2) trips count: {v:?}");
        assert_eq!(too_many[0].file.as_str(), "a.ts");
        // The primary ENV violations are still reported.
        assert_eq!(v.iter().filter(|x| x.rule_id == "ENV").count(), 4);
    }

    #[test]
    fn compose_intersection_by_function() {
        let yaml = r##"
rules:
  - id: CALLS_ACQUIRE
    severity: warning
    match: { event: Call, attrs: { name: "acquire" } }
  - id: CALLS_RISKY
    severity: warning
    match: { event: Call, attrs: { name: "risky" } }
  - id: ACQUIRE_AND_RISKY
    severity: error
    compose: { op: intersection, of: [CALLS_ACQUIRE, CALLS_RISKY], key: [file, function] }
    message: "function does both"
"##;
        let (rules, _) = load(yaml).unwrap();
        let events = vec![
            ev_at(EventKind::Call, "a.ts", 1, json!({ "name": "acquire", "function": "f" })),
            ev_at(EventKind::Call, "a.ts", 2, json!({ "name": "risky", "function": "f" })),
            ev_at(EventKind::Call, "a.ts", 3, json!({ "name": "acquire", "function": "g" })),
        ];
        let v = run(&rules, &EventIndex::build(&events), &[], &MatchContext::default());
        let composed: Vec<_> = v.iter().filter(|x| x.rule_id == "ACQUIRE_AND_RISKY").collect();
        assert_eq!(composed.len(), 1, "only f does both: {v:?}");
        assert_eq!(
            composed[0]
                .matched_event
                .attrs
                .get("function")
                .and_then(|x| x.as_str()),
            Some("f")
        );
    }
}
