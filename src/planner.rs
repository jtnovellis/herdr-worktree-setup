//! Turn discovered candidates into a concrete, inspectable plan.

use crate::discover::Candidate;
use crate::rules::{Action, Rules};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanAction {
    Copy,
    Clone,
    Symlink,
}

impl PlanAction {
    pub fn verb(self) -> &'static str {
        match self {
            PlanAction::Copy => "copy",
            PlanAction::Clone => "clone",
            PlanAction::Symlink => "symlink",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemState {
    Pending,
    /// Already present in the target — never overwritten.
    Exists,
    Excluded,
    Unmatched,
    NestedRepo,
    /// The destination could not be reached safely (a symlinked component in
    /// the worktree). Reported, never written.
    Blocked(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub rel: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub action: Option<PlanAction>,
    pub state: ItemState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Small dev state: copies and explicit symlinks.
    State,
    /// Dependency / build caches: clones.
    Caches,
}

#[derive(Debug, Default, Clone)]
pub struct CopyPlan {
    pub items: Vec<PlanItem>,
}

fn to_plan_action(action: Action) -> Option<PlanAction> {
    match action {
        Action::Copy => Some(PlanAction::Copy),
        Action::Clone => Some(PlanAction::Clone),
        Action::Symlink => Some(PlanAction::Symlink),
        Action::Exclude => None,
    }
}

fn state_for(target: &Path, rel: &str) -> ItemState {
    // Resolve the destination the same way the copier will, so an item whose
    // path crosses a symlink is visible in the plan instead of surfacing only
    // when it is applied.
    match crate::fsops::contained_dst(target, rel) {
        Err(reason) => ItemState::Blocked(reason),
        Ok(dst) if std::fs::symlink_metadata(&dst).is_ok() => ItemState::Exists,
        Ok(_) => ItemState::Pending,
    }
}

impl CopyPlan {
    pub fn build(
        source: &Path,
        target: &Path,
        candidates: Vec<Candidate>,
        rules: &Rules,
    ) -> CopyPlan {
        let mut items = Vec::new();
        for cand in candidates {
            if cand.nested_repo {
                items.push(PlanItem {
                    rel: cand.rel,
                    is_dir: cand.is_dir,
                    is_symlink: cand.is_symlink,
                    action: None,
                    state: ItemState::NestedRepo,
                });
                continue;
            }
            match rules.classify(&cand.rel, cand.is_dir) {
                None => items.push(PlanItem {
                    rel: cand.rel,
                    is_dir: cand.is_dir,
                    is_symlink: cand.is_symlink,
                    action: None,
                    state: ItemState::Unmatched,
                }),
                Some(Action::Exclude) => {
                    // An excluded directory may still hold a wanted child
                    // (`.next/` excluded, `.next/cache/` cloned): look one level down.
                    let mut children = Vec::new();
                    if cand.is_dir && !cand.is_symlink {
                        if let Ok(entries) = std::fs::read_dir(source.join(&cand.rel)) {
                            let mut entries: Vec<_> = entries.flatten().collect();
                            entries.sort_by_key(|e| e.file_name());
                            for entry in entries {
                                let name = entry.file_name().to_string_lossy().into_owned();
                                let rel = format!("{}/{}", cand.rel, name);
                                let Ok(ft) = entry.file_type() else { continue };
                                // Unlike git's enumeration, `read_dir` reports
                                // FIFOs, sockets and devices; copying one would
                                // block forever.
                                if !(ft.is_file() || ft.is_dir() || ft.is_symlink()) {
                                    continue;
                                }
                                let is_symlink = ft.is_symlink();
                                let is_dir = ft.is_dir();
                                if let Some(action) =
                                    rules.classify(&rel, is_dir).and_then(to_plan_action)
                                {
                                    children.push(PlanItem {
                                        state: state_for(target, &rel),
                                        rel,
                                        is_dir,
                                        is_symlink,
                                        action: Some(action),
                                    });
                                }
                            }
                        }
                    }
                    items.push(PlanItem {
                        rel: cand.rel,
                        is_dir: cand.is_dir,
                        is_symlink: cand.is_symlink,
                        action: None,
                        state: ItemState::Excluded,
                    });
                    items.extend(children);
                }
                Some(action) => {
                    let action = to_plan_action(action);
                    items.push(PlanItem {
                        state: state_for(target, &cand.rel),
                        rel: cand.rel,
                        is_dir: cand.is_dir,
                        is_symlink: cand.is_symlink,
                        action,
                    });
                }
            }
        }
        CopyPlan { items }
    }

    pub fn group_of(item: &PlanItem) -> Option<Group> {
        match item.action? {
            PlanAction::Copy | PlanAction::Symlink => Some(Group::State),
            PlanAction::Clone => Some(Group::Caches),
        }
    }

    /// Items (any state) belonging to a group, in plan order.
    pub fn items_in(&self, group: Group) -> Vec<&PlanItem> {
        self.items
            .iter()
            .filter(|i| CopyPlan::group_of(i) == Some(group))
            .collect()
    }

    pub fn pending_in(&self, group: Group) -> Vec<&PlanItem> {
        self.items_in(group)
            .into_iter()
            .filter(|i| i.state == ItemState::Pending)
            .collect()
    }

    /// Items whose destination could not be reached safely.
    pub fn blocked_in(&self, group: Group) -> Vec<(&PlanItem, &str)> {
        self.items_in(group)
            .into_iter()
            .filter_map(|i| match &i.state {
                ItemState::Blocked(reason) => Some((i, reason.as_str())),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::discover::{list_ignored, testrepo};

    #[test]
    fn plan_from_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = testrepo::fixture(tmp.path());
        let target = tmp.path().join("wt");
        std::fs::create_dir_all(target.join(".vercel")).unwrap();
        let rules = Rules::from_config(&Config::default()).unwrap();
        let cands = list_ignored(Path::new("git"), &repo).unwrap();
        let plan = CopyPlan::build(&repo, &target, cands, &rules);

        let find = |rel: &str| {
            plan.items
                .iter()
                .find(|i| i.rel == rel)
                .unwrap_or_else(|| panic!("{rel}"))
        };
        assert_eq!(find(".env").action, Some(PlanAction::Copy));
        assert_eq!(find(".env").state, ItemState::Pending);
        assert_eq!(find(".env.local").action, Some(PlanAction::Copy));
        assert_eq!(find("node_modules").action, Some(PlanAction::Clone));
        assert_eq!(
            find("packages/ui/node_modules").action,
            Some(PlanAction::Clone)
        );
        assert_eq!(find(".turbo").action, Some(PlanAction::Clone));
        assert_eq!(find("dist").state, ItemState::Excluded);
        assert_eq!(find("server.log").state, ItemState::Excluded);
        assert_eq!(find(".next").state, ItemState::Excluded);
        assert_eq!(
            find(".next/cache").action,
            Some(PlanAction::Clone),
            "one-level descend"
        );
        assert!(plan.items.iter().all(|i| i.rel != ".next/server"));
        assert_eq!(find(".vercel").state, ItemState::Exists, "never overwrite");
        assert_eq!(find("vendor-repo").state, ItemState::NestedRepo);
        assert!(find("packages/ui/.env.development").is_symlink);
        assert!(plan.items.iter().all(|i| i.rel != "wip.txt"));

        let state: Vec<&str> = plan
            .pending_in(Group::State)
            .iter()
            .map(|i| i.rel.as_str())
            .collect();
        assert!(state.contains(&".env") && !state.contains(&".vercel"));
        let caches: Vec<&str> = plan
            .pending_in(Group::Caches)
            .iter()
            .map(|i| i.rel.as_str())
            .collect();
        assert!(caches.contains(&"node_modules") && caches.contains(&".next/cache"));
    }
}
