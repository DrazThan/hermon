//! Docker auto-discovery (#92): pure functions over `docker ps` JSON that
//! decide which labeled containers should become remotes, and which
//! previously-discovered ones should be torn down. Nothing here spawns or
//! execs anything — [`crate::engine`]'s scan tick is the only caller, and
//! it is the one place that actually runs `docker ps` and turns
//! [`Discovered`] into a running [`crate::remote::source::RemoteSource`]
//! via [`crate::remote::spec::docker_spec`] and
//! [`crate::remote::spec::to_command`].
//!
//! **Provenance invariant, sanctioned exception** (see the invariant
//! documented at the top of [`crate::remote::spec`]): every name handled
//! here — a container's own name and its optional `dev.hermon.agent.name`
//! label — comes from `docker ps`, i.e. from whatever happens to be
//! running on this machine, not from the user's own `--docker-auto`
//! invocation. #91's provenance invariant forbids exactly that for
//! `--remote`; #92 is the one place allowed to break it, on three
//! conditions kept together here rather than scattered across the crate:
//!
//! 1. every name — container name *and* label value — is validated with
//!    [`crate::remote::spec::validate_name`] before it can reach argv (a
//!    hostile image could otherwise name its own container `-oProxyCommand=…`
//!    the same way an `ssh:`/`docker:` `--remote` spec could);
//! 2. the label value is additionally sanitized ([`crate::render::sanitize`])
//!    and length-capped before it becomes a roster prefix, since unlike a
//!    container name (docker's own charset is already stricter than ours)
//!    it is free-form display text a hostile image fully controls;
//! 3. an explicit `--remote` always wins a name collision — a colliding
//!    label is exactly what a hostile image would carry to get
//!    auto-followed (label spoofing, threat review 2026-08-31), so the
//!    collision is refused and logged visibly rather than silently
//!    resolved.
//!
//! `--docker-auto` itself is a trust extension, not a bug: running any
//! third-party image with the label at all opts every container it starts
//! into being followed. See the README's `--docker-auto` section for the
//! plain statement of that trade-off.

use std::collections::{HashMap, HashSet};

use crate::remote::spec::validate_name;
use crate::render::{clip, sanitize};

/// The label that opts a container into `--docker-auto` at all. Passed
/// bare to `docker ps --filter label=…`, so any value (or none) counts —
/// only [`AGENT_NAME_LABEL`] carries meaning.
pub const AGENT_LABEL: &str = "dev.hermon.agent";

/// The label whose value overrides the roster name a container's own
/// `docker ps` name would otherwise give it.
pub const AGENT_NAME_LABEL: &str = "dev.hermon.agent.name";

/// Longest a `dev.hermon.agent.name` label may be once sanitized. It
/// becomes a roster prefix, not a bounded protocol field like #90's wire
/// frames, so a hostile image could otherwise hand us an arbitrarily long
/// string to spam the roster with.
const MAX_LABEL_NAME_LEN: usize = 64;

/// One row of `docker ps --format {{json .}}` output, the fields this
/// module reads. `#[serde(default)]` on every field: a `docker ps` build
/// that omits one is a missing value here, not a parse failure.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct PsEntry {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "Names", default)]
    names: String,
    #[serde(rename = "Labels", default)]
    labels: String,
}

/// One running, labeled container as `docker ps` reported it, before any
/// validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// Docker's own container id — the stable identity a name change (a
    /// `docker rename`, or an edited `dev.hermon.agent.name` label) is
    /// tracked against across ticks.
    pub id: String,
    /// The container name `docker exec` targets.
    pub name: String,
    /// The raw `dev.hermon.agent.name` label value, if the container set
    /// one — not yet validated or sanitized.
    pub agent_name: Option<String>,
}

/// Parses `docker ps --format {{json .}}` output: one JSON object per
/// line (docker's own format — not a JSON array), kept only when its
/// `Labels` field actually carries [`AGENT_LABEL`]. `docker ps --filter
/// label=…` already does this filtering server-side; re-checking it here
/// means an unlabeled container can never appear even if the filter is
/// ever dropped or misapplied by a caller.
///
/// A line that fails to parse, or names no container id, is skipped, not
/// fatal — a future `docker ps` field change must not take discovery down.
pub fn parse_ps(stdout: &str) -> Vec<Container> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<PsEntry>(line).ok())
        .filter(|e| !e.id.is_empty() && has_label(&e.labels, AGENT_LABEL))
        .map(|e| Container {
            id: e.id,
            // A container normally has exactly one name; docker joins
            // several with commas in the rare cases it has more. The
            // first is as good a choice as any and keeps a single
            // `validate_name`-clean token.
            name: e.names.split(',').next().unwrap_or_default().to_string(),
            agent_name: label_value(&e.labels, AGENT_NAME_LABEL),
        })
        .collect()
}

/// Whether `docker ps`'s comma-joined `key=value,…` `Labels` field carries
/// `key` at all (with any value, or none).
fn has_label(labels: &str, key: &str) -> bool {
    labels
        .split(',')
        .any(|kv| kv.split('=').next() == Some(key))
}

/// Reads one label's value out of the same `Labels` field. Docker doesn't
/// escape a comma inside a label value in this format, so a value
/// containing one would be split wrong here — a pre-existing `docker ps`
/// limitation, not something this module works around.
fn label_value(labels: &str, key: &str) -> Option<String> {
    labels
        .split(',')
        .find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key))
        .map(|(_, v)| v.to_string())
}

/// One container cleared to become (or stay) a remote: both names already
/// validated, the display name sanitized and length-capped, ready for
/// [`crate::remote::spec::docker_spec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub id: String,
    /// The container name docker itself uses.
    pub container: String,
    /// The roster prefix this remote's keys will carry.
    pub name: String,
}

/// What one discovery tick decided: remotes to start, remotes (by name) to
/// tear down, and anything worth a visible log line — an invalid name, or
/// a collision with an explicit `--remote`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sync {
    pub spawn: Vec<Discovered>,
    pub remove: Vec<String>,
    pub warnings: Vec<String>,
}

/// Reconciles this tick's `docker ps` listing against what was running
/// last tick — pure: no docker, no clock, nothing spawned. `managed` is
/// the caller's own bookkeeping from the previous tick (container id ->
/// the name it's running under); `explicit` is every name already taken
/// by a `--remote` flag, which always wins a collision. Returns this
/// tick's decisions plus the bookkeeping to pass back in next tick.
///
/// Rename handling: if a still-present container's computed name changes
/// (its own name changed, or its label did), the old name is torn down
/// and the container respawns under the new one — a running
/// [`crate::remote::source::RemoteSource`] has no rename operation, so
/// this is the only way to change what a container's keys are prefixed
/// with.
pub fn reconcile(
    containers: &[Container],
    explicit: &HashSet<String>,
    managed: &HashMap<String, String>,
) -> (Sync, HashMap<String, String>) {
    let mut sync = Sync::default();
    let mut next: HashMap<String, String> = HashMap::new();
    // Names claimed by a container already processed this tick, so two
    // containers racing for the same label don't both spawn.
    let mut claimed: HashSet<String> = HashSet::new();

    for c in containers {
        if let Err(e) = validate_name(&c.name) {
            sync.warnings.push(format!(
                "docker-auto: container {} has an invalid name {:?} ({e}), skipping",
                c.id, c.name
            ));
            continue;
        }

        let name = match &c.agent_name {
            Some(raw) => match validate_name(raw) {
                Ok(()) => clip(&sanitize(raw), MAX_LABEL_NAME_LEN),
                Err(e) => {
                    sync.warnings.push(format!(
                        "docker-auto: container {:?} has an invalid {AGENT_NAME_LABEL} label \
                         {raw:?} ({e}), falling back to the container name",
                        c.name
                    ));
                    c.name.clone()
                }
            },
            None => c.name.clone(),
        };

        if explicit.contains(&name) {
            sync.warnings.push(format!(
                "docker-auto: container {:?} labeled name {name:?} collides with an explicit \
                 --remote, ignoring (possible label spoofing)",
                c.name
            ));
            continue;
        }
        if !claimed.insert(name.clone()) {
            sync.warnings.push(format!(
                "docker-auto: container {:?} labeled name {name:?} collides with another \
                 discovered container this tick, ignoring",
                c.name
            ));
            continue;
        }

        if managed.get(&c.id) != Some(&name) {
            if let Some(old) = managed.get(&c.id) {
                sync.remove.push(old.clone());
            }
            sync.spawn.push(Discovered {
                id: c.id.clone(),
                container: c.name.clone(),
                name: name.clone(),
            });
        }
        next.insert(c.id.clone(), name);
    }

    for (id, name) in managed {
        if !next.contains_key(id) {
            sync.remove.push(name.clone());
        }
    }

    (sync, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps_line(id: &str, name: &str, labels: &str) -> String {
        format!(
            r#"{{"ID":"{id}","Names":"{name}","Labels":"{labels}","Image":"x","State":"running"}}"#
        )
    }

    // -------------------------------------------------------------- parse_ps

    #[test]
    fn parse_ps_reads_labeled_containers() {
        let stdout = ps_line("abc123", "job1", "dev.hermon.agent=1");
        let containers = parse_ps(&stdout);
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].id, "abc123");
        assert_eq!(containers[0].name, "job1");
        assert_eq!(containers[0].agent_name, None);
    }

    #[test]
    fn parse_ps_reads_the_agent_name_label() {
        let stdout = ps_line(
            "abc123",
            "job1",
            "dev.hermon.agent=1,dev.hermon.agent.name=worker1",
        );
        let containers = parse_ps(&stdout);
        assert_eq!(containers[0].agent_name, Some("worker1".to_string()));
    }

    #[test]
    fn parse_ps_never_returns_an_unlabeled_container() {
        let stdout = ps_line("abc123", "job1", "some.other.label=1");
        assert!(parse_ps(&stdout).is_empty());
    }

    #[test]
    fn parse_ps_skips_malformed_lines_without_failing_the_rest() {
        let stdout = format!(
            "not json at all\n{}\n",
            ps_line("abc123", "job1", "dev.hermon.agent=1")
        );
        let containers = parse_ps(&stdout);
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].id, "abc123");
    }

    #[test]
    fn parse_ps_skips_entries_missing_an_id() {
        let stdout = ps_line("", "job1", "dev.hermon.agent=1");
        assert!(parse_ps(&stdout).is_empty());
    }

    #[test]
    fn parse_ps_reads_multiple_lines() {
        let stdout = format!(
            "{}\n{}\n",
            ps_line("id1", "job1", "dev.hermon.agent=1"),
            ps_line("id2", "job2", "dev.hermon.agent=1")
        );
        let containers = parse_ps(&stdout);
        assert_eq!(containers.len(), 2);
    }

    // -------------------------------------------------------------- reconcile

    fn container(id: &str, name: &str, agent_name: Option<&str>) -> Container {
        Container {
            id: id.to_string(),
            name: name.to_string(),
            agent_name: agent_name.map(str::to_string),
        }
    }

    #[test]
    fn a_new_container_is_spawned() {
        let containers = vec![container("id1", "job1", None)];
        let (sync, next) = reconcile(&containers, &HashSet::new(), &HashMap::new());
        assert_eq!(
            sync.spawn,
            vec![Discovered {
                id: "id1".to_string(),
                container: "job1".to_string(),
                name: "job1".to_string(),
            }]
        );
        assert!(sync.remove.is_empty());
        assert_eq!(next.get("id1"), Some(&"job1".to_string()));
    }

    #[test]
    fn a_container_gone_from_docker_ps_is_removed() {
        let managed = HashMap::from([("id1".to_string(), "job1".to_string())]);
        let (sync, next) = reconcile(&[], &HashSet::new(), &managed);
        assert_eq!(sync.remove, vec!["job1".to_string()]);
        assert!(sync.spawn.is_empty());
        assert!(next.is_empty());
    }

    #[test]
    fn an_unchanged_container_neither_spawns_nor_removes() {
        let containers = vec![container("id1", "job1", None)];
        let managed = HashMap::from([("id1".to_string(), "job1".to_string())]);
        let (sync, next) = reconcile(&containers, &HashSet::new(), &managed);
        assert!(sync.spawn.is_empty());
        assert!(sync.remove.is_empty());
        assert_eq!(next.get("id1"), Some(&"job1".to_string()));
    }

    #[test]
    fn a_renamed_container_tears_down_the_old_name_and_spawns_the_new_one() {
        // Same container id, its dev.hermon.agent.name label changed.
        let containers = vec![container("id1", "job1", Some("worker2"))];
        let managed = HashMap::from([("id1".to_string(), "worker1".to_string())]);
        let (sync, next) = reconcile(&containers, &HashSet::new(), &managed);
        assert_eq!(sync.remove, vec!["worker1".to_string()]);
        assert_eq!(
            sync.spawn,
            vec![Discovered {
                id: "id1".to_string(),
                container: "job1".to_string(),
                name: "worker2".to_string(),
            }]
        );
        assert_eq!(next.get("id1"), Some(&"worker2".to_string()));
    }

    #[test]
    fn a_name_colliding_with_an_explicit_remote_is_refused_and_logged() {
        let containers = vec![container("id1", "job1", None)];
        let explicit = HashSet::from(["job1".to_string()]);
        let (sync, next) = reconcile(&containers, &explicit, &HashMap::new());
        assert!(sync.spawn.is_empty(), "explicit --remote wins");
        assert!(next.is_empty());
        assert!(
            sync.warnings.iter().any(|w| w.contains("collides")),
            "{:?}",
            sync.warnings
        );
    }

    #[test]
    fn a_container_that_becomes_a_collision_after_a_rename_is_torn_down() {
        // job1 was legitimately auto-discovered as "worker1"; its label
        // then changes to collide with an explicit --remote named "job1".
        let containers = vec![container("id1", "job1", Some("job1"))];
        let explicit = HashSet::from(["job1".to_string()]);
        let managed = HashMap::from([("id1".to_string(), "worker1".to_string())]);
        let (sync, next) = reconcile(&containers, &explicit, &managed);
        assert_eq!(sync.remove, vec!["worker1".to_string()]);
        assert!(sync.spawn.is_empty());
        assert!(!next.contains_key("id1"));
    }

    #[test]
    fn two_containers_racing_for_the_same_label_only_spawn_the_first() {
        let containers = vec![
            container("id1", "job1", Some("worker")),
            container("id2", "job2", Some("worker")),
        ];
        let (sync, next) = reconcile(&containers, &HashSet::new(), &HashMap::new());
        assert_eq!(sync.spawn.len(), 1);
        assert_eq!(sync.spawn[0].id, "id1");
        assert_eq!(next.len(), 1);
        assert!(
            sync.warnings.iter().any(|w| w.contains("collides")),
            "{:?}",
            sync.warnings
        );
    }

    #[test]
    fn an_invalid_container_name_is_skipped_and_logged() {
        let containers = vec![container("id1", "-oProxyCommand=evil", None)];
        let (sync, next) = reconcile(&containers, &HashSet::new(), &HashMap::new());
        assert!(sync.spawn.is_empty());
        assert!(next.is_empty());
        assert!(
            sync.warnings.iter().any(|w| w.contains("invalid name")),
            "{:?}",
            sync.warnings
        );
    }

    #[test]
    fn an_invalid_label_falls_back_to_the_container_name() {
        let containers = vec![container("id1", "job1", Some("bad name; rm -rf"))];
        let (sync, _next) = reconcile(&containers, &HashSet::new(), &HashMap::new());
        assert_eq!(sync.spawn[0].name, "job1");
        assert!(
            sync.warnings
                .iter()
                .any(|w| w.contains("falling back to the container name")),
            "{:?}",
            sync.warnings
        );
    }

    #[test]
    fn a_hostile_label_is_sanitized_and_length_capped() {
        let hostile = "x\x1b[31m".repeat(20); // control bytes, well over the cap
        let containers = vec![container("id1", "job1", Some(&hostile))];
        // Control bytes fail validate_name's charset check, so this falls
        // back to the container name — the cap/sanitize path is exercised
        // by a long but charset-clean label instead.
        let (sync, _next) = reconcile(&containers, &HashSet::new(), &HashMap::new());
        assert_eq!(sync.spawn[0].name, "job1");

        let long_clean = "a".repeat(200);
        let containers = vec![container("id2", "job2", Some(&long_clean))];
        let (sync, _next) = reconcile(&containers, &HashSet::new(), &HashMap::new());
        assert_eq!(sync.spawn[0].name.chars().count(), MAX_LABEL_NAME_LEN);
    }
}
