//! Version-one workflow schema and strict YAML loading.
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::Path,
};
use yaml_rust2::{
    Yaml,
    parser::{Event, Parser},
    scanner::TScalarStyle,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub version: u32,
    pub name: String,
    pub steps: BTreeMap<String, Step>,
    #[serde(default)]
    pub limits: Limits,
    pub templates: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub bot: Option<String>,
    pub gate: Option<String>,
    pub action: Option<String>,
    pub next: Option<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub produces: Vec<String>,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub check: Vec<String>,
    pub tag: Option<String>,
    #[serde(default)]
    pub on_block: BTreeMap<String, String>,
    #[serde(default)]
    pub on_verdict: BTreeMap<String, String>,
    pub require_different_family: Option<String>,
    pub max_runs: Option<u32>,
    pub max_rounds: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub attempt_timeout_minutes: u32,
    pub transient_retries: u32,
    pub stall_alert_minutes: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            attempt_timeout_minutes: 90,
            transient_retries: 2,
            stall_alert_minutes: 30,
        }
    }
}

impl Workflow {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("{}: read workflow", path.display()))?;
        Self::from_str(&path.display().to_string(), &text)
    }
    pub fn from_str(path_label: &str, text: &str) -> Result<Self> {
        Self::parse(text).map_err(|error| anyhow!("{path_label}: {error:#}"))
    }
    fn parse(text: &str) -> Result<Self> {
        let value = yaml_value(text)?;
        let workflow: Self = serde_path_to_error::deserialize(value)
            .map_err(|error| anyhow!("{}: {}", error.path(), error.inner()))?;
        workflow.validate()?;
        Ok(workflow)
    }
    fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "version: expected workflow version 1");
        ensure!(!self.name.trim().is_empty(), "name: must not be empty");
        ensure!(!self.steps.is_empty(), "steps: must not be empty");
        ensure!(
            self.limits.attempt_timeout_minutes > 0,
            "limits.attempt_timeout_minutes: must be positive"
        );
        ensure!(
            self.limits.stall_alert_minutes > 0,
            "limits.stall_alert_minutes: must be positive"
        );
        for (name, step) in &self.steps {
            let path = format!("steps.{name}");
            ensure!(
                !name.trim().is_empty(),
                "steps: step name must not be empty"
            );
            let kinds = [&step.bot, &step.gate, &step.action];
            ensure!(
                kinds.iter().filter(|kind| kind.is_some()).count() == 1,
                "{path}: exactly one of bot, gate or action is required"
            );
            for value in kinds.into_iter().flatten() {
                ensure!(
                    !value.trim().is_empty(),
                    "{path}: step kind must not be empty"
                );
            }
            if let Some(gate) = &step.gate {
                ensure!(
                    ["owner_go", "owner_merge"].contains(&gate.as_str()),
                    "{path}.gate: unknown gate {gate:?}"
                );
            }
            if let Some(action) = &step.action {
                ensure!(
                    action == "open_pr",
                    "{path}.action: unknown action {action:?}"
                );
            }
            for (field, limit) in [("max_runs", step.max_runs), ("max_rounds", step.max_rounds)] {
                ensure!(limit != Some(0), "{path}.{field}: must be positive");
            }
            if let Some(target) = &step.next {
                self.target(target, &format!("{path}.next"))?;
            }
            for (kind, target) in &step.on_block {
                ensure!(
                    [
                        "owner_question",
                        "test_defect",
                        "capability",
                        "transient",
                        "scope_change",
                        "unknown"
                    ]
                    .contains(&kind.as_str()),
                    "{path}.on_block.{kind}: unknown block kind"
                );
                self.target(target, &format!("{path}.on_block.{kind}"))?;
            }
            for (verdict, target) in &step.on_verdict {
                ensure!(
                    ["clean", "changes_needed"].contains(&verdict.as_str()),
                    "{path}.on_verdict.{verdict}: unknown verdict"
                );
                self.target(target, &format!("{path}.on_verdict.{verdict}"))?;
            }
            if let Some(target) = &step.require_different_family {
                self.target(target, &format!("{path}.require_different_family"))?;
                ensure!(
                    self.steps[target].bot.is_some(),
                    "{path}.require_different_family: target must be a bot step"
                );
            }
            for (index, check) in step.check.iter().enumerate() {
                let valid = match check.split_once(':') {
                    None => ["lint", "tests_fail", "tests_pass", "project_check"]
                        .contains(&check.as_str()),
                    Some((kind, arg)) => {
                        ["only_paths_changed", "frozen_unchanged", "cmd"].contains(&kind)
                            && !arg.is_empty()
                            && !arg.chars().any(|c: char| c.is_whitespace() || c == ':')
                    }
                };
                ensure!(
                    valid,
                    "{path}.check[{index}]: unknown check or invalid parameter {check:?}"
                );
            }
        }
        // Remove bounded nodes before cycle detection: an SCC with a bounded
        // node can still contain an infinite subcycle that bypasses that node.
        // Owner gates break automatic execution with fresh human consent.
        self.validate_cycles()
    }
    fn target(&self, target: &str, path: &str) -> Result<()> {
        ensure!(
            self.steps.contains_key(target),
            "{path}: missing target {target:?}"
        );
        Ok(())
    }
    fn validate_cycles(&self) -> Result<()> {
        let mut incoming: BTreeMap<&str, usize> = self
            .steps
            .iter()
            .filter(|(_, step)| {
                step.max_runs.is_none() && step.max_rounds.is_none() && step.gate.is_none()
            })
            .map(|(name, _)| (name.as_str(), 0))
            .collect();
        for name in incoming.keys().copied().collect::<Vec<_>>() {
            for target in self.steps[name].targets() {
                if let Some(count) = incoming.get_mut(target.as_str()) {
                    *count += 1;
                }
            }
        }
        let mut ready: VecDeque<_> = incoming
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(name, _)| *name)
            .collect();
        // Topological removal avoids recursive traversal of a long flat graph.
        while let Some(name) = ready.pop_front() {
            for target in self.steps[name].targets() {
                if let Some(count) = incoming.get_mut(target.as_str()) {
                    *count -= 1;
                    if *count == 0 {
                        ready.push_back(target);
                    }
                }
            }
        }
        if let Some((name, _)) = incoming.iter().find(|(_, count)| **count > 0) {
            bail!(
                "steps.{name}: unbounded automatic cycle; add max_runs, max_rounds or an owner gate"
            );
        }
        Ok(())
    }
}

impl Step {
    fn targets(&self) -> impl Iterator<Item = &String> {
        self.next
            .iter()
            .chain(self.on_block.values())
            .chain(self.on_verdict.values())
    }
}

// Events preserve duplicate keys and reject anchors/tags before expansion.
// Bounds limit recursion and allocation for configuration files.
pub(crate) fn yaml_value(text: &str) -> Result<Value> {
    let mut yaml = StrictYaml {
        parser: Parser::new_from_str(text),
        nodes: 0,
    };
    ensure!(
        yaml.next("$")? == Event::StreamStart,
        "expected YAML stream"
    );
    ensure!(
        yaml.next("$")? == Event::DocumentStart,
        "expected one YAML document"
    );
    let value = yaml.value("$", 0)?;
    ensure!(
        yaml.next("$")? == Event::DocumentEnd,
        "expected document end"
    );
    ensure!(
        yaml.next("$")? == Event::StreamEnd,
        "multiple YAML documents are not allowed"
    );
    Ok(value)
}

struct StrictYaml<'a> {
    parser: Parser<std::str::Chars<'a>>,
    nodes: usize,
}
impl StrictYaml<'_> {
    fn next(&mut self, path: &str) -> Result<Event> {
        self.parser
            .next_token()
            .map(|(event, _)| event)
            .map_err(|error| anyhow!("{path}: YAML syntax or unresolved alias: {error}"))
    }
    fn value(&mut self, path: &str, depth: usize) -> Result<Value> {
        self.nodes += 1;
        ensure!(
            depth <= 128 && self.nodes <= 100_000,
            "{path}: YAML nesting or node limit exceeded"
        );
        match self.next(path)? {
            Event::Scalar(text, style, anchor, tag) => {
                Self::metadata(path, anchor, tag.is_some())?;
                if style != TScalarStyle::Plain {
                    return Ok(Value::String(text));
                }
                Ok(match Yaml::from_str(&text) {
                    Yaml::Integer(n) => Value::from(n),
                    Yaml::Boolean(b) => Value::Bool(b),
                    Yaml::Null => Value::Null,
                    Yaml::Real(n) => Value::Number(n.parse().map_err(|_| {
                        anyhow!("{path}: non-finite YAML numbers are not supported")
                    })?),
                    _ => Value::String(text),
                })
            }
            Event::SequenceStart(anchor, tag) => {
                Self::metadata(path, anchor, tag.is_some())?;
                let mut array = Vec::new();
                while !matches!(
                    self.parser.peek().map_err(|e| anyhow!("{path}: {e}"))?.0,
                    Event::SequenceEnd
                ) {
                    array.push(self.value(&format!("{path}[{}]", array.len()), depth + 1)?);
                }
                self.next(path)?;
                Ok(Value::Array(array))
            }
            Event::MappingStart(anchor, tag) => {
                Self::metadata(path, anchor, tag.is_some())?;
                let mut object = Map::new();
                loop {
                    let event = self.next(path)?;
                    if event == Event::MappingEnd {
                        break;
                    }
                    let Event::Scalar(key, style, anchor, tag) = event else {
                        bail!("{path}: mapping keys must be strings (aliases are forbidden)");
                    };
                    let key_path = format!("{path}.{key}");
                    Self::metadata(&key_path, anchor, tag.is_some())?;
                    ensure!(
                        style != TScalarStyle::Plain
                            || matches!(Yaml::from_str(&key), Yaml::String(_)),
                        "{key_path}: mapping keys must be strings"
                    );
                    ensure!(key != "<<", "{key_path}: YAML merge key << is forbidden");
                    ensure!(
                        !object.contains_key(&key),
                        "{key_path}: duplicate key {key:?}"
                    );
                    object.insert(key, self.value(&key_path, depth + 1)?);
                }
                Ok(Value::Object(object))
            }
            Event::Alias(_) => bail!("{path}: YAML alias is forbidden"),
            _ => bail!("{path}: expected a YAML value"),
        }
    }
    fn metadata(path: &str, anchor: usize, tagged: bool) -> Result<()> {
        ensure!(anchor == 0, "{path}: YAML anchor is forbidden");
        ensure!(!tagged, "{path}: YAML tag is forbidden");
        Ok(())
    }
}
