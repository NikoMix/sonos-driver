//! Crash-safe record of the rooms the "All Sonos" output has grouped.
//!
//! Forming the party changes state that lives in the speakers, not in this
//! process. If the service is killed — or the machine bluescreens — mid-party,
//! nothing on the Windows side remembers that the household was rearranged, and
//! the driver renders no UI to undo it from. The user would be left splitting
//! rooms by hand in the Sonos app.
//!
//! So the party is journalled: the absorbed coordinators are written to a small
//! file as they join and the file is deleted when the party disbands. `vad
//! ungroup` replays it, releasing exactly the rooms this driver took and
//! nothing the user grouped themselves.
//!
//! The journal is advisory. Every write failure is logged and swallowed:
//! failing to *record* a party is no reason to refuse to *play* one.

use std::fs;
use std::path::{Path, PathBuf};

/// Where the journal lives when no explicit path is given.
///
/// `%ProgramData%` rather than a user profile: the service runs as `LocalSystem`,
/// while `vad ungroup` is typically run by an admin user, and both have to see
/// the same file.
#[must_use]
pub fn default_journal_path() -> PathBuf {
    let base = std::env::var_os("ProgramData").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("sonos-driver").join("all-sonos-group.txt")
}

/// Record `coordinators` as the rooms currently absorbed into the party.
///
/// Overwrites any previous record: the journal describes the party as it is
/// now, not how it got there.
pub fn record(path: &Path, coordinators: &[String]) {
    if coordinators.is_empty() {
        clear(path);
        return;
    }

    let mut body = coordinators.join("\n");
    body.push('\n');

    let write = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(path, body));
    if let Err(error) = write {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not record the All Sonos group; `vad ungroup` will not be able to undo it after a crash"
        );
    }
}

/// Forget the recorded party, because it has been disbanded.
pub fn clear(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "could not clear the All Sonos group record; `vad ungroup` may try to split rooms that are already standalone"
            );
        }
    }
}

/// The rooms a previous run left grouped, if any.
///
/// Returns an empty vector when there is no journal, which is the normal case.
#[must_use]
pub fn recorded(path: &Path) -> Vec<String> {
    let Ok(body) = fs::read_to_string(path) else {
        return Vec::new();
    };
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{clear, record, recorded};
    use std::path::PathBuf;

    /// A unique scratch path per test, so the suite can run in parallel.
    fn scratch(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("sonos-party-{name}-{}.txt", std::process::id()));
        path
    }

    #[test]
    fn a_recorded_party_can_be_replayed() {
        let path = scratch("replay");
        record(&path, &["RINCON_A".to_owned(), "RINCON_B".to_owned()]);
        assert_eq!(recorded(&path), vec!["RINCON_A", "RINCON_B"]);
        clear(&path);
    }

    #[test]
    fn no_journal_means_no_party_to_undo() {
        let path = scratch("absent");
        clear(&path);
        assert!(recorded(&path).is_empty());
    }

    #[test]
    fn shrinking_the_party_rewrites_rather_than_appends() {
        let path = scratch("shrink");
        record(&path, &["RINCON_A".to_owned(), "RINCON_B".to_owned()]);
        record(&path, &["RINCON_A".to_owned()]);
        assert_eq!(
            recorded(&path),
            vec!["RINCON_A"],
            "a room that left the party must not be split again by `vad ungroup`"
        );
        clear(&path);
    }

    #[test]
    fn an_empty_party_leaves_nothing_behind() {
        let path = scratch("empty");
        record(&path, &["RINCON_A".to_owned()]);
        record(&path, &[]);
        assert!(
            recorded(&path).is_empty(),
            "an empty record would make `vad ungroup` a no-op forever"
        );
    }

    #[test]
    fn clearing_a_missing_journal_is_not_an_error() {
        let path = scratch("idempotent");
        clear(&path);
        clear(&path);
    }
}
