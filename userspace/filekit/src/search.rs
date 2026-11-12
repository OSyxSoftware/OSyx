//! The launcher's search: how well a name matches what was typed, the
//! settings that can be found by name, and files picked from a list of
//! paths.

use alloc::string::String;
use alloc::vec::Vec;

/// How well `name` matches `query`, smaller is better; `None` when a word
/// of the query does not occur in it. Case is ignored.
///
/// 0: the same text. 1: the name starts with the query. 2: every word of
/// the query starts a word of the name. 3: every word occurs somewhere.
pub fn score(query: &str, name: &str) -> Option<u32> {
    let q = query.trim().to_lowercase();
    let n = name.to_lowercase();
    if q.is_empty() {
        return None;
    }
    if n == q {
        return Some(0);
    }
    if n.starts_with(&q) {
        return Some(1);
    }
    let starts_word = |word: &str| {
        n.match_indices(word).any(|(i, _)| i == 0 || !n[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric()))
    };
    let mut worst = 2;
    for word in q.split_whitespace() {
        if starts_word(word) {
            continue;
        }
        if !n.contains(word) {
            return None;
        }
        worst = 3;
    }
    Some(worst)
}

/// A setting the launcher can find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    pub title: &'static str,
    /// The section of Settings it is in, as `settings <section>` takes it.
    pub section: &'static str,
    /// Other words people look for it by.
    pub keywords: &'static str,
}

const fn s(title: &'static str, section: &'static str, keywords: &'static str) -> Setting {
    Setting { title, section, keywords }
}

pub const SETTINGS: [Setting; 22] = [
    s("Host name", "general", "computer name"),
    s("Accent colour", "appearance", "color theme"),
    s("Wallpaper", "appearance", "background picture desktop"),
    s("Displays", "display", "screen resolution monitor"),
    s("Keyboard layout", "keyboard", "language polish us keys"),
    s("Pointer speed", "mouse", "mouse cursor sensitivity"),
    s("Natural scrolling", "mouse", "touchpad wheel direction"),
    s("Left-handed mouse", "mouse", "buttons swap"),
    s("Volume", "sound", "audio loudness mute speaker"),
    s("Sound output", "sound", "audio speakers headphones device"),
    s("Battery", "power", "charge percent laptop"),
    s("Power", "power", "shutdown sleep lid"),
    s("Time zone", "datetime", "clock region"),
    s("Date and time", "datetime", "clock seconds calendar"),
    s("Wi-Fi", "network", "wireless wifi wlan"),
    s("IP address", "network", "ipv4 dhcp static gateway dns ethernet"),
    s("Network", "network", "internet connection adapter"),
    s("Disks", "storage", "storage volumes space usage"),
    s("Installed applications", "apps", "programs default browser"),
    s("Check for updates", "about", "update upgrade version"),
    s("About this computer", "about", "system version osyx"),
    s("Settings", "general", "preferences options configuration"),
];

/// Settings matching `query`, best first (a match in the title beats one
/// in the keywords).
pub fn settings(query: &str) -> Vec<&'static Setting> {
    let mut hits: Vec<(u32, usize)> = SETTINGS
        .iter()
        .enumerate()
        .filter_map(|(i, st)| {
            let by_title = score(query, st.title);
            let by_keyword = score(query, st.keywords).map(|k| k + 4);
            by_title.into_iter().chain(by_keyword).min().map(|k| (k, i))
        })
        .collect();
    hits.sort();
    hits.into_iter().map(|(_, i)| &SETTINGS[i]).collect()
}

/// The last component of a path.
pub fn file_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// Indices into `paths` of the files whose name matches `query`, best
/// first, at most `limit`: better matches, then shorter names, then
/// shallower paths.
pub fn files(paths: &[String], query: &str, limit: usize) -> Vec<usize> {
    let mut hits: Vec<(u32, usize, usize, usize)> = paths
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let name = file_name(p);
            score(query, name).map(|k| (k, name.chars().count(), p.matches('/').count(), i))
        })
        .collect();
    hits.sort();
    hits.truncate(limit);
    hits.into_iter().map(|h| h.3).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn scores_order_exact_prefix_word_and_substring() {
        assert_eq!(score("files", "Files"), Some(0));
        assert_eq!(score("fil", "Files"), Some(1));
        assert_eq!(score("man", "Task Manager"), Some(2));
        assert_eq!(score("ask", "Task Manager"), Some(3));
        assert_eq!(score("task man", "Task Manager"), Some(1));
        assert_eq!(score("man task", "Task Manager"), Some(2), "words in any order");
        assert_eq!(score("task xyz", "Task Manager"), None);
        assert_eq!(score("", "Task Manager"), None);
        assert_eq!(score("  ", "Task Manager"), None);
        assert_eq!(score("ŻÓŁ", "żółw.txt"), Some(1), "case is ignored beyond ASCII");
        assert_eq!(score("txt", "notes.txt"), Some(2), "an extension starts a word");
    }

    #[test]
    fn settings_are_found_by_title_and_by_other_words() {
        let titles = |q: &str| settings(q).iter().map(|s| s.title).collect::<Vec<_>>();
        assert_eq!(titles("wallpaper"), ["Wallpaper"]);
        assert_eq!(titles("background"), ["Wallpaper"]);
        assert_eq!(titles("wifi").first(), Some(&"Wi-Fi"));
        assert_eq!(titles("time").first(), Some(&"Time zone"), "a title match comes before a keyword match");
        assert!(titles("audio").contains(&"Volume") && titles("audio").contains(&"Sound output"));
        assert!(titles("qqq").is_empty());
        // Every section name is one Settings understands.
        for st in SETTINGS {
            assert!(
                ["general", "appearance", "display", "keyboard", "mouse", "sound", "power", "datetime", "network", "storage", "apps", "about"]
                    .contains(&st.section),
                "{}",
                st.section
            );
        }
    }

    #[test]
    fn files_rank_by_match_then_name_length_then_depth() {
        let paths: Vec<String> =
            vec!["/home/user/Documents/report-final.txt", "/home/user/report.txt", "/home/user/a/b/report.txt", "/home/user/Music/song.mp3", "/home/user/Pictures/my report.png"]
                .into_iter()
                .map(String::from)
                .collect();
        let found = |q: &str, n: usize| files(&paths, q, n).into_iter().map(|i| paths[i].as_str()).collect::<Vec<_>>();
        assert_eq!(
            found("report", 10),
            ["/home/user/report.txt", "/home/user/a/b/report.txt", "/home/user/Documents/report-final.txt", "/home/user/Pictures/my report.png"]
        );
        assert_eq!(found("report", 2).len(), 2);
        assert_eq!(found("mp3", 10), ["/home/user/Music/song.mp3"]);
        assert!(found("user", 10).is_empty(), "only the file name is searched, not its folders");
        assert_eq!(file_name("/a/b/c.txt"), "c.txt");
        assert_eq!(file_name("/a/b/"), "b");
    }
}
