use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::types::{AgentConfig, TeamConfig};

/// Find the first team that contains the given agent.
pub fn find_team_for_agent<'a>(
    agent_id: &str,
    teams: &'a HashMap<String, TeamConfig>,
) -> Option<(&'a String, &'a TeamConfig)> {
    teams
        .iter()
        .find(|(_, team)| team.agents.iter().any(|a| a == agent_id))
}

/// Check if a mentioned ID is a valid teammate of the current agent in the given team.
pub fn is_teammate(
    mentioned_id: &str,
    current_agent_id: &str,
    team_id: &str,
    teams: &HashMap<String, TeamConfig>,
    agents: &HashMap<String, AgentConfig>,
) -> bool {
    let Some(team) = teams.get(team_id) else {
        return false;
    };
    mentioned_id != current_agent_id
        && team.agents.iter().any(|a| a == mentioned_id)
        && agents.contains_key(mentioned_id)
}

/// A teammate mention extracted from a response.
pub struct TeammateMention {
    pub teammate_id: String,
    pub message: String,
}

/// Extract `[@agent_id: message]` teammate mentions from a response.
pub fn extract_teammate_mentions(
    response: &str,
    current_agent_id: &str,
    team_id: &str,
    teams: &HashMap<String, TeamConfig>,
    agents: &HashMap<String, AgentConfig>,
) -> Vec<TeammateMention> {
    let re = Regex::new(r"\[@(\S+?):\s*([\s\S]*?)\]").unwrap();
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for caps in re.captures_iter(response) {
        let candidate_id = caps[1].to_lowercase();
        if !seen.contains(&candidate_id)
            && is_teammate(&candidate_id, current_agent_id, team_id, teams, agents)
        {
            results.push(TeammateMention {
                teammate_id: candidate_id.clone(),
                message: caps[2].trim().to_string(),
            });
            seen.insert(candidate_id);
        }
    }
    results
}

/// Get the reset flag path for a specific agent.
pub fn agent_reset_flag(agent_id: &str, workspace_path: &Path) -> PathBuf {
    workspace_path.join(agent_id).join("reset_flag")
}

/// Detect if message mentions multiple agents (across teams).
pub fn detect_multiple_agents(
    message: &str,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
) -> Vec<String> {
    let re = Regex::new(r"@(\S+)").unwrap();
    let mut valid_agents: Vec<String> = Vec::new();

    for caps in re.captures_iter(message) {
        let agent_id = caps[1].to_lowercase();
        if agents.contains_key(&agent_id) && !valid_agents.contains(&agent_id) {
            valid_agents.push(agent_id);
        }
    }

    // If all agents are in the same team, don't trigger the easter egg
    if valid_agents.len() > 1 {
        for team in teams.values() {
            if valid_agents.iter().all(|a| team.agents.contains(a)) {
                return vec![];
            }
        }
    }

    valid_agents
}

/// Parse result from agent routing.
pub struct RoutingResult {
    pub agent_id: String,
    pub message: String,
    pub is_team: bool,
}

/// Parse @agent_id or @team_id prefix from a message.
pub fn parse_agent_routing(
    raw_message: &str,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
) -> RoutingResult {
    // Easter egg: check for multiple agent mentions across teams
    let mentioned = detect_multiple_agents(raw_message, agents, teams);
    if mentioned.len() > 1 {
        let agent_list: Vec<String> = mentioned.iter().map(|t| format!("@{t}")).collect();
        let agent_list_str = agent_list.join(", ");

        let usage: Vec<String> = mentioned
            .iter()
            .map(|t| format!("• `@{t} [your message]`"))
            .collect();

        let message = format!(
            "🚀 **Agent-to-Agent Collaboration - Coming Soon!**\n\n\
             You mentioned multiple agents: {agent_list_str}\n\n\
             Right now, I can only route to one agent at a time. But we're working on something cool:\n\n\
             ✨ **Multi-Agent Coordination** - Agents will be able to collaborate on complex tasks!\n\
             ✨ **Smart Routing** - Send instructions to multiple agents at once!\n\
             ✨ **Agent Handoffs** - One agent can delegate to another!\n\n\
             For now, please send separate messages to each agent:\n\
             {}\n\n\
             _Stay tuned for updates! 🎉_",
            usage.join("\n")
        );

        return RoutingResult {
            agent_id: "error".into(),
            message,
            is_team: false,
        };
    }

    // Try to match @agent_id or @team_id prefix
    let re = Regex::new(r"^@(\S+)\s+([\s\S]*)$").unwrap();
    if let Some(caps) = re.captures(raw_message) {
        let candidate_id = caps[1].to_lowercase();
        let rest = caps[2].to_string();

        // Check agent IDs
        if agents.contains_key(&candidate_id) {
            return RoutingResult {
                agent_id: candidate_id,
                message: rest,
                is_team: false,
            };
        }

        // Check team IDs — resolve to leader agent
        if let Some(team) = teams.get(&candidate_id) {
            return RoutingResult {
                agent_id: team.leader_agent.clone(),
                message: rest,
                is_team: true,
            };
        }

        // Match by agent name (case-insensitive)
        for (id, config) in agents {
            if config.name.to_lowercase() == candidate_id {
                return RoutingResult {
                    agent_id: id.clone(),
                    message: rest.clone(),
                    is_team: false,
                };
            }
        }

        // Match by team name (case-insensitive)
        for config in teams.values() {
            if config.name.to_lowercase() == candidate_id {
                return RoutingResult {
                    agent_id: config.leader_agent.clone(),
                    message: rest.clone(),
                    is_team: true,
                };
            }
        }
    }

    RoutingResult {
        agent_id: "default".into(),
        message: raw_message.to_string(),
        is_team: false,
    }
}
