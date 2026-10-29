//! Comment template lookup and intake-time placeholder validation.
use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Debug)]
pub struct Templates {
    bodies: BTreeMap<String, String>,
}

// Each template has its own placeholder whitelist; templates contain no logic.
const DEFINITIONS: &[(&str, &[&str], &str)] = &[
    (
        "started.md",
        &["workflow", "base_branch", "branch", "run_status_cmd"],
        "Started {{workflow}} on {{branch}}.\n\nTechnical: base {{base_branch}}; {{run_status_cmd}}\n",
    ),
    (
        "question.md",
        &[
            "owner_mention",
            "bot",
            "step",
            "question",
            "technical",
            "options",
            "run_status_cmd",
        ],
        "{{owner_mention}} {{question}}\n\nTechnical: {{bot}} / {{step}}: {{technical}}\n{{options}}\n",
    ),
    (
        "go_request.md",
        &[
            "owner_mention",
            "artifact_links",
            "technical",
            "run_status_cmd",
        ],
        "{{owner_mention}} Please review the plan and reply GO to continue.\n\nTechnical: {{technical}}\n{{artifact_links}}\n",
    ),
    (
        "answer_ack.md",
        &[
            "owner_mention",
            "bot",
            "step",
            "answer",
            "technical",
            "run_status_cmd",
        ],
        "Applied your answer to {{step}}.\n\nTechnical: {{technical}}\n",
    ),
    (
        "step_done.md",
        &[
            "bot",
            "step",
            "summary",
            "technical",
            "artifact_links",
            "run_status_cmd",
        ],
        "{{step}} is complete.\n\nTechnical: {{technical}}\n",
    ),
    (
        "alert.md",
        &[
            "owner_mention",
            "bot",
            "step",
            "summary",
            "reason",
            "technical",
            "run_status_cmd",
        ],
        "{{owner_mention}} The run needs attention.\n\nTechnical: {{technical}}\n{{run_status_cmd}}\n",
    ),
    (
        "outcome.md",
        &[
            "workflow",
            "summary",
            "technical",
            "pr",
            "merge_commit",
            "docs",
            "artifact_links",
            "run_status_cmd",
        ],
        "{{summary}}\n\nTechnical: {{technical}}\n{{artifact_links}}\n",
    ),
];
impl Templates {
    /// Directories name template layers themselves. Only missing files fall
    /// through; an invalid selected override is an error.
    pub fn load(project_override: Option<&Path>, pack: Option<&Path>) -> Result<Self> {
        let mut bodies = BTreeMap::new();
        for &(name, allowed, builtin) in DEFINITIONS {
            let mut selected = None;
            for directory in [project_override, pack].into_iter().flatten() {
                let path = directory.join(name);
                match fs::read_to_string(&path) {
                    Ok(body) => {
                        selected = Some((path.display().to_string(), body));
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("{}: read template", path.display()));
                    }
                }
            }
            let (label, body) =
                selected.unwrap_or_else(|| (format!("built-in/{name}"), builtin.to_owned()));
            Self::validate(name, &body, allowed)
                .with_context(|| format!("{label}: invalid template"))?;
            bodies.insert(name.to_owned(), body);
        }
        Ok(Self { bodies })
    }
    /// Only `owner_mention` is trusted tracker markup. Bot values are always text.
    pub fn render(
        &self,
        name: &str,
        values: &BTreeMap<String, String>,
        owner_mention: &str,
        run_id: &str,
        request_id: &str,
    ) -> Result<String> {
        let body = self
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("unknown template {name}"))?
            .replace("<!-- hermon:", "&lt;!-- hermon:");
        let marker = marker(run_id, request_id)?;
        let mut rendered = String::new();
        let mut remaining = body.as_str();
        while let Some(start) = remaining.find("{{") {
            rendered.push_str(&remaining[..start]);
            let after = &remaining[start + 2..];
            let end = after.find("}}").expect("validated placeholder");
            let key = &after[..end];
            if key == "owner_mention" {
                ensure!(
                    !owner_mention.trim().is_empty(),
                    "owner mention is required"
                );
                rendered.push_str(owner_mention);
            } else {
                let value = values
                    .get(key)
                    .ok_or_else(|| anyhow::anyhow!("missing template value {key}"))?;
                rendered.push_str(&escape_text(value));
            }
            remaining = &after[end + 2..];
        }
        rendered.push_str(remaining);
        rendered.push_str("\n\n");
        rendered.push_str(&marker);
        Ok(rendered)
    }
    pub fn preview(&self, name: &str) -> Result<String> {
        let values = DEFINITIONS
            .iter()
            .flat_map(|(_, fields, _)| fields.iter())
            .map(|field| ((*field).to_owned(), format!("sample {field}")))
            .collect();
        self.render(
            name,
            &values,
            "[Sample owner](https://linear.app/example/profiles/owner)",
            "sample-run",
            "sample-request",
        )
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.bodies.get(name).map(String::as_str)
    }
    fn validate(name: &str, body: &str, allowed: &[&str]) -> Result<()> {
        let mut remaining = body;
        let mut owner = false;
        while let Some(start) = remaining.find("{{") {
            ensure!(
                !remaining[..start].contains("}}"),
                "unmatched placeholder closing braces"
            );
            let after_open = &remaining[start + 2..];
            let Some(end) = after_open.find("}}") else {
                bail!("unclosed placeholder");
            };
            let placeholder = &after_open[..end];
            ensure!(
                allowed.contains(&placeholder),
                "unknown placeholder {{{{{placeholder}}}}} for {name}"
            );
            owner |= placeholder == "owner_mention";
            remaining = &after_open[end + 2..];
        }
        ensure!(
            !remaining.contains("}}"),
            "unmatched placeholder closing braces"
        );
        ensure!(
            !["question.md", "go_request.md"].contains(&name) || owner,
            "{name} requires {{{{owner_mention}}}}"
        );
        Ok(())
    }
}

pub fn marker(run_id: &str, request_id: &str) -> Result<String> {
    for id in [run_id, request_id] {
        ensure!(
            !id.is_empty()
                && id.len() <= 256
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b)),
            "invalid run/request marker ID"
        );
    }
    Ok(format!("<!-- hermon:run={run_id};request={request_id} -->"))
}

fn escape_text(value: &str) -> String {
    let mut escaped = String::new();
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            // Linear converts bare resource URLs into mentions. Break both URL
            // schemes and @ handles, in addition to escaping markdown links.
            '@' | ':' => {
                escaped.push(c);
                escaped.push('\u{200b}');
            }
            '\\' | '*' | '_' | '[' | ']' | '(' | ')' | '`' | '#' | '!' | '|' | '~' | '+' | '-'
            | '=' | '{' | '}' => {
                escaped.push('\\');
                escaped.push(c);
            }
            _ => escaped.push(c),
        }
    }
    escaped
}
