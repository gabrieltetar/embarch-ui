//! The Debug tab's backend (milestone-1.md §4.7): backlog-on-open plus a
//! live tail, both mediated through `embarch-core-client` — `embarch-ui`
//! never reads Core's logfile directly, since Core can run on a different
//! machine than whatever's asking (the whole reason `embarch-topology`
//! exists, embarch-ui/design.md §3 decision 7).
//!
//! A background task polls `GET /logs/recent` on an interval (the same
//! "server polls, browser only ever holds one SSE connection" shape
//! `snapshot.rs`/`study_designer.rs` already use) and republishes only the
//! genuinely new lines since the last poll over a `tokio::sync::watch`
//! channel, so a client's own `/api/logs/events` SSE stream sees each new
//! line exactly once, not the whole tail window every tick.

use embarch_core_client::CoreClient;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How many trailing lines `POST /logs/recent` is asked for on each poll —
/// large enough that normal log volume between two polls doesn't outrun the
/// window (which would make `diff_new_lines` unable to anchor and fall back
/// to replaying the whole window), small enough to stay a cheap call.
const POLL_TAIL: usize = 500;
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Runs for the lifetime of the process. Only ever sends on the `watch`
/// channel when there's something genuinely new — an SSE client that's
/// been open a while shouldn't see empty ticks, and a client that just
/// connected gets `/api/logs/recent`'s own one-shot backlog fetch instead
/// of whatever happened to be the channel's last value.
pub async fn poll_loop(core: Arc<CoreClient>, tx: watch::Sender<Vec<String>>) {
    let mut previous: Vec<String> = Vec::new();
    loop {
        match core.logs_recent(POLL_TAIL).await {
            Ok(latest) => {
                let fresh = diff_new_lines(&previous, &latest);
                if !fresh.is_empty() {
                    let _ = tx.send(fresh);
                }
                previous = latest;
            }
            Err(e) => {
                // Core being unreachable is an ordinary, expected state
                // here too (design.md §3 decision 5's own "confirmed"
                // reasoning) — logged once server-side, not surfaced as a
                // client-visible error stream; the Dashboard tab's own
                // `core_reachable` flag is the one place that's shown.
                tracing::debug!("logs poll failed: {e:#}");
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `previous`/`new` are both "last `POLL_TAIL` lines of the same
/// append-only file," fetched a `POLL_INTERVAL` apart. Anchors on
/// `previous`'s last line, found by scanning `new` from the end backward
/// (the most recent matching occurrence, in case that exact line repeats
/// earlier in the window too) — everything after it is genuinely new.
/// If that anchor line isn't found in `new` at all — either nothing
/// changed (`new == previous`) or more than `POLL_TAIL` lines were
/// appended in one interval, aging the anchor out of the window entirely
/// — falls back to replaying the whole new window rather than silently
/// dropping content; a few duplicate lines in a debug viewer is a smaller
/// problem than missing ones.
fn diff_new_lines(previous: &[String], new: &[String]) -> Vec<String> {
    if previous == new {
        return Vec::new();
    }
    if let Some(anchor) = previous.last() {
        if let Some(pos) = new.iter().rposition(|line| line == anchor) {
            return new[pos + 1..].to_vec();
        }
    }
    new.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_change_produces_no_new_lines() {
        let a = lines(&["one", "two", "three"]);
        assert!(diff_new_lines(&a, &a.clone()).is_empty());
    }

    #[test]
    fn appended_lines_are_found_past_the_anchor() {
        let previous = lines(&["one", "two", "three"]);
        let new = lines(&["one", "two", "three", "four", "five"]);
        assert_eq!(diff_new_lines(&previous, &new), lines(&["four", "five"]));
    }

    #[test]
    fn window_sliding_forward_still_anchors_correctly() {
        // "one" aged out of the tail window entirely; "three" is still the
        // anchor to find.
        let previous = lines(&["one", "two", "three"]);
        let new = lines(&["two", "three", "four"]);
        assert_eq!(diff_new_lines(&previous, &new), lines(&["four"]));
    }

    #[test]
    fn anchor_aging_out_entirely_falls_back_to_the_whole_new_window() {
        let previous = lines(&["one", "two", "three"]);
        let new = lines(&["ten", "eleven", "twelve"]);
        assert_eq!(diff_new_lines(&previous, &new), new);
    }

    #[test]
    fn empty_previous_replays_the_first_real_window() {
        let new = lines(&["one", "two"]);
        assert_eq!(diff_new_lines(&[], &new), new);
    }

    #[test]
    fn repeated_anchor_line_uses_the_most_recent_occurrence() {
        let previous = lines(&["start", "repeat"]);
        let new = lines(&["start", "repeat", "middle", "repeat", "end"]);
        assert_eq!(diff_new_lines(&previous, &new), lines(&["end"]));
    }
}
