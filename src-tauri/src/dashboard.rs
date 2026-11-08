//! Assembles the dashboard: one card per AI tool, in registry order.

use agentpulse_collectors::gemini_cli::TelemetryState;
use agentpulse_core::dto::{
    AgentCardDto, CollectorCardDto, CollectorStatusDto, DashboardDto, ModelUsageDto, OverviewDto,
};
use agentpulse_core::ids::AgentId;
use agentpulse_core::registry::AGENTS;
use agentpulse_core::time::UnixMs;
use agentpulse_engine::CollectorInfo;

pub fn build(
    now: UnixMs,
    overview: OverviewDto,
    models: Vec<(AgentId, ModelUsageDto)>,
    statuses: Vec<CollectorStatusDto>,
    collectors: Vec<CollectorInfo>,
    source_notes: &[(AgentId, String)],
) -> DashboardDto {
    let mut agents = Vec::with_capacity(AGENTS.len());
    for spec in AGENTS {
        let info = collectors.iter().find(|c| c.agent == spec.agent);
        // The mock card exists only when a mock collector is registered (debug builds).
        if spec.agent == AgentId::Mock && info.is_none() {
            continue;
        }
        let collector = info.map(|c| CollectorCardDto {
            collector_id: c.collector_id.clone(),
            enabled: c.enabled,
            status: statuses.iter().find(|s| s.collector_id == c.collector_id).cloned(),
        });
        let planned_note = match collector {
            Some(_) => None,
            None => planned_note(spec.agent).map(str::to_owned),
        };
        agents.push(AgentCardDto {
            agent: spec.agent,
            display_name: spec.display_name.to_owned(),
            provider: spec.provider,
            collector,
            planned_note,
            source_note: source_notes.iter().find(|(a, _)| *a == spec.agent).map(|(_, n)| n.clone()),
            accounts: overview.accounts.iter().filter(|a| a.agents.contains(&spec.agent)).cloned().collect(),
            models_today: models.iter().filter(|(a, _)| *a == spec.agent).map(|(_, m)| m.clone()).collect(),
        });
    }
    DashboardDto { generated_at: now, agents }
}

/// Gemini CLI's telemetry, as the user set it. AgentPulse never changes it, and
/// tokens come from the session logs either way (docs/providers/gemini_cli.md).
pub fn gemini_telemetry_note(t: TelemetryState) -> &'static str {
    match t {
        TelemetryState::Off => "Telemetry is off in Gemini CLI. It isn't needed: tokens come from its session logs.",
        TelemetryState::LocalFile => {
            "Telemetry is on in Gemini CLI, but AgentPulse doesn't read it yet. Tokens come from its session logs."
        }
        TelemetryState::Other => {
            "Gemini CLI sends telemetry elsewhere; AgentPulse doesn't read it. Tokens come from its session logs."
        }
    }
}

/// What Antigravity's card says about its data (docs/providers/antigravity.md).
pub const ANTIGRAVITY_NOTE: &str = "Tokens come from Antigravity's conversation files. Quota isn't available: Antigravity shows it only in its own app. Conversations saved only in its older encrypted format aren't counted.";

/// Roadmap position of collectors not yet in this build (architecture §11).
fn planned_note(agent: AgentId) -> Option<&'static str> {
    Some(match agent {
        // Order approved after M2 (docs/providers/README.md).
        AgentId::GeminiCli => "Collector planned for M5.",
        AgentId::Antigravity => "Collector planned for M6.",
        AgentId::ClaudeCode => "Collector planned for M4.",
        AgentId::Codex => "Collector planned for M3.",
        AgentId::Mock => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, agent: AgentId, enabled: bool) -> CollectorInfo {
        CollectorInfo { collector_id: id.into(), agent, enabled }
    }

    #[test]
    fn every_real_tool_gets_a_card_and_missing_collectors_say_so() {
        let d = build(0, OverviewDto { generated_at: 0, accounts: vec![] }, vec![], vec![], vec![], &[]);
        let names: Vec<_> = d.agents.iter().map(|a| a.agent).collect();
        assert_eq!(names, vec![AgentId::ClaudeCode, AgentId::Codex, AgentId::Antigravity, AgentId::GeminiCli]);
        assert!(d.agents.iter().all(|a| a.collector.is_none() && a.planned_note.is_some()));
    }

    #[test]
    fn mock_card_appears_only_with_a_mock_collector() {
        let d = build(
            0,
            OverviewDto { generated_at: 0, accounts: vec![] },
            vec![(AgentId::Mock, ModelUsageDto { model: "m".into(), input: 1, output: 2, records: 1 })],
            vec![],
            vec![info("mock-healthy", AgentId::Mock, true)],
            &[],
        );
        let mock = d.agents.iter().find(|a| a.agent == AgentId::Mock).unwrap();
        assert!(mock.collector.as_ref().unwrap().enabled);
        assert!(mock.planned_note.is_none());
        assert_eq!(mock.models_today.len(), 1);
    }

    #[test]
    fn gemini_card_carries_its_source_note_and_no_planned_note() {
        let note = gemini_telemetry_note(TelemetryState::Off).to_owned();
        let d = build(
            0,
            OverviewDto { generated_at: 0, accounts: vec![] },
            vec![],
            vec![],
            vec![info("gemini-cli", AgentId::GeminiCli, true)],
            &[(AgentId::GeminiCli, note.clone())],
        );
        let g = d.agents.iter().find(|a| a.agent == AgentId::GeminiCli).unwrap();
        assert!(g.planned_note.is_none());
        assert_eq!(g.source_note.as_deref(), Some(note.as_str()));
        assert!(d.agents.iter().filter(|a| a.agent != AgentId::GeminiCli).all(|a| a.source_note.is_none()));
    }
}
