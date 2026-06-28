//! A concise, Cobra-flavored textual rule grammar that parses into the same
//! [`RawRule`](super::RawRule) structures the YAML loader produces, then reuses
//! the existing compiler. It is purely an alternative front-end — every rule
//! expressible in YAML is expressible here, and vice versa.
//!
//! ```text
//! rule NO_MANUAL_GRAPHQL_OPERATION_TYPES (error) {
//!   lang typescript
//!   in "apps/*/src/**/*.{ts,tsx}"
//!   not in "**/generated/**", "**/*.generated.ts"
//!   match TypeDecl[name ~ /.*(Query|Mutation|Subscription)(Variables)?$/]
//!   message "Use generated GraphQL types."
//! }
//! ```
//!
//! Predicate operators (inside `Kind[ ... ]`):
//!   `attr = "v"` / `attr = 5`   equality            (`attr`)
//!   `attr ~ /re/`               regex               (`attr.regex`)
//!   `attr !~ /re/`              negated regex        (`attr.not.regex`)
//!   `attr in ["a","b"]`        membership           (`attr.any`)
//!   `attr > 5` / `< <= >=`     numeric comparison    (`attr.gt` …)
//!   `attr == $v`               equals a binding      (`attr.eq_ref`)
//!   `attr != $v`               differs from binding  (`attr.ne_ref`)
//!
//! Bodies: `match <pat> [where scope <clause>]*`, `sequence [in scope] { <step>+ }`,
//! `compose <op> of A, B [by file, function]`, `count RULE per file > N`.

use super::{
    AdrGuard, AppliesTo, CompiledRule, PathFilter, RawAnchor, RawCompose, RawCount, RawMatch,
    RawRule, RawSequence, RawStep, RawUnless, RawWhereScope, RuleFile, Severity, WaiverGuard,
};
use codepolicy_events::{EventKind, Language};
use serde_yaml_ng::Value;
use std::collections::{BTreeMap, HashSet};

/// Friendly syntactic kinds accepted in `arg: kind` annotations (matches the
/// `argN_kind` values the frontend emits). These are *syntactic* forms, not
/// static types.
const KNOWN_KINDS: &[&str] = &[
    "string",
    "template",
    "number",
    "bool",
    "identifier",
    "member",
    "call",
    "object",
    "array",
    "function",
    "regex",
    "null",
    "undefined",
    "other",
];

/// Receiver of a call-sugar pattern: a literal object or a capture variable.
enum Recv {
    Lit(String),
    Var(String),
}

/// Attribute predicates of a pattern, keyed by the compiler's suffix convention.
type Attrs = BTreeMap<String, Value>;
/// Captures introduced by a pattern: variable name -> source attribute.
type Binds = BTreeMap<String, String>;
/// A parsed pattern: (event-kind name | `None` for `any`, predicates, captures).
type Pattern = (Option<String>, Attrs, Binds);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DslError(pub String);

type R<T> = Result<T, DslError>;

/// Parse and compile rules written in the textual grammar.
pub fn load(text: &str) -> R<(Vec<CompiledRule>, RuleFile)> {
    let file = parse(text)?;
    let rules = file
        .rules
        .iter()
        .map(super::compile_rule)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| DslError(format!("{e}")))?;
    Ok((rules, file))
}

/// Parse the textual grammar into a [`RuleFile`] (no compilation).
pub fn parse(text: &str) -> R<RuleFile> {
    let toks = lex(text)?;
    let mut p = Parser {
        toks,
        pos: 0,
        bound: HashSet::new(),
    };
    p.parse_file()
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Regex(String),
    Num(String),
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    LParen,
    RParen,
    Comma,
    Eq,
    EqEq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
    Tilde,
    NotTilde,
    Star,
    Plus,
    Question,
    Pipe,
    Dollar,
    Dot,
    DotDot,
    Colon,
}

fn lex(src: &str) -> R<Vec<(Tok, usize)>> {
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut line = 1usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        let two = |a: char| i + 1 < b.len() && b[i + 1] == a;
        let push = |out: &mut Vec<(Tok, usize)>, t: Tok| out.push((t, line));
        match c {
            '{' => {
                push(&mut out, Tok::LBrace);
                i += 1;
            }
            '}' => {
                push(&mut out, Tok::RBrace);
                i += 1;
            }
            '[' => {
                push(&mut out, Tok::LBrack);
                i += 1;
            }
            ']' => {
                push(&mut out, Tok::RBrack);
                i += 1;
            }
            '(' => {
                push(&mut out, Tok::LParen);
                i += 1;
            }
            ')' => {
                push(&mut out, Tok::RParen);
                i += 1;
            }
            ',' => {
                push(&mut out, Tok::Comma);
                i += 1;
            }
            '|' => {
                push(&mut out, Tok::Pipe);
                i += 1;
            }
            '$' => {
                push(&mut out, Tok::Dollar);
                i += 1;
            }
            '*' => {
                push(&mut out, Tok::Star);
                i += 1;
            }
            '+' => {
                push(&mut out, Tok::Plus);
                i += 1;
            }
            '?' => {
                push(&mut out, Tok::Question);
                i += 1;
            }
            '~' => {
                push(&mut out, Tok::Tilde);
                i += 1;
            }
            ':' => {
                push(&mut out, Tok::Colon);
                i += 1;
            }
            '.' if two('.') => {
                push(&mut out, Tok::DotDot);
                i += 2;
            }
            '.' => {
                push(&mut out, Tok::Dot);
                i += 1;
            }
            '=' if two('=') => {
                push(&mut out, Tok::EqEq);
                i += 2;
            }
            '=' => {
                push(&mut out, Tok::Eq);
                i += 1;
            }
            '!' if two('=') => {
                push(&mut out, Tok::Ne);
                i += 2;
            }
            '!' if two('~') => {
                push(&mut out, Tok::NotTilde);
                i += 2;
            }
            '>' if two('=') => {
                push(&mut out, Tok::Ge);
                i += 2;
            }
            '>' => {
                push(&mut out, Tok::Gt);
                i += 1;
            }
            '<' if two('=') => {
                push(&mut out, Tok::Le);
                i += 2;
            }
            '<' => {
                push(&mut out, Tok::Lt);
                i += 1;
            }
            '"' => {
                let (s, ni) = read_string(&b, i + 1, line)?;
                push(&mut out, Tok::Str(s));
                i = ni;
            }
            '/' => {
                let (s, ni) = read_regex(&b, i + 1, line)?;
                push(&mut out, Tok::Regex(s));
                i = ni;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.') {
                    i += 1;
                }
                push(&mut out, Tok::Num(b[start..i].iter().collect()));
            }
            c if is_ident_start(c) => {
                let start = i;
                while i < b.len() && is_ident_char(b[i]) {
                    i += 1;
                }
                push(&mut out, Tok::Ident(b[start..i].iter().collect()));
            }
            other => {
                return Err(DslError(format!("line {line}: unexpected character `{other}`")));
            }
        }
    }
    Ok(out)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn read_string(b: &[char], mut i: usize, line: usize) -> R<(String, usize)> {
    let mut s = String::new();
    while i < b.len() {
        match b[i] {
            '"' => return Ok((s, i + 1)),
            '\\' if i + 1 < b.len() => {
                s.push(b[i + 1]);
                i += 2;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    Err(DslError(format!("line {line}: unterminated string")))
}

fn read_regex(b: &[char], mut i: usize, line: usize) -> R<(String, usize)> {
    let mut s = String::new();
    while i < b.len() {
        match b[i] {
            '/' => return Ok((s, i + 1)),
            '\\' if i + 1 < b.len() && b[i + 1] == '/' => {
                s.push('/');
                i += 2;
            }
            '\\' if i + 1 < b.len() => {
                s.push('\\');
                s.push(b[i + 1]);
                i += 2;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    Err(DslError(format!("line {line}: unterminated /regex/")))
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    /// Capture variables bound so far in the current rule (for unification:
    /// a `$var`'s first use binds, later uses match-equal).
    bound: HashSet<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }
    fn line(&self) -> usize {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map(|(_, l)| *l)
            .unwrap_or(0)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|(t, _)| t.clone());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn at_kw(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(x)) if x == s)
    }
    fn eat_kw(&mut self, s: &str) -> R<()> {
        if self.at_kw(s) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected `{s}`")))
        }
    }
    fn expect(&mut self, t: &Tok) -> R<()> {
        if self.peek() == Some(t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected {t:?}")))
        }
    }
    fn err(&self, msg: &str) -> DslError {
        DslError(format!("line {}: {msg}, found {:?}", self.line(), self.peek()))
    }
    fn ident(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Ident(s)) => Ok(s),
            _ => Err(self.err("expected an identifier")),
        }
    }
    fn string(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Str(s)) => Ok(s),
            _ => Err(self.err("expected a \"string\"")),
        }
    }

    fn parse_file(&mut self) -> R<RuleFile> {
        let mut rules = Vec::new();
        let mut waivers_dir = None;
        let mut adr_dir = None;
        while self.peek().is_some() {
            if self.at_kw("rule") {
                rules.push(self.parse_rule()?);
            } else if self.at_kw("waivers") {
                self.bump();
                waivers_dir = Some(self.string()?);
            } else if self.at_kw("adrs") {
                self.bump();
                adr_dir = Some(self.string()?);
            } else {
                return Err(self.err("expected `rule`, `waivers`, or `adrs`"));
            }
        }
        Ok(RuleFile {
            rules,
            waivers_dir,
            adr_dir,
        })
    }

    fn parse_rule(&mut self) -> R<RawRule> {
        self.eat_kw("rule")?;
        self.bound.clear(); // bindings are scoped to one rule
        let id = self.ident()?;
        let mut severity = Severity::Error;
        if self.peek() == Some(&Tok::LParen) {
            self.bump();
            let sev = self.ident()?;
            severity = match sev.as_str() {
                "error" => Severity::Error,
                "warning" => Severity::Warning,
                other => return Err(self.err(&format!("unknown severity `{other}`"))),
            };
            self.expect(&Tok::RParen)?;
        }
        self.expect(&Tok::LBrace)?;

        let mut description = None;
        let mut message = None;
        let mut languages: Option<Vec<Language>> = None;
        let mut include: Vec<String> = Vec::new();
        let mut exclude: Vec<String> = Vec::new();
        let mut unless = RawUnless::default();
        let mut has_unless = false;
        let mut match_: Option<RawMatch> = None;
        let mut match_sequence: Option<RawSequence> = None;
        let mut where_scope: Option<RawWhereScope> = None;
        let mut compose: Option<RawCompose> = None;
        let mut count: Option<RawCount> = None;

        while self.peek() != Some(&Tok::RBrace) {
            if self.peek().is_none() {
                return Err(self.err("unexpected end of input inside `rule { }`"));
            }
            if self.at_kw("lang") {
                self.bump();
                languages = Some(self.parse_langs()?);
            } else if self.at_kw("in") {
                self.bump();
                include.extend(self.parse_str_list()?);
            } else if self.at_kw("not") {
                self.bump();
                self.eat_kw("in")?;
                exclude.extend(self.parse_str_list()?);
            } else if self.at_kw("desc") {
                self.bump();
                description = Some(self.string()?);
            } else if self.at_kw("message") {
                self.bump();
                message = Some(self.string()?);
            } else if self.at_kw("unless") {
                self.bump();
                self.parse_unless(&mut unless)?;
                has_unless = true;
            } else if self.at_kw("match") {
                self.bump();
                let (kind, attrs, _binds) = self.parse_pattern()?;
                let event = self.require_kind(kind)?;
                match_ = Some(RawMatch { event, attrs });
                if self.at_kw("where") {
                    where_scope = Some(self.parse_where_scope()?);
                }
            } else if self.at_kw("sequence") {
                self.bump();
                match_sequence = Some(self.parse_sequence()?);
            } else if self.at_kw("compose") {
                self.bump();
                compose = Some(self.parse_compose()?);
            } else if self.at_kw("count") {
                self.bump();
                count = Some(self.parse_count()?);
            } else {
                return Err(self.err("unexpected statement in rule body"));
            }
        }
        self.expect(&Tok::RBrace)?;

        let applies_to = if languages.is_some() || !include.is_empty() || !exclude.is_empty() {
            let paths = if include.is_empty() && exclude.is_empty() {
                None
            } else {
                Some(PathFilter { include, exclude })
            };
            Some(AppliesTo { languages, paths })
        } else {
            None
        };

        Ok(RawRule {
            id,
            severity,
            description,
            message,
            applies_to,
            match_,
            match_sequence,
            where_scope,
            compose,
            count,
            unless: if has_unless { Some(unless) } else { None },
        })
    }

    fn parse_langs(&mut self) -> R<Vec<Language>> {
        let mut out = Vec::new();
        loop {
            let name = self.ident()?;
            let lang: Language =
                serde_json::from_value(serde_json::Value::String(name.to_lowercase()))
                    .map_err(|_| self.err(&format!("unknown language `{name}`")))?;
            out.push(lang);
            if self.peek() == Some(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(out)
    }

    fn parse_str_list(&mut self) -> R<Vec<String>> {
        let mut out = vec![self.string()?];
        while self.peek() == Some(&Tok::Comma) {
            self.bump();
            out.push(self.string()?);
        }
        Ok(out)
    }

    fn parse_unless(&mut self, unless: &mut RawUnless) -> R<()> {
        if self.at_kw("path") {
            self.bump();
            let globs = self.parse_str_list()?;
            unless.path_matches = Some(globs);
        } else if self.at_kw("waiver") {
            self.bump();
            // optional explicit rule id
            let rule = if matches!(self.peek(), Some(Tok::Ident(_))) {
                Some(self.ident()?)
            } else {
                None
            };
            unless.waiver_exists = Some(WaiverGuard { rule });
        } else if self.at_kw("adr") {
            self.bump();
            let topic = self.string()?;
            unless.adr_exists = Some(AdrGuard { topic });
        } else {
            return Err(self.err("expected `path`, `waiver`, or `adr` after `unless`"));
        }
        Ok(())
    }

    fn require_kind(&self, kind: Option<String>) -> R<EventKind> {
        match kind {
            Some(name) => event_kind(&name).ok_or_else(|| self.err_static("unknown event kind")),
            None => Err(self.err_static("`any` is only allowed as a sequence step, not here")),
        }
    }
    fn err_static(&self, msg: &str) -> DslError {
        DslError(format!("line {}: {msg}", self.line()))
    }

    /// `Kind[ pred, pred ]`, `Kind`, or `any`. Returns (kind-name, attrs);
    /// kind-name is `None` for the `any` wildcard.
    /// Returns (kind-name, attrs, binds). `kind` is `None` for `any`; binds are
    /// non-empty only for call-sugar captures (`$obj.m($x)`).
    fn parse_pattern(&mut self) -> R<Pattern> {
        if self.at_kw("any") {
            self.bump();
            return Ok((None, BTreeMap::new(), BTreeMap::new()));
        }
        // `$recv.name(args)` — capture/unify the receiver.
        if self.peek() == Some(&Tok::Dollar) {
            let recv = self.var_value()?;
            self.expect(&Tok::Dot)?;
            let name = self.ident()?;
            let (attrs, binds) = self.parse_call_sugar(name, Some(Recv::Var(recv)))?;
            return Ok((Some("Call".to_string()), attrs, binds));
        }
        let id = self.ident()?;
        match self.peek() {
            Some(Tok::LBrack) => {
                // explicit form: Kind[ pred, ... ]
                self.bump();
                let mut attrs = BTreeMap::new();
                if self.peek() != Some(&Tok::RBrack) {
                    loop {
                        let (k, v) = self.parse_pred()?;
                        attrs.insert(k, v);
                        if self.peek() == Some(&Tok::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(&Tok::RBrack)?;
                Ok((Some(id), attrs, BTreeMap::new()))
            }
            Some(Tok::LParen) => {
                // call sugar: name(args)
                let (attrs, binds) = self.parse_call_sugar(id, None)?;
                Ok((Some("Call".to_string()), attrs, binds))
            }
            Some(Tok::Dot) => {
                // literal receiver: obj.name(args)
                self.bump();
                let name = self.ident()?;
                let (attrs, binds) = self.parse_call_sugar(name, Some(Recv::Lit(id)))?;
                Ok((Some("Call".to_string()), attrs, binds))
            }
            // bare event kind (e.g. EnvAccess)
            _ => Ok((Some(id), BTreeMap::new(), BTreeMap::new())),
        }
    }

    /// Parse `( argpat, ... )` for `name(...)`, building attribute predicates
    /// (`argN`, `argN_kind`, `arg_count`) plus any captures.
    fn parse_call_sugar(&mut self, name: String, recv: Option<Recv>) -> R<(Attrs, Binds)> {
        let mut attrs = BTreeMap::new();
        let mut binds = BTreeMap::new();
        attrs.insert("name".to_string(), Value::String(name));
        match recv {
            Some(Recv::Lit(s)) => {
                attrs.insert("receiver".to_string(), Value::String(s));
            }
            Some(Recv::Var(v)) => self.unify_var(&v, "receiver", &mut attrs, &mut binds),
            None => {}
        }
        self.expect(&Tok::LParen)?;
        let mut idx = 0usize;
        let mut rest = false;
        if self.peek() != Some(&Tok::RParen) {
            loop {
                if self.peek() == Some(&Tok::DotDot) {
                    self.bump();
                    rest = true;
                    break;
                }
                self.parse_argpat(idx, &mut attrs, &mut binds)?;
                idx += 1;
                if self.peek() == Some(&Tok::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(&Tok::RParen)?;
        // Arity: exact unless a trailing `..` allows more.
        if rest {
            attrs.insert("arg_count.ge".to_string(), Value::String(idx.to_string()));
        } else {
            attrs.insert("arg_count".to_string(), Value::String(idx.to_string()));
        }
        Ok((attrs, binds))
    }

    fn parse_argpat(&mut self, idx: usize, attrs: &mut Attrs, binds: &mut Binds) -> R<()> {
        let key = format!("arg{idx}");
        match self.peek() {
            Some(Tok::Ident(s)) if s == "_" => {
                self.bump(); // wildcard: no value predicate
            }
            Some(Tok::Dollar) => {
                let v = self.var_value()?;
                self.unify_var(&v, &key, attrs, binds);
            }
            Some(Tok::Str(_)) => {
                let s = self.string()?;
                attrs.insert(key.clone(), Value::String(s));
            }
            Some(Tok::Num(_)) => {
                let n = self.num_value()?;
                attrs.insert(key.clone(), Value::String(n));
            }
            Some(Tok::Ident(_)) => {
                let s = self.ident()?;
                attrs.insert(key.clone(), Value::String(s));
            }
            _ => {
                return Err(self.err("expected an argument pattern: _, $var, \"str\", number, or name"))
            }
        }
        // optional `: kind` (syntactic kind of the argument expression)
        if self.peek() == Some(&Tok::Colon) {
            self.bump();
            let kind = self.ident()?;
            if !KNOWN_KINDS.contains(&kind.as_str()) {
                return Err(self.err(&format!(
                    "unknown argument kind `{kind}`; expected one of {KNOWN_KINDS:?}"
                )));
            }
            attrs.insert(format!("arg{idx}_kind"), Value::String(kind));
        }
        Ok(())
    }

    /// Unification: a variable's first use binds it (to `attr`); later uses
    /// become an equality predicate (`attr.eq_ref`).
    fn unify_var(&mut self, var: &str, attr: &str, attrs: &mut Attrs, binds: &mut Binds) {
        if self.bound.contains(var) {
            attrs.insert(format!("{attr}.eq_ref"), Value::String(var.to_string()));
        } else {
            self.bound.insert(var.to_string());
            binds.insert(var.to_string(), attr.to_string());
        }
    }

    /// One `attr <op> <value>` predicate, returned as the (key, value) pair the
    /// compiler expects (operator encoded in the key suffix).
    fn parse_pred(&mut self) -> R<(String, Value)> {
        let attr = self.ident()?;
        // membership operator is the keyword `in`
        if self.at_kw("in") {
            self.bump();
            self.expect(&Tok::LBrack)?;
            let mut items = Vec::new();
            if self.peek() != Some(&Tok::RBrack) {
                loop {
                    items.push(Value::String(self.string()?));
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.expect(&Tok::RBrack)?;
            return Ok((format!("{attr}.any"), Value::Sequence(items)));
        }
        let op = self.bump().ok_or_else(|| self.err("expected an operator"))?;
        match op {
            Tok::Eq => {
                let v = self.scalar_value()?;
                Ok((attr, Value::String(v)))
            }
            Tok::Tilde => Ok((format!("{attr}.regex"), Value::String(self.regex_value()?))),
            Tok::NotTilde => Ok((
                format!("{attr}.not.regex"),
                Value::String(self.regex_value()?),
            )),
            Tok::Gt => Ok((format!("{attr}.gt"), Value::String(self.num_value()?))),
            Tok::Lt => Ok((format!("{attr}.lt"), Value::String(self.num_value()?))),
            Tok::Ge => Ok((format!("{attr}.ge"), Value::String(self.num_value()?))),
            Tok::Le => Ok((format!("{attr}.le"), Value::String(self.num_value()?))),
            Tok::EqEq => Ok((format!("{attr}.eq_ref"), Value::String(self.var_value()?))),
            Tok::Ne => Ok((format!("{attr}.ne_ref"), Value::String(self.var_value()?))),
            other => Err(self.err(&format!("unexpected operator {other:?}"))),
        }
    }

    fn scalar_value(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Str(s)) => Ok(s),
            Some(Tok::Num(s)) => Ok(s),
            _ => Err(self.err("expected a \"string\" or number")),
        }
    }
    fn num_value(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Num(s)) => Ok(s),
            _ => Err(self.err("expected a number")),
        }
    }
    fn regex_value(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Regex(s)) => Ok(s),
            _ => Err(self.err("expected a /regex/")),
        }
    }
    fn var_value(&mut self) -> R<String> {
        self.expect(&Tok::Dollar)?;
        self.ident()
    }

    fn parse_where_scope(&mut self) -> R<RawWhereScope> {
        let mut ws = RawWhereScope {
            contains: None,
            not_contains: None,
            followed_by: None,
        };
        while self.at_kw("where") {
            self.bump();
            self.eat_kw("scope")?;
            if self.at_kw("not") {
                self.bump();
                self.eat_kw("contains")?;
                ws.not_contains = Some(self.clause_match()?);
            } else if self.at_kw("contains") {
                self.bump();
                ws.contains = Some(self.clause_match()?);
            } else if self.at_kw("followed") {
                self.bump();
                self.eat_kw("by")?;
                ws.followed_by = Some(self.clause_match()?);
            } else {
                return Err(self.err("expected `contains`, `not contains`, or `followed by`"));
            }
        }
        Ok(ws)
    }

    fn clause_match(&mut self) -> R<RawMatch> {
        let (kind, attrs, _binds) = self.parse_pattern()?;
        let event = self.require_kind(kind)?;
        Ok(RawMatch { event, attrs })
    }

    fn parse_sequence(&mut self) -> R<RawSequence> {
        let anchor = if self.at_kw("in") {
            self.bump();
            self.eat_kw("scope")?;
            Some(RawAnchor {
                within: Some("ScopeStart..ScopeEnd".to_string()),
            })
        } else {
            None
        };
        self.expect(&Tok::LBrace)?;
        let mut steps = Vec::new();
        while self.peek() != Some(&Tok::RBrace) {
            if self.peek().is_none() {
                return Err(self.err("unexpected end of input inside `sequence { }`"));
            }
            steps.push(self.parse_step()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(RawSequence { anchor, steps })
    }

    fn parse_step(&mut self) -> R<RawStep> {
        let negate = if self.at_kw("not") {
            self.bump();
            true
        } else {
            false
        };

        let (event, attrs, mut binds, alt) = if self.peek() == Some(&Tok::LParen) {
            // alternation of single-event patterns (captures inside alts are dropped)
            self.bump();
            let mut alts: Vec<Vec<RawStep>> = Vec::new();
            loop {
                let (kind, a, _b) = self.parse_pattern()?;
                alts.push(vec![RawStep {
                    event: kind,
                    attrs: a,
                    quant: None,
                    negate: None,
                    bind: None,
                    alt: None,
                }]);
                if self.peek() == Some(&Tok::Pipe) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
            (None, BTreeMap::new(), BTreeMap::new(), Some(alts))
        } else {
            let (kind, a, b) = self.parse_pattern()?;
            (kind, a, b, None)
        };

        let quant = match self.peek() {
            Some(Tok::Star) => {
                self.bump();
                Some("zero_or_more".to_string())
            }
            Some(Tok::Plus) => {
                self.bump();
                Some("one_or_more".to_string())
            }
            Some(Tok::Question) => {
                self.bump();
                Some("optional".to_string())
            }
            _ => None,
        };

        // explicit `as v=attr, ...` bindings merge with any call-sugar captures
        if self.at_kw("as") {
            self.bump();
            for (k, v) in self.parse_bindlist()? {
                binds.insert(k, v);
            }
        }

        Ok(RawStep {
            event,
            attrs,
            quant,
            negate: Some(negate),
            bind: if binds.is_empty() { None } else { Some(binds) },
            alt,
        })
    }

    fn parse_bindlist(&mut self) -> R<BTreeMap<String, String>> {
        let mut m = BTreeMap::new();
        loop {
            let var = self.ident()?;
            self.expect(&Tok::Eq)?;
            let attr = self.ident()?;
            self.bound.insert(var.clone()); // so later `$var` unifies as a backreference
            m.insert(var, attr);
            if self.peek() == Some(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(m)
    }

    fn parse_compose(&mut self) -> R<RawCompose> {
        let op = self.ident()?;
        self.eat_kw("of")?;
        let mut of = vec![self.ident()?];
        while self.peek() == Some(&Tok::Comma) {
            self.bump();
            of.push(self.ident()?);
        }
        let mut key = Vec::new();
        if self.at_kw("by") {
            self.bump();
            key.push(self.ident()?);
            while self.peek() == Some(&Tok::Comma) {
                self.bump();
                key.push(self.ident()?);
            }
        }
        Ok(RawCompose { op, of, key })
    }

    fn parse_count(&mut self) -> R<RawCount> {
        let rule = self.ident()?;
        self.eat_kw("per")?;
        let scope = self.ident()?;
        let op = match self.bump() {
            Some(Tok::Gt) => "gt",
            Some(Tok::Lt) => "lt",
            Some(Tok::Ge) => "ge",
            Some(Tok::Le) => "le",
            Some(Tok::EqEq) => "eq",
            _ => return Err(self.err("expected a comparison (>, <, >=, <=, ==)")),
        }
        .to_string();
        let n: u64 = match self.bump() {
            Some(Tok::Num(s)) => s
                .parse()
                .map_err(|_| self.err_static("count threshold must be an integer"))?,
            _ => return Err(self.err("expected an integer threshold")),
        };
        Ok(RawCount {
            rule,
            scope,
            op,
            n,
        })
    }
}

fn event_kind(name: &str) -> Option<EventKind> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_event_rule() {
        let src = r#"
            rule NO_MANUAL_GRAPHQL_OPERATION_TYPES (error) {
              lang typescript
              in "apps/*/src/**/*.{ts,tsx}"
              not in "**/generated/**", "**/*.generated.ts"
              match TypeDecl[name ~ /.*(Query|Mutation|Subscription)(Variables)?$/]
              message "Use generated GraphQL types."
            }
        "#;
        let (rules, _) = load(src).unwrap();
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.id, "NO_MANUAL_GRAPHQL_OPERATION_TYPES");
        assert_eq!(r.event, EventKind::TypeDecl);
        assert_eq!(r.preds.len(), 1);
        assert!(matches!(r.preds[0], super::super::AttrPred::Regex { .. }));
        assert!(r.message.as_deref().unwrap().contains("generated"));
    }

    #[test]
    fn parses_sequence_with_multibind_and_alt() {
        let src = r#"
            rule EVENT_LISTENER_LEAK (warning) {
              lang typescript, javascript
              sequence in scope {
                Call[name="addEventListener"] as obj=receiver, ev=string_args
                not Call[name="removeEventListener", receiver==$obj, string_args==$ev] *
              }
              message "leak"
            }
        "#;
        let (rules, _) = load(src).unwrap();
        let seq = rules[0].sequence.as_ref().unwrap();
        assert!(seq.within_scope);
        assert!(seq.has_captures);
        assert_eq!(seq.steps.len(), 2);
        // first step binds two vars
        assert_eq!(seq.steps[0].bind.len(), 2);
    }

    #[test]
    fn parses_compose_and_count() {
        let src = r#"
            rule A (error) { compose intersection of X, Y by file, function }
            rule B (error) { count Z per file > 10 }
        "#;
        let (rules, _) = load(src).unwrap();
        assert!(rules[0].compose.is_some());
        assert!(rules[1].count.is_some());
    }

    #[test]
    fn parses_where_scope() {
        let src = r#"
            rule ACQUIRE (warning) {
              match Call[name="acquire"]
              where scope not contains Call[name="release"]
            }
        "#;
        let (rules, _) = load(src).unwrap();
        let ws = rules[0].where_scope.as_ref().unwrap();
        assert!(ws.not_contains.is_some());
    }

    #[test]
    fn reports_a_line_number_on_error() {
        let src = "rule X (error) {\n  match\n}";
        let e = load(src).unwrap_err();
        assert!(e.0.contains("line"), "error should carry a line: {}", e.0);
    }

    #[test]
    fn positional_sugar_desugars_to_arg_predicates() {
        let src = r#"
            rule POS (warning) {
              lang typescript
              match foo(a, $x: string, b)
            }
        "#;
        let (rules, _) = load(src).unwrap();
        let r = &rules[0];
        assert_eq!(r.event, EventKind::Call);
        let names: Vec<&str> = r.preds.iter().map(|p| p.attr_name()).collect();
        for want in ["name", "arg0", "arg1_kind", "arg2", "arg_count"] {
            assert!(names.contains(&want), "missing predicate on `{want}`: {names:?}");
        }
    }

    #[test]
    fn unification_first_binds_then_backrefs() {
        let src = r#"
            rule U (warning) {
              sequence in scope {
                $obj.addEventListener($ev, ..)
                not $obj.removeEventListener($ev, ..) *
              }
            }
        "#;
        let (rules, _) = load(src).unwrap();
        let seq = rules[0].sequence.as_ref().unwrap();
        assert_eq!(seq.steps.len(), 2);
        assert_eq!(seq.steps[0].bind.len(), 2, "first step binds obj + ev");
        assert!(seq.steps[1].bind.is_empty(), "second step only backreferences");
        if let crate::StepMatcher::Event { preds, .. } = &seq.steps[1].matcher {
            let eqrefs = preds
                .iter()
                .filter(|p| matches!(p, crate::AttrPred::EqRef { .. }))
                .count();
            assert_eq!(eqrefs, 2, "receiver and arg0 are backreferenced");
        } else {
            panic!("expected an Event matcher");
        }
    }
}
