//! Central Linear app adapter and durable request replies.
use crate::{
    hub::{Alert, Block, BlockKind, Tracker},
    store::RunStore,
    templates::{Templates, marker},
};
use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

/// Host-owned configuration. Never serialize credentials into a run directory.
pub struct LinearConfig {
    pub endpoint: String,
    pub token: String,
    pub issue_id: String,
    pub owner_ids: BTreeSet<String>,
    pub owner_mention: String,
}
#[derive(Debug, Clone)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct Comment {
    pub id: String,
    pub body: String,
    pub user_id: String,
    pub parent_id: Option<String>,
}
#[derive(Debug, Clone)]
pub struct CommentPage {
    pub comments: Vec<Comment>,
    pub next_cursor: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    since: Option<String>,
    after: Option<String>,
    max_seen: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    kind: String,
    body: String,
    id: String,
    posted: bool,
    cursor: Option<String>,
    reply: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u32,
    endpoint: String,
    issue_id: String,
    owner_ids: BTreeSet<String>,
    owner_mention: String,
    requests: BTreeMap<String, Request>,
}
/// One instance/state directory belongs to one issue and owner configuration.
/// The host serializes access under its run lock.
pub struct LinearTracker {
    config: LinearConfig,
    templates: Templates,
    store: RunStore,
    state: State,
    http: ureq::Agent,
}
impl LinearTracker {
    pub fn new(
        config: LinearConfig,
        templates: Templates,
        state_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let uri: ureq::http::Uri = config.endpoint.parse().context("invalid Linear endpoint")?;
        let local = matches!(uri.host(), Some("127.0.0.1" | "localhost" | "[::1]"));
        ensure!(
            uri.scheme_str() == Some("https") || (local && uri.scheme_str() == Some("http")),
            "Linear requires HTTPS (HTTP is only allowed for loopback fixtures)"
        );
        ensure!(
            !config.token.is_empty() && !config.token.contains(['\r', '\n']),
            "invalid Linear token"
        );
        ensure!(
            !config.issue_id.is_empty()
                && !config.owner_ids.is_empty()
                && config.owner_ids.iter().all(|id| !id.is_empty()),
            "issue and owner IDs are required"
        );
        ensure!(
            !config.owner_mention.trim().is_empty(),
            "owner mention is required"
        );
        let store = RunStore::new(state_dir)?;
        let state = store.read_run::<State>()?.unwrap_or_else(|| State {
            schema: 1,
            endpoint: config.endpoint.clone(),
            issue_id: config.issue_id.clone(),
            owner_ids: config.owner_ids.clone(),
            owner_mention: config.owner_mention.clone(),
            requests: BTreeMap::new(),
        });
        ensure!(
            state.schema == 1
                && state.endpoint == config.endpoint
                && state.issue_id == config.issue_id
                && state.owner_ids == config.owner_ids
                && state.owner_mention == config.owner_mention,
            "tracker state belongs to different issue/owners or schema"
        );
        let http = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .max_redirects(0)
            .build()
            .into();
        Ok(Self {
            config,
            templates,
            store,
            state,
            http,
        })
    }
    fn save(&self) -> Result<()> {
        self.store.write_run(&self.state)
    }
    fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        // Do not include server-controlled error strings or request headers in errors.
        let payload = serde_json::to_vec(&json!({"query":query,"variables":variables}))?;
        let mut response = self
            .http
            .post(&self.config.endpoint)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .header("Content-Type", "application/json")
            .send(payload.as_slice())
            .map_err(|error| match error {
                ureq::Error::StatusCode(code) => anyhow!("Linear HTTP status {code}"),
                _ => anyhow!("Linear transport failed"),
            })?;
        let value: Value = response
            .body_mut()
            .read_json()
            .map_err(|_| anyhow!("invalid Linear JSON response"))?;
        ensure!(
            value
                .get("errors")
                .is_none_or(|errors| errors.as_array().is_some_and(Vec::is_empty)),
            "Linear GraphQL operation failed"
        );
        value
            .get("data")
            .filter(|v| v.is_object())
            .cloned()
            .context("missing Linear data")
    }
    pub fn post_comment(
        &mut self,
        kind: &str,
        run_id: &str,
        request_id: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<String> {
        let key = marker(run_id, request_id)?;
        let body =
            self.templates
                .render(kind, values, &self.config.owner_mention, run_id, request_id)?;
        if let Some(request) = self.state.requests.get(&key) {
            ensure!(
                request.kind == kind && request.body == body,
                "request ID already bound to different comment contents"
            );
            if request.posted {
                return Ok(request.id.clone());
            }
            // Recover a lost mutation response with the client-supplied UUID.
            if let Ok(data) = self.graphql(
                "query HermonComment($id:String!) { comment(id:$id) { id body } }",
                json!({"id":request.id}),
            ) && let Some(comment) = data.get("comment").filter(|v| v.is_object())
            {
                ensure!(
                    comment["id"] == request.id && comment["body"] == body,
                    "recovered comment does not match request"
                );
                self.state.requests.get_mut(&key).unwrap().posted = true;
                self.save()?;
                return Ok(self.state.requests[&key].id.clone());
            }
        } else {
            // Reserving a stable UUID before the network effect prevents a crash
            // or ambiguous transport retry from creating a second thread.
            let id = comment_uuid(&format!(
                "{}\n{}\n{key}",
                self.config.endpoint, self.config.issue_id
            ));
            self.state.requests.insert(
                key.clone(),
                Request {
                    kind: kind.into(),
                    body: body.clone(),
                    id,
                    posted: false,
                    cursor: None,
                    reply: None,
                },
            );
            self.save()?;
        }
        let id = self.state.requests[&key].id.clone();
        let data = self.graphql("mutation HermonPost($input:CommentCreateInput!) { commentCreate(input:$input) { success comment { id } } }", json!({"input":{"id":id,"issueId":self.config.issue_id,"body":body}}))?;
        ensure!(
            data["commentCreate"]["success"].as_bool() == Some(true),
            "Linear comment creation was unsuccessful"
        );
        let id = string(&data["commentCreate"]["comment"], "id")?;
        let request = self.state.requests.get_mut(&key).unwrap();
        request.id = id.clone();
        request.posted = true;
        self.save()?;
        Ok(id)
    }
    pub fn read_issue(&mut self) -> Result<Issue> {
        let data = self.graphql(
            "query HermonIssue($id:String!) { issue(id:$id) { id title description } }",
            json!({"id":self.config.issue_id}),
        )?;
        issue(&data["issue"])
    }
    /// Verify the configured central app can authenticate, before intake.
    pub fn authenticate(&self) -> Result<()> {
        let data = self.graphql("query HermonDoctor { viewer { id } }", json!({}))?;
        string(&data["viewer"], "id")?;
        Ok(())
    }
    pub fn find_labeled(&mut self, label: &str) -> Result<Vec<Issue>> {
        let mut result = vec![];
        let mut after = None::<String>;
        loop {
            let data = self.graphql("query HermonLabeled($label:String!, $after:String) { issues(first:100, after:$after, filter:{labels:{some:{name:{eq:$label}}}}) { nodes { id title description } pageInfo { hasNextPage endCursor } } }", json!({"label":label,"after":after}))?;
            let connection = &data["issues"];
            for node in connection["nodes"]
                .as_array()
                .context("invalid issue list")?
            {
                result.push(issue(node)?);
            }
            let next = page_after(connection)?;
            if next.is_none() {
                return Ok(result);
            }
            ensure!(next != after, "Linear issue pagination did not advance");
            after = next;
        }
    }
    pub fn list_comments_since(&mut self, cursor: Option<&str>) -> Result<CommentPage> {
        let mut cursor: Cursor = cursor
            .map(serde_json::from_str)
            .transpose()
            .context("invalid reply cursor")?
            .unwrap_or_default();
        let filter = cursor
            .since
            .as_ref()
            .map(|since| json!({"createdAt":{"gte":since}}))
            .unwrap_or_else(|| json!({}));
        let data = self.graphql("query HermonComments($id:String!, $after:String, $filter:CommentFilter) { issue(id:$id) { comments(first:100,after:$after,filter:$filter,orderBy:createdAt) { nodes { id body createdAt user { id } parent { id } } pageInfo { hasNextPage endCursor } } } }", json!({"id":self.config.issue_id,"after":cursor.after,"filter":filter}))?;
        let connection = &data["issue"]["comments"];
        let mut comments = vec![];
        for node in connection["nodes"]
            .as_array()
            .context("invalid comment list")?
        {
            let timestamp = string(node, "createdAt")?;
            let time = chrono::DateTime::parse_from_rfc3339(&timestamp)
                .context("invalid comment timestamp")?
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            if cursor.max_seen.as_ref().is_none_or(|max| &time > max) {
                cursor.max_seen = Some(time);
            }
            comments.push(Comment {
                id: string(node, "id")?,
                body: string(node, "body")?,
                user_id: node["user"]["id"].as_str().unwrap_or_default().into(),
                parent_id: node["parent"]["id"].as_str().map(str::to_owned),
            });
        }
        let next = page_after(connection)?;
        if let Some(after) = next {
            ensure!(
                cursor.after.as_ref() != Some(&after),
                "Linear reply pagination did not advance"
            );
            cursor.after = Some(after);
        } else {
            cursor.after = None;
            if let Some(max) = &cursor.max_seen {
                cursor.since = Some(max.clone());
            }
            cursor.max_seen = None;
        }
        Ok(CommentPage {
            comments,
            next_cursor: Some(serde_json::to_string(&cursor)?),
        })
    }
    fn reply(&mut self, run_id: &str, request_id: &str, exact_go: bool) -> Result<Option<String>> {
        let key = marker(run_id, request_id)?;
        let mut request = self
            .state
            .requests
            .get(&key)
            .cloned()
            .context("reply request has not been posted")?;
        ensure!(request.posted, "reply request is not delivered");
        if request.reply.is_some() {
            return Ok(request.reply);
        }
        // Bound each hub observation while retaining unfinished pagination.
        for _ in 0..32 {
            let page = self.list_comments_since(request.cursor.as_deref())?;
            request.cursor = page.next_cursor;
            request.reply = page
                .comments
                .into_iter()
                .find(|comment| {
                    comment.parent_id.as_deref() == Some(&request.id)
                        && self.config.owner_ids.contains(&comment.user_id)
                        && if exact_go {
                            comment.body == "GO"
                        } else {
                            !comment.body.trim().is_empty()
                        }
                })
                .map(|comment| comment.body);
            self.state.requests.insert(key.clone(), request.clone());
            self.save()?;
            let cursor: Cursor =
                serde_json::from_str(request.cursor.as_deref().context("missing reply cursor")?)?;
            if request.reply.is_some() || cursor.after.is_none() {
                break;
            }
        }
        Ok(request.reply)
    }
}
impl Tracker for LinearTracker {
    fn gate_approved(
        &mut self,
        run_id: &str,
        step: &str,
        request: u32,
    ) -> std::result::Result<bool, Block> {
        let mut action = || -> Result<bool> {
            ensure!(request > 0, "gate request numbers start at one");
            let id = format!("gate:{step}:{request}");
            self.post_comment(
                "go_request.md",
                run_id,
                &id,
                &context_values(
                    step,
                    "",
                    &format!("Approval for step {step}, request {request}"),
                    "",
                ),
            )?;
            Ok(self.reply(run_id, &id, true)?.is_some())
        };
        action().map_err(tracker_block)
    }
    fn alert(&mut self, run_id: &str, alert: &Alert) -> Result<()> {
        let digest = Sha256::digest(format!("{}\n{}", alert.step, alert.reason));
        let id = format!("alert:{digest:x}");
        self.post_comment(
            "alert.md",
            run_id,
            &id,
            &context_values(&alert.step, "", &alert.reason, ""),
        )?;
        Ok(())
    }
    fn question(
        &mut self,
        run_id: &str,
        step: &str,
        request: u32,
        bot: &str,
        questions: &[String],
    ) -> std::result::Result<(), Block> {
        let values = context_values(step, bot, &questions.join("\n"), "");
        self.post_comment(
            "question.md",
            run_id,
            &format!("question:{step}:{request}"),
            &values,
        )
        .map(|_| ())
        .map_err(tracker_block)
    }
    fn answer(
        &mut self,
        run_id: &str,
        step: &str,
        request: u32,
    ) -> std::result::Result<Option<String>, Block> {
        let mut action = || -> Result<Option<String>> {
            let answer = self.reply(run_id, &format!("question:{step}:{request}"), false)?;
            if let Some(answer) = &answer {
                self.post_comment(
                    "answer_ack.md",
                    run_id,
                    &format!("answer:{step}:{request}"),
                    &context_values(step, "", answer, answer),
                )?;
            }
            Ok(answer)
        };
        action().map_err(tracker_block)
    }
}
fn tracker_block(error: anyhow::Error) -> Block {
    let reason = format!("{error:#}");
    let kind = if reason.contains("HTTP status 401") || reason.contains("HTTP status 403") {
        BlockKind::Capability
    } else {
        BlockKind::Transient
    };
    Block::new(kind, reason)
}
fn context_values(
    step: &str,
    bot: &str,
    technical: &str,
    answer: &str,
) -> BTreeMap<String, String> {
    [
        "workflow",
        "base_branch",
        "branch",
        "run_status_cmd",
        "bot",
        "step",
        "question",
        "technical",
        "options",
        "artifact_links",
        "answer",
        "summary",
        "reason",
        "pr",
        "merge_commit",
        "docs",
    ]
    .into_iter()
    .map(|key| {
        let value = match key {
            "step" => step,
            "bot" => bot,
            "question" | "technical" | "reason" => technical,
            "answer" => answer,
            "run_status_cmd" => "hermon status",
            _ => "",
        };
        (key.into(), value.into())
    })
    .collect()
}
fn string(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("invalid Linear {key}"))
}
fn issue(value: &Value) -> Result<Issue> {
    Ok(Issue {
        id: string(value, "id")?,
        title: string(value, "title")?,
        description: value["description"].as_str().unwrap_or_default().into(),
    })
}
fn page_after(value: &Value) -> Result<Option<String>> {
    match value["pageInfo"]["hasNextPage"]
        .as_bool()
        .context("invalid Linear page info")?
    {
        true => Ok(Some(string(&value["pageInfo"], "endCursor")?)),
        false => Ok(None),
    }
}
fn comment_uuid(key: &str) -> String {
    let mut bytes: [u8; 32] = Sha256::digest(key.as_bytes()).into();
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex = bytes[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
