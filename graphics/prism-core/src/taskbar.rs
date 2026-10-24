//! One taskbar entry for a Windows application's process family.
use alloc::vec::Vec;
use prism_proto::{state, WindowInfo};

pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub win32: bool,
}

fn root(pid: u32, processes: &[Process]) -> Option<u32> {
    let mut current = processes.iter().find(|p| p.pid == pid && p.win32)?;
    for _ in 0..processes.len() {
        let Some(parent) = processes.iter().find(|p| p.pid == current.parent && p.win32) else {
            return Some(current.pid);
        };
        current = parent;
    }
    Some(pid) // Defensive bound if given a cyclic/inconsistent snapshot.
}

pub fn group(windows: &[WindowInfo], processes: &[Process]) -> Vec<WindowInfo> {
    let mut groups: Vec<((bool, u32), WindowInfo)> = Vec::new();
    for window in windows {
        let parent = root(window.pid, processes);
        let key = parent.map_or((false, window.id), |pid| (true, pid));
        if let Some((_, entry)) = groups.iter_mut().find(|(k, _)| *k == key) {
            let active = (entry.flags | window.flags) & state::ACTIVATED;
            let minimized = (entry.flags & window.flags) & state::MINIMIZED;
            // The parent's main window stays the click target and title,
            // even if a child window happened to be announced first.
            if (!window.title.is_empty(), parent == Some(window.pid)) > (!entry.title.is_empty(), parent == Some(entry.pid)) {
                *entry = window.clone();
            }
            entry.flags = (entry.flags & !(state::ACTIVATED | state::MINIMIZED)) | active | minimized;
        } else {
            groups.push((key, window.clone()));
        }
    }
    groups.into_iter().map(|(_, entry)| entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::ToString, vec};

    fn window(id: u32, pid: u32, title: &str, flags: u32) -> WindowInfo {
        WindowInfo { id, pid, title: title.to_string(), flags }
    }

    #[test]
    fn descendants_share_the_parent_entry_without_merging_other_applications() {
        let processes = [
            Process { pid: 5, parent: 1, win32: true },
            Process { pid: 6, parent: 5, win32: true },
            Process { pid: 7, parent: 6, win32: true },
            Process { pid: 8, parent: 1, win32: true },
            Process { pid: 9, parent: 1, win32: false },
        ];
        let windows = vec![
            window(10, 7, "child", state::ACTIVATED),
            window(11, 5, "Zen Browser", state::MINIMIZED),
            window(12, 6, "dialog", 0),
            window(13, 8, "Text Editor", 0),
            window(14, 9, "Terminal one", 0),
            window(15, 9, "Terminal two", 0),
        ];
        let tasks = group(&windows, &processes);
        assert_eq!(tasks.len(), 4);
        assert_eq!((tasks[0].id, tasks[0].title.as_str()), (11, "Zen Browser"));
        assert_ne!(tasks[0].flags & state::ACTIVATED, 0);
        assert_eq!(tasks[0].flags & state::MINIMIZED, 0);
        assert_eq!((tasks[1].id, tasks[2].id, tasks[3].id), (13, 14, 15));
    }

    #[test]
    fn removing_parent_window_keeps_child_accessible_and_empty_snapshot_is_safe() {
        let processes = [Process { pid: 5, parent: 1, win32: true }, Process { pid: 6, parent: 5, win32: true }];
        let windows = [window(20, 6, "dialog", state::MINIMIZED)];
        assert_eq!(group(&windows, &processes), windows);
        assert_eq!(group(&windows, &[]), windows);
        assert!(group(&[], &processes).is_empty());
    }
}
