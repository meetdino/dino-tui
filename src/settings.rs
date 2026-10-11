//! `:settings`: dinod's settings document in `$EDITOR`, as TOML (the form dino keeps it in), and
//! back to dinod on save. dinod validates it; an error opens the editor again with the message at
//! the top. The document goes both ways as plain JSON, not through dino-core's `Settings`: a
//! newer dinod's settings this dino-tui doesn't know come back as they went.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde_json::Value;

const HEAD: &str =
    "# dino's settings, from dinod. Save and quit to apply them; quit without saving to leave them as they are.\n# What each one does: https://github.com/meetdino/dino/blob/main/docs/settings.md\n";

/// The settings as TOML, with what to know at the top: `locked` paths an organization sets, and
/// `error`, why the last save didn't take.
pub fn document(settings: &Value, locked: &[String], error: Option<&str>) -> anyhow::Result<String> {
    let table = to_toml(settings).ok_or_else(|| anyhow::anyhow!("dinod's settings aren't a table"))?;
    let mut head = HEAD.to_string();
    if !locked.is_empty() {
        head += &format!("# Set by your organization, so changes to them don't take: {}\n", locked.join(", "));
    }
    if let Some(e) = error {
        head += "#\n";
        for line in e.lines() {
            head += &format!("# ERROR: {line}\n");
        }
    }
    Ok(format!("{head}\n{}", toml::to_string_pretty(&table)?))
}

/// The edited document as dinod's JSON: comments go, as TOML has it.
pub fn parse(text: &str) -> Result<Value, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string().trim().to_string())?;
    serde_json::to_value(table).map_err(|e| e.to_string())
}

/// JSON as TOML: nulls (absent in TOML) go.
fn to_toml(v: &Value) -> Option<toml::Value> {
    Some(match v {
        Value::Null => return None,
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => toml::Value::Integer(i),
            (None, Some(f)) => toml::Value::Float(f),
            _ => return None,
        },
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Array(a) => toml::Value::Array(a.iter().filter_map(to_toml).collect()),
        Value::Object(o) => toml::Value::Table(o.iter().filter_map(|(k, v)| Some((k.clone(), to_toml(v)?))).collect()),
    })
}

/// Edit `path` in the user's editor; false when it couldn't run.
fn run_editor(path: &Path) -> anyhow::Result<()> {
    let editor = std::env::var("VISUAL").ok().filter(|e| !e.trim().is_empty()).or_else(|| std::env::var("EDITOR").ok().filter(|e| !e.trim().is_empty())).unwrap_or_else(|| "vi".into());
    // Through sh, so `EDITOR="code -w"` works.
    let status = std::process::Command::new("/bin/sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("sh").arg(path).status()?;
    anyhow::ensure!(status.success(), "{editor} exited with {status}");
    Ok(())
}

/// The whole round: ask, edit, save, and again while dinod says no. What to say at the end.
/// `ask` sends a request as plain JSON.
pub fn edit(ask: &mut dyn FnMut(&Value) -> std::io::Result<Value>) -> anyhow::Result<String> {
    let got = ask(&serde_json::json!({"type": "settings"}))?;
    if let Some(e) = crate::conn::raw_error(&got) {
        anyhow::bail!("dinod: {e}");
    }
    let locked: Vec<String> = got["locked"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    // dinod's own file doesn't parse: say so first; saving is refused until it's fixed by hand.
    let mut error = got["error"].as_str().map(|e| format!("dinod can't read settings.toml: {e}"));
    let mut text = document(&got["settings"], &locked, error.as_deref())?;
    let dir = std::env::temp_dir();
    let path = dir.join(format!("dino-settings-{}.toml", std::process::id()));
    let result = (|| loop {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
        f.write_all(text.as_bytes())?;
        drop(f);
        run_editor(&path)?;
        let edited = std::fs::read_to_string(&path)?;
        if edited == text {
            return Ok(if error.is_some() { "settings not saved".to_string() } else { "settings unchanged".to_string() });
        }
        // What the user wrote, without the head this put on it.
        let body: String = edited.lines().skip_while(|l| l.starts_with('#') || l.trim().is_empty()).map(|l| format!("{l}\n")).collect();
        let reopen = |e: String, body: &str| -> anyhow::Result<String> {
            let mut head = HEAD.to_string();
            if !locked.is_empty() {
                head += &format!("# Set by your organization, so changes to them don't take: {}\n", locked.join(", "));
            }
            head += "#\n";
            for line in e.lines() {
                head += &format!("# ERROR: {line}\n");
            }
            Ok(format!("{head}\n{body}"))
        };
        match parse(&edited) {
            Err(e) => {
                error = Some(e.clone());
                text = reopen(e, &body)?;
            }
            Ok(settings) => {
                let said = ask(&serde_json::json!({"type": "set_settings", "settings": settings}))?;
                match crate::conn::raw_error(&said) {
                    None => return Ok("settings saved".to_string()),
                    Some(e) => {
                        error = Some(e.clone());
                        text = reopen(e, &body)?;
                    }
                }
            }
        }
    })();
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_to_toml_and_back() {
        let settings = serde_json::json!({
            "machine": {"onboarded": true, "lid": {"enabled": false, "max_hours": 2.5}, "nothing": null},
            "agents": {"claude": {"mode": "plan", "model": null}},
            "repos": {"/Users/x/my repo": {"setup": ["npm i", "make"]}},
            "terminal": {"start_with": "shell"},
            "experimental": {"free_models": false, "something_newer": {"x": 1}},
        });
        let doc = document(&settings, &["policies.allow_bypass".into()], Some("bad value\nline two")).unwrap();
        assert!(doc.contains("# Set by your organization, so changes to them don't take: policies.allow_bypass"));
        assert!(doc.contains("# ERROR: bad value\n# ERROR: line two"));
        assert!(doc.contains("[repos.\"/Users/x/my repo\"]"), "{doc}");
        let back = parse(&doc).unwrap();
        // Nulls are gone, the rest (a newer dinod's field too) as it was.
        assert_eq!(back["machine"]["lid"]["max_hours"], 2.5);
        assert_eq!(back["agents"]["claude"], serde_json::json!({"mode": "plan"}));
        assert_eq!(back["experimental"]["something_newer"]["x"], 1);
        assert!(back["machine"].get("nothing").is_none());
        assert!(parse("[machine\nx = ").is_err());
    }
}
