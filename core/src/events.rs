//! 端末間の同期で通知する「変更の影響範囲」。

/// 要求のパスから、変更の影響範囲を決める。
pub fn scopes_for(path: &str) -> Vec<&'static str> {
    let rest = path.strip_prefix("/api/").unwrap_or(path);
    let mut parts = rest.split('/');
    let first = parts.next().unwrap_or("");
    let third = parts.nth(1).unwrap_or("");
    match (first, third) {
        ("elements", _) => vec!["scene"],
        ("scenes", "markdown" | "elements") => vec!["scene"],
        ("scenes", _) | ("chapters", _) => vec!["tree", "scene"],
        ("projects", "files") | ("files", _) => vec!["files"],
        ("projects", "chats") | ("chats", _) => vec!["chat"],
        ("projects", "preferences") | ("preferences", _) => vec!["prefs"],
        ("projects", "snapshots") => vec!["snapshots"],
        ("projects", "variables") | ("variables", _) => vec!["structure"],
        ("projects", "characters" | "character-relations") | ("characters" | "character-facts" | "character-relations", _) => {
            vec!["characters", "structure"]
        }
        ("snapshots", _) => vec!["snapshots", "tree", "scene", "files"],
        // 提案の承認は、場面・ファイル・チャットのすべてに影響しうる。
        ("proposals", _) => vec!["chat", "scene", "files", "history"],
        ("projects", "outline") => vec!["tree", "scene"],
        ("projects", "generate") => vec!["settings"],
        ("projects", _) => vec!["tree", "projects"],
        ("settings" | "usage", _) => vec!["settings"],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_follow_paths() {
        assert_eq!(scopes_for("/api/elements/x"), ["scene"]);
        assert_eq!(scopes_for("/api/scenes/x/markdown"), ["scene"]);
        assert_eq!(scopes_for("/api/scenes/x"), ["tree", "scene"]);
        assert_eq!(scopes_for("/api/projects/p/files"), ["files"]);
        assert_eq!(scopes_for("/api/chats/c/messages"), ["chat"]);
        assert_eq!(scopes_for("/api/proposals/p/approve"), ["chat", "scene", "files", "history"]);
        assert_eq!(scopes_for("/api/snapshots/s/restore"), ["snapshots", "tree", "scene", "files"]);
        assert_eq!(scopes_for("/api/projects"), ["tree", "projects"]);
        assert_eq!(scopes_for("/api/projects/p/variables"), ["structure"]);
        assert_eq!(scopes_for("/api/characters/c/facts"), ["characters", "structure"]);
        assert_eq!(scopes_for("/api/projects/p/character-relations"), ["characters", "structure"]);
        assert!(scopes_for("/api/auth/pair").is_empty());
    }
}
