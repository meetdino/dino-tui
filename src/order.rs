//! The list's order: sessions grouped by the repo (or folder) they run in, as dinod's `Tree` has
//! it; what needs you first, then working, done, idle and ended, as `dino ls` sorts them. A group
//! comes as early as its most urgent session; pinned sessions stay at the top of their group.

use dino_core::ipc::{RepoInfo, SessionInfo};
use dino_core::status::Status;

use crate::format;

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    /// The repo's main checkout, or the folder; `host:path` over SSH.
    pub path: String,
    /// Indices into the sessions, in order.
    pub sessions: Vec<usize>,
}

/// Sessions in the order dinod numbered them: `10` after `9`.
pub fn id_order(id: &str) -> (usize, String) {
    (id.parse().unwrap_or(usize::MAX), id.to_string())
}

/// The repo `cwd` is in (its main checkout or one of its worktrees, the deepest that holds it).
fn repo_of<'a>(cwd: &str, tree: &'a [RepoInfo]) -> Option<&'a RepoInfo> {
    let inside = |p: &str| !p.is_empty() && (cwd == p || cwd.strip_prefix(p).is_some_and(|rest| rest.starts_with('/')));
    tree.iter()
        .filter_map(|r| {
            let deepest = std::iter::once(r.path.as_str()).chain(r.worktrees.iter().map(|w| w.path.as_str())).filter(|p| inside(p)).map(str::len).max()?;
            Some((deepest, r))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, r)| r)
}

/// What group a session goes in: (key, name, path).
fn group_of(s: &SessionInfo, tree: &[RepoInfo]) -> (String, String, String) {
    if let Some(h) = &s.host {
        let path = format!("{h}:{}", s.cwd);
        return (path.clone(), format::printable(&format!("{h}:{}", base(&s.cwd))), path);
    }
    match repo_of(&s.cwd, tree) {
        Some(r) => (r.path.clone(), format::printable(&r.name), r.path.clone()),
        None => (s.cwd.clone(), format::printable(base(&s.cwd)), s.cwd.clone()),
    }
}

fn base(p: &str) -> &str {
    let p = p.trim_end_matches('/');
    match p.rsplit('/').next() {
        Some(b) if !b.is_empty() => b,
        _ => "/",
    }
}

/// Whether `s` matches a `/` filter: its name, folder, agent or group, any case.
pub fn matches(s: &SessionInfo, group: &str, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    [format::name(s), format::folder(s), s.cwd.clone(), format::agent(s).to_string(), group.to_string()].iter().any(|t| t.to_lowercase().contains(&f))
}

pub fn groups(sessions: &[SessionInfo], tree: &[RepoInfo], filter: &str) -> Vec<Group> {
    let mut out: Vec<(Group, Status)> = vec![];
    for (i, s) in sessions.iter().enumerate() {
        let (key, name, path) = group_of(s, tree);
        if !matches(s, &name, filter) {
            continue;
        }
        match out.iter_mut().find(|(g, _)| g.path == key) {
            Some((g, best)) => {
                g.sessions.push(i);
                *best = (*best).min(Status::of(s));
            }
            None => out.push((Group { name, path, sessions: vec![i] }, Status::of(s))),
        }
    }
    for (g, _) in &mut out {
        g.sessions.sort_by_key(|&i| {
            let s = &sessions[i];
            (!s.pinned, Status::of(s), id_order(&s.id))
        });
    }
    out.sort_by(|(a, sa), (b, sb)| sa.cmp(sb).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.path.cmp(&b.path)));
    out.into_iter().map(|(g, _)| g).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, cwd: &str, activity: Option<&str>, pinned: bool) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": format!("claude-{id}"), "agent_id": "claude", "title": null, "exited": false,
            "output_ms_ago": 60_000, "bells": 0, "requests": 0, "in_flight": 0, "input_tokens": 0,
            "output_tokens": 0, "last_model": null, "tier": null, "activity": activity, "cwd": cwd, "pinned": pinned,
        }))
        .unwrap()
    }

    fn repo(path: &str, worktrees: &[&str]) -> RepoInfo {
        serde_json::from_value(serde_json::json!({
            "path": path, "name": base(path),
            "worktrees": worktrees.iter().map(|w| serde_json::json!({"path": w, "branch": "b"})).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn grouped_by_repo_with_what_needs_you_first() {
        let tree = [repo("/r/app", &["/w/app-wt1"]), repo("/r/site", &[])];
        let sessions = [
            s("1", "/r/site", None, false),
            s("2", "/r/app/src", Some("done"), false),
            s("3", "/w/app-wt1", Some("needs:Run: ls?"), false),
            s("4", "/tmp/x", Some("working"), false),
            s("5", "/r/app", None, true),
        ];
        let g = groups(&sessions, &tree, "");
        let names: Vec<_> = g.iter().map(|g| g.name.as_str()).collect();
        // app has the session that needs you, x one working, site only idle ones.
        assert_eq!(names, ["app", "x", "site"]);
        // Pinned first, then by status.
        let ids: Vec<_> = g[0].sessions.iter().map(|&i| sessions[i].id.as_str()).collect();
        assert_eq!(ids, ["5", "3", "2"]);
    }

    #[test]
    fn a_filter_keeps_matching_sessions_and_their_groups() {
        let sessions = [s("1", "/r/site", None, false), s("2", "/r/app", None, false)];
        let g = groups(&sessions, &[], "SITE");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].sessions, [0]);
        assert!(groups(&sessions, &[], "claude-2").iter().all(|g| g.sessions == [1]));
    }

    #[test]
    fn ids_sort_as_numbers() {
        assert!(id_order("9") < id_order("10"));
    }
}
