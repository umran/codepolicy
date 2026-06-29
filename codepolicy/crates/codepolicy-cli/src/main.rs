//! `codepolicy` command-line interface (proposal §10.3).

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand, ValueEnum};
use codepolicy_core::Project;
use codepolicy_frontends::{frontend_for, frontends, SourceFile};
use codepolicy_match::summarize;
use codepolicy_report::{render, Format as ReportFormat};
use codepolicy_rules::{load, CompiledRule, RuleFile};

/// The bundled starter policy packs written by `codepolicy init`.
const EXAMPLE_RULES_YAML: &str = include_str!("../assets/codepolicy.yaml");
const EXAMPLE_RULES_DSL: &str = include_str!("../assets/codepolicy.rules");
const CONFIG_NAME: &str = "codepolicy.yaml";

#[derive(Parser)]
#[command(
    name = "codepolicy",
    version,
    about = "A Cobra-style lexeme policy engine: enforce your own rules over a codebase"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check a repository against the policy rules.
    Check {
        /// Repository root to scan.
        #[arg(default_value = ".")]
        root: Utf8PathBuf,
        /// Path to the rules file (defaults to <root>/codepolicy.yaml).
        #[arg(long)]
        rules: Option<Utf8PathBuf>,
        /// Output format.
        #[arg(long, value_enum, default_value = "human")]
        format: OutFormat,
    },
    /// Print the lexeme (token) stream for a single file (as JSON).
    Tokens {
        /// Source file to lex.
        file: Utf8PathBuf,
    },
    /// Explain a rule by id.
    ExplainRule {
        /// Rule id, e.g. NO_DIRECT_GRAPHQL_CLIENT.
        id: String,
        #[arg(long)]
        rules: Option<Utf8PathBuf>,
    },
    /// Write a starter rules file into the given directory.
    Init {
        #[arg(default_value = ".")]
        root: Utf8PathBuf,
        /// Output format: `yaml` (codepolicy.yaml) or `rules` (textual DSL, codepolicy.rules).
        #[arg(long, value_enum, default_value = "yaml")]
        format: InitFormat,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum InitFormat {
    Yaml,
    Rules,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutFormat {
    Human,
    Json,
    Agent,
}

impl From<OutFormat> for ReportFormat {
    fn from(f: OutFormat) -> Self {
        match f {
            OutFormat::Human => ReportFormat::Human,
            OutFormat::Json => ReportFormat::Json,
            OutFormat::Agent => ReportFormat::Agent,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let code = match dispatch(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            2
        }
    };
    std::process::exit(code);
}

fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Check {
            root,
            rules,
            format,
        } => cmd_check(root, rules, format.into()),
        Command::Tokens { file } => cmd_tokens(file),
        Command::ExplainRule { id, rules } => cmd_explain(id, rules),
        Command::Init { root, format } => cmd_init(root, format),
    }
}

fn load_rules(
    root: &Utf8Path,
    rules: Option<Utf8PathBuf>,
) -> Result<(Vec<CompiledRule>, RuleFile)> {
    let path = match rules {
        Some(p) => p,
        None => {
            // Prefer codepolicy.yaml, fall back to the textual codepolicy.rules.
            let yaml = root.join(CONFIG_NAME);
            let dsl = root.join("codepolicy.rules");
            if !yaml.exists() && dsl.exists() {
                dsl
            } else {
                yaml
            }
        }
    };
    if !path.exists() {
        bail!("no rules file at {path}. Run `codepolicy init` or pass --rules <file>.");
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    // `.yaml`/`.yml` use the YAML loader; any other extension uses the textual DSL.
    let is_yaml = matches!(path.extension(), Some("yaml") | Some("yml"));
    let (compiled, file) = if is_yaml {
        load(&text).map_err(|e| anyhow::anyhow!("{path}: {e}"))?
    } else {
        codepolicy_rules::dsl::load(&text).map_err(|e| anyhow::anyhow!("{path}: {e}"))?
    };
    Ok((compiled, file))
}

fn cmd_check(root: Utf8PathBuf, rules: Option<Utf8PathBuf>, format: ReportFormat) -> Result<i32> {
    let (compiled, file) = load_rules(&root, rules)?;
    let project = Project::new(root);
    let ctx = project.load_context(file.waivers_dir.as_deref(), file.adr_dir.as_deref());
    let violations = project.check(&compiled, &ctx);
    print!("{}", render(&violations, format));
    let (errors, _) = summarize(&violations);
    Ok(if errors > 0 { 1 } else { 0 })
}

fn cmd_tokens(file: Utf8PathBuf) -> Result<i32> {
    let fes = frontends();
    let Some(frontend) = frontend_for(&fes, &file) else {
        bail!("no frontend supports {file}");
    };
    let text = std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?;
    let source = SourceFile {
        path: file.clone(),
        text: &text,
    };
    let ts = frontend.lex(&source)?;
    println!("{}", serde_json::to_string_pretty(&ts.resolved_json())?);
    Ok(0)
}

fn cmd_explain(id: String, rules: Option<Utf8PathBuf>) -> Result<i32> {
    let root = Utf8PathBuf::from(".");
    let (compiled, _) = load_rules(&root, rules)?;
    let Some(rule) = compiled.iter().find(|r| r.id == id) else {
        bail!("no rule with id `{id}`");
    };
    println!("{} [{}]", rule.id, rule.severity.label());
    if let Some(desc) = &rule.description {
        println!("\n{desc}");
    }
    if let Some(langs) = &rule.languages {
        println!("languages: {langs:?}");
    }
    if let Some(comp) = &rule.compose {
        println!(
            "\ncomposes ({:?}) of {:?} keyed by {:?}",
            comp.op, comp.of, comp.key
        );
    } else if let Some(cnt) = &rule.count {
        println!(
            "\ncounts `{}` per {:?}; fires when count {:?} {}",
            cnt.rule, cnt.scope, cnt.op, cnt.n
        );
    } else if let Some(seq) = &rule.sequence {
        println!(
            "\nmatches a sequence of {} step(s){}:",
            seq.steps.len(),
            if seq.within_scope {
                " (anchored within a scope)"
            } else {
                ""
            }
        );
        for (i, step) in seq.steps.iter().enumerate() {
            println!("  {}. {step:?}", i + 1);
        }
    } else {
        println!("\nmatches a token pattern:");
        if rule.preds.is_empty() {
            println!("  (any token)");
        } else {
            for pred in &rule.preds {
                println!("  - {pred:?}");
            }
        }
    }
    if let Some(ws) = &rule.where_scope {
        println!("where_scope (enclosing block):");
        if let Some(c) = &ws.contains {
            println!("  - contains: {:?}", c.preds);
        }
        if let Some(c) = &ws.not_contains {
            println!("  - not_contains: {:?}", c.preds);
        }
        if let Some(c) = &ws.followed_by {
            println!("  - followed_by: {:?}", c.preds);
        }
    }
    if let Some(unless) = &rule.unless {
        println!("unless:");
        if unless.path_matches.is_some() {
            println!("  - path.matches: <globs>");
        }
        if let Some(rule_id) = &unless.waiver_rule {
            println!("  - waiver.exists: rule={rule_id}");
        }
        if let Some(topic) = &unless.adr_topic {
            println!("  - adr.exists: topic={topic:?}");
        }
    }
    if let Some(msg) = &rule.message {
        println!("\nremediation: {msg}");
    }
    Ok(0)
}

fn cmd_init(root: Utf8PathBuf, format: InitFormat) -> Result<i32> {
    let (filename, content, is_dsl) = match format {
        InitFormat::Yaml => ("codepolicy.yaml", EXAMPLE_RULES_YAML, false),
        InitFormat::Rules => ("codepolicy.rules", EXAMPLE_RULES_DSL, true),
    };
    let path = root.join(filename);
    if path.exists() {
        bail!("{path} already exists; refusing to overwrite.");
    }
    std::fs::write(&path, content).with_context(|| format!("writing {path}"))?;
    // Count rules for a friendly message.
    let count = if is_dsl {
        codepolicy_rules::dsl::load(content)
            .map(|(r, _)| r.len())
            .unwrap_or(0)
    } else {
        load(content).map(|(r, _)| r.len()).unwrap_or(0)
    };
    println!("Wrote {path} with {count} starter rules.");
    Ok(0)
}
