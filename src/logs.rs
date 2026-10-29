//! The Debug tab's backend (`embarch-doc/embarch-ui/spec.md`): backlog-on-open plus a
//! live tail, both mediated through `embarch-core-client` — `embarch-ui`
//! never reads Core's logfile directly, since Core can run on a different
//! machine than whatever's asking (the whole reason `embarch-topology`
//! exists, decision 7).
//!
//! A background task polls `GET /logs/recent` on an interval (the same
//! "server polls, browser only ever holds one SSE connection" shape
//! `snapshot.rs`/`study_designer.rs` already use) and republishes only the
//! genuinely new lines since the last poll over a `tokio::sync::watch`
//! channel, so a client's own `/api/logs/events` SSE stream sees each new
//! line exactly once, not the whole tail window every tick.
//!
//! **`embarch-api`'s logs arrive by a different route, and have to**
//! (decision 13, `embarch-api` decision 43).
//! `embarch-api` is not a service — it is spawned per Claude Code session
//! as an MCP server, or run once as a CLI and gone — so there is no
//! `/logs/recent` to call and, in the case that motivated this, no process
//! left to call it on. It appends to a rolling file instead, and this reads
//! that file directly.
//!
//! That is a real exception to decision 7's "never read a logfile, always
//! go over HTTP," and it is worth being precise about why it does not
//! reopen that argument. Decision 7's reasoning is that **Core** can run on
//! a different machine than `embarch-ui` — a real, supported topology. Not
//! so for `embarch-api`: it is spawned by the MCP client sitting in front of
//! the engineer, on the engineer's machine, which is the same machine this
//! UI is opened on. Reading its file locally is correct for the same reason
//! reading Core's was not.
//!
//! Both halves share `poll_new_lines`/`diff_new_lines` — only the fetch
//! differs.

use embarch_core_client::CoreClient;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How many trailing lines `GET /logs/recent` is asked for on each poll —
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
            Ok(latest) => publish_new_lines(&mut previous, latest, &tx),
            Err(e) => {
                // Core being unreachable is an ordinary, expected state
                // here too (decision 26) — logged once server-side, not
                // surfaced as a client-visible error stream; the Dashboard
                // tab's own `core_reachable` flag is the one place that's
                // shown.
                tracing::debug!("logs poll failed: {e:#}");
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The same loop against `embarch-api`'s rolling file instead of Core's HTTP
/// surface (decision 13). The path comes from
/// `embarch_core_client::api_log`, which is also what `embarch-api` itself
/// writes through — one definition, so the writer and the reader cannot
/// drift apart.
///
/// **An empty result is the ordinary case, not a failure**: on a machine
/// where `embarch-api` has never run, the file simply isn't there, and
/// `api_log::read_recent` says so with `Ok(vec![])` rather than an error.
/// This tab shows nothing, which is the truth.
pub async fn api_poll_loop(tx: watch::Sender<Vec<String>>) {
    let mut previous: Vec<String> = Vec::new();
    loop {
        // A blocking read on the runtime's worker threads would be fine at
        // this size, but `spawn_blocking` costs nothing here and keeps the
        // rule intact.
        match tokio::task::spawn_blocking(|| embarch_core_client::api_log::read_recent(POLL_TAIL)).await {
            Ok(Ok(latest)) => publish_new_lines(&mut previous, latest, &tx),
            Ok(Err(e)) => tracing::debug!("embarch-api logs poll failed: {e:#}"),
            Err(e) => tracing::debug!("embarch-api logs poll task failed: {e:#}"),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Diff against the previous window and publish only what's genuinely new,
/// updating `previous` in place. Never sends an empty batch — an SSE client
/// that has been open a while shouldn't see empty ticks, and a client that
/// just connected gets its own one-shot backlog fetch instead of whatever
/// happened to be the channel's last value.
fn publish_new_lines(previous: &mut Vec<String>, latest: Vec<String>, tx: &watch::Sender<Vec<String>>) {
    let fresh = diff_new_lines(previous, &latest);
    if !fresh.is_empty() {
        let _ = tx.send(fresh);
    }
    *previous = latest;
}

/// `previous`/`new` are both "last `POLL_TAIL` lines of the same
/// append-only file," fetched a `POLL_INTERVAL` apart. The window can only
/// have slid forward, so the two overlap as a *run*: some suffix of
/// `previous` is the same stretch of file as the matching-length prefix of
/// `new`, except that the very last line of that run may have grown —
/// Core's tail can end mid-line (decision 13), so a poll can catch a line
/// before its trailing newline. Finds the longest such overlap and publishes
/// whatever follows it. If there is no overlap at all — more than
/// `POLL_TAIL` lines were appended in one interval, aging the whole previous
/// window out — falls back to replaying the whole new window rather than
/// silently dropping content; a few duplicate lines in a debug viewer is a
/// smaller problem than missing ones.
///
/// **Matching a run rather than one anchor line is load-bearing.** This used
/// to anchor on `previous`'s last line alone, found by scanning `new`
/// backward for its most recent occurrence — which silently swallowed every
/// line between the real anchor and a *later* identical line, and log lines
/// repeat verbatim all the time (a retry, a heartbeat, a stack frame). The
/// window is a contiguous run of one file, so the position of the previous
/// window's end is pinned by the whole overlap, not by one line that happens
/// to appear twice.
///
/// **The overlap's last line only needs to extend, not equal, its
/// counterpart** (`tasks/ui/059`) — otherwise a still-growing last line
/// breaks the run's exact equality outright, and the search falls through to
/// a *shorter*, coincidental overlap on whatever the pre-growth content
/// repeats earlier in the window, republishing lines already sent.
/// **When the last line matched by growth rather than equality, the return
/// starts at `k - 1`, not `k`:** the grown line is itself republished, so its
/// new tail reaches the console (one duplicated line, the tolerance above)
/// instead of being swallowed for good — `*previous` is overwritten with the
/// grown text, so a line dropped here is never offered again. Equality at the
/// last position still returns `new[k..]`, or every steady-state poll would
/// republish its last line forever. Decision 70.
fn diff_new_lines(previous: &[String], new: &[String]) -> Vec<String> {
    if previous == new {
        return Vec::new();
    }
    let max_overlap = previous.len().min(new.len());
    for k in (1..=max_overlap).rev() {
        let prev_tail = &previous[previous.len() - k..];
        let new_head = &new[..k];
        let body_matches = prev_tail[..k - 1] == new_head[..k - 1];
        let last_matches = new_head[k - 1].starts_with(prev_tail[k - 1].as_str());
        if body_matches && last_matches {
            let grew = new_head[k - 1] != prev_tail[k - 1];
            return new[if grew { k - 1 } else { k }..].to_vec();
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

    /// The regression the one-line anchor got wrong: "repeat" appears again
    /// later in the new window, and anchoring on its *last* occurrence
    /// swallowed "middle" and the second "repeat" outright. Every line after
    /// the previous window is new, however often any of them repeats.
    #[test]
    fn a_line_repeating_later_in_the_window_does_not_swallow_what_precedes_it() {
        let previous = lines(&["start", "repeat"]);
        let new = lines(&["start", "repeat", "middle", "repeat", "end"]);
        assert_eq!(diff_new_lines(&previous, &new), lines(&["middle", "repeat", "end"]));
    }

    /// A window made entirely of identical lines still advances by exactly
    /// what was appended — the longest overlap is the whole previous window.
    #[test]
    fn identical_repeated_lines_advance_by_what_was_appended() {
        let previous = lines(&["tick", "tick", "tick"]);
        let new = lines(&["tick", "tick", "tick", "tick"]);
        assert_eq!(diff_new_lines(&previous, &new), lines(&["tick"]));
    }

    /// `tasks/ui/059`: the previous window's last line is still growing
    /// (Core's tail can end mid-line, decision 13) *and* its pre-growth
    /// content repeats earlier in the window. Exact equality used to fail the
    /// full-length match and fall through to the coincidental overlap on the
    /// earlier "A", republishing "B". Now "B" stays unsent and the grown line
    /// is published (leg 123 refused a first fix that returned nothing here).
    #[test]
    fn a_growing_trailing_line_with_a_repeat_publishes_the_grown_line_only() {
        let previous = lines(&["A", "B", "A"]);
        let new = lines(&["A", "B", "A2"]);
        assert_eq!(diff_new_lines(&previous, &new), lines(&["A2"]));
    }

    #[test]
    fn a_growing_trailing_line_with_a_repeat_and_a_new_line_publishes_both() {
        let previous = lines(&["A", "B", "A"]);
        let new = lines(&["A", "B", "A2", "C"]);
        assert_eq!(diff_new_lines(&previous, &new), lines(&["A2", "C"]));
    }

    #[test]
    fn plain_growth_without_a_repeat_publishes_the_grown_line() {
        let previous = lines(&["X", "Y"]);
        let new = lines(&["X", "Y2"]);
        assert_eq!(diff_new_lines(&previous, &new), lines(&["Y2"]));
    }

    #[test]
    fn steady_state_publishes_nothing() {
        let a = lines(&["X", "Y"]);
        assert!(diff_new_lines(&a, &a.clone()).is_empty());
        // Window slid, last line unchanged: equality at the last position.
        let slid = lines(&["Y", "Z"]);
        assert!(diff_new_lines(&lines(&["X", "Y", "Z"]), &slid).is_empty());
    }

    #[test]
    fn a_line_growing_across_two_polls_is_delivered_in_full() {
        let p1 = lines(&["X", "Y2"]);
        let p2 = lines(&["X", "Y2Z"]);
        assert_eq!(diff_new_lines(&p1, &p2), lines(&["Y2Z"]));
        let p3 = lines(&["X", "Y2Z", "W"]);
        assert_eq!(diff_new_lines(&p2, &p3), lines(&["W"]));
    }
}
