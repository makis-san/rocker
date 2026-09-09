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
    if let Some(g) = &rule.name_glob {
        if !glob_match(g, &c.name) {
            return false;
        }
    }
    if let Some(g) = &rule.image_glob {
        if !glob_match(g, &c.image) {
            return false;
        }
    }
    // Label matching is wired once the engine layer surfaces labels.
    true
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

    #[test]
    fn member_key_prefers_compose() {
        let mut c = Container {
            id: crate::ContainerId::new("abc123"),
            name: "proj-web-1".into(),
            image: "nginx".into(),
            state: crate::ContainerState::Running,
            status: "Up".into(),
            ports: vec![],
            compose_project: Some("proj".into()),
            compose_service: Some("web".into()),
        };
        assert_eq!(Group::member_key(&c), "compose:proj/web");
        c.compose_service = None;
        c.compose_project = None;
        assert_eq!(Group::member_key(&c), "name:proj-web-1");
    }
}
