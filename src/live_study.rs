//! The Live Study tab's server side: one session per study, subscribed once
//! to embarch-core's live event stream, holding everything that arrived so
//! far in bounded rings.
//!
//! # Why the rings are here and not in the browser
//!
//! A browser that opens the tab mid-run, or reloads it, has missed
//! everything Core pushed before it connected — Core keeps no replay
//! (`embarch-core` decision 41) and a second subscription would start at
//! "now" with a hole of unknown size. So embarch-ui subscribes **once**, per
//! study, and keeps the run so far; a browser connecting to
//! `GET /api/live/events` gets a **snapshot frame first** and incremental
//! frames after it. That is the whole mechanism that makes opening mid-run,
//! and reloading mid-run, work at all.
//!
//! It also means a second tab costs Core nothing, the same way the
//! Dashboard's single `poll_loop` does.
//!
//! # What is bounded, and what says so
//!
//! Every ring here has a cap and **counts what it dropped**. A capped ring
//! that silently discarded its oldest would make a short feed and a complete
//! one indistinguishable, which is the one thing a person reading a run
//! cannot afford. The counts ride in the snapshot and in the incremental
//! frames, and `assets/app.js` renders them ("showing the last 5,000 of
//! 8,412").
//!
//! # What this module refuses to do
//!
//! **It does not reframe a console.** A `StreamText` chunk arrives exactly
//! as Core read it off the wire and can end mid-line and mid-UTF-8
//! (`embarch-core` decision 70). Lines are assembled *here*, carrying the
//! partial remainder between chunks, and the remainder is published as a
//! partial line rather than padded into a whole one.
//!
//! **It does not decide a study's status.** Status comes from Core — live
//! over the stream, polled when the stream will not open or drops — and
//! `lagged` is surfaced, never swallowed.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};

use embarch_core_client::{
    CoreClient, FollowItem, FollowMode, FollowOptions, StudyEvent,
};
use crate::trace::{self, ClockAnchor, Projection};
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use serde::Serialize;
use std::convert::Infallible;
use serde_json::{json, Value};
use tokio::sync::broadcast;

/// The chronological feed. 5,000 rows is several minutes of a chatty study
/// and about a megabyte of JSON in the snapshot frame — past that a browser
/// is rendering a log it cannot read, and the disk record is the right tool.
const MAX_FEED_EVENTS: usize = 5_000;
/// Per `Text` tap. Two caps, because either one alone is escapable: 2,000
/// lines of 4 KB each, or 256 KB in one line.
const MAX_CONSOLE_LINES: usize = 2_000;
const MAX_CONSOLE_BYTES: usize = 256 * 1024;
/// A single line longer than this is cut, and the cut is stated on the line.
/// A console that streams a megabyte with no newline in it is a binary tap
/// declared as `Text`, and the honest answer is to say so rather than to
/// grow without bound.
const MAX_CONSOLE_LINE_BYTES: usize = 8 * 1024;
/// Per sample tap. Beyond this the series decimates 2:1 and says so — a plot
/// of 50,000 points is already far past a screen's worth of columns, and
/// dropping the oldest instead would silently change what "the whole run"
/// means mid-run.
const MAX_SERIES_POINTS: usize = 50_000;
/// `embarch-study-designer`'s own `MAX_STEPS_PER_STUDY`. Stated rather than
/// imported because it bounds a `Vec` here, not a `heapless` buffer.
const MAX_STEPS: usize = 64;
/// How many sessions are kept. A finished session is worth keeping so the
/// tab can be reopened on it without a disk read; a dozen of them is not.
const MAX_SESSIONS: usize = 8;

/// Frames a browser can receive. One JSON object per SSE frame, `kind`-
/// tagged the way Core's own events are.
///
/// Serialized once on the session's own thread and broadcast as a `String`:
/// a study with a power tap and two consoles can have several subscribers,
/// and serializing per subscriber would do the same work three times.
fn frame(kind: &str, mut value: Value) -> String {
    if let Value::Object(map) = &mut value {
        map.insert("kind".to_string(), Value::String(kind.to_string()));
    }
    value.to_string()
}

/// One line of a console, or the partial remainder at its end.
#[derive(Debug, Clone, Serialize)]
pub struct ConsoleLine {
    /// Monotonic within a session, so a browser can tell a re-sent line from
    /// a new one after a reconnect.
    seq: u64,
    step_index: u32,
    rx_utc_ms: u64,
    text: String,
    /// Set on a line this module cut at [`MAX_CONSOLE_LINE_BYTES`]. The UI
    /// marks it; a cut line that read as a whole one would be this module
    /// inventing a line ending, which is the thing it exists not to do.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
}

#[derive(Debug, Default)]
struct Console {
    lines: VecDeque<ConsoleLine>,
    bytes: usize,
    /// Bytes arrived since the last newline — **not** a line yet, and shown
    /// as a partial one.
    partial: String,
    partial_step: u32,
    partial_rx_utc_ms: u64,
    /// Lines dropped off the front because a cap was hit.
    dropped: u64,
    /// Every line this tap has ever produced, dropped ones included.
    total: u64,
}

impl Console {
    /// Feeds one verbatim chunk in, returning the lines it completed.
    ///
    /// Splitting on `\n` only, and trimming a trailing `\r` — a DUT shell
    /// speaking CRLF would otherwise put a stray carriage return at the end
    /// of every line. Nothing else about the text is touched: no ANSI
    /// stripping, no whitespace trimming, no prompt detection.
    fn push_chunk(
        &mut self,
        seq: &mut u64,
        step_index: u32,
        rx_utc_ms: u64,
        text: &str,
    ) -> Vec<ConsoleLine> {
        if self.partial.is_empty() {
            self.partial_step = step_index;
            self.partial_rx_utc_ms = rx_utc_ms;
        }
        self.partial.push_str(text);

        let mut completed = Vec::new();
        while let Some(at) = self.partial.find('\n') {
            let mut line: String = self.partial.drain(..=at).collect();
            line.pop(); // the '\n'
            if line.ends_with('\r') {
                line.pop();
            }
            let truncated = line.len() > MAX_CONSOLE_LINE_BYTES;
            if truncated {
                // On a char boundary: `text` is already a `String`, and
                // cutting mid-character would produce a panic rather than a
                // short line.
                let mut cut = MAX_CONSOLE_LINE_BYTES;
                while cut > 0 && !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                line.truncate(cut);
            }
            *seq += 1;
            self.total += 1;
            let entry = ConsoleLine {
                seq: *seq,
                step_index: self.partial_step,
                rx_utc_ms: self.partial_rx_utc_ms,
                text: line,
                truncated,
            };
            self.bytes += entry.text.len();
            self.lines.push_back(entry.clone());
            completed.push(entry);
            self.partial_step = step_index;
            self.partial_rx_utc_ms = rx_utc_ms;
        }

        while self.lines.len() > MAX_CONSOLE_LINES || self.bytes > MAX_CONSOLE_BYTES {
            match self.lines.pop_front() {
                Some(gone) => {
                    self.bytes = self.bytes.saturating_sub(gone.text.len());
                    self.dropped += 1;
                }
                None => break,
            }
        }
        completed
    }

    fn to_json(&self, tap: &str) -> Value {
        json!({
            "tap": tap,
            "lines": self.lines.iter().collect::<Vec<_>>(),
            "partial": self.partial,
            "dropped": self.dropped,
            "total": self.total,
        })
    }
}

/// One tap's live sample series.
///
/// **Decimated, never truncated.** Dropping the oldest points would make a
/// plot that silently changes what window it covers as the run goes on; a
/// 2:1 decimation keeps the whole run on screen and says, in `stride`, that
/// it is showing one point in N.
#[derive(Debug, Default)]
struct Series {
    points: Vec<[f64; 2]>,
    unit: Option<String>,
    /// One point kept per `stride` arriving. Starts at 1.
    stride: u32,
    /// Samples seen since the last one kept.
    skipped: u32,
    /// Every sample this tap has produced, kept or not.
    total: u64,
}

impl Series {
    fn push(&mut self, rx_utc_ms: u64, value: f64, unit: Option<&str>) -> bool {
        if self.stride == 0 {
            self.stride = 1;
        }
        if self.unit.is_none() {
            self.unit = unit.map(|u| u.to_string());
        }
        self.total += 1;
        if self.skipped + 1 < self.stride {
            self.skipped += 1;
            return false;
        }
        self.skipped = 0;
        self.points.push([rx_utc_ms as f64, value]);
        if self.points.len() > MAX_SERIES_POINTS {
            // Keep every other point and double the stride, so the series
            // stays evenly sampled across the *whole* run rather than dense
            // at the start and sparse at the end.
            let mut keep = Vec::with_capacity(self.points.len() / 2 + 1);
            for (i, p) in self.points.iter().enumerate() {
                if i % 2 == 0 {
                    keep.push(*p);
                }
            }
            self.points = keep;
            self.stride = self.stride.saturating_mul(2);
        }
        true
    }

    fn to_json(&self, tap: &str) -> Value {
        json!({
            "tap": tap,
            "points": self.points,
            "unit": self.unit,
            "stride": self.stride.max(1),
            "total": self.total,
        })
    }
}

// ---- the live Time chart ----------------------------------------------------
//
// **The same chart the Live Study tab draws off disk, fed as the run happens.**
// `src/time_chart.rs` builds it post-hoc from rendered files; this builds it
// from the events Core pushes, through the same [`crate::trace::Projection`]
// and the same row decoder, so a run watched live and the same run reloaded
// afterwards are one picture rather than two.
//
// Three rules, and every one of them is about not moving something a reader has
// already looked at:
//
// 1. **A mark is never placed eagerly.** A lane stores each mark's
//    `core_rx_utc_ms` only; placement happens when a frame is serialized. This
//    is sound because anchors append monotonically in the host clock — a new
//    anchor can never land *between* two existing ones — so a mark already
//    bracketed never moves, and only marks past the last anchor are affected,
//    which the projection already refuses to place.
// 2. **A tier is never silently promoted.** The axis exists once the trace has
//    given two frames carrying both clocks, and not before; until then the tab
//    says it is waiting. Where a placement genuinely must change, the epoch
//    bumps and the browser re-snapshots.
// 3. **A non-monotone anchor is rejected and counted, never inserted.**
//    `current_utc_ms()` is not monotonic — an NTP correction mid-capture makes
//    an arrival go backwards — and the post-hoc path sorts and dedups its
//    anchors, which an append-only live push cannot do. A backwards anchor
//    would break `project_ms`'s binary-search precondition and land a mark
//    anywhere.

/// Marks kept per lane, live. A ring like every other here, and it counts what
/// it dropped for the same reason: a short chart and a complete one must not
/// look alike.
const MAX_LIVE_MARKS: usize = 20_000;
/// Anchors kept. One per record-carrying frame — a few thousand across a long
/// capture at the outpost's own frame rate, and the projection binary-searches
/// them, so this is a bound on memory rather than on fidelity.
const MAX_LIVE_ANCHORS: usize = 200_000;
/// How often a `time_chart` frame goes out at most. A traced study pushes a
/// frame every few milliseconds; redrawing per frame would send the browser
/// pictures the compositor never shows.
const TIME_CHART_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

/// One event on a lane, **unplaced**.
#[derive(Debug, Clone)]
struct LiveMark {
    core_rx_utc_ms: u64,
    sub: String,
    label: String,
}

/// One lane of live marks.
#[derive(Debug, Default)]
struct LiveLane {
    label: String,
    kind: &'static str,
    marks: VecDeque<LiveMark>,
    /// Marks dropped off the front because the cap was hit.
    dropped: u64,
    /// Every mark this lane has ever held, dropped ones included.
    total: u64,
    /// How many of `marks` have already gone out **placed**. Everything from
    /// here on is either placeable now or still past the last anchor.
    ///
    /// Counted from the front of the ring, so a drop moves it too — see
    /// [`LiveLane::push`].
    emitted: usize,
}

impl LiveLane {
    fn push(&mut self, mark: LiveMark) {
        self.total += 1;
        self.marks.push_back(mark);
        while self.marks.len() > MAX_LIVE_MARKS {
            self.marks.pop_front();
            self.dropped += 1;
            // The cursor is an index into the ring, so dropping the front
            // moves it. Saturating rather than wrapping: a lane whose whole
            // emitted prefix has aged out has nothing left to re-emit.
            self.emitted = self.emitted.saturating_sub(1);
        }
    }

    /// The marks this lane can place now and has not sent yet.
    ///
    /// **Walks forward and stops at the first one that will not place.**
    /// Anchors are monotone and marks are in arrival order, so the first
    /// unplaceable mark is past the last anchor and so is everything after it —
    /// there is nothing further on to find. Those are the *pending* count, and
    /// they are drawn at the leading edge as a number, never at a guessed
    /// position.
    fn take_placeable(&mut self, proj: &Projection, uncertain: bool) -> (Vec<Value>, usize) {
        let mut out = Vec::new();
        while self.emitted < self.marks.len() {
            let mark = &self.marks[self.emitted];
            let Some(t) = proj.place(mark.core_rx_utc_ms) else { break };
            out.push(json!({
                "t": t,
                "core_rx_utc_ms": mark.core_rx_utc_ms,
                "kind": self.kind,
                "sub": mark.sub,
                "label": mark.label,
                "uncertain": uncertain,
            }));
            self.emitted += 1;
        }
        (out, self.marks.len() - self.emitted)
    }

    /// Every mark this lane currently holds that places, for a snapshot — and
    /// it resets the cursor, because a snapshot *is* everything sent so far.
    fn place_all(&mut self, proj: &Projection, uncertain: bool) -> (Vec<Value>, usize) {
        self.emitted = 0;
        self.take_placeable(proj, uncertain)
    }
}

/// The axis a live session is building, and the honest accounting of what it
/// refused on the way.
#[derive(Debug, Default)]
struct LiveAxis {
    /// One per record-carrying frame, appended in arrival order and **never
    /// sorted**: an append-only feed has no later frames to sort against, so
    /// monotonicity is enforced at the door instead.
    anchors: Vec<ClockAnchor>,
    /// Which axis the marks already sent were placed on. Bumped only where a
    /// placement genuinely must change — the header frame arriving and
    /// promoting the tier, or a stale prefix being dropped mid-run — and the
    /// browser re-snapshots rather than patching across it.
    epoch: u64,
    /// Anchors refused because their arrival went backwards against the one
    /// before them. **An NTP correction mid-capture is the realistic cause**,
    /// and inserting one would break the projection's binary-search
    /// precondition and land a mark anywhere. Counted so a chart that quietly
    /// stopped gaining resolution says why.
    non_monotone: u64,
    /// Whether the capture's header frame has arrived. Until it has, rows carry
    /// no `us` and contribute no anchor, so the axis does not exist yet.
    header_seen: bool,
    frames: u64,
    rows: u64,
    /// Rows the live decoder pushed that this build could not read as rows.
    rows_unparsed: u64,
    /// How many leading anchors were dropped as a stale pre-reset prefix.
    stale_prefix_dropped: usize,
    /// The tap that is drawing the axis.
    tap: Option<String>,
}

impl LiveAxis {
    /// Folds one pushed frame in. Returns true when the axis changed in a way
    /// that invalidates every placement already sent.
    fn ingest(&mut self, tap: &str, header_seen: bool, rows: &[String]) -> bool {
        self.frames += 1;
        self.rows += rows.len() as u64;
        if self.tap.is_none() {
            self.tap = Some(tap.to_string());
        }
        // A second traced tap cannot also draw the axis. Its frames are
        // recorded in the counts and contribute no anchors.
        if self.tap.as_deref() != Some(tap) {
            return false;
        }

        let mut epoch_bump = false;
        if header_seen && !self.header_seen {
            self.header_seen = true;
            // **The tier just improved.** Every row before this one carried an
            // empty `us` and contributed no anchor, so nothing placed so far is
            // wrong — but the axis is about to exist for the first time, and a
            // browser holding "no axis yet" must re-snapshot rather than
            // receive marks on an axis it has never seen.
            epoch_bump = true;
        }

        // Decoded through `trace::parse_rows`, which is the same function the
        // post-hoc path uses on the rendered file — the whole reason Core
        // pushes CSV rows rather than a struct.
        let parsed = trace::parse_rows(rows.iter().map(String::as_str), rows.len());
        self.rows_unparsed += parsed.unparsed as u64;
        let Some(anchor) = trace::anchor_for_frame(&parsed.rows) else {
            return epoch_bump;
        };

        if let Some(last) = self.anchors.last() {
            if anchor.rx_utc_ms < last.rx_utc_ms {
                // **Rejected, not inserted.** See `LiveAxis::non_monotone`.
                self.non_monotone += 1;
                return epoch_bump;
            }
        }
        if self.anchors.len() < MAX_LIVE_ANCHORS {
            self.anchors.push(anchor);
        }

        // A stale pre-reset prefix can only be recognised once there is enough
        // after it to recognise it against, so this fires mid-run or not at
        // all — and when it does, every placement already sent was made off a
        // dead epoch's anchors. That is exactly the case the epoch exists for.
        let before = self.anchors.len();
        trace::drop_stale_prefix(&mut self.anchors);
        if self.anchors.len() < before {
            self.stale_prefix_dropped += before - self.anchors.len();
            epoch_bump = true;
        }
        if epoch_bump {
            self.epoch += 1;
        }
        epoch_bump
    }

    /// Median milliseconds between consecutive anchors — how finely this
    /// capture can place anything, which is what a projected mark's accuracy
    /// is. `None` until there are two.
    fn resolution_ms(&self) -> Option<f64> {
        if self.anchors.len() < 2 {
            return None;
        }
        let mut deltas: Vec<u64> = self
            .anchors
            .windows(2)
            .map(|w| w[1].rx_utc_ms.saturating_sub(w[0].rx_utc_ms))
            .collect();
        deltas.sort_unstable();
        let mid = deltas.len() / 2;
        Some(if deltas.len().is_multiple_of(2) {
            (deltas[mid - 1] + deltas[mid]) as f64 / 2.0
        } else {
            deltas[mid] as f64
        })
    }

    fn projection(&self) -> Projection {
        Projection::from_live_anchors(&self.anchors, self.resolution_ms())
    }

    fn describe(&self, proj: &Projection) -> Value {
        if !proj.placeable() {
            return json!({
                "placeable": false,
                "unit": "us",
                "axis_clock": "dut-cycles",
                "epoch": self.epoch,
                "source": self.tap,
                "note": if self.tap.is_none() {
                    "This study declares an outpost trace and none of its frames has arrived yet, \
                     so there is no axis to draw on. Nothing is placed against a guess in the \
                     meantime — the chart appears when the trace's first stamped-and-dated frame \
                     does.".to_string()
                } else if !self.header_seen {
                    format!(
                        "The trace '{}' is arriving and its header frame has not, so its records \
                         carry a raw cycle count and no rate to divide it by. The axis appears \
                         when the header does; embarch-core repeats it, so this is a wait rather \
                         than a refusal.",
                        self.tap.as_deref().unwrap_or("?")
                    )
                } else {
                    "Fewer than two frames of this capture carry both clocks, so there is nothing \
                     to tie the DUT's counter to embarch-core's own. One more stamped frame is \
                     all it takes.".to_string()
                },
                "frames": self.frames,
                "rows": self.rows,
                "non_monotone": self.non_monotone,
            });
        }
        json!({
            "placeable": true,
            "unit": "us",
            "axis_clock": "dut-cycles",
            "epoch": self.epoch,
            "source": self.tap,
            "t_from": proj.t_from(),
            "t_to": proj.t_to(),
            "projected": true,
            "accuracy_ms": proj.accuracy_ms,
            "window_from_ms": proj.window_from_ms(),
            "window_to_ms": proj.window_to_ms(),
            "frames": self.frames,
            "rows": self.rows,
            "rows_unparsed": self.rows_unparsed,
            "non_monotone": self.non_monotone,
            "stale_prefix_dropped": self.stale_prefix_dropped,
            "note": format!(
                "The axis is the DUT's own microsecond counter, from the trace '{}' as it \
                 arrives. Everything else in this study is stamped on embarch-core's receipt \
                 clock and is projected onto it through the frames that carry both — good to \
                 about {} ms so far. This axis grows as the run does: a mark past its leading \
                 edge is counted, never drawn at a guessed position.",
                self.tap.as_deref().unwrap_or("?"),
                proj.accuracy_ms.map(|v| format!("{v}")).unwrap_or_else(|| "?".to_string()),
            ),
        })
    }
}

/// One row of the chronological feed.
#[derive(Debug, Clone, Serialize)]
struct FeedRow {
    seq: u64,
    /// What the UI filters on: `step`, `samples`, `gatt`, `text`, `status`,
    /// `lagged`, `transport`, `polled`, `unrecognized`.
    kind: &'static str,
    text: String,
}

/// Everything one session holds. Guarded by a std mutex and never held
/// across an await — the follow task takes it, mutates, serializes a frame
/// and drops it.
#[derive(Debug, Default)]
struct SessionState {
    status: String,
    reason: Option<String>,
    current_step: Option<u32>,
    total_steps: Option<u32>,
    study_name: Option<String>,
    /// `live` or `polling`, as Core's own follow reports it — the tab says
    /// which is in force rather than presenting both as the same thing.
    mode: &'static str,
    mode_detail: Option<String>,
    /// Events Core told us **it** dropped for this subscriber. Displayed,
    /// never swallowed: the live feed has holes and the disk record does
    /// not, which is exactly what the tab's "reload from disk" offers.
    lagged: u64,
    seq: u64,
    feed: VecDeque<FeedRow>,
    feed_dropped: u64,
    feed_total: u64,
    steps: Vec<Value>,
    consoles: BTreeMap<String, Console>,
    series: BTreeMap<String, Series>,
    /// Filled in on a terminal status by re-reading `GET /study/{id}` —
    /// the SSE frame carries no `StudyResult`, so provenance and the stream
    /// list only exist in the authoritative record.
    provenance: Option<Value>,
    streams: Option<Value>,
    /// Set when that re-read failed. The tab says the run finished and the
    /// record could not be read, which is two facts, not one.
    record_note: Option<String>,
    /// True once the follow task has exited.
    finished: bool,
    /// The axis the Time chart is drawing on, built from the trace as it
    /// arrives.
    axis: LiveAxis,
    /// Every step this run has finished, in the shape the projection takes.
    ///
    /// **This is what `StepCompleted` carrying its own stamps bought.** The
    /// three values are Core's own, from the same `events.json` row, so a live
    /// step band and the one a reload draws are the same band through the same
    /// `project_steps_on` — not a live approximation of it.
    step_stamps: Vec<trace::StepStamp>,
    /// One lane of unplaced marks per stream, keyed the way the post-hoc
    /// chart keys them so a live lane and a reloaded one are the same lane.
    lanes: BTreeMap<String, LiveLane>,
    /// When a `time_chart` frame last went out, so a traced study pushing a
    /// frame every few milliseconds does not redraw per frame.
    last_time_chart: Option<std::time::Instant>,
}

impl SessionState {
    fn push_feed(&mut self, kind: &'static str, text: String) -> FeedRow {
        self.seq += 1;
        self.feed_total += 1;
        let row = FeedRow { seq: self.seq, kind, text };
        self.feed.push_back(row.clone());
        while self.feed.len() > MAX_FEED_EVENTS {
            self.feed.pop_front();
            self.feed_dropped += 1;
        }
        row
    }

    /// Records one mark on a lane, **unplaced** — see the module rules above.
    fn push_mark(&mut self, key: &str, label: &str, kind: &'static str, mark: LiveMark) {
        let lane = self.lanes.entry(key.to_string()).or_default();
        if lane.label.is_empty() {
            lane.label = label.to_string();
            lane.kind = kind;
        }
        lane.push(mark);
    }

    /// The whole chart as it stands: every lane's placeable marks, on the axis
    /// as it stands. What a snapshot carries, and what an epoch bump forces.
    fn time_chart_snapshot(&mut self) -> Value {
        let proj = self.axis.projection();
        let axis = self.axis.describe(&proj);
        let lanes: Vec<Value> = self
            .lanes
            .iter_mut()
            .map(|(key, lane)| {
                let (marks, pending) = lane.place_all(&proj, proj.projected());
                json!({
                    "key": key,
                    "label": lane.label,
                    "kind": lane.kind,
                    "total": lane.total,
                    "dropped": lane.dropped,
                    "pending": pending,
                    "marks": marks,
                })
            })
            .collect();
        let step_row = trace::project_steps_on(&self.step_stamps, &proj);
        json!({
            "axis": axis,
            "epoch": self.axis.epoch,
            "replace": true,
            "lanes": lanes,
            // The same bands the Trace card and the post-hoc chart draw,
            // through the same projection — not a live rendering of them.
            "bands": step_row.as_ref().map(|r| &r.bands),
            "steps_placeable": step_row.as_ref().is_some_and(|r| r.placeable),
            "steps_note": step_row.as_ref().map(|r| r.note.clone()),
        })
    }

    /// The marks that became placeable since the last frame, and nothing else.
    fn time_chart_delta(&mut self) -> Value {
        let proj = self.axis.projection();
        let axis = self.axis.describe(&proj);
        let lanes: Vec<Value> = self
            .lanes
            .iter_mut()
            .map(|(key, lane)| {
                let (marks, pending) = lane.take_placeable(&proj, proj.projected());
                json!({
                    "key": key,
                    "label": lane.label,
                    "kind": lane.kind,
                    "total": lane.total,
                    "dropped": lane.dropped,
                    "pending": pending,
                    "marks": marks,
                })
            })
            .collect();
        let step_row = trace::project_steps_on(&self.step_stamps, &proj);
        json!({
            "axis": axis,
            "epoch": self.axis.epoch,
            "replace": false,
            "lanes": lanes,
            // The same bands the Trace card and the post-hoc chart draw,
            // through the same projection — not a live rendering of them.
            "bands": step_row.as_ref().map(|r| &r.bands),
            "steps_placeable": step_row.as_ref().is_some_and(|r| r.placeable),
            "steps_note": step_row.as_ref().map(|r| r.note.clone()),
        })
    }

    /// A `time_chart` frame, throttled — or a full replacement when the axis
    /// itself changed, which is never throttled because everything already
    /// drawn is on the wrong axis until it lands.
    fn time_chart_frame(&mut self, epoch_bumped: bool) -> Option<String> {
        if epoch_bumped {
            self.last_time_chart = Some(std::time::Instant::now());
            return Some(frame("time_chart", self.time_chart_snapshot()));
        }
        let now = std::time::Instant::now();
        if let Some(last) = self.last_time_chart {
            if now.duration_since(last) < TIME_CHART_MIN_INTERVAL {
                return None;
            }
        }
        self.last_time_chart = Some(now);
        Some(frame("time_chart", self.time_chart_delta()))
    }

    fn status_json(&self) -> Value {
        json!({
            "status": self.status,
            "reason": self.reason,
            "current_step": self.current_step,
            "total_steps": self.total_steps,
            "study_name": self.study_name,
            "mode": self.mode,
            "mode_detail": self.mode_detail,
            "lagged": self.lagged,
            "finished": self.finished,
            "record_note": self.record_note,
        })
    }
}

/// One study being watched.
pub struct LiveSession {
    pub study_id: String,
    state: StdMutex<SessionState>,
    tx: broadcast::Sender<String>,
}

impl LiveSession {
    fn new(study_id: String) -> LiveSession {
        let (tx, _) = broadcast::channel(512);
        LiveSession {
            study_id,
            state: StdMutex::new(SessionState {
                status: "pending".to_string(),
                mode: "live",
                ..SessionState::default()
            }),
            tx,
        }
    }

    /// The whole run so far, as one frame. **This is what makes opening the
    /// tab mid-run work**, and it is sent to every subscriber on connect
    /// before any incremental frame.
    pub fn snapshot(&self) -> String {
        let mut state = self.state.lock().unwrap();
        // Placed here rather than held placed: a browser connecting mid-run
        // gets the chart as the axis stands *now*, which is the only version
        // of it that is true.
        let time_chart = state.time_chart_snapshot();
        frame(
            "snapshot",
            json!({
                "study_id": self.study_id,
                "status": state.status_json(),
                "steps": state.steps,
                "feed": state.feed.iter().collect::<Vec<_>>(),
                "feed_dropped": state.feed_dropped,
                "feed_total": state.feed_total,
                "feed_cap": MAX_FEED_EVENTS,
                "consoles": state
                    .consoles
                    .iter()
                    .map(|(name, c)| c.to_json(name))
                    .collect::<Vec<_>>(),
                "series": state
                    .series
                    .iter()
                    .map(|(name, s)| s.to_json(name))
                    .collect::<Vec<_>>(),
                "provenance": state.provenance,
                "streams": state.streams,
                "time_chart": time_chart,
            }),
        )
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    pub fn is_finished(&self) -> bool {
        self.state.lock().unwrap().finished
    }

    fn send(&self, payload: String) {
        // No subscribers is the ordinary case — nobody has the tab open.
        // Exactly the posture Core takes toward its own broadcast.
        let _ = self.tx.send(payload);
    }
}

/// Every live session, and the one place a new one is started.
pub struct LiveStudies {
    core: Arc<CoreClient>,
    sessions: StdMutex<Vec<Arc<LiveSession>>>,
}

impl LiveStudies {
    pub fn new(core: Arc<CoreClient>) -> Arc<LiveStudies> {
        Arc::new(LiveStudies {
            core,
            sessions: StdMutex::new(Vec::new()),
        })
    }

    /// The session most recently started. What `GET /api/live/events` with
    /// no `study` falls back to, so the tab can be opened straight onto a
    /// run that is already going.
    pub fn latest(&self) -> Option<Arc<LiveSession>> {
        self.sessions.lock().unwrap().last().cloned()
    }

    /// The session for `study_id`, starting a follow if there is not one.
    ///
    /// Idempotent on purpose: a run registers one, and opening a running
    /// study from the studies list registers the same one. **One
    /// subscription per study, never one per browser.**
    pub fn ensure(self: &Arc<Self>, study_id: &str) -> Arc<LiveSession> {
        {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(existing) = sessions.iter().find(|s| s.study_id == study_id) {
                return existing.clone();
            }
            let session = Arc::new(LiveSession::new(study_id.to_string()));
            sessions.push(session.clone());
            // Evict finished sessions oldest-first. A running one is never
            // evicted: its follow task is still writing to it.
            while sessions.len() > MAX_SESSIONS {
                match sessions.iter().position(|s| s.is_finished()) {
                    Some(i) => {
                        sessions.remove(i);
                    }
                    None => break,
                }
            }
            drop(sessions);
            tokio::spawn(follow(self.core.clone(), session.clone()));
            session
        }
    }
}

/// Follows one study to its end, writing everything that arrives into the
/// session and broadcasting an incremental frame per item.
///
/// Uses the shared client's own `follow_study`, which already subscribes,
/// polls once for a study that was over before we arrived, falls back to
/// polling on a drop, and reports `lagged` — the same mechanism
/// `embarch-api`'s `study_watch` runs on. Re-implementing it here would mean
/// two copies of the one piece of logic that has to be right about missing
/// data.
async fn follow(core: Arc<CoreClient>, session: Arc<LiveSession>) {
    let options = FollowOptions {
        // No deadline: a study runs as long as it runs, and Core's own
        // watchdog is what bounds a hung one. The 30-minute backstop the
        // polled watcher used is gone with it — it ended a *watch*, not a
        // study, and a person watching a long soak would have lost the feed.
        deadline: None,
        ..FollowOptions::default()
    };

    let outcome = core
        .follow_study(&session.study_id, &options, |item| {
            let payload = ingest(&session, item);
            if let Some(payload) = payload {
                session.send(payload);
            }
        })
        .await;

    // A terminal status means the authoritative record exists. The SSE frame
    // carries no `StudyResult`, so provenance and the stream list can only
    // come from here (`embarch-core` decision 24 — Core reads it off disk on
    // demand rather than holding one).
    let terminal = match &outcome {
        Ok(o) => o.terminal_status.clone(),
        Err(_) => None,
    };
    if terminal.is_some() {
        match core.get_study_status(&session.study_id).await {
            Ok(status) => {
                let mut state = session.state.lock().unwrap();
                if let Some(result) = status.result {
                    state.provenance = Some(
                        serde_json::to_value(crate::study_designer::provenance_view(
                            &result.provenance,
                        ))
                        .unwrap_or(Value::Null),
                    );
                    state.streams = serde_json::to_value(&result.streams).ok();
                }
            }
            Err(e) => {
                let mut state = session.state.lock().unwrap();
                state.record_note = Some(format!(
                    "the study finished, but its record could not be read back: {e:#}"
                ));
            }
        }
    }

    let payload = {
        let mut state = session.state.lock().unwrap();
        state.finished = true;
        match &outcome {
            Ok(o) => {
                if let Some(status) = &o.terminal_status {
                    state.status = status.clone();
                }
                if o.reason.is_some() {
                    state.reason = o.reason.clone();
                }
            }
            Err(e) => {
                // The follow itself failed — which is a fact about this
                // watch, not about the study. Said as one: the status stays
                // whatever Core last reported.
                state.record_note = Some(format!("watching this study stopped: {e:#}"));
            }
        }
        frame(
            "status",
            json!({
                "status": state.status_json(),
                "provenance": state.provenance,
                "streams": state.streams,
            }),
        )
    };
    session.send(payload);
}

/// One item off the follow, folded into the session. Returns the frame to
/// broadcast, if the item produces one.
fn ingest(session: &LiveSession, item: FollowItem) -> Option<String> {
    let mut state = session.state.lock().unwrap();
    match item {
        FollowItem::Transport { mode, detail } => {
            state.mode = match mode {
                FollowMode::Live => "live",
                FollowMode::Polling => "polling",
            };
            state.mode_detail = Some(detail.clone());
            let row = state.push_feed("transport", detail);
            Some(frame("status", json!({ "status": state.status_json(), "row": row })))
        }
        FollowItem::Lagged { missed } => {
            state.lagged += missed;
            let row = state.push_feed(
                "lagged",
                format!(
                    "embarch-core dropped {missed} event(s) from this feed — the study is \
                     unaffected and its record on disk is complete"
                ),
            );
            Some(frame("status", json!({ "status": state.status_json(), "row": row })))
        }
        FollowItem::Unrecognized { event, reason, .. } => {
            let row = state.push_feed("unrecognized", format!("frame '{event}': {reason}"));
            Some(frame("event", json!({ "row": row })))
        }
        FollowItem::Polled { status, current_step, total_steps, reason } => {
            state.status = status.clone();
            state.current_step = current_step;
            state.total_steps = total_steps;
            state.reason = reason;
            let row = state.push_feed("polled", format!("polled: {status}"));
            Some(frame("status", json!({ "status": state.status_json(), "row": row })))
        }
        FollowItem::Event(StudyEvent::StatusChanged { status, reason, .. }) => {
            state.status = status.clone();
            if reason.is_some() {
                state.reason = reason.clone();
            }
            let row = state.push_feed(
                "status",
                match &reason {
                    Some(r) => format!("status: {status} — {r}"),
                    None => format!("status: {status}"),
                },
            );
            Some(frame("status", json!({ "status": state.status_json(), "row": row })))
        }
        FollowItem::Event(StudyEvent::StepCompleted {
            step_index,
            result,
            started_utc_ms,
            ended_utc_ms,
            delay_before_ms,
            ..
        }) => {
            // The step's own record, verbatim, so `assets/app.js` renders it
            // with the same `outcomeBadge`/`stepDetail` it uses for a study
            // read back from disk (`embarch-ui` decisions 20 and 23). A
            // second rendering here is the defect those decisions exist to
            // stop.
            let mut row_json = serde_json::to_value(&*result).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut row_json {
                map.insert("index".to_string(), json!(step_index));
            }
            let label = format!(
                "step {}: {}",
                step_index + 1,
                result.step_name.as_str()
            );
            if state.steps.len() < MAX_STEPS {
                state.steps.push(row_json.clone());
            }
            // Both stamps or neither: a band with one end is not a band, and an
            // older Core sends neither. `None` is the study predating them,
            // which the chart renders as "no step row" rather than as a gap.
            if let (Some(from), Some(to)) = (started_utc_ms, ended_utc_ms) {
                if state.step_stamps.len() < MAX_STEPS {
                    state.step_stamps.push(trace::StepStamp {
                        index: step_index as usize,
                        name: result.step_name.as_str().to_string(),
                        outcome: match &result.outcome {
                            embarch_study_designer::Outcome::Pass => "Pass".to_string(),
                            embarch_study_designer::Outcome::TimedOut => "TimedOut".to_string(),
                            embarch_study_designer::Outcome::Fail { .. } => "Fail".to_string(),
                        },
                        reason: match &result.outcome {
                            embarch_study_designer::Outcome::Fail { reason } => {
                                Some(reason.as_str().to_string())
                            }
                            _ => None,
                        },
                        delay_before_ms: delay_before_ms.unwrap_or(0),
                        started_utc_ms: from,
                        ended_utc_ms: to,
                    });
                }
            }
            // `current_step` keeps Core's own meaning — the 0-based index of
            // the last step that *finished* (`embarch-core` decision 43).
            // Nothing here renumbers it.
            state.current_step = Some(step_index);
            let feed = state.push_feed("step", label);
            Some(frame(
                "step",
                json!({ "step": row_json, "status": state.status_json(), "row": feed }),
            ))
        }
        FollowItem::Event(StudyEvent::SampleBatch { stream_name, samples, .. }) => {
            let series = state.series.entry(stream_name.clone()).or_default();
            let mut added: Vec<[f64; 2]> = Vec::new();
            for sample in &samples {
                let unit = serde_json::to_value(sample.unit)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()));
                if series.push(sample.rx_utc_ms, f64::from(sample.value), unit.as_deref()) {
                    added.push([sample.rx_utc_ms as f64, f64::from(sample.value)]);
                }
            }
            let stride = series.stride.max(1);
            let total = series.total;
            let feed = state.push_feed(
                "samples",
                format!("{stream_name}: {} sample(s)", samples.len()),
            );
            Some(frame(
                "samples",
                json!({
                    "tap": stream_name,
                    "points": added,
                    "stride": stride,
                    "total": total,
                    "row": feed,
                }),
            ))
        }
        FollowItem::Event(StudyEvent::GattTranscript {
            step_index,
            entry,
            core_rx_utc_ms,
            ..
        }) => {
            // The entry's own `rx_utc_ms` is dev-bench uptime, so it is not a
            // clock this mark can be laid against anything else on. An older
            // Core pushes no `core_rx_utc_ms` at all, and a mark with no clock
            // is one this chart does not draw rather than one it guesses at.
            if let Some(stamp) = core_rx_utc_ms {
                state.push_mark(
                    "gatt:gatt",
                    "gatt",
                    "gatt",
                    LiveMark {
                        core_rx_utc_ms: stamp,
                        sub: format!("{} {}", entry.direction.as_str(), entry.kind.as_str()),
                        label: format!(
                            "{} {} · {} · {} bytes",
                            entry.direction.as_str(),
                            entry.kind.as_str(),
                            entry
                                .characteristic_uuid
                                .map(|u| u.to_hyphenated().to_string())
                                .unwrap_or_else(|| "(no characteristic)".to_string()),
                            entry.payload.len()
                        ),
                    },
                );
            }
            let feed = state.push_feed(
                "gatt",
                format!(
                    "step {}: {} {} ({} byte payload)",
                    step_index + 1,
                    entry.direction.as_str(),
                    entry.kind.as_str(),
                    entry.payload.len()
                ),
            );
            Some(frame(
                "gatt",
                json!({
                    "step_index": step_index,
                    "entry": serde_json::to_value(&*entry).unwrap_or(Value::Null),
                    "row": feed,
                }),
            ))
        }
        FollowItem::Event(StudyEvent::OutpostRows {
            stream_name,
            header_seen,
            rows,
            ..
        }) => {
            // **The axis, built as the trace arrives** — `embarch-outpost`
            // decision 10's live half, on this side. The rows decode through
            // `trace::parse_rows`, which is literally the function the
            // post-hoc path uses on the rendered file; that is the whole point
            // of Core pushing CSV rows rather than a struct.
            let bumped = state.axis.ingest(&stream_name, header_seen, &rows);
            state.time_chart_frame(bumped)
        }
        FollowItem::Event(StudyEvent::StreamText {
            stream_name,
            step_index,
            rx_utc_ms,
            core_rx_utc_ms,
            text,
            ..
        }) => {
            let mut seq = state.seq;
            let console = state.consoles.entry(stream_name.clone()).or_default();
            let completed = console.push_chunk(&mut seq, step_index, rx_utc_ms, &text);
            let partial = console.partial.clone();
            let dropped = console.dropped;
            let total = console.total;
            state.seq = seq;
            // **A console line is placeable live because Core stamps the chunk
            // that carried it**, and every line completed by one chunk shares
            // that chunk's instant — which is exactly as precise as the
            // recording is, and is the same rule the post-hoc path applies to
            // the arrival sidecar. `rx_utc_ms` is not used for this: on a
            // bench-mediated tap it is dev-bench uptime (suite decision 3).
            let stamp = core_rx_utc_ms.unwrap_or(rx_utc_ms);
            let key = format!("console:{stream_name}");
            for line in &completed {
                state.push_mark(
                    &key,
                    &stream_name,
                    "console",
                    LiveMark {
                        core_rx_utc_ms: stamp,
                        sub: String::new(),
                        label: line.text.chars().take(160).collect(),
                    },
                );
            }
            // No feed row per chunk: a console is chatty by nature and one
            // feed row per chunk would bury every step completion. The
            // console card is where its content belongs.
            Some(frame(
                "console",
                json!({
                    "tap": stream_name,
                    "lines": completed,
                    "partial": partial,
                    "dropped": dropped,
                    "total": total,
                }),
            ))
        }
    }
}

// ---- routes -----------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct LiveEventsQuery {
    /// Which study to watch. Omitted means "whatever is running or ran last
    /// in this process", which is what makes the tab useful the moment it is
    /// opened after a run was started elsewhere in the UI.
    study: Option<String>,
}

/// `GET /api/live/events` — the Live Study tab's stream.
///
/// **A full snapshot frame on connect, then incremental frames.** That
/// ordering is the whole feature: a browser that opens the tab mid-run, or
/// reloads it, replays the run so far from this process's own rings rather
/// than starting at "now" with a hole Core cannot fill (`embarch-core`
/// decision 41).
pub async fn api_live_events(
    State(state): State<crate::AppState>,
    axum::extract::Query(q): axum::extract::Query<LiveEventsQuery>,
) -> axum::response::Response {
    let session = match q.study {
        // Opening a study by id registers a session for it if there is not
        // one — that is what makes "open a study Core reports as running"
        // work from a fresh browser.
        Some(id) if !id.is_empty() => state.live.ensure(&id),
        _ => match state.live.latest() {
            Some(session) => session,
            None => {
                return (
                    axum::http::StatusCode::NOT_FOUND,
                    "no study has been run or opened in this embarch-ui process yet",
                )
                    .into_response()
            }
        },
    };

    let snapshot = session.snapshot();
    let rx = session.subscribe();
    let stream = futures_util::stream::unfold(
        (rx, Some(snapshot)),
        |(mut rx, pending)| async move {
            if let Some(first) = pending {
                let event = Event::default().event("live").data(first);
                return Some((Ok::<Event, Infallible>(event), (rx, None)));
            }
            {
                match rx.recv().await {
                    Ok(payload) => {
                        let event = Event::default().event("live").data(payload);
                        Some((Ok(event), (rx, None)))
                    }
                    // This *browser* fell behind embarch-ui's own broadcast,
                    // which is a different fact from Core's `lagged` and is
                    // reported as its own frame rather than folded into it.
                    // The rings are intact, so the remedy is a reload, and
                    // the frame says so.
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        let event = Event::default().event("live").data(
                            json!({
                                "kind": "browser_lagged",
                                "missed": missed,
                            })
                            .to_string(),
                        );
                        Some((Ok(event), (rx, None)))
                    }
                    Err(broadcast::error::RecvError::Closed) => None,
                }
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// `POST /api/live/run` — run a saved study from the Live Study tab.
///
/// The Study Designer still owns building and validating a study: it holds
/// the project, the registry, the struct layouts and the `.eap` protocols,
/// and re-deriving any of that here would be a second authority on what a
/// saved file means. **Only the hand-off changed** — a run registers a
/// [`LiveSession`] instead of writing a single process-wide `RunState`.
pub async fn api_live_run(
    State(state): State<crate::AppState>,
    axum::extract::Json(req): axum::extract::Json<LiveRunRequest>,
) -> axum::response::Response {
    crate::study_designer::run_saved_study(&state, &req.slug, req.allow_version_mismatch).await
}

#[derive(Debug, serde::Deserialize)]
pub struct LiveRunRequest {
    pub slug: String,
    #[serde(default)]
    pub allow_version_mismatch: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_that_ends_mid_line_is_held_as_partial_and_never_padded() {
        // The console invariant: this module assembles lines, and a chunk
        // that has not finished one is shown as partial rather than
        // completed. Core frames nothing (`embarch-core` decision 70) and
        // neither does this until a newline actually arrives.
        let mut console = Console::default();
        let mut seq = 0;

        let done = console.push_chunk(&mut seq, 0, 10, "uart:~$ hal");
        assert!(done.is_empty(), "no newline arrived, so no line completed");
        assert_eq!(console.partial, "uart:~$ hal");
        assert_eq!(console.total, 0);

        let done = console.push_chunk(&mut seq, 0, 11, "f a line\nand the start of");
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].text, "uart:~$ half a line");
        // The line carries the stamp of the chunk it *started* in, not the
        // one that finished it.
        assert_eq!(done[0].rx_utc_ms, 10);
        assert_eq!(console.partial, "and the start of");
    }

    #[test]
    fn a_crlf_console_does_not_leave_a_carriage_return_on_every_line() {
        let mut console = Console::default();
        let mut seq = 0;
        let done = console.push_chunk(&mut seq, 0, 1, "one\r\ntwo\r\n");
        assert_eq!(done.len(), 2);
        assert_eq!(done[0].text, "one");
        assert_eq!(done[1].text, "two");
    }

    #[test]
    fn a_console_over_its_cap_drops_the_oldest_and_counts_what_it_dropped() {
        // A capped ring that said nothing would make a short console and a
        // complete one indistinguishable.
        let mut console = Console::default();
        let mut seq = 0;
        for i in 0..(MAX_CONSOLE_LINES + 10) {
            console.push_chunk(&mut seq, 0, i as u64, &format!("line {i}\n"));
        }
        assert_eq!(console.lines.len(), MAX_CONSOLE_LINES);
        assert_eq!(console.dropped, 10);
        assert_eq!(console.total, MAX_CONSOLE_LINES as u64 + 10);
        assert_eq!(console.lines.front().unwrap().text, "line 10");
    }

    #[test]
    fn one_enormous_line_is_cut_and_says_it_was_cut() {
        let mut console = Console::default();
        let mut seq = 0;
        let huge = "x".repeat(MAX_CONSOLE_LINE_BYTES * 2);
        let done = console.push_chunk(&mut seq, 0, 1, &format!("{huge}\n"));
        assert_eq!(done.len(), 1);
        assert!(done[0].truncated, "a cut line that read as whole would be a lie");
        assert_eq!(done[0].text.len(), MAX_CONSOLE_LINE_BYTES);
    }

    #[test]
    fn a_series_past_its_cap_decimates_rather_than_dropping_the_oldest() {
        // Dropping the oldest would silently change what window the plot
        // covers as the run goes on. Decimating keeps the whole run and
        // says, in `stride`, that it is showing one point in N.
        let mut series = Series::default();
        for i in 0..(MAX_SERIES_POINTS + 1) {
            series.push(i as u64, i as f64, Some("Milliamps"));
        }
        assert!(series.points.len() <= MAX_SERIES_POINTS);
        assert_eq!(series.stride, 2);
        // The first sample survives — that is the whole difference from
        // dropping the oldest.
        assert_eq!(series.points[0], [0.0, 0.0]);
        assert_eq!(series.total, MAX_SERIES_POINTS as u64 + 1);
    }

    #[test]
    fn a_decimated_series_keeps_sampling_at_its_stride() {
        let mut series = Series { stride: 4, ..Default::default() };
        assert!(!series.push(1, 1.0, None));
        assert!(!series.push(2, 2.0, None));
        assert!(!series.push(3, 3.0, None));
        assert!(series.push(4, 4.0, None), "one in four is kept");
        assert_eq!(series.points.len(), 1);
        assert_eq!(series.total, 4);
    }

    #[test]
    fn the_feed_ring_counts_its_drops() {
        let mut state = SessionState::default();
        for i in 0..(MAX_FEED_EVENTS + 5) {
            state.push_feed("step", format!("row {i}"));
        }
        assert_eq!(state.feed.len(), MAX_FEED_EVENTS);
        assert_eq!(state.feed_dropped, 5);
        assert_eq!(state.feed_total, MAX_FEED_EVENTS as u64 + 5);
    }

    #[test]
    fn a_snapshot_carries_the_whole_run_so_far() {
        // The one frame that makes opening the tab mid-run work.
        let session = LiveSession::new("abc".to_string());
        {
            let mut state = session.state.lock().unwrap();
            state.status = "running".to_string();
            state.push_feed("step", "step 1: connect".to_string());
            let mut seq = state.seq;
            let console = state.consoles.entry("dev-bench".to_string()).or_default();
            console.push_chunk(&mut seq, 0, 1, "booted\npartial");
            state.seq = seq;
            state.series.entry("rail".to_string()).or_default().push(1, 1.5, Some("Milliamps"));
        }

        let snapshot: Value = serde_json::from_str(&session.snapshot()).unwrap();
        assert_eq!(snapshot["kind"], "snapshot");
        assert_eq!(snapshot["study_id"], "abc");
        assert_eq!(snapshot["status"]["status"], "running");
        assert_eq!(snapshot["feed"].as_array().unwrap().len(), 1);
        let console = &snapshot["consoles"][0];
        assert_eq!(console["tap"], "dev-bench");
        assert_eq!(console["lines"][0]["text"], "booted");
        assert_eq!(console["partial"], "partial", "the remainder is published as partial");
        assert_eq!(snapshot["series"][0]["points"][0][1], 1.5);
    }

    #[test]
    fn lagged_is_carried_into_the_status_rather_than_swallowed() {
        let session = LiveSession::new("abc".to_string());
        let payload = ingest(&session, FollowItem::Lagged { missed: 12 }).unwrap();
        let value: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["status"]["lagged"], 12);
        assert_eq!(value["row"]["kind"], "lagged");

        // And it accumulates: two lagged frames are 12 + 3 missed, not 3.
        ingest(&session, FollowItem::Lagged { missed: 3 });
        assert_eq!(session.state.lock().unwrap().lagged, 15);
    }

    #[test]
    fn a_transport_change_is_recorded_so_the_tab_can_say_which_mode_is_in_force() {
        let session = LiveSession::new("abc".to_string());
        ingest(
            &session,
            FollowItem::Transport {
                mode: FollowMode::Polling,
                detail: "the event stream would not open; polling instead".to_string(),
            },
        );
        let state = session.state.lock().unwrap();
        assert_eq!(state.mode, "polling");
        assert!(state.mode_detail.as_deref().unwrap().contains("polling instead"));
    }

    // ---- the live axis ---------------------------------------------------

    /// One frame's worth of pushed rows, in `outpost::csv_header()`'s shape.
    /// `us` is what makes a row anchor the axis; an empty one is a row that
    /// arrived before the capture's header.
    fn trace_rows(frame: u64, rx_utc_ms: u64, us: &[&str]) -> Vec<String> {
        us.iter()
            .map(|u| {
                format!("{frame},1,{rx_utc_ms},1000,{u},thread_switch_in,3,0,main")
            })
            .collect()
    }

    /// **A non-monotone anchor is rejected and counted, never inserted.**
    /// `current_utc_ms()` is not monotonic — an NTP correction mid-capture
    /// makes an arrival go backwards — and the post-hoc path sorts and dedups
    /// its anchors, which an append-only live push cannot do. Inserting one
    /// would break the projection's binary-search precondition and land a mark
    /// anywhere.
    #[test]
    fn a_non_monotone_anchor_is_rejected_and_counted() {
        let mut axis = LiveAxis::default();
        axis.ingest("outpost", true, &trace_rows(0, 1_000, &["10000"]));
        axis.ingest("outpost", true, &trace_rows(1, 1_010, &["20000"]));
        assert_eq!(axis.anchors.len(), 2);

        // The wall clock steps back 40 ms between two frames.
        axis.ingest("outpost", true, &trace_rows(2, 970, &["30000"]));
        assert_eq!(axis.anchors.len(), 2, "a backwards arrival must not be inserted");
        assert_eq!(axis.non_monotone, 1, "and it must be counted, not swallowed");

        // A later, forward one still lands.
        axis.ingest("outpost", true, &trace_rows(3, 1_020, &["30000"]));
        assert_eq!(axis.anchors.len(), 3);
    }

    /// **The axis does not exist until the trace has given it two frames
    /// carrying both clocks**, and a study that declares a trace says it is
    /// waiting rather than drawing a guessed one.
    #[test]
    fn the_axis_waits_for_the_trace_rather_than_being_invented() {
        let mut axis = LiveAxis::default();
        assert!(!axis.projection().placeable());

        // A frame before the header: the rows carry a cycle count and no rate
        // to divide it by, so `us` is empty and nothing anchors.
        axis.ingest("outpost", false, &trace_rows(0, 1_000, &["", ""]));
        assert!(!axis.projection().placeable());
        assert!(axis.describe(&axis.projection())["note"]
            .as_str()
            .unwrap()
            .contains("header frame has not"));

        // One stamped frame still is not a tie between two clocks.
        axis.ingest("outpost", true, &trace_rows(1, 1_010, &["10000"]));
        assert!(!axis.projection().placeable());

        // Two is.
        axis.ingest("outpost", true, &trace_rows(2, 1_020, &["20000"]));
        let proj = axis.projection();
        assert!(proj.placeable() && proj.projected());
        assert_eq!(proj.place(1_015), Some(15_000));
    }

    /// **The header arriving promotes the tier, and a promotion is an epoch
    /// bump rather than a silent move.** Everything already drawn was drawn
    /// with no axis at all; the browser re-snapshots rather than receiving
    /// marks on an axis it has never seen.
    #[test]
    fn the_header_arriving_bumps_the_epoch_rather_than_promoting_silently() {
        let mut axis = LiveAxis::default();
        assert!(!axis.ingest("outpost", false, &trace_rows(0, 1_000, &[""])));
        assert_eq!(axis.epoch, 0);
        assert!(axis.ingest("outpost", true, &trace_rows(1, 1_010, &["10000"])));
        assert_eq!(axis.epoch, 1, "the tier improved, so the epoch moved with it");
        // And it moves once, not on every frame after it.
        assert!(!axis.ingest("outpost", true, &trace_rows(2, 1_020, &["20000"])));
        assert_eq!(axis.epoch, 1);
    }

    /// **A mark is never placed eagerly, and a mark past the leading edge is a
    /// count rather than a position.** A lane hands out only what the axis can
    /// place *now*, walks forward, and stops at the first one it cannot —
    /// because anchors are monotone, so is everything after it.
    #[test]
    fn a_mark_past_the_leading_edge_is_pending_and_is_placed_when_the_axis_reaches_it() {
        let mut axis = LiveAxis::default();
        axis.ingest("outpost", true, &trace_rows(0, 1_000, &["10000"]));
        axis.ingest("outpost", true, &trace_rows(1, 1_010, &["20000"]));

        let mut lane = LiveLane { label: "gatt".into(), kind: "gatt", ..Default::default() };
        for ms in [1_005u64, 1_009, 1_040] {
            lane.push(LiveMark { core_rx_utc_ms: ms, sub: String::new(), label: String::new() });
        }

        let (placed, pending) = lane.take_placeable(&axis.projection(), true);
        assert_eq!(placed.len(), 2, "two marks are inside the axis so far");
        assert_eq!(pending, 1, "the third is past its leading edge and is counted, not drawn");
        // Halfway between the two anchors in the host clock, so halfway
        // between their DUT stamps — the projection interpolates, it does not
        // re-origin.
        assert_eq!(placed[0]["t"], 15_000);
        assert!(placed[0]["uncertain"].as_bool().unwrap());

        // The axis grows past it, and only then is it placed — once.
        axis.ingest("outpost", true, &trace_rows(2, 1_050, &["60000"]));
        let (placed, pending) = lane.take_placeable(&axis.projection(), true);
        assert_eq!(placed.len(), 1, "exactly the one that was pending");
        assert_eq!(pending, 0);
        // And a mark already sent is never re-sent, which is what stops a
        // reader watching a mark move.
        assert_eq!(lane.take_placeable(&axis.projection(), true).0.len(), 0);
    }

    /// A live console line and a live GATT entry land on the same lane keys the
    /// post-hoc chart uses, so a run watched live and the same run reloaded are
    /// one chart rather than two.
    #[test]
    fn a_live_console_line_becomes_a_mark_on_the_lane_the_reload_will_use() {
        let session = LiveSession::new("abc".to_string());
        ingest(
            &session,
            FollowItem::Event(StudyEvent::StreamText {
                study_id: "abc".to_string(),
                stream_id: 1,
                stream_name: "dev-bench".to_string(),
                step_index: 0,
                rx_utc_ms: 5,
                core_rx_utc_ms: Some(1_700_000_000_005),
                text: "booted\nready\n".to_string(),
            }),
        );
        let state = session.state.lock().unwrap();
        let lane = state.lanes.get("console:dev-bench").expect("the console lane exists");
        assert_eq!(lane.total, 2, "one mark per completed line");
        assert_eq!(lane.kind, "console");
        // Unplaced, deliberately: there is no axis yet, and the mark holds the
        // one thing that will place it later.
        assert_eq!(lane.marks[0].core_rx_utc_ms, 1_700_000_000_005);
    }

    #[test]
    fn a_text_event_produces_a_console_frame_and_no_feed_row() {
        // One feed row per console chunk would bury every step completion,
        // and a chatty dev-bench is present in every study.
        let session = LiveSession::new("abc".to_string());
        let payload = ingest(
            &session,
            FollowItem::Event(StudyEvent::StreamText {
                study_id: "abc".to_string(),
                stream_id: 1,
                stream_name: "dev-bench".to_string(),
                step_index: 0,
                rx_utc_ms: 5,
                core_rx_utc_ms: Some(1_700_000_000_005),
                text: "hello\n".to_string(),
            }),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["kind"], "console");
        assert_eq!(value["lines"][0]["text"], "hello");
        assert_eq!(session.state.lock().unwrap().feed.len(), 0);
    }
}
