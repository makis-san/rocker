//! Container grouping (PLAN §5.1). Pure resolution logic lives here.

use serde::{Deserialize, Serialize};

use crate::container::Container;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GroupId(pub String);

impl GroupId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GroupKind {
    /// User-curated membership, keyed by [`GroupRule::member_key`] semantics.
    Manual { member_keys: Vec<String> },
    /// Saved filter that populates dynamically.
    Rule { rule: GroupRule },
}

/// A saved filter. Empty fields match everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRule {
    #[serde(default)]
    pub name_glob: Option<String>,
    #[serde(default)]
    pub image_glob: Option<String>,
    /// `key=value` label selectors, all of which must match.
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    /// `#rrggbb` accent for the group header.
    pub color: String,
    pub icon: String,
    #[serde(flatten)]
    pub kind: GroupKind,
}

impl Group {
    /// Stable membership key for a container, most-specific first (PLAN §5.1):
    /// Compose `service@project`, then container name, then id.
    pub fn member_key(c: &Container) -> String {
        match (&c.compose_service, &c.compose_project) {
            (Some(svc), Some(proj)) => format!("compose:{proj}/{svc}"),
            _ if !c.name.is_empty() => format!("name:{}", c.name),
            _ => format!("id:{}", c.id),
        }
    }

    /// Whether `container` belongs to this group right now.
    pub fn contains(&self, container: &Container) -> bool {
        match &self.kind {
            GroupKind::Manual { member_keys } => member_keys
                .iter()
                .any(|k| k == &Self::member_key(container)),
            GroupKind::Rule { rule } => rule_matches(rule, container),
        }
    }
}

fn rule_matches(rule: &GroupRule, c: &Container) -> bool {
    // An entirely empty rule matches nothing — a group with no selector is a
    // mistake, not "everything".
    if rule.name_glob.as_deref().unwrap_or("").is_empty()
        && rule.image_glob.as_deref().unwrap_or("").is_empty()
        && rule.labels.is_empty()
    {
        return false;
    }
    if let Some(g) = rule.name_glob.as_deref().filter(|s| !s.is_empty()) {
        if !glob_match(g, &c.name) {
            return false;
        }
    }
    if let Some(g) = rule.image_glob.as_deref().filter(|s| !s.is_empty()) {
        if !glob_match(g, &c.image) {
            return false;
        }
    }
    for (k, v) in &rule.labels {
        let hit = c
            .labels
            .iter()
            .any(|(ck, cv)| ck == k && (v.is_empty() || cv == v));
        if !hit {
            return false;
        }
    }
    true
}

/// Which kind of section a resolved row belongs to. `User` sections are
/// editable/removable in the Groups screen; the others are automatic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionId {
    User(GroupId),
    Compose(String),
    Ungrouped,
}

/// One rendered section of the container list.
pub struct ResolvedSection<'a> {
    pub id: SectionId,
    pub label: String,
    /// `#rrggbb` accent for a user group, `None` for an automatic section.
    pub color: Option<String>,
    pub containers: Vec<&'a Container>,
}

/// Resolve `containers` (already sorted by the caller) and the user `groups`
/// into display sections (PLAN §5.1):
///
/// 1. every user group that currently has at least one member, in config
///    order — a container can appear under more than one;
/// 2. then the containers claimed by no user group, smart-grouped by Compose
///    project (consecutive runs), with the project-less remainder last.
pub fn resolve<'a>(containers: &'a [Container], groups: &[Group]) -> Vec<ResolvedSection<'a>> {
    let mut out: Vec<ResolvedSection<'a>> = Vec::new();
    let mut claimed: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for g in groups {
        let members: Vec<&Container> = containers.iter().filter(|c| g.contains(c)).collect();
        for c in &members {
            claimed.insert(c.id.0.as_str());
        }
        if members.is_empty() {
            continue;
        }
        out.push(ResolvedSection {
            id: SectionId::User(g.id.clone()),
            label: g.name.clone(),
            color: Some(g.color.clone()),
            containers: members,
        });
    }

    let leftover: Vec<&Container> = containers
        .iter()
        .filter(|c| !claimed.contains(c.id.0.as_str()))
        .collect();

    let mut i = 0;
    while i < leftover.len() {
        let project = leftover[i].compose_project.as_deref();
        let start = i;
        while i < leftover.len() && leftover[i].compose_project.as_deref() == project {
            i += 1;
        }
        let run = leftover[start..i].to_vec();
        out.push(match project {
            Some(p) => ResolvedSection {
                id: SectionId::Compose(p.to_string()),
                label: p.to_string(),
                color: None,
                containers: run,
            },
            None => ResolvedSection {
                id: SectionId::Ungrouped,
                label: "Ungrouped".to_string(),
                color: None,
                containers: run,
            },
        });
    }

    out
}

/// Minimal `*` / `?` glob, case-sensitive. Good enough for name and image
/// selectors; replaced with a real matcher if selectors grow.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn inner(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => inner(&p[1..], t) || (!t.is_empty() && inner(p, &t[1..])),
            Some(b'?') => !t.is_empty() && inner(&p[1..], &t[1..]),
            Some(&c) => t.first() == Some(&c) && inner(&p[1..], &t[1..]),
        }
    }
    inner(pattern.as_bytes(), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_basics() {
        assert!(glob_match("web-*", "web-1"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("api?", "api1"));
        assert!(!glob_match("web-*", "db-1"));
        assert!(!glob_match("api?", "api"));
    }

    fn container(name: &str, image: &str) -> Container {
        Container {
            id: crate::ContainerId::new(format!("id-{name}")),
            name: name.into(),
            image: image.into(),
            state: crate::ContainerState::Running,
            status: "Up".into(),
            ports: vec![],
            compose_project: None,
            compose_service: None,
            labels: vec![],
        }
    }

    #[test]
    fn member_key_prefers_compose() {
        let mut c = container("proj-web-1", "nginx");
        c.compose_project = Some("proj".into());
        c.compose_service = Some("web".into());
        assert_eq!(Group::member_key(&c), "compose:proj/web");
        c.compose_service = None;
        c.compose_project = None;
        assert_eq!(Group::member_key(&c), "name:proj-web-1");
    }

    #[test]
    fn rule_matches_globs_and_labels() {
        let mut c = container("api-1", "ghcr.io/acme/api:1.2");
        c.labels = vec![("tier".into(), "backend".into())];

        let r = GroupRule {
            name_glob: Some("api-*".into()),
            image_glob: None,
            labels: vec![],
        };
        assert!(rule_matches(&r, &c));
        assert!(!rule_matches(
            &GroupRule {
                name_glob: Some("web-*".into()),
                ..r.clone()
            },
            &c
        ));

        // Label selector: key-only matches any value; key=value must match.
        assert!(rule_matches(
            &GroupRule {
                name_glob: None,
                image_glob: None,
                labels: vec![("tier".into(), String::new())],
            },
            &c
        ));
        assert!(!rule_matches(
            &GroupRule {
                name_glob: None,
                image_glob: None,
                labels: vec![("tier".into(), "frontend".into())],
            },
            &c
        ));

        // An empty rule matches nothing.
        assert!(!rule_matches(&GroupRule::default(), &c));
    }

    #[test]
    fn resolve_sections_user_then_smart() {
        let mut a = container("a", "img");
        a.compose_project = Some("proj".into());
        let mut b = container("b", "img");
        b.compose_project = Some("proj".into());
        let mut cc = container("db", "postgres");
        cc.labels = vec![("role".into(), "db".into())];
        let list = vec![a, b, cc];

        let groups = vec![Group {
            id: GroupId::new("g1"),
            name: "Databases".into(),
            color: "#886644".into(),
            icon: String::new(),
            kind: GroupKind::Rule {
                rule: GroupRule {
                    name_glob: None,
                    image_glob: Some("postgres*".into()),
                    labels: vec![],
                },
            },
        }];

        let sections = resolve(&list, &groups);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].id, SectionId::User(GroupId::new("g1")));
        assert_eq!(sections[0].containers.len(), 1);
        assert_eq!(sections[1].id, SectionId::Compose("proj".into()));
        assert_eq!(sections[1].containers.len(), 2);
    }
}
