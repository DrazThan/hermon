//! Bounded, read-only Hermes store registry. Refreshed by the existing scan loop.
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::bots::{BotInfo, BotOwner, MonitorOptions, default_home, display, path_bytes, store_id};
use crate::source::hermes::HermesSource;
use crate::source::{SessionMeta, Source};

const MAX_STORES: usize = 128;
const MAX_ENTRIES: usize = 4096;

pub struct Store {
    pub path: PathBuf,
    pub source: HermesSource,
    pub owner: Option<BotOwner>,
    pub log: Option<PathBuf>,
    pub available: bool,
    pub last_activity: Option<f64>,
    active: bool,
}

pub struct HermesProfiles {
    options: MonitorOptions,
    stores: BTreeMap<PathBuf, Store>,
    refreshed: Option<Instant>,
    discovery_warnings: Vec<String>,
    pub warnings: Vec<String>,
}

impl HermesProfiles {
    pub fn new(options: MonitorOptions) -> Self {
        Self {
            options,
            stores: BTreeMap::new(),
            refreshed: None,
            discovery_warnings: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn candidates(&mut self, root: &std::path::Path) -> Vec<(PathBuf, String, PathBuf)> {
        let mut out = vec![(
            root.join("state.db"),
            "default".into(),
            root.join("logs/agent.log"),
        )];
        let profiles = root.join("profiles");
        match fs::read_dir(&profiles) {
            Ok(entries) => {
                let mut entries = entries.take(MAX_ENTRIES + 1).collect::<Vec<_>>();
                if entries.len() > MAX_ENTRIES {
                    self.discovery_warnings
                        .push("Hermes discovery incomplete: directory entry limit (4096)".into());
                    entries.truncate(MAX_ENTRIES);
                }
                let mut entries = entries
                    .into_iter()
                    .filter_map(|e| match e {
                        Ok(e) => Some(e),
                        Err(_) => {
                            self.discovery_warnings.push(
                                "Hermes profiles: unreadable directory entry; discovery incomplete"
                                    .into(),
                            );
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                entries.sort_by_key(|e| e.file_name());
                for e in entries {
                    match fs::metadata(e.path()) {
                        Ok(m) if m.is_dir() => {
                            let Some(slug) = e.file_name().to_str().map(str::to_owned) else {
                                self.discovery_warnings.push(
                                    "Hermes profile name is not UTF-8; ownership unknown".into(),
                                );
                                out.push((
                                    e.path().join("state.db"),
                                    String::new(),
                                    e.path().join("logs/agent.log"),
                                ));
                                continue;
                            };
                            out.push((
                                e.path().join("state.db"),
                                slug,
                                e.path().join("logs/agent.log"),
                            ));
                        }
                        Ok(_) => {}
                        Err(_) => self.discovery_warnings.push(format!(
                            "Hermes profile {}: unavailable directory or symlink",
                            display(&e.file_name().to_string_lossy())
                        )),
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => self
                .discovery_warnings
                .push("Hermes profiles directory unavailable; discovery incomplete".into()),
        }
        out
    }

    fn refresh(&mut self) {
        if self
            .refreshed
            .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        self.refreshed = Some(Instant::now());
        self.discovery_warnings.clear();
        let root = self
            .options
            .hermes_home
            .clone()
            .unwrap_or_else(default_home);
        if fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink())
            && fs::metadata(&root).is_err()
        {
            self.discovery_warnings
                .push("Hermes root symlink unavailable; discovery incomplete".into());
        }
        let candidates = self.candidates(&root);
        let mut groups: BTreeMap<PathBuf, Vec<(String, PathBuf)>> = BTreeMap::new();
        for (path, slug, log) in candidates {
            match fs::canonicalize(&path) {
                Ok(canonical) if canonical.is_file() => {
                    groups.entry(canonical).or_default().push((slug, log));
                }
                Ok(_) => self
                    .discovery_warnings
                    .push(format!("Hermes {}: state.db is not a file", display(&slug))),
                Err(e) => {
                    // An absent DB is ordinary, but a dangling DB link is not.
                    if e.kind() != std::io::ErrorKind::NotFound
                        || fs::symlink_metadata(&path).is_ok()
                    {
                        self.discovery_warnings
                            .push(format!("Hermes {}: state.db unavailable", display(&slug)));
                    }
                }
            }
        }
        let explicit = self
            .options
            .hermes_db
            .as_ref()
            .map(|p| fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
        if let Some(path) = &explicit
            && !matches!(fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        {
            groups.entry(path.clone()).or_default();
        }
        let mut ordered = groups.into_iter().collect::<Vec<_>>();
        ordered.sort_by(|a, b| {
            let main = |v: &Vec<(String, PathBuf)>| !v.iter().any(|(s, _)| s == "default");
            main(&a.1)
                .cmp(&main(&b.1))
                .then_with(|| a.1.first().map(|x| &x.0).cmp(&b.1.first().map(|x| &x.0)))
                .then(a.0.cmp(&b.0))
        });
        for store in self.stores.values_mut() {
            store.active = false;
        }
        let mut admitted = 0;
        for (path, aliases) in ordered {
            if explicit.as_ref().is_some_and(|p| p != &path) {
                continue;
            }
            if admitted >= MAX_STORES {
                self.discovery_warnings
                    .push("Hermes discovery incomplete: store limit (128)".into());
                break;
            }
            admitted += 1;
            let existing_slug = self
                .stores
                .get(&path)
                .and_then(|s| s.owner.as_ref())
                .map(|o| o.slug.as_str());
            let winner = aliases
                .iter()
                .find(|(s, _)| s == "default")
                .or_else(|| {
                    aliases
                        .iter()
                        .find(|(s, _)| Some(s.as_str()) == existing_slug)
                })
                .or_else(|| aliases.first());
            let owner = winner
                .filter(|(slug, _)| !slug.is_empty())
                .map(|(slug, _)| BotOwner {
                    store: store_id("", &path_bytes(&path)),
                    slug: slug.clone(),
                    label: display(slug),
                    origin: String::new(),
                });
            if aliases.len() > 1 {
                let names = aliases
                    .iter()
                    .map(|(s, _)| display(s))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.discovery_warnings.push(format!(
                    "Hermes canonical store aliases: {names}; counted once"
                ));
            }
            let log = winner.map(|(_, l)| l.clone());
            let store = self.stores.entry(path.clone()).or_insert_with(|| Store {
                path: path.clone(),
                source: HermesSource::from_path(path),
                owner: None,
                log: None,
                available: false,
                last_activity: None,
                active: false,
            });
            store.owner = owner;
            store.log = log;
            store.active = true;
        }
        // Keep a bounded unavailable inventory, but never admit old retargeted stores.
        let active: HashSet<_> = self
            .stores
            .iter()
            .filter(|(_, s)| s.active)
            .map(|(p, _)| p.clone())
            .collect();
        let mut retained = active.len();
        self.stores.retain(|p, _| {
            if active.contains(p) {
                true
            } else if retained < MAX_STORES {
                retained += 1;
                true
            } else {
                false
            }
        });
    }

    pub fn scan(&mut self) -> Vec<(PathBuf, Option<BotOwner>, SessionMeta)> {
        self.refresh();
        self.warnings = self.discovery_warnings.clone();
        let mut out = Vec::new();
        for store in self.stores.values_mut() {
            if store.active {
                let sessions = store.source.sessions();
                store.available = store.source.healthy();
                if store.available {
                    store.last_activity = sessions.iter().map(|s| s.last_ts).reduce(f64::max);
                }
                for mut s in sessions {
                    s.last_tool = store.source.last_tool(&s.id);
                    out.push((store.path.clone(), store.owner.clone(), s));
                }
            } else {
                store.available = false;
            }
            if !store.available {
                let name = store
                    .owner
                    .as_ref()
                    .map_or_else(|| display(&store.path.to_string_lossy()), BotOwner::display);
                self.warnings.push(format!(
                    "Hermes {name}: store unavailable (missing, unreadable or bad schema); retrying"
                ));
            }
        }
        self.warnings.truncate(128);
        out
    }
    pub fn bots(&self) -> Vec<BotInfo> {
        self.stores
            .values()
            .filter_map(|s| {
                Some(BotInfo {
                    owner: s.owner.clone()?,
                    available: s.available,
                    last_activity: s.last_activity,
                })
            })
            .collect()
    }
    pub fn source(&self, path: &std::path::Path) -> Option<&HermesSource> {
        self.stores
            .get(path)
            .filter(|s| s.active && s.available)
            .map(|s| &s.source)
    }
    pub fn logs(&self) -> Vec<(PathBuf, Option<BotOwner>)> {
        self.stores
            .values()
            .filter(|s| s.active)
            .filter_map(|s| Some((s.log.clone()?, s.owner.clone())))
            .collect()
    }
}
