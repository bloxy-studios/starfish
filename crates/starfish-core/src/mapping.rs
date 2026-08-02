//! Model-string → Hyperagent-agent resolution.
//!
//! Resolution order (per MISSION.md §5 and ROADMAP "Mapping engine"):
//!   1. exact agent id match
//!   2. exact agent name match (case-insensitive)
//!   3. mapping table rows (wildcard patterns like `claude-*sonnet*`),
//!      checked per-surface first, then surface-agnostic rows
//!   4. the `hyperagent-default` alias / any unknown model → the default agent
//!
//! Claude Code hard-codes `claude-*` model names, so the mapping table is what
//! makes the Anthropic surface usable.

use serde::{Deserialize, Serialize};

use crate::hyperagent::AgentInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    Openai,
    Anthropic,
}

/// One row of the mapping table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingRule {
    /// Wildcard pattern matched against the client-supplied model string.
    /// `*` matches any run of characters; matching is case-insensitive.
    pub pattern: String,
    /// Which surface this rule applies to; `None` = both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<Surface>,
    /// Agent id the pattern resolves to.
    pub agent_id: String,
}

/// The default-model alias clients can use on either surface.
pub const DEFAULT_MODEL_ALIAS: &str = "hyperagent-default";

/// Simple `*` wildcard matcher, case-insensitive.
pub fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !value.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 && !pattern.ends_with('*') {
            let tail = &value[pos.min(value.len())..];
            if !tail.ends_with(part) || tail.len() < part.len() {
                return false;
            }
            return true;
        } else {
            match value[pos.min(value.len())..].find(part) {
                Some(found) => pos += found + part.len(),
                None => return false,
            }
        }
    }
    true
}

/// Resolve a model string to an agent id.
pub fn resolve_model<'a>(
    model: &str,
    surface: Surface,
    agents: &'a [AgentInfo],
    rules: &'a [MappingRule],
    default_agent_id: Option<&'a str>,
) -> Option<String> {
    let trimmed = model.trim();

    // 1. exact agent id
    if let Some(a) = agents.iter().find(|a| a.id == trimmed) {
        return Some(a.id.clone());
    }
    // 2. exact agent name (case-insensitive)
    if let Some(a) = agents.iter().find(|a| a.name.eq_ignore_ascii_case(trimmed)) {
        return Some(a.id.clone());
    }
    // 3. mapping table: surface-specific rows first, then both-surface rows
    for pass in [Some(surface), None] {
        for rule in rules.iter().filter(|r| r.surface == pass) {
            if wildcard_match(&rule.pattern, trimmed) {
                return Some(rule.agent_id.clone());
            }
        }
    }
    // 4. default alias or fall-through default
    if let Some(d) = default_agent_id {
        return Some(d.to_string());
    }
    // Last resort: single-agent accounts route everything to that agent.
    if agents.len() == 1 {
        return Some(agents[0].id.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> Vec<AgentInfo> {
        vec![
            AgentInfo {
                id: "agent_research".into(),
                name: "Deep Researcher".into(),
                description: Some("does research".into()),
            },
            AgentInfo {
                id: "agent_fast".into(),
                name: "Quick Bot".into(),
                description: None,
            },
        ]
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match(
            "claude-*sonnet*",
            "claude-3-7-sonnet-20250219"
        ));
        assert!(wildcard_match("claude-*sonnet*", "CLAUDE-4-SONNET"));
        assert!(!wildcard_match("claude-*sonnet*", "claude-3-haiku"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("gpt-4*", "gpt-4o-mini"));
        assert!(!wildcard_match("gpt-4*", "gpt-3.5"));
        assert!(wildcard_match("exact", "EXACT"));
        assert!(!wildcard_match("exact", "exactly"));
        assert!(wildcard_match("*mini", "gpt-4o-mini"));
        assert!(!wildcard_match("*mini", "mini-me"));
    }

    #[test]
    fn resolution_order() {
        let agents = agents();
        let rules = vec![
            MappingRule {
                pattern: "claude-*sonnet*".into(),
                surface: Some(Surface::Anthropic),
                agent_id: "agent_research".into(),
            },
            MappingRule {
                pattern: "claude-*haiku*".into(),
                surface: Some(Surface::Anthropic),
                agent_id: "agent_fast".into(),
            },
        ];

        // exact id wins
        assert_eq!(
            resolve_model(
                "agent_fast",
                Surface::Openai,
                &agents,
                &rules,
                Some("agent_research")
            ),
            Some("agent_fast".into())
        );
        // exact name (case-insensitive)
        assert_eq!(
            resolve_model("deep researcher", Surface::Openai, &agents, &rules, None),
            Some("agent_research".into())
        );
        // claude hard-coded names route via table
        assert_eq!(
            resolve_model(
                "claude-sonnet-4-5-20250929",
                Surface::Anthropic,
                &agents,
                &rules,
                Some("agent_fast")
            ),
            Some("agent_research".into())
        );
        assert_eq!(
            resolve_model(
                "claude-3-5-haiku-latest",
                Surface::Anthropic,
                &agents,
                &rules,
                None
            ),
            Some("agent_fast".into())
        );
        // alias/unknown → default
        assert_eq!(
            resolve_model(
                DEFAULT_MODEL_ALIAS,
                Surface::Openai,
                &agents,
                &rules,
                Some("agent_fast")
            ),
            Some("agent_fast".into())
        );
        // unknown with no default but 2 agents → None
        assert_eq!(
            resolve_model("gpt-4o", Surface::Openai, &agents, &rules, None),
            None
        );
    }

    #[test]
    fn single_agent_fallback() {
        let one = vec![AgentInfo {
            id: "only".into(),
            name: "Only".into(),
            description: None,
        }];
        assert_eq!(
            resolve_model("whatever", Surface::Openai, &one, &[], None),
            Some("only".into())
        );
    }
}
