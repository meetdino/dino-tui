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

/// A group of the sidebar: a place sessions run (a repo's main checkout, one of its worktrees, a
/// folder, an SSH host) or, grouped by state, a status.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    /// The repo's (or folder's) name, or the status.
    pub name: String,
    /// What follows it: where it is (`~/work/app`), or for a worktree its branch.
    pub detail: Detail,
    pub sessions: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Detail {
    Path(String),
    Branch(String),
    Count(usize),
}

/// The sidebar's sections when grouped by project, as the app's sidebar files sessions: each repo's
/// main checkout, then its worktrees (by branch), repos by name; then folders outside any repo
/// (dinod's tree lists those too, with no branch), then SSH hosts. In each, pinned sessions first, then as dinod numbered them: rows stay put while
/// their status changes.
pub fn places(sessions: &[SessionInfo], tree: &[RepoInfo]) -> Vec<Section> {
    // (sort key, section)
    let mut out: Vec<((u8, String, u8, String), Section)> = vec![];
    for (i, s) in sessions.iter().enumerate() {
        let (key, name, detail) = if let Some(h) = &s.host {
            ((3, h.to_lowercase(), 0, s.cwd.clone()), format::printable(&format!("{h}:{}", base(&s.cwd))), Detail::Path(format::printable(&format!("{h}:{}", s.cwd))))
        } else if let Some(r) = repo_of(&s.cwd, tree).filter(|r| r.default_branch.is_some() || !r.worktrees.is_empty()) {
            let inside = |p: &str| !p.is_empty() && (s.cwd == p || s.cwd.strip_prefix(p).is_some_and(|rest| rest.starts_with('/')));
            let wt = r.worktrees.iter().filter(|w| w.path != r.path && inside(&w.path)).max_by_key(|w| w.path.len());
            match wt {
                Some(w) => {
                    let branch = w.branch.clone().unwrap_or_else(|| base(&w.path).to_string());
                    ((1, r.name.to_lowercase(), 1, branch.clone()), format::printable(&r.name), Detail::Branch(format::printable(&branch)))
                }
                None => ((1, r.name.to_lowercase(), 0, r.path.clone()), format::printable(&r.name), Detail::Path(format::short_path(&r.path))),
            }
        } else {
            ((2, base(&s.cwd).to_lowercase(), 0, s.cwd.clone()), format::printable(base(&s.cwd)), Detail::Path(format::short_path(&s.cwd)))
        };
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, sec)) => sec.sessions.push(i),
            None => out.push((key, Section { name, detail, sessions: vec![i] })),
        }
    }
    out.sort_by(|(a, _), (b, _)| a.cmp(b));
    out.into_iter()
        .map(|(_, mut sec)| {
            sec.sessions.sort_by_key(|&i| (!sessions[i].pinned, id_order(&sessions[i].id)));
            sec
        })
        .collect()
}

/// The sidebar's sections when grouped by state, as the app's: Needs you, Working, Done, Idle (an
/// ended session is idle there), each only when it has sessions.
pub fn states(sessions: &[SessionInfo]) -> Vec<Section> {
    let of = |s: &SessionInfo| match Status::of(s) {
        Status::NeedsYou => 0,
        Status::Working => 1,
        Status::Done => 2,
        _ => 3,
    };
    ["Needs you", "Working", "Done", "Idle"]
        .iter()
        .enumerate()
        .filter_map(|(k, name)| {
            let mut ids: Vec<usize> = (0..sessions.len()).filter(|&i| of(&sessions[i]) == k).collect();
            ids.sort_by_key(|&i| (!sessions[i].pinned, id_order(&sessions[i].id)));
            (!ids.is_empty()).then(|| Section { name: name.to_string(), detail: Detail::Count(ids.len()), sessions: ids })
        })
        .collect()
}

/// The branch a session's folder has out, when dinod's tree knows it.
pub fn branch_of(s: &SessionInfo, tree: &[RepoInfo]) -> Option<String> {
    let r = repo_of(&s.cwd, tree)?;
    let inside = |p: &str| !p.is_empty() && (s.cwd == p || s.cwd.strip_prefix(p).is_some_and(|rest| rest.starts_with('/')));
    r.worktrees.iter().filter(|w| inside(&w.path)).max_by_key(|w| w.path.len()).and_then(|w| w.branch.clone())
}

/// The project a session is in, short: its repo's name, else its folder's.
pub fn project_of(s: &SessionInfo, tree: &[RepoInfo]) -> String {
    group_of(s, tree).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_split_worktrees_and_keep_their_order() {
        let tree: Vec<RepoInfo> = serde_json::from_value(serde_json::json!([
            {"path": "/r/app", "name": "app", "worktrees": [{"path": "/r/app", "branch": "main"}, {"path": "/w/app-tabs", "branch": "tabs-compact"}]},
            {"path": "/r/site", "name": "site", "worktrees": [], "default_branch": "main"},
            // A plain folder, as dinod's tree lists one: no branch. It goes with the folders.
            {"path": "/notes", "name": "notes", "worktrees": []},
        ]))
        .unwrap();
        let sessions = [
            s("1", "/r/site", Some("needs:x"), false),
            s("2", "/w/app-tabs", None, false),
            s("3", "/notes", None, false),
            s("4", "/r/app/src", Some("working"), false),
            s("5", "/r/app", None, true),
            s("10", "/r/app", None, false),
        ];
        let p = places(&sessions, &tree);
        let heads: Vec<(&str, &Detail)> = p.iter().map(|g| (g.name.as_str(), &g.detail)).collect();
        assert_eq!(
            heads,
            [("app", &Detail::Path("/r/app".into())), ("app", &Detail::Branch("tabs-compact".into())), ("site", &Detail::Path("/r/site".into())), ("notes", &Detail::Path("/notes".into()))]
        );
        // Pinned first, then by number, whatever their status.
        let ids: Vec<&str> = p[0].sessions.iter().map(|&i| sessions[i].id.as_str()).collect();
        assert_eq!(ids, ["5", "4", "10"]);
        assert_eq!(branch_of(&sessions[1], &tree).as_deref(), Some("tabs-compact"));
        assert_eq!(branch_of(&sessions[3], &tree).as_deref(), Some("main"));
        let st = states(&sessions);
        assert_eq!(st.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["Needs you", "Working", "Idle"]);
        assert_eq!(st[2].detail, Detail::Count(4));
    }

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
