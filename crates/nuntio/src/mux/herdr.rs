//! herdr's JSON, as its CLI and socket print it. The parsers are lenient:
//! entries that lack a field they need are skipped, unknown fields ignored.

use serde_json::Value;

/// A herdr session (`herdr session list --json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrSession {
    pub name: String,
    pub default: bool,
    pub running: bool,
    /// The path of the session's API socket.
    pub socket: String,
}

/// A saved SSH machine (`herdr machine list --json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub id: String,
    pub label: String,
    pub target: String,
    /// The session herdr uses on the machine.
    pub session: String,
}

/// What an agent in a herdr pane is doing. The order is how much attention
/// it wants: the greatest needs it most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentStatus {
    Idle,
    Unknown,
    Working,
    Done,
    Blocked,
}

impl AgentStatus {
    fn parse(word: &str) -> Self {
        match word {
            "idle" => AgentStatus::Idle,
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        }
    }

    /// The word herdr uses for it.
    pub fn word(self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Unknown => "unknown",
            AgentStatus::Working => "working",
            AgentStatus::Done => "done",
            AgentStatus::Blocked => "blocked",
        }
    }
}

/// An agent running in a herdr pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub pane_id: String,
    pub workspace_id: String,
    /// The agent's name, e.g. `omp` or `claude`.
    pub agent: String,
    pub cwd: String,
    pub status: AgentStatus,
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

/// The JSON in `text`: the whole text, or else its first line that is JSON
/// (a socket answer is one line).
fn document(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok().or_else(|| {
        text.lines()
            .find_map(|line| serde_json::from_str(line).ok())
    })
}

/// `result.<key>` of a CLI output or socket response, as an array.
fn result_list(text: &str, key: &str) -> Vec<Value> {
    let Some(doc) = document(text) else {
        return Vec::new();
    };
    doc.pointer(&format!("/result/{key}"))
        .or_else(|| doc.get(key))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `herdr session list --json`.
pub fn parse_sessions(json: &str) -> Vec<HerdrSession> {
    let Some(doc) = document(json) else {
        return Vec::new();
    };
    let list = doc
        .get("sessions")
        .and_then(Value::as_array)
        .or_else(|| doc.as_array());
    list.into_iter()
        .flatten()
        .filter_map(|s| {
            Some(HerdrSession {
                name: string(s, "name")?,
                default: s.get("default").and_then(Value::as_bool).unwrap_or(false),
                running: s.get("running").and_then(Value::as_bool).unwrap_or(false),
                socket: string(s, "socket_path").unwrap_or_default(),
            })
        })
        .collect()
}

/// `herdr machine list --json`: the enabled machines.
pub fn parse_machines(json: &str) -> Vec<Machine> {
    let Some(doc) = document(json) else {
        return Vec::new();
    };
    doc.as_array()
        .into_iter()
        .flatten()
        .filter(|m| m.get("enabled").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|m| {
            let target = string(m, "target")?;
            Some(Machine {
                id: string(m, "id")?,
                label: string(m, "label").unwrap_or_else(|| target.clone()),
                session: string(m, "session").unwrap_or_else(|| "default".to_owned()),
                target,
            })
        })
        .collect()
}

/// `herdr agent list` or the socket's answer to `agent.list`.
pub fn parse_agents(json: &str) -> Vec<Agent> {
    result_list(json, "agents")
        .iter()
        .filter_map(|a| {
            Some(Agent {
                pane_id: string(a, "pane_id")?,
                workspace_id: string(a, "workspace_id").unwrap_or_default(),
                agent: string(a, "agent").unwrap_or_default(),
                cwd: string(a, "cwd").unwrap_or_default(),
                status: AgentStatus::parse(
                    a.get("agent_status").and_then(Value::as_str).unwrap_or(""),
                ),
            })
        })
        .collect()
}

/// `herdr pane list`: the workspace and working directory of each pane.
pub fn parse_pane_workspaces(json: &str) -> Vec<(String, String)> {
    result_list(json, "panes")
        .iter()
        .filter_map(|p| Some((string(p, "workspace_id")?, string(p, "cwd")?)))
        .collect()
}

/// `herdr workspace list`: id and label of each workspace.
pub fn parse_workspaces(json: &str) -> Vec<(String, String)> {
    result_list(json, "workspaces")
        .iter()
        .filter_map(|w| {
            let id = string(w, "workspace_id")?;
            let label = string(w, "label").unwrap_or_default();
            Some((id, label))
        })
        .collect()
}

/// The id of the workspace `herdr workspace create` made.
pub fn parse_created_workspace(json: &str) -> Option<String> {
    document(json)?
        .pointer("/result/workspace/workspace_id")?
        .as_str()
        .map(str::to_owned)
}

/// `"1 blocked · 2 working"`: how many agents need attention, none if no
/// agent is blocked, done or working.
pub fn summary(agents: &[Agent]) -> Option<String> {
    let parts: Vec<String> = [
        AgentStatus::Blocked,
        AgentStatus::Done,
        AgentStatus::Working,
    ]
    .into_iter()
    .filter_map(|status| {
        let n = agents.iter().filter(|a| a.status == status).count();
        (n > 0).then(|| format!("{n} {}", status.word()))
    })
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSIONS: &str = r#"{"sessions":[{"default":true,"name":"default","running":true,"session_dir":"/home/felix/.config/herdr","socket_path":"/home/felix/.config/herdr/herdr.sock"},{"default":false,"name":"nv","running":false,"session_dir":"/x/nv","socket_path":"/x/nv/herdr.sock"}]}"#;

    const MACHINES: &str = r#"[
  {"id": "df3b", "label": "stkn.org", "target": "stkn.org", "session": "default", "enabled": true, "selected": false},
  {"id": "9b34", "label": "off", "target": "off.example", "session": "work", "enabled": false, "selected": false}
]"#;

    const AGENTS: &str = r#"{"id":"cli:agent:list","result":{"agents":[
{"agent":"omp","agent_status":"working","cwd":"/home/felix/code/nuntio","focused":true,"pane_id":"w8:p1","tab_id":"w8:t1","workspace_id":"w8"},
{"agent":"claude","agent_status":"blocked","cwd":"/tmp/x","pane_id":"w9:p2","tab_id":"w9:t1","workspace_id":"w9"},
{"agent":"codex","agent_status":"strange","cwd":"/tmp/y","pane_id":"w9:p3","workspace_id":"w9"},
{"agent_status":"idle"}]}}"#;

    #[test]
    fn sessions() {
        let sessions = parse_sessions(SESSIONS);
        assert_eq!(
            sessions,
            [
                HerdrSession {
                    name: "default".into(),
                    default: true,
                    running: true,
                    socket: "/home/felix/.config/herdr/herdr.sock".into(),
                },
                HerdrSession {
                    name: "nv".into(),
                    default: false,
                    running: false,
                    socket: "/x/nv/herdr.sock".into(),
                },
            ]
        );
        assert!(parse_sessions("not json").is_empty());
        assert!(parse_sessions("").is_empty());
    }

    #[test]
    fn only_enabled_machines() {
        assert_eq!(
            parse_machines(MACHINES),
            [Machine {
                id: "df3b".into(),
                label: "stkn.org".into(),
                target: "stkn.org".into(),
                session: "default".into(),
            }]
        );
        assert!(parse_machines("{}").is_empty());
    }

    #[test]
    fn agents_from_the_cli_and_from_the_socket() {
        let agents = parse_agents(AGENTS);
        assert_eq!(agents.len(), 3);
        assert_eq!(
            agents[0],
            Agent {
                pane_id: "w8:p1".into(),
                workspace_id: "w8".into(),
                agent: "omp".into(),
                cwd: "/home/felix/code/nuntio".into(),
                status: AgentStatus::Working,
            }
        );
        assert_eq!(agents[1].status, AgentStatus::Blocked);
        assert_eq!(agents[2].status, AgentStatus::Unknown);

        let line = r#"{"id":"list","result":{"agents":[{"agent":"omp","agent_status":"done","cwd":"/a","pane_id":"p","workspace_id":"w"}]}}
"#;
        let agents = parse_agents(line);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, AgentStatus::Done);

        assert!(parse_agents(r#"{"id":"list","error":{"code":"x"}}"#).is_empty());
        assert!(parse_agents("").is_empty());
    }

    #[test]
    fn status_order_is_the_attention_order() {
        use AgentStatus::*;
        assert!(Idle < Unknown && Unknown < Working && Working < Done && Done < Blocked);
        assert_eq!(Blocked.word(), "blocked");
        assert_eq!(Unknown.word(), "unknown");
    }

    #[test]
    fn panes_and_workspaces() {
        let panes = r#"{"id":"cli:pane:list","result":{"panes":[{"agent_status":"idle","cwd":"/a/b","pane_id":"w1:p1","workspace_id":"w1"},{"cwd":"/c","pane_id":"w2:p1","workspace_id":"w2"},{"pane_id":"w3:p1"}]}}"#;
        assert_eq!(
            parse_pane_workspaces(panes),
            [
                ("w1".to_owned(), "/a/b".to_owned()),
                ("w2".to_owned(), "/c".to_owned())
            ]
        );
        let workspaces = r#"{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[{"focused":true,"label":"nuntio","number":1,"workspace_id":"w8"},{"label":"two","workspace_id":"w9"}]}}"#;
        assert_eq!(
            parse_workspaces(workspaces),
            [
                ("w8".to_owned(), "nuntio".to_owned()),
                ("w9".to_owned(), "two".to_owned())
            ]
        );
    }

    #[test]
    fn created_workspace() {
        let created = r#"{"id":"cli:workspace:create","result":{"type":"workspace_created","workspace":{"workspace_id":"w10","label":"x"}}}"#;
        assert_eq!(parse_created_workspace(created).as_deref(), Some("w10"));
        assert_eq!(parse_created_workspace(r#"{"error":{}}"#), None);
        assert_eq!(parse_created_workspace(""), None);
    }

    fn agent(status: AgentStatus) -> Agent {
        Agent {
            pane_id: "p".into(),
            workspace_id: "w".into(),
            agent: "a".into(),
            cwd: "/".into(),
            status,
        }
    }

    #[test]
    fn summaries() {
        use AgentStatus::*;
        assert_eq!(summary(&[]), None);
        assert_eq!(summary(&[agent(Idle), agent(Unknown)]), None);
        assert_eq!(
            summary(&[agent(Working), agent(Blocked), agent(Working)]).as_deref(),
            Some("1 blocked · 2 working")
        );
        assert_eq!(
            summary(&[agent(Working), agent(Done), agent(Blocked)]).as_deref(),
            Some("1 blocked · 1 done · 1 working")
        );
    }
}
