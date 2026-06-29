//! Rule schema and compilation.
//!
//! Rules are written in the textual DSL (or YAML), parsed into [`RawRule`], then
//! compiled into a [`CompiledRule`] with globs and regexes pre-built. Every rule
//! matches over the lexeme (token) stream. A rule body is one of: a single
//! `match` (a token pattern), a `match_sequence` (ordered token patterns), a
//! `compose` set-algebra over other rules, or a `count` threshold.

use tyrant_token::{Language, TokenRef};
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
    /// Directory of structured waivers. Default `.tyrant/waivers`.
    #[serde(default)]
    pub waivers_dir: Option<String>,
    /// Directory of ADRs. Default `docs/adr`.
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
    /// Single-token-pattern body. Mutually exclusive with the others.
    #[serde(rename = "match", default)]
    pub match_: Option<RawMatch>,
    /// Sequence body — an ordered run of token patterns.
    #[serde(default)]
    pub match_sequence: Option<RawSequence>,
    /// Scope-relative predicates on a single `match`.
    #[serde(default)]
    pub where_scope: Option<RawWhereScope>,
    /// Set algebra over other rules' violations.
    #[serde(default)]
    pub compose: Option<RawCompose>,
    /// Cardinality threshold over another rule's violations.
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

/// A token pattern: a set of field predicates (`node_kind`, `class`, `text`,
/// `text.regex`, `curly_depth.gt`, …). Empty matches any token.
#[derive(Debug, Default, Deserialize)]
pub struct RawMatch {
    #[serde(flatten)]
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
    /// e.g. "ScopeStart..ScopeEnd" — restrict the sequence to one `{}` block.
    #[serde(default)]
    pub within: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RawStep {
    /// Token-pattern predicates. Empty matches any token (the wildcard).
    #[serde(default)]
    pub attrs: BTreeMap<String, serde_yaml_ng::Value>,
    #[serde(default)]
    pub quant: Option<String>,
    #[serde(default)]
    pub negate: Option<bool>,
    /// Capture: `{ var: field }` — bind the (scalar) value of `field` to `var`.
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

/// One compiled field predicate.
#[derive(Debug, Clone)]
pub enum AttrPred {
    /// `field = v` — some string form equals `v`.
    Eq { attr: String, value: String },
    /// `field in [..]` — some string form is in the set.
    AnyOf { attr: String, values: Vec<String> },
    /// `field ~ /p/` — some string form matches `p`.
    Regex { attr: String, re: Regex },
    /// `field !~ /p/` — no string form matches `p`.
    NotRegex { attr: String, re: Regex },
    /// `field >|<|>=|<= n` — numeric comparison.
    Cmp { attr: String, op: CmpOp, n: f64 },
    /// Backreference (`:var`) — some string form equals a captured value.
    EqRef { attr: String, var: String },
    /// Differs from a captured value (`field.ne_ref`; YAML form only).
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

    /// Evaluate against a token field's normalized string forms, with a capture
    /// environment for `eq_ref`/`ne_ref` (empty for single-token rules).
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

/// What a single sequence step matches against one token.
#[derive(Debug, Clone)]
pub enum StepMatcher {
    /// A token pattern — all predicates must hold (empty = any token).
    Pat(Vec<AttrPred>),
    /// Match if any alternative matches.
    Alt(Vec<StepMatcher>),
}

impl StepMatcher {
    pub fn matches(&self, t: &TokenRef, binds: &Bindings) -> bool {
        match self {
            StepMatcher::Pat(preds) => preds
                .iter()
                .all(|p| p.eval(&t.field_strings(p.attr_name()), binds)),
            StepMatcher::Alt(alts) => alts.iter().any(|m| m.matches(t, binds)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompiledStep {
    pub matcher: StepMatcher,
    pub quant: Quant,
    pub negate: bool,
    /// `(var, field)` pairs — capture each `field`'s (scalar) value into `var`
    /// when this step matches.
    pub bind: Vec<(String, String)>,
}

impl CompiledStep {
    /// Whether this step accepts the given token (honoring `negate`).
    pub fn accepts(&self, t: &TokenRef, binds: &Bindings) -> bool {
        let m = self.matcher.matches(t, binds);
        if self.negate {
            !m
        } else {
            m
        }
    }

    /// Apply this step's captures (if any) to produce an extended environment.
    pub fn apply_bind(&self, t: &TokenRef, binds: &Bindings) -> Bindings {
        if self.bind.is_empty() {
            return binds.clone();
        }
        let mut next = binds.clone();
        for (var, attr) in &self.bind {
            if let Some(v) = t.field_strings(attr).into_iter().next() {
                next.insert(var.clone(), v);
            }
        }
        next
    }
}

/// A token pattern used by `where_scope` clauses.
#[derive(Debug, Clone)]
pub struct ClauseMatcher {
    pub preds: Vec<AttrPred>,
}

impl ClauseMatcher {
    pub fn matches(&self, t: &TokenRef) -> bool {
        let empty = Bindings::new();
        self.preds
            .iter()
            .all(|p| p.eval(&t.field_strings(p.attr_name()), &empty))
    }
}

/// Scope-relative predicates evaluated against the matched token's enclosing
/// `{}` block. All present clauses are ANDed.
#[derive(Debug)]
pub struct CompiledWhereScope {
    pub contains: Option<ClauseMatcher>,
    pub not_contains: Option<ClauseMatcher>,
    pub followed_by: Option<ClauseMatcher>,
}

/// Set-algebra operator for `compose`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    Intersection,
    Union,
    Difference,
}

/// One component of a `compose`/`count` locus key.
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

/// Scope over which a `count` rule aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountScope {
    File,
    Function,
}

/// Comparison operator for a `count` threshold.
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
    /// True when anchored to a single `{}` block.
    pub within_scope: bool,
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
    /// Single-token-pattern predicates. Meaningful only when `sequence` is `None`.
    pub preds: Vec<AttrPred>,
    /// Present for `match_sequence` rules.
    pub sequence: Option<CompiledSequence>,
    /// Present for a single `match` carrying a `where_scope` clause.
    pub where_scope: Option<CompiledWhereScope>,
    /// Present for `compose` rules; a post-pass over other violations.
    pub compose: Option<CompiledCompose>,
    /// Present for `count` rules; a post-pass over other violations.
    pub count: Option<CompiledCount>,
    pub unless: Option<CompiledUnless>,
}

impl CompiledRule {
    /// Whether this rule is a post-pass aggregation over other rules' results.
    pub fn is_aggregate(&self) -> bool {
        self.compose.is_some() || self.count.is_some()
    }

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

fn step_matcher(raw: &RawStep, rule_id: &str) -> Result<StepMatcher, RuleError> {
    if let Some(alts) = &raw.alt {
        let mut matchers = Vec::new();
        for alt in alts {
            // Each alternative must be a single step.
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
    Ok(StepMatcher::Pat(compile_preds(&raw.attrs, rule_id)?))
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
            StepMatcher::Pat(preds) => preds
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

fn compile_clause_matcher(m: &RawMatch, rule_id: &str) -> Result<ClauseMatcher, RuleError> {
    Ok(ClauseMatcher {
        preds: compile_preds(&m.attrs, rule_id)?,
    })
}

fn compile_where_scope(ws: &RawWhereScope, rule_id: &str) -> Result<CompiledWhereScope, RuleError> {
    let conv = |m: &Option<RawMatch>| -> Result<Option<ClauseMatcher>, RuleError> {
        m.as_ref()
            .map(|m| compile_clause_matcher(m, rule_id))
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

    let mut preds = Vec::new();
    let mut sequence = None;
    let mut compose = None;
    let mut count = None;
    if let Some(m) = &raw.match_ {
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
                    msg: "`where_scope` is only valid on a single `match` rule".into(),
                });
            }
            Some(compile_where_scope(ws, &raw.id)?)
        }
    };

    Ok(CompiledRule {
        id: raw.id.clone(),
        severity: raw.severity,
        description: raw.description.clone(),
        message: raw.message.clone(),
        languages,
        include,
        exclude,
        preds,
        sequence,
        where_scope,
        compose,
        count,
        unless,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_attr_operators() {
        let yaml = r##"
rules:
  - id: T
    severity: error
    match:
      node_kind: "identifier"
      text.regex: ".*Query$"
      curly_depth.gt: 2
"##;
        let (rules, _) = load(yaml).unwrap();
        let preds = &rules[0].preds;
        assert!(rules[0].sequence.is_none());
        assert_eq!(preds.len(), 3);
        let empty = Bindings::new();
        let cmp = preds.iter().find(|p| matches!(p, AttrPred::Cmp { .. })).unwrap();
        assert!(cmp.eval(&["3".into()], &empty));
        assert!(!cmp.eval(&["1".into()], &empty));
        let re = preds.iter().find(|p| matches!(p, AttrPred::Regex { .. })).unwrap();
        assert!(re.eval(&["UserQuery".into()], &empty));
        assert!(!re.eval(&["UserList".into()], &empty));
    }

    #[test]
    fn eq_ref_uses_bindings() {
        let p = AttrPred::EqRef {
            attr: "text".into(),
            var: "n".into(),
        };
        let mut binds = Bindings::new();
        binds.insert("n".into(), "foo".into());
        assert!(p.eval(&["foo".into()], &binds));
        assert!(!p.eval(&["bar".into()], &binds));
        assert!(!p.eval(&["foo".into()], &Bindings::new())); // unbound -> false
    }

    #[test]
    fn compiles_token_sequence_with_capture() {
        let yaml = r##"
rules:
  - id: REPEATED_IDENT
    severity: warning
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - attrs: { class: "ident" }
          bind: { n: text }
        - attrs: { class: "ident", text.eq_ref: n }
          negate: true
          quant: zero_or_more
"##;
        let (rules, _) = load(yaml).unwrap();
        let seq = rules[0].sequence.as_ref().unwrap();
        assert!(seq.within_scope);
        assert!(seq.has_captures);
        assert_eq!(seq.steps.len(), 2);
        assert!(!seq.steps[0].bind.is_empty());
    }

    #[test]
    fn rejects_both_bodies() {
        let yaml = r##"
rules:
  - id: BAD
    severity: error
    match: { node_kind: "x" }
    match_sequence: { steps: [ { attrs: {} } ] }
"##;
        assert!(load(yaml).is_err());
    }
}
