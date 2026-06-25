//! Pure, FFI-free process-loopback selection logic.
//!
//! This module mirrors [`crate::device`]: it holds the policy and decision logic
//! for *per-application* capture — parsing a capture target into a PID, matching
//! a process name to a PID against a snapshot, and the OS-version gate decision —
//! with **no** `unsafe` and no Windows types, so the parts most likely to harbour
//! off-by-one or precedence bugs are unit-tested without hardware. The Windows
//! FFI that feeds it (toolhelp enumeration, `RtlGetVersion`, and
//! `ActivateAudioInterfaceAsync`) lives in [`crate::wasapi`].

use std::collections::HashSet;

use audio_core::{Error, Result};

/// Minimum Windows build that supports `ActivateAudioInterfaceAsync`
/// process-loopback activation: Windows 11 / Windows Server 2022 (build 20348).
///
/// Earlier builds return `E_NOTIMPL` from `GetActivateResult`, so the capture
/// path gates on this value *before* activating and returns a clear typed error
/// rather than letting the FFI fail opaquely (or panic).
pub(crate) const MIN_PROCESS_LOOPBACK_BUILD: u32 = 20_348;

/// Whether the host OS build number supports process-loopback capture.
#[must_use]
pub(crate) const fn meets_min_build(build: u32) -> bool {
    build >= MIN_PROCESS_LOOPBACK_BUILD
}

/// Which audio a process-loopback capture pulls relative to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessLoopbackMode {
    /// Capture only the target process tree's audio.
    IncludeTree,
    /// Capture everything *except* the target process tree's audio.
    ExcludeTree,
}

impl ProcessLoopbackMode {
    /// Map the CLI/config `exclude` boolean onto a mode.
    #[must_use]
    pub(crate) const fn from_exclude(exclude: bool) -> Self {
        if exclude {
            Self::ExcludeTree
        } else {
            Self::IncludeTree
        }
    }
}

/// A minimal process record, the FFI-free input to the PID resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcInfo {
    /// The process id.
    pub(crate) pid: u32,
    /// The parent process id (used to pick a tree root among duplicates).
    pub(crate) parent_pid: u32,
    /// The executable image name (basename, e.g. `"spotify.exe"`).
    pub(crate) name: String,
}

/// A parsed capture selector: an explicit PID or a process image name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessSelector {
    /// An explicit numeric process id.
    Pid(u32),
    /// A process image name (with or without the `.exe` suffix).
    Name(String),
}

/// Parse a user-supplied capture target into a [`ProcessSelector`].
///
/// A string that parses as a non-zero `u32` is treated as an explicit PID;
/// anything else is treated as a process image name.
///
/// # Errors
///
/// Returns [`Error::Capture`] for an empty selector or an explicit PID of `0`
/// (the System Idle Process, never a valid capture target).
pub(crate) fn parse_selector(raw: &str) -> Result<ProcessSelector> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::capture("empty process capture target"));
    }
    if let Ok(pid) = trimmed.parse::<u32>() {
        if pid == 0 {
            return Err(Error::capture(
                "process id 0 (System Idle Process) is not a valid capture target",
            ));
        }
        return Ok(ProcessSelector::Pid(pid));
    }
    Ok(ProcessSelector::Name(trimmed.to_owned()))
}

/// Resolve a selector to a single target PID against a process snapshot.
///
/// For an explicit [`ProcessSelector::Pid`] the id must be present in the
/// snapshot. For a [`ProcessSelector::Name`] the basename is matched
/// case-insensitively (a trailing `.exe` is optional on either side); when
/// several instances match, a process-tree root (a match whose parent is not
/// itself a match) is preferred, with the lowest PID as a deterministic
/// tie-break.
///
/// # Errors
///
/// Returns [`Error::Capture`] when nothing in the snapshot matches the selector.
pub(crate) fn resolve_pid(selector: &ProcessSelector, processes: &[ProcInfo]) -> Result<u32> {
    match selector {
        ProcessSelector::Pid(pid) => {
            if processes.iter().any(|p| p.pid == *pid) {
                Ok(*pid)
            } else {
                Err(Error::capture(format!("no running process with pid {pid}")))
            }
        }
        ProcessSelector::Name(name) => resolve_name(name, processes),
    }
}

/// Resolve a process *name* to a PID with the duplicate/tree-root policy.
fn resolve_name(name: &str, processes: &[ProcInfo]) -> Result<u32> {
    let target = normalise(name);
    let matches: Vec<&ProcInfo> = processes
        .iter()
        .filter(|p| normalise(&p.name) == target)
        .collect();

    match matches.as_slice() {
        [] => Err(Error::capture(format!(
            "no running process matches '{name}'"
        ))),
        [single] => Ok(single.pid),
        many => {
            // Prefer a tree root: a match whose parent is not itself a match
            // (e.g. the first browser process that spawned the rest). Fall back
            // to all matches, then take the lowest PID for determinism.
            let match_pids: HashSet<u32> = many.iter().map(|p| p.pid).collect();
            let root_min = many
                .iter()
                .filter(|p| !match_pids.contains(&p.parent_pid))
                .map(|p| p.pid)
                .min();
            let lowest = many.iter().map(|p| p.pid).min();
            root_min
                .or(lowest)
                .ok_or_else(|| Error::capture(format!("no running process matches '{name}'")))
        }
    }
}

/// Normalise an exe image name for comparison: trim, lowercase, drop a trailing
/// `.exe` so `"Spotify.exe"` and `"spotify"` compare equal.
fn normalise(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    match lower.strip_suffix(".exe") {
        Some(stem) => stem.to_owned(),
        None => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: u32, name: &str) -> ProcInfo {
        ProcInfo {
            pid,
            parent_pid: parent,
            name: name.to_owned(),
        }
    }

    #[test]
    fn min_build_gate_is_20348() {
        assert_eq!(MIN_PROCESS_LOOPBACK_BUILD, 20_348);
        assert!(!meets_min_build(19_045)); // Windows 10 22H2
        assert!(!meets_min_build(20_347)); // one below the threshold
        assert!(meets_min_build(20_348)); // Windows Server 2022
        assert!(meets_min_build(22_631)); // Windows 11 23H2
        assert!(meets_min_build(26_100)); // Windows 11 24H2
    }

    #[test]
    fn mode_maps_exclude_flag() {
        assert_eq!(
            ProcessLoopbackMode::from_exclude(false),
            ProcessLoopbackMode::IncludeTree
        );
        assert_eq!(
            ProcessLoopbackMode::from_exclude(true),
            ProcessLoopbackMode::ExcludeTree
        );
    }

    #[test]
    fn parses_numeric_pid() {
        assert_eq!(
            parse_selector("1234").expect("pid"),
            ProcessSelector::Pid(1234)
        );
        assert_eq!(
            parse_selector("  77 ").expect("pid"),
            ProcessSelector::Pid(77)
        );
    }

    #[test]
    fn parses_name_selector() {
        assert_eq!(
            parse_selector("spotify.exe").expect("name"),
            ProcessSelector::Name("spotify.exe".to_owned())
        );
        // A name that merely contains digits is still a name.
        assert_eq!(
            parse_selector("foo2bar").expect("name"),
            ProcessSelector::Name("foo2bar".to_owned())
        );
    }

    #[test]
    fn rejects_empty_and_zero_selectors() {
        assert!(parse_selector("   ").is_err());
        assert!(parse_selector("0").is_err());
    }

    #[test]
    fn resolves_explicit_pid_when_present() {
        let procs = [proc(10, 4, "a.exe"), proc(20, 4, "b.exe")];
        assert_eq!(
            resolve_pid(&ProcessSelector::Pid(20), &procs).expect("pid"),
            20
        );
    }

    #[test]
    fn explicit_pid_absent_errors() {
        let procs = [proc(10, 4, "a.exe")];
        assert!(resolve_pid(&ProcessSelector::Pid(999), &procs).is_err());
    }

    #[test]
    fn resolves_name_case_insensitively_with_optional_suffix() {
        let procs = [proc(100, 4, "Spotify.exe")];
        assert_eq!(
            resolve_pid(&ProcessSelector::Name("spotify".to_owned()), &procs).expect("pid"),
            100
        );
        assert_eq!(
            resolve_pid(&ProcessSelector::Name("SPOTIFY.EXE".to_owned()), &procs).expect("pid"),
            100
        );
    }

    #[test]
    fn duplicate_names_pick_tree_root() {
        // chrome.exe tree: 200 is the root (parent 4 is not a chrome match);
        // 201 and 202 are children of 200. The root (200) must win even though
        // it is not the lowest-PID rule on its own here.
        let procs = [
            proc(202, 200, "chrome.exe"),
            proc(201, 200, "chrome.exe"),
            proc(200, 4, "chrome.exe"),
        ];
        assert_eq!(
            resolve_pid(&ProcessSelector::Name("chrome".to_owned()), &procs).expect("pid"),
            200
        );
    }

    #[test]
    fn duplicate_roots_tie_break_on_lowest_pid() {
        // Two independent roots (neither is the other's parent): lowest wins.
        let procs = [proc(500, 4, "game.exe"), proc(300, 4, "game.exe")];
        assert_eq!(
            resolve_pid(&ProcessSelector::Name("game".to_owned()), &procs).expect("pid"),
            300
        );
    }

    #[test]
    fn unmatched_name_errors() {
        let procs = [proc(1, 0, "system")];
        assert!(resolve_pid(&ProcessSelector::Name("absent".to_owned()), &procs).is_err());
    }
}
