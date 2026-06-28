//! Rule schema and compilation (proposal §8, §9.1.1).
//!
//! Rules are written in YAML, parsed into [`RawRule`], then compiled into a
//! [`CompiledRule`] whose globs and regexes are pre-built for fast matching.
//! A rule body is either a single-event `match` (§8.2–8.5) or a
//! `match_sequence` (§8.6–8.7) with quantifiers, alternation, negation, and
//! capture/backreferences.

use codepolicy_events::{Event, EventKind, Language};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod dsl;

/// Capture environment threaded through a sequence match.
pub type Bindings = BTreeMap<String, String>;

#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("failed to parse rules: {0}")]
    Parse(#[from] serde_yaml_ng::Error),
    #[error("rule `{rule}`: {msg}")]
    Compile { rule: String, msg: String },
}

/// Severity of a rule violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "ERROR",
            Severity::Warning => "WARNING",
        }
    }
}

// ---------------------------------------------------------------------------
// Raw (deserialized) schema
// ---------------------------------------------------------------------------

/// Top-level config file: a list of rules plus optional escape-hatch dirs.
#[derive(Debug, Deserialize)]
pub struct RuleFile {
    #[serde(default)]
    pub rules: Vec<RawRule>,
    /// Directory of structured waivers (proposal §14). Default `.codepolicy/waivers`.
    #[serde(default)]
    pub waivers_dir: Option<String>,
    /// Directory of ADRs (proposal §14). Default `docs/adr`.
    #[serde(default)]
    pub adr_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RawRule {
    pub id: String,
    pub severity: Severity,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub applies_to: Option<AppliesTo>,
    /// Single-event body (§8.2–8.5). Mutually exclusive with `match_sequence`.
    #[serde(rename = "match", default)]
    pub match_: Option<RawMatch>,
    /// Sequence body (§8.6–8.7).
    #[serde(default)]
    pub match_sequence: Option<RawSequence>,
    /// Scope-relative predicates on a single-event match (§8.9).
    #[serde(default)]
    pub where_scope: Option<RawWhereScope>,
    /// Set algebra over other rules' violations (§8.10).
    #[serde(default)]
    pub compose: Option<RawCompose>,
    /// Cardinality threshold over another rule's violations (§8.10).
    #[serde(default)]
    pub count: Option<RawCount>,
    #[serde(default)]
    pub unless: Option<RawUnless>,
}

#[derive(Debug, Deserialize)]
pub struct RawWhereScope {
    #[serde(default)]
    pub contains: Option<RawMatch>,
    #[serde(default)]
    pub not_contains: Option<RawMatch>,
    #[serde(default)]
    pub followed_by: Option<RawMatch>,
}

#[derive(Debug, Deserialize)]
pub struct RawCompose {
    pub op: String,
    pub of: Vec<String>,
    #[serde(default)]
    pub key: Vec<String>,
}

fn default_count_scope() -> String {
    "file".to_string()
}

#[derive(Debug, Deserialize)]
pub struct RawCount {
    pub rule: String,
    #[serde(default = "default_count_scope")]
    pub scope: String,
    pub op: String,
    pub n: u64,
}

#[derive(Debug, Deserialize)]
pub struct AppliesTo {
    #[serde(default)]
    pub languages: Option<Vec<Language>>,
    #[serde(default)]
    pub paths: Option<PathFilter>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PathFilter {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct RawMatch {
    pub event: EventKind,
    #[serde(default)]
    pub attrs: BTreeMap<String, serde_yaml_ng::Value>,
}

#[derive(Debug, Deserialize)]
pub struct RawSequence {
    #[serde(default)]
    pub anchor: Option<RawAnchor>,
    pub steps: Vec<RawStep>,
}

#[derive(Debug, Deserialize)]
pub struct RawAnchor {
    /// e.g. "ScopeStart..ScopeEnd" — restrict the sequence to one enclosing scope.
    #[serde(default)]
    pub within: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RawStep {
    /// Event kind, or "Any" for the wildcard. Omitted with `alt`.
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, serde_yaml_ng::Value>,
    #[serde(default)]
    pub quant: Option<String>,
    #[serde(default)]
    pub negate: Option<bool>,
    /// Capture: `{ var: attr }` — bind the (scalar) value of `attr` to `var`.
    #[serde(default)]
    pub bind: Option<BTreeMap<String, String>>,
    /// Alternation: a list of single-step alternatives.
    #[serde(default)]
    pub alt: Option<Vec<Vec<RawStep>>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawUnless {
    #[serde(default, rename = "path.matches")]
    pub path_matches: Option<Vec<String>>,
    #[serde(default, rename = "waiver.exists")]
    pub waiver_exists: Option<WaiverGuard>,
    #[serde(default, rename = "adr.exists")]
    pub adr_exists: Option<AdrGuard>,
}

#[derive(Debug, Deserialize)]
pub struct WaiverGuard {
    #[serde(default)]
    pub rule: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AdrGuard {
    pub topic: String,
}

// ---------------------------------------------------------------------------
// Compiled representation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Gt,
    Lt,
    Ge,
    Le,
}

impl CmpOp {
    pub fn test(self, x: f64, n: f64) -> bool {
        match self {
            CmpOp::Gt => x > n,
            CmpOp::Lt => x < n,
            CmpOp::Ge => x >= n,
            CmpOp::Le => x <= n,
        }
    }
}

/// One compiled attribute predicate (proposal §9.1.1).
#[derive(Debug, Clone)]
pub enum AttrPred {
    /// `attr: v` — the attribute's string forms contain `v`.
    Eq { attr: String, value: String },
    /// `attr.any: [..]` — the attribute overlaps the listed values.
    AnyOf { attr: String, values: Vec<String> },
    /// `attr.regex: p` / `attr.any.regex: p` — some string form matches `p`.
    Regex { attr: String, re: Regex },
    /// `attr.not.regex: p` — **no** string form matches `p`.
    NotRegex { attr: String, re: Regex },
    /// `attr.gt/.lt/.ge/.le: n` — numeric comparison (§8.8).
    Cmp { attr: String, op: CmpOp, n: f64 },
    /// `attr.eq_ref: var` — attribute equals a captured value (§8.7).
    EqRef { attr: String, var: String },
    /// `attr.ne_ref: var` — attribute differs from a captured value (§8.7).
    NeRef { attr: String, var: String },
}

impl AttrPred {
    pub fn attr_name(&self) -> &str {
        match self {
            AttrPred::Eq { attr, .. }
            | AttrPred::AnyOf { attr, .. }
            | AttrPred::Regex { attr, .. }
            | AttrPred::NotRegex { attr, .. }
            | AttrPred::Cmp { attr, .. }
            | AttrPred::EqRef { attr, .. }
            | AttrPred::NeRef { attr, .. } => attr,
        }
    }

    /// Evaluate against an event's normalized attribute strings, with a capture
    /// environment for `eq_ref`/`ne_ref` (empty for single-event rules).
    pub fn eval(&self, values: &[String], binds: &Bindings) -> bool {
        match self {
            AttrPred::Eq { value, .. } => values.iter().any(|v| v == value),
            AttrPred::AnyOf { values: set, .. } => values.iter().any(|v| set.contains(v)),
            AttrPred::Regex { re, .. } => values.iter().any(|v| re.is_match(v)),
            AttrPred::NotRegex { re, .. } => !values.iter().any(|v| re.is_match(v)),
            AttrPred::Cmp { op, n, .. } => values
                .iter()
                .any(|v| v.parse::<f64>().map(|x| op.test(x, *n)).unwrap_or(false)),
            AttrPred::EqRef { var, .. } => {
                binds.get(var).is_some_and(|bound| values.iter().any(|v| v == bound))
            }
            AttrPred::NeRef { var, .. } => match binds.get(var) {
                Some(bound) => !values.is_empty() && values.iter().all(|v| v != bound),
                None => false,
            },
        }
    }
}

/// Quantifier on a sequence step. `OneOrMore` is expanded into `One` + `ZeroOrMore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quant {
    One,
    Optional,
    ZeroOrMore,
}

/// What a single sequence step matches against one event.
#[derive(Debug, Clone)]
pub enum StepMatcher {
    /// Match an event kind (None = any kind) with attribute predicates.
    Event {
        kind: Option<EventKind>,
        preds: Vec<AttrPred>,
    },
    /// Match if any alternative matches (alternation of single-event matchers).
    Alt(Vec<StepMatcher>),
}

impl StepMatcher {
    pub fn matches(&self, ev: &Event, binds: &Bindings) -> bool {
        match self {
            StepMatcher::Event { kind, preds } => {
                if let Some(k) = kind {
                    if ev.kind != *k {
                        return false;
                    }
                }
                preds
                    .iter()
                    .all(|p| p.eval(&ev.attr_strings(p.attr_name()), binds))
            }
            StepMatcher::Alt(alts) => alts.iter().any(|m| m.matches(ev, binds)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompiledStep {
    pub matcher: StepMatcher,
    pub quant: Quant,
    pub negate: bool,
    /// `(var, attr)` pairs — capture each `attr`'s (scalar) value into `var`
    /// when this step matches. Empty if the step binds nothing. A step may bind
    /// several values at once (e.g. both `receiver` and `string_args`).
    pub bind: Vec<(String, String)>,
}

impl CompiledStep {
    /// Whether this step accepts the given event (honoring `negate`).
    pub fn accepts(&self, ev: &Event, binds: &Bindings) -> bool {
        let m = self.matcher.matches(ev, binds);
        if self.negate {
            !m
        } else {
            m
        }
    }

    /// Apply this step's captures (if any) to produce an extended environment.
    pub fn apply_bind(&self, ev: &Event, binds: &Bindings) -> Bindings {
        if self.bind.is_empty() {
            return binds.clone();
        }
        let mut next = binds.clone();
        for (var, attr) in &self.bind {
            if let Some(v) = ev.attr_strings(attr).into_iter().next() {
                next.insert(var.clone(), v);
            }
        }
        next
    }
}

/// A single-event matcher used by `where_scope` clauses (§8.9).
#[derive(Debug, Clone)]
pub struct EventMatcher {
    pub kind: EventKind,
    pub preds: Vec<AttrPred>,
}

impl EventMatcher {
    pub fn matches(&self, ev: &Event) -> bool {
        if ev.kind != self.kind {
            return false;
        }
        let empty = Bindings::new();
        self.preds
            .iter()
            .all(|p| p.eval(&ev.attr_strings(p.attr_name()), &empty))
    }
}

/// Scope-relative predicates evaluated against the matched event's enclosing
/// `ScopeStart`/`ScopeEnd` region (§8.9). All present clauses are ANDed.
#[derive(Debug)]
pub struct CompiledWhereScope {
    pub contains: Option<EventMatcher>,
    pub not_contains: Option<EventMatcher>,
    pub followed_by: Option<EventMatcher>,
}

/// Set-algebra operator for `compose` (§8.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    Intersection,
    Union,
    Difference,
}

/// One component of a `compose` locus key (§8.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPart {
    File,
    Function,
}

#[derive(Debug)]
pub struct CompiledCompose {
    pub op: SetOp,
    pub of: Vec<String>,
    pub key: Vec<KeyPart>,
}

/// Scope over which a `count` rule aggregates (§8.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountScope {
    File,
    Function,
}

/// Comparison operator for a `count` threshold (§8.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountOp {
    Gt,
    Lt,
    Ge,
    Le,
    Eq,
}

impl CountOp {
    pub fn test(self, x: u64, n: u64) -> bool {
        match self {
            CountOp::Gt => x > n,
            CountOp::Lt => x < n,
            CountOp::Ge => x >= n,
            CountOp::Le => x <= n,
            CountOp::Eq => x == n,
        }
    }
}

#[derive(Debug)]
pub struct CompiledCount {
    pub rule: String,
    pub scope: CountScope,
    pub op: CountOp,
    pub n: u64,
}

#[derive(Debug)]
pub struct CompiledSequence {
    /// True when anchored to a single `ScopeStart..ScopeEnd` region.
    pub within_scope: bool,
    /// Steps, with an implicit leading `Any*` already prepended (index 0).
    pub steps: Vec<CompiledStep>,
    /// Whether any step binds or backreferences (disables (si,ei) memoization).
    pub has_captures: bool,
}

#[derive(Debug)]
pub struct CompiledUnless {
    pub path_matches: Option<GlobSet>,
    pub waiver_rule: Option<String>,
    pub adr_topic: Option<String>,
}

#[derive(Debug)]
pub struct CompiledRule {
    pub id: String,
    pub severity: Severity,
    pub description: Option<String>,
    pub message: Option<String>,
    pub languages: Option<Vec<Language>>,
    pub include: Option<GlobSet>,
    pub exclude: Option<GlobSet>,
    /// Single-event kind. Meaningful only when `sequence` is `None`.
    pub event: EventKind,
    /// Single-event predicates. Meaningful only when `sequence` is `None`.
    pub preds: Vec<AttrPred>,
    /// Present for `match_sequence` rules (§8.6).
    pub sequence: Option<CompiledSequence>,
    /// Present for single-event rules carrying a `where_scope` clause (§8.9).
    pub where_scope: Option<CompiledWhereScope>,
    /// Present for `compose` rules (§8.10); a post-pass over other violations.
    pub compose: Option<CompiledCompose>,
    /// Present for `count` rules (§8.10); a post-pass over other violations.
    pub count: Option<CompiledCount>,
    pub unless: Option<CompiledUnless>,
}

impl CompiledRule {
    /// Whether this rule is a post-pass aggregation over other rules' results.
    pub fn is_aggregate(&self) -> bool {
        self.compose.is_some() || self.count.is_some()
    }
}

impl CompiledRule {
    pub fn applies_to_language(&self, language: Language) -> bool {
        match &self.languages {
            None => true,
            Some(langs) => langs.contains(&language),
        }
    }

    pub fn path_in_scope(&self, rel_path: &str) -> bool {
        if let Some(exclude) = &self.exclude {
            if exclude.is_match(rel_path) {
                return false;
            }
        }
        match &self.include {
            None => true,
            Some(include) => include.is_match(rel_path),
        }
    }

    /// Whether any matcher in this rule references events of `kind` (single
    /// event, sequence steps, or `where_scope` clauses). Used to decide whether
    /// the generic `Token` stream must be emitted.
    pub fn references_kind(&self, kind: EventKind) -> bool {
        if self.sequence.is_none()
            && self.compose.is_none()
            && self.count.is_none()
            && self.event == kind
        {
            return true;
        }
        if let Some(ws) = &self.where_scope {
            for m in [&ws.contains, &ws.not_contains, &ws.followed_by]
                .into_iter()
                .flatten()
            {
                if m.kind == kind {
                    return true;
                }
            }
        }
        if let Some(seq) = &self.sequence {
            if seq.steps.iter().any(|s| matcher_refs_kind(&s.matcher, kind)) {
                return true;
            }
        }
        false
    }
}

fn matcher_refs_kind(m: &StepMatcher, kind: EventKind) -> bool {
    match m {
        StepMatcher::Event { kind: Some(k), .. } => *k == kind,
        StepMatcher::Event { kind: None, .. } => false,
        StepMatcher::Alt(alts) => alts.iter().any(|a| matcher_refs_kind(a, kind)),
    }
}

// ---------------------------------------------------------------------------
// Loading & compilation
// ---------------------------------------------------------------------------

pub fn parse(yaml: &str) -> Result<RuleFile, RuleError> {
    Ok(serde_yaml_ng::from_str(yaml)?)
}

pub fn load(yaml: &str) -> Result<(Vec<CompiledRule>, RuleFile), RuleError> {
    let file = parse(yaml)?;
    let rules = file
        .rules
        .iter()
        .map(compile_rule)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((rules, file))
}

fn build_globset(patterns: &[String], rule_id: &str) -> Result<Option<GlobSet>, RuleError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let glob = Glob::new(p).map_err(|e| RuleError::Compile {
            rule: rule_id.to_string(),
            msg: format!("invalid path glob `{p}`: {e}"),
        })?;
        b.add(glob);
    }
    let set = b.build().map_err(|e| RuleError::Compile {
        rule: rule_id.to_string(),
        msg: format!("could not build glob set: {e}"),
    })?;
    Ok(Some(set))
}

fn yaml_strings(v: &serde_yaml_ng::Value) -> Vec<String> {
    match v {
        serde_yaml_ng::Value::String(s) => vec![s.clone()],
        serde_yaml_ng::Value::Bool(b) => vec![b.to_string()],
        serde_yaml_ng::Value::Number(n) => vec![n.to_string()],
        serde_yaml_ng::Value::Sequence(seq) => seq.iter().flat_map(yaml_strings).collect(),
        _ => vec![],
    }
}

fn yaml_scalar(v: &serde_yaml_ng::Value) -> Option<String> {
    match v {
        serde_yaml_ng::Value::String(s) => Some(s.clone()),
        serde_yaml_ng::Value::Bool(b) => Some(b.to_string()),
        serde_yaml_ng::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn compile_pred(
    key: &str,
    value: &serde_yaml_ng::Value,
    rule_id: &str,
) -> Result<AttrPred, RuleError> {
    let mut parts = key.split('.');
    let attr = parts.next().unwrap_or("").to_string();
    let ops: Vec<&str> = parts.collect();

    let mk_err = |msg: String| RuleError::Compile {
        rule: rule_id.to_string(),
        msg,
    };
    let compile_re =
        |pat: &str| Regex::new(pat).map_err(|e| mk_err(format!("invalid regex for attr `{key}`: {e}")));
    let scalar = || {
        yaml_scalar(value).ok_or_else(|| mk_err(format!("attr `{key}` expects a scalar value")))
    };
    let number = || {
        yaml_scalar(value)
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| mk_err(format!("attr `{key}` expects a number")))
    };

    match ops.as_slice() {
        [] => Ok(AttrPred::Eq {
            attr,
            value: scalar()?,
        }),
        ["any"] => Ok(AttrPred::AnyOf {
            attr,
            values: yaml_strings(value),
        }),
        ["regex"] | ["any", "regex"] => Ok(AttrPred::Regex {
            attr,
            re: compile_re(&scalar()?)?,
        }),
        ["not", "regex"] => Ok(AttrPred::NotRegex {
            attr,
            re: compile_re(&scalar()?)?,
        }),
        ["gt"] => Ok(AttrPred::Cmp { attr, op: CmpOp::Gt, n: number()? }),
        ["lt"] => Ok(AttrPred::Cmp { attr, op: CmpOp::Lt, n: number()? }),
        ["ge"] => Ok(AttrPred::Cmp { attr, op: CmpOp::Ge, n: number()? }),
        ["le"] => Ok(AttrPred::Cmp { attr, op: CmpOp::Le, n: number()? }),
        ["eq_ref"] => Ok(AttrPred::EqRef { attr, var: scalar()? }),
        ["ne_ref"] => Ok(AttrPred::NeRef { attr, var: scalar()? }),
        other => Err(mk_err(format!(
            "unknown attribute operator `.{}` on `{attr}`",
            other.join(".")
        ))),
    }
}

fn compile_preds(
    attrs: &BTreeMap<String, serde_yaml_ng::Value>,
    rule_id: &str,
) -> Result<Vec<AttrPred>, RuleError> {
    attrs
        .iter()
        .map(|(k, v)| compile_pred(k, v, rule_id))
        .collect()
}

fn parse_event_kind(s: &str, rule_id: &str) -> Result<EventKind, RuleError> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(|_| {
        RuleError::Compile {
            rule: rule_id.to_string(),
            msg: format!("unknown event kind `{s}`"),
        }
    })
}

fn step_matcher(raw: &RawStep, rule_id: &str) -> Result<StepMatcher, RuleError> {
    if let Some(alts) = &raw.alt {
        let mut matchers = Vec::new();
        for alt in alts {
            // Each alternative must be a single step (single-event alternation).
            let [one] = alt.as_slice() else {
                return Err(RuleError::Compile {
                    rule: rule_id.to_string(),
                    msg: "each `alt` alternative must be a single step".into(),
                });
            };
            matchers.push(step_matcher(one, rule_id)?);
        }
        return Ok(StepMatcher::Alt(matchers));
    }
    let kind = match raw.event.as_deref() {
        None | Some("Any") => None,
        Some(k) => Some(parse_event_kind(k, rule_id)?),
    };
    Ok(StepMatcher::Event {
        kind,
        preds: compile_preds(&raw.attrs, rule_id)?,
    })
}

fn parse_quant(raw: &RawStep, rule_id: &str) -> Result<Quant, RuleError> {
    match raw.quant.as_deref() {
        None | Some("one") => Ok(Quant::One),
        Some("optional") => Ok(Quant::Optional),
        Some("zero_or_more") => Ok(Quant::ZeroOrMore),
        // one_or_more is expanded by the caller; treated as One here.
        Some("one_or_more") => Ok(Quant::One),
        Some(other) => Err(RuleError::Compile {
            rule: rule_id.to_string(),
            msg: format!("unknown quantifier `{other}`"),
        }),
    }
}

fn compile_step(raw: &RawStep, rule_id: &str) -> Result<Vec<CompiledStep>, RuleError> {
    let matcher = step_matcher(raw, rule_id)?;
    let negate = raw.negate.unwrap_or(false);
    let bind: Vec<(String, String)> = raw
        .bind
        .as_ref()
        .map(|m| {
            m.iter()
                .map(|(var, attr)| (var.clone(), attr.clone()))
                .collect()
        })
        .unwrap_or_default();
    let quant = parse_quant(raw, rule_id)?;

    // Expand one_or_more into One + ZeroOrMore (bind only on the first).
    if raw.quant.as_deref() == Some("one_or_more") {
        Ok(vec![
            CompiledStep {
                matcher: matcher.clone(),
                quant: Quant::One,
                negate,
                bind,
            },
            CompiledStep {
                matcher,
                quant: Quant::ZeroOrMore,
                negate,
                bind: Vec::new(),
            },
        ])
    } else {
        Ok(vec![CompiledStep {
            matcher,
            quant,
            negate,
            bind,
        }])
    }
}

fn step_has_captures(step: &CompiledStep) -> bool {
    if !step.bind.is_empty() {
        return true;
    }
    fn matcher_refs(m: &StepMatcher) -> bool {
        match m {
            StepMatcher::Event { preds, .. } => preds
                .iter()
                .any(|p| matches!(p, AttrPred::EqRef { .. } | AttrPred::NeRef { .. })),
            StepMatcher::Alt(alts) => alts.iter().any(matcher_refs),
        }
    }
    matcher_refs(&step.matcher)
}

fn compile_sequence(raw: &RawSequence, rule_id: &str) -> Result<CompiledSequence, RuleError> {
    let within_scope = raw
        .anchor
        .as_ref()
        .and_then(|a| a.within.as_deref())
        .map(|w| w.contains("ScopeStart") && w.contains("ScopeEnd"))
        .unwrap_or(false);

    // The matcher enumerates start positions, so the first step may begin
    // anywhere in the region — no implicit leading wildcard step is needed.
    let mut steps = Vec::new();
    for raw_step in &raw.steps {
        steps.extend(compile_step(raw_step, rule_id)?);
    }
    let has_captures = steps.iter().any(step_has_captures);
    Ok(CompiledSequence {
        within_scope,
        steps,
        has_captures,
    })
}

fn compile_event_matcher(m: &RawMatch, rule_id: &str) -> Result<EventMatcher, RuleError> {
    Ok(EventMatcher {
        kind: m.event,
        preds: compile_preds(&m.attrs, rule_id)?,
    })
}

fn compile_where_scope(ws: &RawWhereScope, rule_id: &str) -> Result<CompiledWhereScope, RuleError> {
    let conv = |m: &Option<RawMatch>| -> Result<Option<EventMatcher>, RuleError> {
        m.as_ref()
            .map(|m| compile_event_matcher(m, rule_id))
            .transpose()
    };
    Ok(CompiledWhereScope {
        contains: conv(&ws.contains)?,
        not_contains: conv(&ws.not_contains)?,
        followed_by: conv(&ws.followed_by)?,
    })
}

fn compile_compose(c: &RawCompose, rule_id: &str) -> Result<CompiledCompose, RuleError> {
    let mk_err = |msg: String| RuleError::Compile {
        rule: rule_id.to_string(),
        msg,
    };
    let op = match c.op.as_str() {
        "intersection" => SetOp::Intersection,
        "union" => SetOp::Union,
        "difference" => SetOp::Difference,
        other => return Err(mk_err(format!("unknown compose op `{other}`"))),
    };
    if c.of.is_empty() {
        return Err(mk_err("`compose.of` must list at least one rule id".into()));
    }
    let key = if c.key.is_empty() {
        vec![KeyPart::File]
    } else {
        c.key
            .iter()
            .map(|k| match k.as_str() {
                "file" => Ok(KeyPart::File),
                "function" => Ok(KeyPart::Function),
                other => Err(mk_err(format!("unknown compose key `{other}`"))),
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(CompiledCompose {
        op,
        of: c.of.clone(),
        key,
    })
}

fn compile_count(c: &RawCount, rule_id: &str) -> Result<CompiledCount, RuleError> {
    let mk_err = |msg: String| RuleError::Compile {
        rule: rule_id.to_string(),
        msg,
    };
    let scope = match c.scope.as_str() {
        "file" => CountScope::File,
        "function" => CountScope::Function,
        other => return Err(mk_err(format!("unknown count scope `{other}`"))),
    };
    let op = match c.op.as_str() {
        "gt" => CountOp::Gt,
        "lt" => CountOp::Lt,
        "ge" => CountOp::Ge,
        "le" => CountOp::Le,
        "eq" => CountOp::Eq,
        other => return Err(mk_err(format!("unknown count op `{other}`"))),
    };
    Ok(CompiledCount {
        rule: c.rule.clone(),
        scope,
        op,
        n: c.n,
    })
}

pub fn compile_rule(raw: &RawRule) -> Result<CompiledRule, RuleError> {
    let (languages, include, exclude) = match &raw.applies_to {
        None => (None, None, None),
        Some(applies) => {
            let (include, exclude) = match &applies.paths {
                None => (None, None),
                Some(pf) => (
                    build_globset(&pf.include, &raw.id)?,
                    build_globset(&pf.exclude, &raw.id)?,
                ),
            };
            (applies.languages.clone(), include, exclude)
        }
    };

    let unless = match &raw.unless {
        None => None,
        Some(u) => Some(CompiledUnless {
            path_matches: match &u.path_matches {
                Some(globs) => build_globset(globs, &raw.id)?,
                None => None,
            },
            waiver_rule: u
                .waiver_exists
                .as_ref()
                .map(|w| w.rule.clone().unwrap_or_else(|| raw.id.clone())),
            adr_topic: u.adr_exists.as_ref().map(|a| a.topic.clone()),
        }),
    };

    // Body: exactly one of `match` / `match_sequence` / `compose` / `count`.
    let bodies = [
        raw.match_.is_some(),
        raw.match_sequence.is_some(),
        raw.compose.is_some(),
        raw.count.is_some(),
    ]
    .into_iter()
    .filter(|b| *b)
    .count();
    if bodies != 1 {
        return Err(RuleError::Compile {
            rule: raw.id.clone(),
            msg: "rule must have exactly one of `match`, `match_sequence`, `compose`, `count`".into(),
        });
    }

    let mut event = EventKind::File; // placeholder for non-`match` bodies
    let mut preds = Vec::new();
    let mut sequence = None;
    let mut compose = None;
    let mut count = None;
    if let Some(m) = &raw.match_ {
        event = m.event;
        preds = compile_preds(&m.attrs, &raw.id)?;
    } else if let Some(seq) = &raw.match_sequence {
        sequence = Some(compile_sequence(seq, &raw.id)?);
    } else if let Some(c) = &raw.compose {
        compose = Some(compile_compose(c, &raw.id)?);
    } else if let Some(c) = &raw.count {
        count = Some(compile_count(c, &raw.id)?);
    }

    let where_scope = match &raw.where_scope {
        None => None,
        Some(ws) => {
            if raw.match_.is_none() {
                return Err(RuleError::Compile {
                    rule: raw.id.clone(),
                    msg: "`where_scope` is only valid on single-event `match` rules".into(),
                });
            }
            Some(compile_where_scope(ws, &raw.id)?)
        }
    };

    let rule = CompiledRule {
        id: raw.id.clone(),
        severity: raw.severity,
        description: raw.description.clone(),
        message: raw.message.clone(),
        languages,
        include,
        exclude,
        event,
        preds,
        sequence,
        where_scope,
        compose,
        count,
        unless,
    };

    // The compact token stream supports single-event `match Token[...]` only.
    if rule.references_kind(EventKind::Token)
        && (rule.sequence.is_some() || rule.where_scope.is_some())
    {
        return Err(RuleError::Compile {
            rule: raw.id.clone(),
            msg: "Token matching is single-event only (`match Token[...]`); \
                  sequences and where_scope over tokens are not supported"
                .into(),
        });
    }
    Ok(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_preds(rule: &CompiledRule) -> &[AttrPred] {
        assert!(rule.sequence.is_none());
        &rule.preds
    }

    #[test]
    fn compiles_attr_operators() {
        let yaml = r##"
rules:
  - id: T
    severity: error
    match:
      event: Import
      attrs:
        source: "@apollo/client"
        symbols.any: ["useQuery", "gql"]
        name.regex: ".*Query$"
        text.not.regex: "#\\d+"
        range_lines.gt: 200
"##;
        let (rules, _) = load(yaml).unwrap();
        let preds = single_preds(&rules[0]);
        assert_eq!(preds.len(), 5);
        let empty = Bindings::new();
        let anyof = preds
            .iter()
            .find(|p| matches!(p, AttrPred::AnyOf { .. }))
            .unwrap();
        assert!(anyof.eval(&["gql".into()], &empty));
        assert!(!anyof.eval(&["useMemo".into()], &empty));
        let cmp = preds.iter().find(|p| matches!(p, AttrPred::Cmp { .. })).unwrap();
        assert!(cmp.eval(&["250".into()], &empty));
        assert!(!cmp.eval(&["100".into()], &empty));
    }

    #[test]
    fn eq_ref_uses_bindings() {
        let p = AttrPred::EqRef {
            attr: "receiver".into(),
            var: "obj".into(),
        };
        let mut binds = Bindings::new();
        binds.insert("obj".into(), "mu".into());
        assert!(p.eval(&["mu".into()], &binds));
        assert!(!p.eval(&["other".into()], &binds));
        assert!(!p.eval(&["mu".into()], &Bindings::new())); // unbound -> false
    }

    #[test]
    fn compiles_sequence_with_capture() {
        let yaml = r##"
rules:
  - id: LOCK_WITHOUT_UNLOCK
    severity: warning
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - event: Call
          attrs: { name.regex: "lock$" }
          bind: { obj: receiver }
        - event: Call
          attrs: { name.regex: "unlock$", receiver.eq_ref: obj }
          negate: true
          quant: zero_or_more
"##;
        let (rules, _) = load(yaml).unwrap();
        let seq = rules[0].sequence.as_ref().unwrap();
        assert!(seq.within_scope);
        assert!(seq.has_captures);
        // two declared steps (matching enumerates start positions, no implicit step)
        assert_eq!(seq.steps.len(), 2);
        assert!(!seq.steps[0].bind.is_empty());
    }

    #[test]
    fn rejects_both_bodies() {
        let yaml = r##"
rules:
  - id: BAD
    severity: error
    match: { event: Call }
    match_sequence: { steps: [ { event: Call } ] }
"##;
        assert!(load(yaml).is_err());
    }
}
