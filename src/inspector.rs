use crate::model::{
    Conversation, InspectorTab, SessionTelemetry, ToolState, ToolTrace, format_compact_tokens,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectorAction {
    Close,
    SelectTab(InspectorTab),
    AskSide(String),
    InsertDraft(String),
    JumpToMessage(usize),
    BranchAt(usize),
    CompressContext,
}

pub fn can_compress_conversation(conversation: Option<&Conversation>, writable: bool) -> bool {
    writable
        && conversation
            .is_some_and(|chat| !chat.status.busy() && !chat.rewinding && chat.messages.len() >= 2)
}

pub fn telemetry_metric_cells(t: &SessionTelemetry) -> Vec<(&'static str, String)> {
    let speed = t
        .tokens_per_sec
        .map_or_else(|| "—".into(), |tps| format!("{tps:.0} tok/s"));
    let extra = metric_extra(t);
    vec![
        ("Input", format_compact_tokens(t.input_tokens)),
        ("Output", format_compact_tokens(t.output_tokens)),
        ("Cached", format_compact_tokens(t.cache_read_tokens)),
        ("Reasoning", format_compact_tokens(t.reasoning_tokens)),
        ("Speed", speed),
        extra,
    ]
}

fn metric_extra(t: &SessionTelemetry) -> (&'static str, String) {
    if let Some(cost) = t.cost_usd.filter(|&cost| cost > 0.0) {
        ("Cost", format!("${cost:.3}"))
    } else {
        ("API Calls", t.api_calls.to_string())
    }
}

pub fn tool_badge_spec(trace: &ToolTrace) -> (String, &'static str) {
    match &trace.state {
        ToolState::Running => (running_badge(trace.progress_pct), "side-badge-pending"),
        ToolState::Finished => (finished_badge(trace.duration_s), "side-badge-ok"),
        ToolState::RiskFlagged(_) => ("RISK".into(), "side-badge-err"),
    }
}

fn running_badge(percent: Option<u8>) -> String {
    percent.map_or_else(|| "RUN".into(), |pct| format!("RUN {pct}%"))
}

fn finished_badge(duration_s: Option<f64>) -> String {
    duration_s.map_or_else(|| "DONE".into(), |seconds| format!("{seconds:.1}s"))
}

pub fn tool_subtitle_text(trace: &ToolTrace) -> String {
    if let ToolState::RiskFlagged(risk) = &trace.state {
        return flagged_subtitle(risk, &trace.detail);
    }
    trace.detail.clone()
}

fn flagged_subtitle(risk: &str, detail: &str) -> String {
    if detail.is_empty() {
        format!("Flagged: {risk}")
    } else {
        format!("Flagged ({risk}) · {detail}")
    }
}

pub fn tab_badge_label(base: &str, count: usize) -> String {
    if count == 0 {
        base.to_string()
    } else {
        format!("{base} · {count}")
    }
}

pub fn workspace_short_badge(t: &SessionTelemetry) -> String {
    if !t.branch.is_empty() {
        return format!("⎇ {}", t.branch);
    }
    t.cwd
        .rsplit('/')
        .find(|seg| !seg.is_empty())
        .map_or_else(String::new, str::to_string)
}

pub fn workspace_detail_line(t: &SessionTelemetry) -> String {
    match (t.branch.is_empty(), t.cwd.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("Branch: {}", t.branch),
        (true, false) => t.cwd.clone(),
        (false, false) => format!("⎇ {} · {}", t.branch, t.cwd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_inspector_badges_and_telemetry_cells_format_cleanly() {
        assert_eq!(tab_badge_label("Side-Ask", 0), "Side-Ask");
        assert_eq!(tab_badge_label("Side-Ask", 3), "Side-Ask · 3");

        let mut telemetry = SessionTelemetry {
            input_tokens: 12_500,
            output_tokens: 900,
            cache_read_tokens: 10_000,
            reasoning_tokens: 400,
            tokens_per_sec: Some(78.4),
            api_calls: 4,
            cwd: "/srv/hermes/app".into(),
            branch: "main".into(),
            ..Default::default()
        };
        assert_eq!(workspace_short_badge(&telemetry), "⎇ main");
        assert_eq!(
            workspace_detail_line(&telemetry),
            "⎇ main · /srv/hermes/app"
        );
        telemetry.branch.clear();
        assert_eq!(workspace_short_badge(&telemetry), "app");

        let cells = telemetry_metric_cells(&telemetry);
        assert_eq!(cells.len(), 6);
        assert_eq!(cells[0], ("Input", "12.5k".into()));
        assert_eq!(cells[4], ("Speed", "78 tok/s".into()));
        assert_eq!(cells[5], ("API Calls", "4".into()));

        telemetry.cost_usd = Some(0.125);
        let with_cost = telemetry_metric_cells(&telemetry);
        assert_eq!(with_cost[5], ("Cost", "$0.125".into()));

        let running = ToolTrace {
            tool_id: "1".into(),
            name: "terminal".into(),
            detail: "ls".into(),
            progress_pct: Some(40),
            duration_s: None,
            state: ToolState::Running,
        };
        assert_eq!(tool_badge_spec(&running).0, "RUN 40%");
        assert_eq!(tool_subtitle_text(&running), "ls");
    }
}
