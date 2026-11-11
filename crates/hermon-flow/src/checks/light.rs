//! Opt-in light proofs. The pin lives outside the worker's Git authority.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedProof {
    pub reference: String,
    pub object: String,
    pub commit: String,
    pub expectation_sha256: String,
    ticket: String,
    branch: String,
    base: String,
    paths: BTreeMap<String, Vec<String>>,
    note_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Amendments {
    pub schema: u32,
    pub red: RedProof,
    pub red_commit: String,
    pub candidate_sha: String,
    pub paths: Vec<String>,
    pub diff_sha256: String,
    pub empty: bool,
}
impl Amendments {
    pub fn disposition(&self) -> String {
        format!(
            "{}:{}",
            if self.empty { "empty" } else { "accepted" },
            self.diff_sha256
        )
    }
}
impl GitChecks {
    /// Hosts with per-attempt log directories share a run-scoped pin directory.
    pub fn set_proof_dir(&mut self, directory: PathBuf) -> Result<()> {
        fs::create_dir_all(&directory)?;
        let directory = fs::canonicalize(directory)?;
        ensure!(
            !directory.starts_with(&self.config.workdir),
            "RED proof must be outside worktree"
        );
        self.proof_dir = directory;
        Ok(())
    }
    fn expectation_digest(&self) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                &self.context.new_tests,
                &self.context.baseline_failures,
            ))?)
        ))
    }
    fn pin_path(&self, reference: &str) -> PathBuf {
        self.proof_dir.join(format!(
            "red-{:x}.json",
            Sha256::digest(reference.as_bytes())
        ))
    }
    fn proof(&self, reference: &str, object: &str, note: &EvidenceNote) -> Result<RedProof> {
        Ok(RedProof {
            reference: reference.into(),
            object: object.into(),
            commit: note.commit.clone(),
            expectation_sha256: self.expectation_digest()?,
            ticket: self.config.ticket.clone(),
            branch: self.config.branch.clone(),
            base: self.config.base.clone(),
            paths: self.config.paths.clone(),
            note_sha256: format!("{:x}", Sha256::digest(self.annotation(object)?.as_bytes())),
        })
    }
    /// Record only genuine RED checks; unused strict note serialization is unchanged.
    pub(super) fn pin_red(&self, reference: &str, object: &str, note: &EvidenceNote) -> Result<()> {
        if note.tag != "red"
            || !note.commands.iter().any(|c| {
                c.name == "test"
                    && c.exit_code.is_some_and(|code| code != 0)
                    && c.tests.as_ref().is_some_and(|r| r.failed > 0)
            })
        {
            return Ok(());
        }
        let proof = self.proof(reference, object, note)?;
        let path = self.pin_path(reference);
        if path.exists() {
            let previous: RedProof = serde_json::from_slice(&fs::read(&path)?)?;
            ensure!(previous == proof, "original RED proof changed");
        } else {
            let mut file = tempfile::NamedTempFile::new_in(&self.proof_dir)?;
            serde_json::to_writer(&mut file, &proof)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(path)?;
        }
        Ok(())
    }
    pub fn red_proof_verified(&self, reference: &str) -> Result<RedProof> {
        let head = self.bound_head()?;
        let (object, note) = self.freeze_identity("red", reference, &head)?;
        let pin: RedProof = serde_json::from_slice(
            &fs::read(self.pin_path(reference)).context("missing original RED pin")?,
        )?;
        ensure!(
            pin == self.proof(reference, &object, &note)?,
            "original RED proof changed"
        );
        ensure!(
            !self.context.new_tests.is_empty() && self.context.baseline_failures.is_empty(),
            "light guard requires baseline-clean independent expected IDs"
        );
        ensure!(
            note.frozen_path_sets.get("tests") == self.config.paths.get("tests"),
            "RED did not freeze the configured test paths"
        );
        for command in &note.commands {
            ensure!(
                hash_file(&command.log.path)? == command.log.sha256,
                "original RED check log changed"
            );
        }
        let failures: BTreeSet<_> = note
            .commands
            .iter()
            .filter_map(|c| c.tests.as_ref())
            .flat_map(|r| r.failures.iter().cloned())
            .collect();
        ensure!(
            self.context.new_tests.is_subset(&failures),
            "RED does not prove expected failures"
        );
        Ok(pin)
    }
    fn test_paths(&self, red: &str, head: &str) -> Result<Vec<String>> {
        let patterns = self
            .config
            .paths
            .get("tests")
            .context("missing tests path set")?;
        ensure!(!patterns.is_empty(), "empty tests path set");
        Ok(self
            .paths_between(red, head)?
            .into_iter()
            .filter(|path| patterns.iter().any(|pattern| glob_matches(pattern, path)))
            .collect())
    }
    fn classify_history(&self, red: &str, head: &str) -> Result<()> {
        let tests = self
            .config
            .paths
            .get("tests")
            .context("missing tests path set")?;
        let production = self
            .config
            .paths
            .get("production")
            .context("missing production path set")?;
        ensure!(
            !tests.is_empty() && !production.is_empty(),
            "light guard requires nonempty tests and production path sets"
        );
        let history = self.text(&["rev-list", "--parents", &format!("{red}..{head}")])?;
        for line in history.lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            ensure!(
                parts.len() == 2,
                "light history must be linear; merge commits are not allowed"
            );
            let mut category = None;
            for path in self.paths_between(parts[1], parts[0])? {
                let test = tests.iter().any(|p| glob_matches(p, &path));
                let prod = production.iter().any(|p| glob_matches(p, &path));
                ensure!(
                    test != prod,
                    "overlapping or unclassified light path {path:?}"
                );
                ensure!(
                    category.is_none_or(|prior| prior == test),
                    "mixed test and production commit {}",
                    parts[0]
                );
                category = Some(test);
            }
        }
        Ok(())
    }
    /// Explicit literal pathspecs, deterministic options, and bounded output.
    pub fn evidence_diff(&self, from: &str, to: &str, paths: Option<&[String]>) -> Result<Vec<u8>> {
        if paths.is_some_and(|p| p.is_empty()) {
            return Ok(Vec::new());
        }
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-relative",
            "--unified=3",
            "--inter-hunk-context=0",
            "--submodule=short",
            "--ignore-submodules=none",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--diff-algorithm=myers",
            "--no-indent-heuristic",
            from,
            to,
            "--",
        ];
        if let Some(paths) = paths {
            args.extend(paths.iter().map(String::as_str));
        }
        let mut command = Command::new("/usr/bin/git");
        command
            .current_dir(&self.config.workdir)
            .args(&args)
            .env_remove("GIT_EXTERNAL_DIFF")
            .env_remove("GIT_DIFF_OPTS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            command.env_remove(key);
        }
        let mut child = command.spawn()?;
        let mut bytes = Vec::new();
        let read = child
            .stdout
            .take()
            .context("git diff stdout")?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes);
        if read.is_err() || bytes.len() > 1024 * 1024 {
            let _ = child.kill();
        }
        let status = child.wait()?;
        read?;
        ensure!(bytes.len() <= 1024 * 1024, "amendment diff exceeds 1 MiB");
        ensure!(status.success(), "git evidence diff failed");
        Ok(bytes)
    }
    pub fn amendments_verified(
        &self,
        red: &str,
        green: &str,
        enforce: bool,
    ) -> Result<(Amendments, Vec<u8>)> {
        let head = self.bound_head()?;
        ensure!(
            accepted_candidate(
                &self.config.workdir,
                &self.config.branch,
                &self.config.base,
                &self.config.ticket,
                green
            )? == head,
            "review candidate changed"
        );
        let (receipt, diff) = self.amendment_delta(red, &head, enforce)?;
        let object = self.text(&[
            "rev-parse",
            "--verify",
            &format!("refs/tags/{green}^{{tag}}"),
        ])?;
        let note = self.load_note(&object)?;
        if enforce || note.amendments.is_some() {
            ensure!(
                note.amendments.as_ref() == Some(&receipt),
                "GREEN amendment proof differs"
            );
        }
        Ok((receipt, diff))
    }
    pub(super) fn amendment_delta(
        &self,
        reference: &str,
        head: &str,
        classify: bool,
    ) -> Result<(Amendments, Vec<u8>)> {
        let red = self.red_proof_verified(reference)?;
        if classify {
            self.classify_history(&red.commit, head)?;
        }
        let paths = self.test_paths(&red.commit, head)?;
        let diff = self.evidence_diff(&red.commit, head, Some(&paths))?;
        let receipt = Amendments {
            schema: 1,
            red_commit: red.commit.clone(),
            red,
            candidate_sha: head.into(),
            paths,
            empty: diff.is_empty(),
            diff_sha256: format!("{:x}", Sha256::digest(&diff)),
        };
        Ok((receipt, diff))
    }
    pub(super) fn validate_amendment_note(&self, note: &EvidenceNote) -> Result<()> {
        if let Some(receipt) = &note.amendments {
            let (current, _) = self.amendment_delta(&receipt.red.reference, &note.commit, true)?;
            ensure!(
                current == *receipt,
                "GREEN amendment digest or RED proof changed"
            );
        }
        Ok(())
    }
}
