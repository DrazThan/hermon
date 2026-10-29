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
        "{{owner_mention}} {{question}}\n\nTechnical: {{technical}}\n{{options}}\n",
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
