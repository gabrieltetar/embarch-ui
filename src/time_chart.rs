//! The Time chart: **one axis, and everything a study did on it.**
//!
//! The Live Study tab already showed everything a study produced and showed
//! none of it *together* — steps in a table, GATT in a table, consoles in two
//! scrolling panes, a plot per data tap, and the outpost trace on an axis of
//! its own. Answering "what was the DUT doing when that notification arrived"
//! meant reading four surfaces and holding the times in your head.
//!
//! This is the one surface that answers it: step bands, GATT activity, console
//! lines, data-tap records, sample strips and the trace's own markers, all
//! placed on a single X axis.
//!
//! # The one thing this module exists not to do
//!
//! **Never read a study CSV's `rx_utc_ms`.** One field name carries two
//! different clocks in this suite (suite decision 3): in an outpost trace it is
//! embarch-core's real epoch clock, and in a study's `data.csv`/`gatt.csv`/
//! struct rows it is **dev-bench uptime** — milliseconds since that board
//! booted, no epoch, no offset ([`embarch-study-designer` decision 72]). The
//! two are not comparable and joining on them is exactly the error class that
//! has already cost this suite a **46× misreport** of the instrument's own run
//! time (`embarch-decision-reversals.md` row 86).
//!
//! So this module reads **`core_rx_utc_ms`** — the column embarch-core appends
//! itself, its own receipt time — and nothing else. [`axis_column`] is the
//! accessor, and it returns `None` rather than falling back: a stream with no
//! Core stamp is a stream this chart says it cannot place, never one it places
//! against the wrong clock.
//!
//! # One clock family, one foreigner
//!
//! Step edges, outpost frame arrivals and `core_rx_utc_ms` on sample, GATT and
//! struct rows are all `current_utc_ms()` in the same embarch-core process. So
//! steps, GATT, console and samples need **no projection relative to each
//! other**. Only the trace crosses a clock boundary, and
//! [`trace::Projection`] is that one crossing.
//!
//! # Two axes, chosen once
//!
//! - **With a usable trace, the DUT's own counter draws the axis**, and
//!   everything else is projected onto it through the frames that carry both
//!   clocks — carrying `accuracy_ms`, the capture's own resolution, because
//!   that is how finely the two clocks are tied together.
//! - **Without one, embarch-core's receipt clock is the axis** and every mark
//!   is exact.
//!
//! **A tier is never silently promoted.** The axis is chosen once per view; an
//! improvement is an [`TimeChartView::axis_epoch`] bump and a visible redraw,
//! never marks quietly moving under a reader who has already looked at them. A
//! completed study's axis cannot change at all, which is what
//! `axis_epoch == 0` says.
//!
//! # Three states for a mark, and the middle one is the point
//!
//! A [`Mark`] is placed, or it is not. `t: None` is drawn as a **count in the
//! gutter**, never at a guessed position — the same posture `placeable: false`
//! already takes on the trace's step row. **A band clamps and a point does
//! not**: clamping a band to the capture's edge is still a truthful drawing of
//! a step that ran past it, but clamping a point mark would draw it at a time
//! it was not at.
//!
//! This matters more than it sounds. A tap's scope is routinely narrower than
//! the study, and [`trace::Projection::place`] refuses outside the *anchors'*
//! range — the trace's range, not the study's. On a trace scoped to two steps
//! of a twelve-step study, **most marks are unplaceable**, and that is the
//! real cost of drawing on the DUT's clock rather than a defect in this code.
//!
//! # Detail is fetched, not carried
//!
//! A mark carries a label and an id, never a payload. Its `id` is
//! `(lane << 40) | row_index`, so "mark 7 of the gatt lane" and "row 7 of the
//! gatt table" are the same object *by construction* — `GET .../mark/{id}` is
//! a thin dispatcher over reads that already exist. Carrying payloads inline
//! would reproduce decision 18's 12.6 MB defect exactly.
//!
//! [`embarch-study-designer` decision 72]: ../../embarch-doc/embarch-study-designer/decisions/versioning.md

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use embarch_study_designer::StreamEncoding;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::studies_api::Table;
use crate::trace::{self, Projection, StepBand, TraceView};
use crate::AppState;

/// Marks kept per lane. A study long enough to overflow this is a real thing,
/// and the answer is to say so rather than to draw a silently-shortened lane —
/// see [`MarkLane::dropped_by_cap`], which the tab renders beside the lane.
const MAX_MARKS_PER_LANE: usize = 200_000;
/// Points kept per sample tap. A 10 kHz rail over a two-minute study is 1.2 M
/// of them; past this the strip is drawn from a prefix and says so.
const MAX_SERIES_POINTS: usize = 500_000;
/// Bins in one windowed answer, matching the trace's own cap. A plot is at most
/// a couple of thousand CSS pixels wide and a bin narrower than a pixel is not
/// a bin.
const MAX_BINS: usize = 4_000;
/// Console lines counted for an unplaceable lane's total. Past this the lane
/// says "more than", which costs a reader nothing they were going to act on.
const MAX_CONSOLE_LINES_COUNTED: usize = 2_000_000;

/// How far a lane index is shifted to make a mark id. 40 bits of row index is
/// 1.1 × 10¹² rows, which is far past [`MAX_MARKS_PER_LANE`] and past anything
/// a capture holds; the remaining 24 bits are 16.7 M lanes against a realistic
/// dozen.
const LANE_SHIFT: u32 = 40;
const ROW_MASK: u64 = (1u64 << LANE_SHIFT) - 1;

fn mark_id(lane: usize, row: usize) -> u64 {
    ((lane as u64) << LANE_SHIFT) | (row as u64 & ROW_MASK)
}

fn split_mark_id(id: u64) -> (usize, usize) {
    ((id >> LANE_SHIFT) as usize, (id & ROW_MASK) as usize)
}

// ---- the view ---------------------------------------------------------------

/// The axis every lane below is drawn on, and what a reader must know to read
/// a position off it.
#[derive(Debug, Clone, Serialize)]
pub struct Axis {
    /// `"us"` when the DUT's own counter drew it, `"ms"` when embarch-core's
    /// receipt clock did. Every `t` in this view is in these units.
    pub unit: &'static str,
    /// Which clock drew the axis. Four values, and the last two are not the
    /// same fact:
    ///
    /// - `"dut-cycles"` — a trace's own microsecond counter.
    /// - `"host-arrival"` — a trace's **frame arrivals**, which is Core's clock
    ///   reached through the capture; the capture's own records did not carry
    ///   a usable DUT stamp.
    /// - `"core-clock"` — there is no trace, and Core's clock **is** the axis
    ///   directly. Every mark is exact and nothing is projected.
    /// - `"none"` — nothing in this study carries a clock at all.
    pub axis_clock: &'static str,
    pub t_from: u64,
    pub t_to: u64,
    /// True when a placement is an interpolation across two clocks rather than
    /// the identity — so every mark carries [`Self::accuracy_ms`] with it.
    pub projected: bool,
    /// How closely a projected instant can be placed, in milliseconds.
    /// `None` when nothing is projected, where a placement is exact.
    pub accuracy_ms: Option<f64>,
    /// False when this study has nothing to draw an axis with at all.
    pub placeable: bool,
    /// What drew it, in a reader's words — a tap name, or embarch-core.
    pub source: String,
    /// The same window in embarch-core's own receipt milliseconds, which is
    /// the unit every stream but the trace is stamped in. Served so the tab can
    /// say *when* a window is, not only where in the capture.
    pub window_from_ms: u64,
    pub window_to_ms: u64,
    /// The sentence the tab renders under the chart. Written here rather than
    /// in `app.js` for the same reason the trace's axis note is: which clock
    /// drew this and what that costs is this side's finding.
    pub note: String,
}

/// One discrete thing that happened, and where it lands on the axis.
///
/// **Three states, and the middle one is the point.** `t: Some` is placed;
/// `t: None` is a real event this axis has no position for, drawn as a gutter
/// count and never at a guessed position. The third state is `uncertain`: a
/// placed mark on a projected axis, good to the axis's `accuracy_ms` and not
/// to the microsecond the lanes beneath it are drawn at.
#[derive(Debug, Clone, Serialize)]
pub struct Mark {
    /// Position in [`Axis::unit`]s, or `None` where this axis cannot place it.
    pub t: Option<u64>,
    /// embarch-core's own receipt stamp for this event. `None` on a trace
    /// marker, which is stamped by the DUT and is natively on this axis
    /// already — there is no crossing to record.
    pub core_rx_utc_ms: Option<u64>,
    /// `"gatt"`, `"struct"`, `"console"` or `"marker"`.
    pub kind: &'static str,
    /// A short qualifier within the lane — a GATT row's direction and kind, a
    /// struct row's decode state, a marker's own name. Rendered as given.
    pub sub: String,
    /// Placed through a cross-clock projection, so its position carries the
    /// axis's `accuracy_ms`.
    pub uncertain: bool,
    /// `(lane << 40) | row_index`. The row index is the row's position in the
    /// tap's own rendered table, so this id and a table row are the same object
    /// by construction rather than by a lookup that could drift.
    pub id: u64,
    pub label: String,
}

/// One lane of marks, and the honest accounting of what is not drawn in it.
#[derive(Debug, Clone, Serialize)]
pub struct MarkLane {
    /// Stable identity, which is what a binned reply is matched on.
    pub key: String,
    pub label: String,
    /// `"gatt"`, `"struct"`, `"console"` or `"marker"`.
    pub kind: &'static str,
    /// How `GET .../mark/{id}` reaches this lane's detail: `"rows"` for a
    /// rendered table, `"text"` for a console, `"trace-markers"` for the
    /// capture's own markers.
    pub source: &'static str,
    /// The tap this lane came from. Empty for the trace's marker lane, which
    /// belongs to the trace as a whole.
    pub tap: String,
    /// Every event in this lane, placed or not.
    pub total: usize,
    /// Events this axis has a position for.
    pub placed: usize,
    /// Events before the axis window opens, and after it closes.
    ///
    /// **Two counts, not one.** "214 console lines before this trace opened" is
    /// a different fact from 3 after it closed, and a single "unplaceable"
    /// number would hide which end a reader should widen the tap towards.
    pub before: usize,
    pub after: usize,
    /// Events past [`MAX_MARKS_PER_LANE`]. Never silently discarded.
    pub dropped_by_cap: usize,
    /// Why this lane draws nothing, or draws less than it holds. Prose, for a
    /// person.
    pub note: Option<String>,
    /// **Never serialized** — the same rule [`trace::Lane::spans`] holds. The
    /// marks stay server-side and reach the browser already binned, through
    /// [`bin_window`], for the window it is about to draw.
    #[serde(skip_serializing)]
    pub marks: Vec<Mark>,
}

/// One sample tap, as a strip rather than as marks.
///
/// **A sample tap is deliberately not a mark lane.** 10 kHz over 147 s is
/// 1.47 M points, which is a smear and not a chart; the min/max/count strip is
/// the same answer `/api/studies/{id}/stream/{name}/series` gives, on this
/// axis instead of the tap's own.
#[derive(Debug, Clone, Serialize)]
pub struct SeriesLane {
    pub key: String,
    pub label: String,
    pub tap: String,
    /// The column plotted. The tap's first numeric column, which for a
    /// `Samples` rendering is `value`.
    pub column: String,
    /// The unit the tap's own rows name, where they name one. Read off the
    /// data rather than assumed — `embarch-study-designer` owns that column,
    /// not this crate.
    pub unit: Option<String>,
    pub total: usize,
    pub placed: usize,
    pub before: usize,
    pub after: usize,
    pub dropped_by_cap: usize,
    pub note: Option<String>,
    /// **Never serialized**, for the same reason as [`MarkLane::marks`].
    /// `(axis position, value)`, sorted by position.
    #[serde(skip_serializing)]
    pub points: Vec<(u64, f64)>,
}

/// The trace this chart's axis came from, or that it could not use — enough for
/// the tab to link the Time chart to the Trace card below it.
#[derive(Debug, Clone, Serialize)]
pub struct TraceRef {
    pub tap: String,
    pub unit: &'static str,
    pub axis_clock: &'static str,
    pub named: bool,
    pub timed: bool,
    pub dual_clock: bool,
    /// True when this trace is what drew the axis. False means it exists and
    /// this chart is drawn on embarch-core's clock instead, with
    /// [`Self::note`] saying why.
    pub drew_the_axis: bool,
    pub note: Option<String>,
}

/// One study, every stream it produced, on one axis.
#[derive(Debug, Clone, Serialize)]
pub struct TimeChartView {
    pub study_id: String,
    pub axis: Axis,
    /// The study's own steps, placed — the same [`trace::StepBand`] the Trace
    /// card draws, through the same [`trace::Projection`]. Not a second
    /// rendering of the same row.
    pub bands: Vec<StepBand>,
    pub steps_placeable: bool,
    pub steps_note: String,
    pub lanes: Vec<MarkLane>,
    pub series: Vec<SeriesLane>,
    pub trace: Option<TraceRef>,
    /// Which axis this view's positions are on.
    ///
    /// **Zero means an axis that cannot change**: a completed study read off
    /// disk, whose files are all there and whose tier was decided once. A live
    /// session bumps it when its axis genuinely improves — a stale prefix
    /// dropped mid-run, or the header frame arriving and promoting the tier —
    /// and the browser re-snapshots rather than patching across it. Patching
    /// across an epoch would be the chart quietly moving marks a reader has
    /// already looked at.
    pub axis_epoch: u64,
    /// Marks past their lane's cap, summed — surfaced at the top because a
    /// chart shortened anywhere must say so where a reader will see it.
    pub marks_dropped_by_cap: usize,
    /// What could not be read at all. Each entry is one tap and one reason; a
    /// tap that failed does not fail the chart, the same way `api_study`'s
    /// three parts each carry their own note.
    pub notes: Vec<String>,
}

// ---- binned windows ---------------------------------------------------------

/// One run of bins holding marks, all of one lane.
///
/// `one` carries the mark when the run is exactly one, so a reader who zoomed
/// in to separate a cluster gets the thing they zoomed in for. A merged run
/// carries a count and **is not clickable** — the same rule
/// [`trace::BinRun::one`] holds about not reporting a merged block's width as a
/// duration.
#[derive(Debug, Clone, Serialize)]
pub struct MarkRun {
    pub c0: usize,
    pub c1: usize,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one: Option<Mark>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BinnedMarkLane {
    pub key: String,
    pub runs: Vec<MarkRun>,
    /// Marks of this lane inside the window, before merging.
    pub visible: usize,
    /// Placed marks of this lane that fall before the window, and after it —
    /// the gutter counts at each end.
    pub before: usize,
    pub after: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeriesBin {
    pub c: usize,
    pub min: f64,
    pub max: f64,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BinnedSeriesLane {
    pub key: String,
    pub bins: Vec<SeriesBin>,
    pub visible: usize,
    pub before: usize,
    pub after: usize,
    /// The window's own extremes, so the strip can be scaled to what is on
    /// screen rather than to the whole run.
    pub min: f64,
    pub max: f64,
}

/// Every lane binned over one window — the reply to `?from&to&width`.
#[derive(Debug, Clone, Serialize)]
pub struct BinnedTimeWindow {
    pub study_id: String,
    /// The window actually binned, after clamping into the axis. A caller that
    /// asked for something outside it is told what it got rather than being
    /// handed bins whose positions it would misplace — the same
    /// clamp-and-say-what-you-clamped-to contract `api_trace_bins` holds.
    pub from: u64,
    pub to: u64,
    pub width: usize,
    pub unit: &'static str,
    /// Repeated from the view, so a browser can refuse a reply that arrived
    /// across an axis change rather than drawing it.
    pub axis_epoch: u64,
    pub lanes: Vec<BinnedMarkLane>,
    pub series: Vec<BinnedSeriesLane>,
}

/// Bins one lane's marks over `[from, to]`.
fn bin_marks(lane: &MarkLane, from: u64, to: u64, width: usize) -> BinnedMarkLane {
    let mut counts = vec![0usize; width];
    let mut one: Vec<Option<&Mark>> = vec![None; width];
    let scale = width as f64 / (to - from).max(1) as f64;
    let (mut visible, mut before, mut after) = (0usize, 0usize, 0usize);

    for m in &lane.marks {
        let Some(t) = m.t else { continue };
        if t < from {
            before += 1;
            continue;
        }
        if t > to {
            after += 1;
            continue;
        }
        let c = (((t - from) as f64 * scale).floor() as usize).min(width - 1);
        counts[c] += 1;
        one[c] = if counts[c] == 1 { Some(m) } else { None };
        visible += 1;
    }

    // Runs are contiguous stretches of occupied bins. A stretch of exactly one
    // occupied bin holding exactly one mark is the clickable case; everything
    // else is a cluster that says it is one.
    let mut runs: Vec<MarkRun> = Vec::new();
    let mut c = 0usize;
    while c < width {
        if counts[c] == 0 {
            c += 1;
            continue;
        }
        let start = c;
        let mut count = 0usize;
        let mut only: Option<&Mark> = None;
        while c < width && counts[c] > 0 {
            count += counts[c];
            if counts[c] == 1 && only.is_none() {
                only = one[c];
            }
            c += 1;
        }
        runs.push(MarkRun {
            c0: start,
            c1: c - 1,
            count,
            one: if count == 1 { only.cloned() } else { None },
        });
    }

    BinnedMarkLane { key: lane.key.clone(), runs, visible, before, after }
}

/// Bins one sample tap's points over `[from, to]`, min/max/count per bin.
///
/// **Min and max, not an average** — an average hides the spike that is usually
/// the reason someone is looking, which is the rule
/// `api_stream_series` already holds.
fn bin_series(lane: &SeriesLane, from: u64, to: u64, width: usize) -> BinnedSeriesLane {
    let mut slots: Vec<Option<(f64, f64, u64)>> = vec![None; width];
    let scale = width as f64 / (to - from).max(1) as f64;
    let (mut visible, mut before, mut after) = (0usize, 0usize, 0usize);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);

    for (t, v) in &lane.points {
        if *t < from {
            before += 1;
            continue;
        }
        if *t > to {
            after += 1;
            continue;
        }
        let c = (((*t - from) as f64 * scale).floor() as usize).min(width - 1);
        match &mut slots[c] {
            Some((min, max, count)) => {
                if *v < *min {
                    *min = *v;
                }
                if *v > *max {
                    *max = *v;
                }
                *count += 1;
            }
            none => *none = Some((*v, *v, 1)),
        }
        lo = lo.min(*v);
        hi = hi.max(*v);
        visible += 1;
    }

    let bins: Vec<SeriesBin> = slots
        .iter()
        .enumerate()
        .filter_map(|(c, s)| s.map(|(min, max, count)| SeriesBin { c, min, max, count }))
        .collect();

    BinnedSeriesLane {
        key: lane.key.clone(),
        bins,
        visible,
        before,
        after,
        min: if lo.is_finite() { lo } else { 0.0 },
        max: if hi.is_finite() { hi } else { 0.0 },
    }
}

/// Bins every lane of `view` over one window.
///
/// The window is **clamped rather than refused**, and the reply says what it
/// clamped to: a caller that asked for a window reaching past the axis is
/// asking a sensible question about the edge of it, and the alternative is a
/// `400` in the middle of a drag.
pub fn bin_window(
    view: &TimeChartView,
    from: u64,
    to: u64,
    width: usize,
) -> Result<BinnedTimeWindow, String> {
    if width == 0 || width > MAX_BINS {
        return Err(format!("width must be between 1 and {MAX_BINS}, not {width}"));
    }
    let full_to = view.axis.t_to.max(view.axis.t_from + 1);
    let from = from.clamp(view.axis.t_from, full_to);
    let to = to.clamp(from, full_to);
    Ok(BinnedTimeWindow {
        study_id: view.study_id.clone(),
        from,
        to,
        width,
        unit: view.axis.unit,
        axis_epoch: view.axis_epoch,
        lanes: view.lanes.iter().map(|l| bin_marks(l, from, to, width)).collect(),
        series: view.series.iter().map(|s| bin_series(s, from, to, width)).collect(),
    })
}

// ---- building ---------------------------------------------------------------

/// The column a **shared** axis is drawn against, and the one refusal that
/// keeps this whole chart honest.
///
/// `core_rx_utc_ms` and nothing else. A rendered row's `rx_utc_ms` is dev-bench
/// uptime (suite decision 3) and is not comparable with anything outside its
/// own capture, so a fallback to it would not be a coarser answer — it would be
/// a different clock silently drawn as this one. `None` means this tap cannot
/// be placed, which is a thing this chart says out loud.
pub fn axis_column(table: &Table) -> Option<usize> {
    table.columns.iter().position(|c| c == "core_rx_utc_ms")
}

/// One raw event, before an axis exists to place it on.
struct RawMark {
    core_rx_utc_ms: u64,
    row_index: usize,
    sub: String,
    label: String,
}

/// A lane's raw events plus what the tap itself could not offer.
struct RawLane {
    key: String,
    label: String,
    kind: &'static str,
    source: &'static str,
    tap: String,
    total: usize,
    dropped_by_cap: usize,
    note: Option<String>,
    marks: Vec<RawMark>,
    /// Set for a lane whose events are natively on the trace's own axis and
    /// need no crossing — the capture's markers.
    native: Vec<(u64, usize, String, String)>,
}

/// Builds one study's whole Time chart in a single pass.
///
/// **One pass, and then cached whole.** This needs the trace view and several
/// rendered tables at once, and `studies_api`'s table cache holds exactly one
/// entry — so going through it would have each tap's fetch evict the last and
/// the chart would re-fetch every file on every pan. Every table is read once
/// here and the finished view is what gets cached.
pub async fn build(state: &AppState, study_id: &str) -> Result<TimeChartView, Response> {
    let mut notes: Vec<String> = Vec::new();

    // ---- what this study declared ---------------------------------------
    let index = match state.core.study_streams(study_id).await {
        Ok(Some(index)) => Some(index),
        Ok(None) => {
            notes.push(
                "this study recorded no streams — it may predate streams/, or never have started"
                    .to_string(),
            );
            None
        }
        Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()),
    };
    let entries = index.as_ref().map(|i| i.streams.as_slice()).unwrap_or(&[]);

    // ---- the study's own steps ------------------------------------------
    let steps = crate::step_stamps(&state.core, study_id).await;

    // ---- the trace, if there is one that decoded -------------------------
    //
    // The first rendered `OutpostTrace` tap. A study with two is legal and
    // vanishingly rare; the second is named in a note rather than silently
    // ignored, because two captures cannot both draw one axis.
    let trace_taps: Vec<&str> = entries
        .iter()
        .filter(|e| matches!(e.encoding, StreamEncoding::OutpostTrace) && e.rendered)
        .map(|e| e.name.as_str())
        .collect();
    if trace_taps.len() > 1 {
        notes.push(format!(
            "this study has {} rendered outpost traces ({}); the first drew the axis and the rest \
             are readable on the Trace card below",
            trace_taps.len(),
            trace_taps.join(", ")
        ));
    }
    let mut trace_view: Option<Arc<TraceView>> = None;
    let mut trace_ref: Option<TraceRef> = None;
    if let Some(tap) = trace_taps.first() {
        match crate::decode_trace(state, study_id, tap).await {
            Ok(view) => trace_view = Some(view),
            Err(_) => notes.push(format!(
                "the outpost trace '{tap}' could not be decoded, so this chart is drawn on \
                 embarch-core's own receipt clock instead. The Trace card below says why."
            )),
        }
    }

    // ---- gather every stream's raw events, before an axis exists ---------
    let mut raw: Vec<RawLane> = Vec::new();

    if let Some(view) = &trace_view {
        // The capture's own markers: an engineer's annotations, and the reason
        // a trace is worth reading against a specific run. **Natively on this
        // axis already** — a marker is stamped by the DUT, so there is no
        // crossing to make and no accuracy to caveat.
        if !view.markers.is_empty() {
            raw.push(RawLane {
                key: "trace-markers".to_string(),
                label: format!("{} · markers", view.tap),
                kind: "marker",
                source: "trace-markers",
                tap: view.tap.clone(),
                total: view.markers.len(),
                dropped_by_cap: 0,
                note: None,
                marks: Vec::new(),
                native: view
                    .markers
                    .iter()
                    .enumerate()
                    .map(|(i, m)| {
                        (
                            m.t,
                            i,
                            m.kind.clone(),
                            if m.label.is_empty() {
                                format!("(unnamed, arg {})", m.arg)
                            } else {
                                format!("{} (arg {})", m.label, m.arg)
                            },
                        )
                    })
                    .collect(),
            });
        }
    }

    for entry in entries {
        match &entry.encoding {
            StreamEncoding::GattTranscript | StreamEncoding::Struct { .. } => {
                let kind: &'static str =
                    if matches!(entry.encoding, StreamEncoding::GattTranscript) {
                        "gatt"
                    } else {
                        "struct"
                    };
                if !entry.rendered {
                    raw.push(empty_lane(
                        &entry.name,
                        kind,
                        "rows",
                        Some(format!(
                            "tap '{}' has no decoded rendering, so there are no rows to place. {}",
                            entry.name,
                            entry.note.clone().unwrap_or_else(|| "Core recorded no reason.".to_string())
                        )),
                    ));
                    continue;
                }
                match fetch_table(state, study_id, &entry.name).await {
                    Ok(table) => raw.push(rows_lane(&entry.name, kind, &table)),
                    Err(e) => raw.push(empty_lane(&entry.name, kind, "rows", Some(e))),
                }
            }
            StreamEncoding::Text => {
                // **A `Text` tap's capture carries no timestamp anywhere**, and
                // that is a defect in the recording rather than in this chart:
                // Core writes a console's raw bytes and nothing else, so a
                // console read back off disk is bytes with no times. The lane
                // is drawn as a count with this sentence beside it rather than
                // omitted — a reader who cannot see the lane cannot tell it
                // from a console that captured nothing.
                let (total, note) = match state
                    .core
                    .get_study_stream(study_id, &entry.name, false)
                    .await
                {
                    Ok(bytes) => (
                        String::from_utf8_lossy(&bytes)
                            .split('\n')
                            .take(MAX_CONSOLE_LINES_COUNTED)
                            .filter(|l| !l.is_empty())
                            .count(),
                        None,
                    ),
                    Err(e) => (0, Some(format!("{e:#}"))),
                };
                raw.push(RawLane {
                    key: format!("text:{}", entry.name),
                    label: entry.name.clone(),
                    kind: "console",
                    source: "text",
                    tap: entry.name.clone(),
                    total,
                    dropped_by_cap: 0,
                    note: Some(note.unwrap_or_else(|| {
                        "A console's capture on disk is bytes and nothing else — embarch-core \
                         records no arrival time per chunk for a Text tap — so these lines cannot \
                         be placed on any shared axis after the run. They are placeable live, and \
                         a Core-side arrival sidecar is what will make them placeable here too."
                            .to_string()
                    })),
                    marks: Vec::new(),
                    native: Vec::new(),
                });
            }
            // Handled below as a strip rather than as marks — see
            // `SeriesLane` for why 1.47 M points is a smear and not a chart.
            StreamEncoding::Samples { .. } => {}
            // `Raw` has nothing declared to decode against, so there is nothing
            // to place. The Data card's hex head is the honest view of it and
            // this chart says nothing about it at all.
            //
            // `OutpostTrace` is handled above, where it draws the axis rather
            // than becoming a lane of marks — its own records are the Trace
            // card's business, and only its markers cross over here.
            StreamEncoding::Raw | StreamEncoding::OutpostTrace => {}
        }
    }

    // The sample taps, read once. Their strips are built below, once there is
    // an axis to project onto.
    let mut sample_tables: Vec<(String, Arc<Table>)> = Vec::new();
    for entry in entries {
        if matches!(entry.encoding, StreamEncoding::Samples { .. }) && entry.rendered {
            match fetch_table(state, study_id, &entry.name).await {
                Ok(t) => sample_tables.push((entry.name.clone(), t)),
                Err(e) => notes.push(e),
            }
        }
    }

    // ---- the axis, chosen once ------------------------------------------
    let projection = match &trace_view {
        Some(v) if v.projection.placeable() => v.projection.clone(),
        _ => {
            // embarch-core's own receipt clock, over the study's whole extent:
            // every step edge and every stamped row this study produced.
            let mut lo = u64::MAX;
            let mut hi = 0u64;
            for s in &steps {
                lo = lo.min(s.started_utc_ms);
                hi = hi.max(s.ended_utc_ms);
            }
            for lane in &raw {
                for m in &lane.marks {
                    lo = lo.min(m.core_rx_utc_ms);
                    hi = hi.max(m.core_rx_utc_ms);
                }
            }
            for (_, table) in &sample_tables {
                if let Some(col) = axis_column(table) {
                    for row in &table.rows {
                        if let Some(ms) = row.get(col).and_then(|c| c.trim().parse::<u64>().ok()) {
                            lo = lo.min(ms);
                            hi = hi.max(ms);
                        }
                    }
                }
            }
            if lo > hi {
                Projection::default()
            } else {
                Projection::core_clock(lo, hi)
            }
        }
    };

    let axis = describe_axis(&projection, trace_view.as_deref(), &mut notes);

    if let (Some(view), Some(entry)) = (
        &trace_view,
        entries.iter().find(|e| Some(e.name.as_str()) == trace_taps.first().copied()),
    ) {
        trace_ref = Some(TraceRef {
            tap: view.tap.clone(),
            unit: view.unit,
            axis_clock: view.axis_clock,
            named: entry.is_named(),
            timed: entry.is_timed(),
            dual_clock: view.dual_clock,
            drew_the_axis: view.projection.placeable(),
            note: view.note.clone(),
        });
    }

    // ---- place everything -----------------------------------------------
    let mut lanes: Vec<MarkLane> = Vec::new();
    let mut marks_dropped_by_cap = 0usize;
    for (i, lane) in raw.iter().enumerate() {
        let mut marks: Vec<Mark> = Vec::new();
        let (mut placed, mut before, mut after) = (0usize, 0usize, 0usize);

        for (t, row, sub, label) in &lane.native {
            placed += 1;
            marks.push(Mark {
                t: Some(*t),
                core_rx_utc_ms: None,
                kind: lane.kind,
                sub: sub.clone(),
                uncertain: false,
                id: mark_id(i, *row),
                label: label.clone(),
            });
        }
        for m in &lane.marks {
            let t = projection.place(m.core_rx_utc_ms);
            match t {
                Some(_) => placed += 1,
                None if m.core_rx_utc_ms < projection.window_from_ms() => before += 1,
                None => after += 1,
            }
            marks.push(Mark {
                t,
                core_rx_utc_ms: Some(m.core_rx_utc_ms),
                kind: lane.kind,
                sub: m.sub.clone(),
                uncertain: t.is_some() && projection.projected(),
                id: mark_id(i, m.row_index),
                label: m.label.clone(),
            });
        }
        marks.sort_by_key(|m| m.t.unwrap_or(u64::MAX));
        marks_dropped_by_cap += lane.dropped_by_cap;

        lanes.push(MarkLane {
            key: lane.key.clone(),
            label: lane.label.clone(),
            kind: lane.kind,
            source: lane.source,
            tap: lane.tap.clone(),
            total: lane.total,
            placed,
            before,
            after,
            dropped_by_cap: lane.dropped_by_cap,
            note: lane.note.clone(),
            marks,
        });
    }

    // ---- sample strips ---------------------------------------------------
    let mut series: Vec<SeriesLane> = Vec::new();
    for (tap, table) in &sample_tables {
        series.push(series_lane(tap, table, &projection));
    }

    // ---- the step row, through the same projection ------------------------
    let step_row = trace::project_steps_on(&steps, &projection);
    let (steps_placeable, steps_note, bands) = match step_row {
        Some(row) => (row.placeable, row.note, row.bands),
        None => (
            false,
            "This study recorded no per-step arrival stamps — it ran before embarch-core kept \
             them, or its events.json has been swept — so there is no step row to draw. Every \
             other lane is unaffected."
                .to_string(),
            Vec::new(),
        ),
    };

    Ok(TimeChartView {
        study_id: study_id.to_string(),
        axis,
        bands,
        steps_placeable,
        steps_note,
        lanes,
        series,
        trace: trace_ref,
        // A finished study's axis cannot change: every file it will ever have
        // is on disk and the tier was decided once, over all of it.
        axis_epoch: 0,
        marks_dropped_by_cap,
        notes,
    })
}

/// The axis, and the sentence that says what reading a position off it costs.
fn describe_axis(
    projection: &Projection,
    trace: Option<&TraceView>,
    notes: &mut Vec<String>,
) -> Axis {
    if !projection.placeable() {
        if let Some(view) = trace {
            notes.push(format!(
                "the outpost trace '{}' has no time base — its own axis is a frame index, which is \
                 an order and not a clock — and this study has nothing else stamped to draw an \
                 axis with",
                view.tap
            ));
        }
        return Axis {
            unit: "ms",
            axis_clock: "none",
            t_from: 0,
            t_to: 0,
            projected: false,
            accuracy_ms: None,
            placeable: false,
            source: "nothing".to_string(),
            window_from_ms: 0,
            window_to_ms: 0,
            note: "Nothing in this study carries a clock this chart can draw on: its steps have \
                   no recorded arrival stamps, no rendered tap carries embarch-core's own \
                   core_rx_utc_ms, and no outpost trace offered a time base. There is no axis, so \
                   nothing is drawn — positions invented from row order would look exactly like \
                   times."
                .to_string(),
        };
    }

    match (trace, projection.projected()) {
        (Some(view), true) => Axis {
            unit: "us",
            axis_clock: "dut-cycles",
            t_from: projection.t_from(),
            t_to: projection.t_to(),
            projected: true,
            accuracy_ms: projection.accuracy_ms,
            placeable: true,
            source: view.tap.clone(),
            window_from_ms: projection.window_from_ms(),
            window_to_ms: projection.window_to_ms(),
            note: format!(
                "The axis is the DUT's own microsecond counter, from the outpost trace '{}'. \
                 Everything else in this study is stamped on embarch-core's receipt clock and is \
                 **projected** onto it through the frames that carry both — good to about {} ms, \
                 which is this capture's own resolution. A mark's position should not be read as \
                 aligned to a trace span's; a span measures to the microsecond and a mark does \
                 not. Marks outside the trace's own window are counted in the gutter rather than \
                 clamped to its edge, because a clamped point would be drawn at a time it was not \
                 at.",
                view.tap,
                projection.accuracy_ms.map(|v| format!("{v}")).unwrap_or_else(|| "?".to_string())
            ),
        },
        (Some(view), false) => Axis {
            unit: "ms",
            axis_clock: "host-arrival",
            t_from: projection.t_from(),
            t_to: projection.t_to(),
            projected: false,
            accuracy_ms: None,
            placeable: true,
            source: view.tap.clone(),
            window_from_ms: projection.window_from_ms(),
            window_to_ms: projection.window_to_ms(),
            note: format!(
                "The axis is embarch-core's own receipt clock, reached through the outpost trace \
                 '{}': that capture's records did not all carry the DUT's counter, so the trace \
                 itself is drawn on frame arrivals. Those arrivals are the same absolute \
                 milliseconds every other stream here is stamped in, so every mark is exactly \
                 placed and nothing is projected — the axis is coarser than the DUT's and it is \
                 not approximate.",
                view.tap
            ),
        },
        (None, _) => Axis {
            unit: "ms",
            axis_clock: "core-clock",
            t_from: projection.t_from(),
            t_to: projection.t_to(),
            projected: false,
            accuracy_ms: None,
            placeable: true,
            source: "embarch-core".to_string(),
            window_from_ms: projection.window_from_ms(),
            window_to_ms: projection.window_to_ms(),
            note: "The axis is embarch-core's own receipt clock, directly — this study has no \
                   outpost trace, so there is no second clock and nothing to project. Every mark \
                   is exactly where Core received it, to the millisecond."
                .to_string(),
        },
    }
}

/// A lane that exists and holds nothing, with the reason attached.
fn empty_lane(
    tap: &str,
    kind: &'static str,
    source: &'static str,
    note: Option<String>,
) -> RawLane {
    RawLane {
        key: format!("{kind}:{tap}"),
        label: tap.to_string(),
        kind,
        source,
        tap: tap.to_string(),
        total: 0,
        dropped_by_cap: 0,
        note,
        marks: Vec::new(),
        native: Vec::new(),
    }
}

/// One rendered table turned into raw marks, through `core_rx_utc_ms` and
/// nothing else.
fn rows_lane(tap: &str, kind: &'static str, table: &Table) -> RawLane {
    let Some(stamp) = axis_column(table) else {
        return empty_lane(
            tap,
            kind,
            "rows",
            Some(format!(
                "tap '{tap}' has no core_rx_utc_ms column, so its rows carry no clock this chart \
                 can share. Its own rx_utc_ms is dev-bench uptime — milliseconds since that board \
                 booted — which is not comparable with anything outside this tap, so it is not \
                 used as a substitute."
            )),
        );
    };

    // Two spellings of the same idea, and both are the tap's own columns
    // rather than this crate's knowledge: a GATT row says which direction and
    // kind it was, a struct row says whether its payload fitted the layout.
    let col = |name: &str| table.columns.iter().position(|c| c == name);
    let dir = col("direction");
    let gk = col("kind");
    let note_col = col("decode_note");
    let step_col = col("step_name");
    let uuid = col("characteristic_uuid");
    let len = col("payload_len");
    let ascii = col("payload_ascii");

    let cell = |row: &Vec<String>, i: Option<usize>| -> String {
        i.and_then(|i| row.get(i)).map(|s| s.trim().to_string()).unwrap_or_default()
    };

    let mut marks = Vec::new();
    let mut dropped_by_cap = 0usize;
    for (i, row) in table.rows.iter().enumerate() {
        let Some(ms) = row.get(stamp).and_then(|c| c.trim().parse::<u64>().ok()) else {
            // A row whose Core stamp did not parse is a row this chart has no
            // position for. Counted in `total` and absent from `marks`, the
            // same way an unplaceable one is.
            continue;
        };
        if marks.len() >= MAX_MARKS_PER_LANE {
            dropped_by_cap += 1;
            continue;
        }
        let (sub, label) = if kind == "gatt" {
            (
                format!("{} {}", cell(row, dir), cell(row, gk)).trim().to_string(),
                {
                    let text = cell(row, ascii);
                    let head: String = text.chars().take(48).collect();
                    format!(
                        "{} {} · {} · {} bytes{}",
                        cell(row, dir),
                        cell(row, gk),
                        cell(row, uuid),
                        cell(row, len),
                        if head.is_empty() { String::new() } else { format!(" · {head}") }
                    )
                },
            )
        } else {
            let note = cell(row, note_col);
            (
                if note.is_empty() { "decoded".to_string() } else { "undecoded".to_string() },
                if note.is_empty() {
                    format!("{} · row {}", cell(row, step_col), i)
                } else {
                    format!("{} · row {} · {}", cell(row, step_col), i, note)
                },
            )
        };
        marks.push(RawMark { core_rx_utc_ms: ms, row_index: i, sub, label });
    }

    RawLane {
        key: format!("{kind}:{tap}"),
        label: tap.to_string(),
        kind,
        source: "rows",
        tap: tap.to_string(),
        total: table.rows.len(),
        dropped_by_cap,
        note: None,
        marks,
        native: Vec::new(),
    }
}

/// One sample tap's strip, projected onto the axis.
fn series_lane(tap: &str, table: &Table, projection: &Projection) -> SeriesLane {
    let numeric = table.numeric_columns();
    let Some(column) = numeric.first().cloned() else {
        return SeriesLane {
            key: format!("series:{tap}"),
            label: tap.to_string(),
            tap: tap.to_string(),
            column: String::new(),
            unit: None,
            total: table.rows.len(),
            placed: 0,
            before: 0,
            after: 0,
            dropped_by_cap: 0,
            note: Some(format!(
                "tap '{tap}' has no numeric column to plot — its columns are: {}",
                table.columns.join(", ")
            )),
            points: Vec::new(),
        };
    };
    let Some(stamp) = axis_column(table) else {
        return SeriesLane {
            key: format!("series:{tap}"),
            label: tap.to_string(),
            tap: tap.to_string(),
            column,
            unit: None,
            total: table.rows.len(),
            placed: 0,
            before: 0,
            after: 0,
            dropped_by_cap: 0,
            note: Some(format!(
                "tap '{tap}' has no core_rx_utc_ms column, so its samples carry no clock this \
                 chart can share. Its own rx_utc_ms is dev-bench uptime and is not a substitute."
            )),
            points: Vec::new(),
        };
    };
    let value = table.columns.iter().position(|c| *c == column).unwrap_or(0);
    // The unit is the tap's own word for the quantity, read off the data
    // rather than assumed — `embarch-study-designer` owns that column.
    let unit = table
        .columns
        .iter()
        .position(|c| c == "unit")
        .and_then(|i| table.rows.iter().find_map(|r| r.get(i).filter(|v| !v.trim().is_empty())))
        .map(|v| v.trim().to_string());

    let mut points: Vec<(u64, f64)> = Vec::new();
    let (mut before, mut after, mut dropped_by_cap) = (0usize, 0usize, 0usize);
    for row in &table.rows {
        let Some(ms) = row.get(stamp).and_then(|c| c.trim().parse::<u64>().ok()) else { continue };
        let Some(v) = row.get(value).and_then(|c| c.trim().parse::<f64>().ok()) else { continue };
        if !v.is_finite() {
            continue;
        }
        match projection.place(ms) {
            Some(t) => {
                if points.len() >= MAX_SERIES_POINTS {
                    dropped_by_cap += 1;
                    continue;
                }
                points.push((t, v));
            }
            None if ms < projection.window_from_ms() => before += 1,
            None => after += 1,
        }
    }
    points.sort_by_key(|p| p.0);
    let placed = points.len();

    SeriesLane {
        key: format!("series:{tap}"),
        label: tap.to_string(),
        tap: tap.to_string(),
        column,
        unit,
        total: table.rows.len(),
        placed,
        before,
        after,
        dropped_by_cap,
        note: None,
        points,
    }
}

/// Fetches and parses one tap's rendered CSV, **bypassing the one-entry table
/// cache on purpose** — see [`build`] for why.
async fn fetch_table(state: &AppState, study_id: &str, name: &str) -> Result<Arc<Table>, String> {
    match state.core.get_study_stream(study_id, name, false).await {
        Ok(bytes) => Ok(Arc::new(Table::parse(&String::from_utf8_lossy(&bytes)))),
        Err(e) => Err(format!("tap '{name}' could not be read: {e:#}")),
    }
}

// ---- routes -----------------------------------------------------------------

/// `GET /api/time-chart/{study_id}` — one study, every stream, one axis.
///
/// Loading the view **is** the refresh: this route always re-reads and
/// re-builds, and `/marks` then answers from what it built until the next load.
/// The same contract `api_trace_view` holds, and for the same reason — a
/// staleness rule nobody can see is worse than a re-read nobody pays for.
pub async fn api_time_chart(
    State(state): State<AppState>,
    Path(study_id): Path<String>,
) -> Response {
    match build(&state, &study_id).await {
        Ok(view) => {
            let view = Arc::new(view);
            let body = (StatusCode::OK, Json(view.as_ref())).into_response();
            *state.time_chart_cache.lock().await =
                Some(CachedTimeChart { study_id, view });
            body
        }
        Err(resp) => resp,
    }
}

#[derive(Debug, Deserialize)]
pub struct MarksQuery {
    from: Option<u64>,
    to: Option<u64>,
    width: Option<usize>,
}

/// `GET /api/time-chart/{study_id}/marks?from&to&width` — one window, binned.
pub async fn api_time_chart_marks(
    State(state): State<AppState>,
    Path(study_id): Path<String>,
    Query(q): Query<MarksQuery>,
) -> Response {
    let view = match cached_or_build(&state, &study_id).await {
        Ok(view) => view,
        Err(resp) => return resp,
    };
    let from = q.from.unwrap_or(view.axis.t_from);
    let to = q.to.unwrap_or(view.axis.t_to);
    match bin_window(&view, from, to, q.width.unwrap_or(1)) {
        Ok(bins) => (StatusCode::OK, Json(bins)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// `GET /api/time-chart/{study_id}/mark/{id}` — one mark's full detail.
///
/// A thin dispatcher, deliberately: the id already says which lane and which
/// row, so this fetches that row from the tap's own rendering. What comes back
/// is the same row the Data card shows for that record, because it *is* that
/// row rather than a second rendering of it.
pub async fn api_time_chart_mark(
    State(state): State<AppState>,
    Path((study_id, id)): Path<(String, u64)>,
) -> Response {
    let view = match cached_or_build(&state, &study_id).await {
        Ok(view) => view,
        Err(resp) => return resp,
    };
    let (lane_index, row_index) = split_mark_id(id);
    let Some(lane) = view.lanes.get(lane_index) else {
        return (
            StatusCode::NOT_FOUND,
            format!("this chart has no lane {lane_index} — it has {}", view.lanes.len()),
        )
            .into_response();
    };
    let mark = lane.marks.iter().find(|m| m.id == id).cloned();

    match lane.source {
        "trace-markers" => {
            let Some(mark) = mark else {
                return (StatusCode::NOT_FOUND, format!("no marker {row_index} in this capture"))
                    .into_response();
            };
            (
                StatusCode::OK,
                Json(json!({
                    "lane": lane.key,
                    "kind": lane.kind,
                    "source": lane.source,
                    "tap": lane.tap,
                    "mark": mark,
                    "note": "A marker is the engineer's own annotation, stamped by the DUT itself \
                             — so it is natively on this axis and carries no projection error. \
                             Its arg is passed through as the number it is; nothing here \
                             interprets it.",
                })),
            )
                .into_response()
        }
        "text" => (
            StatusCode::CONFLICT,
            format!(
                "tap '{}' is a console, and a console's capture on disk carries no arrival time \
                 per line — so this chart placed none of its lines and has no mark to open.",
                lane.tap
            ),
        )
            .into_response(),
        _ => {
            let table = match fetch_table(&state, &study_id, &lane.tap).await {
                Ok(t) => t,
                Err(e) => return (StatusCode::BAD_GATEWAY, e).into_response(),
            };
            let Some(row) = table.rows.get(row_index) else {
                return (
                    StatusCode::NOT_FOUND,
                    format!(
                        "tap '{}' has no row {row_index} — it has {}",
                        lane.tap,
                        table.rows.len()
                    ),
                )
                    .into_response();
            };
            (
                StatusCode::OK,
                Json(json!({
                    "lane": lane.key,
                    "kind": lane.kind,
                    "source": lane.source,
                    "tap": lane.tap,
                    "row_index": row_index,
                    "columns": table.columns,
                    "row": row,
                    "mark": mark,
                })),
            )
                .into_response()
        }
    }
}

/// The cached view for this study, or a fresh build.
///
/// A miss is a deep link straight into a window, or a UI process restarted
/// under an open tab — the same case `api_trace_bins` handles by decoding
/// rather than answering `409`.
async fn cached_or_build(
    state: &AppState,
    study_id: &str,
) -> Result<Arc<TimeChartView>, Response> {
    {
        let guard = state.time_chart_cache.lock().await;
        if let Some(cached) = guard.as_ref() {
            if cached.study_id == study_id {
                return Ok(cached.view.clone());
            }
        }
    }
    let view = Arc::new(build(state, study_id).await?);
    *state.time_chart_cache.lock().await =
        Some(CachedTimeChart { study_id: study_id.to_string(), view: view.clone() });
    Ok(view)
}

/// One built chart, keyed by the study it was built from.
///
/// **A new cache rather than a wider existing one.** `trace_cache` and
/// `table_cache` each hold exactly one entry because each serves one card
/// asking many questions about one file; widening either to fit this chart
/// would change behaviour for those cards, which is the thing a new field
/// cannot do.
pub struct CachedTimeChart {
    pub study_id: String,
    pub view: Arc<TimeChartView>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATT_CSV: &str = "rx_utc_ms,step_index,step_name,direction,kind,service_uuid,\
                            characteristic_uuid,att_status,payload_len,payload_hex,payload_ascii,\
                            core_rx_utc_ms\n\
                            120,0,connect,Rx,Notify,fe59,2559,0,4,01020304,....,1700000000120\n\
                            220,0,connect,Tx,Write,fe59,2559,0,2,0102,..,1700000000220\n";

    fn lane_of(table: &Table) -> RawLane {
        rows_lane("gatt", "gatt", table)
    }

    /// **The one refusal this whole chart rests on.** A rendered row's
    /// `rx_utc_ms` is dev-bench uptime and `core_rx_utc_ms` is embarch-core's
    /// own receipt clock; reading the first would place every mark against a
    /// board's boot time and look completely plausible.
    #[test]
    fn the_axis_column_is_cores_own_stamp_and_never_the_benchs() {
        let table = Table::parse(GATT_CSV);
        let col = axis_column(&table).expect("gatt.csv carries Core's stamp");
        assert_eq!(table.columns[col], "core_rx_utc_ms");
        let lane = lane_of(&table);
        assert_eq!(lane.marks.len(), 2);
        assert_eq!(lane.marks[0].core_rx_utc_ms, 1_700_000_000_120);
        assert_eq!(lane.marks[1].core_rx_utc_ms, 1_700_000_000_220);
    }

    /// A tap with no Core stamp is a tap this chart **says** it cannot place.
    /// Falling back to `rx_utc_ms` would be a different clock drawn as this
    /// one, which is the error class this module exists to make impossible.
    #[test]
    fn a_tap_with_no_core_stamp_is_refused_rather_than_placed_against_bench_uptime() {
        let table = Table::parse("rx_utc_ms,step_name,value,unit,channel_id\n1,connect,1.5,mA,0\n");
        assert!(axis_column(&table).is_none());
        let lane = rows_lane("old", "struct", &table);
        assert!(lane.marks.is_empty());
        assert!(lane.note.as_ref().unwrap().contains("dev-bench uptime"));
    }

    /// **A mark outside the axis window is counted, never clamped.** Clamping
    /// a band to the capture's edge is truthful; clamping a point mark would
    /// draw it at a time it was not at.
    #[test]
    fn a_mark_outside_the_window_is_unplaceable_and_is_never_clamped() {
        let proj = Projection::core_clock(1_000, 2_000);
        assert_eq!(proj.place(1_500), Some(1_500));
        assert_eq!(proj.place(900), None);
        assert_eq!(proj.place(2_100), None);
        // And the two sides are told apart, which is what makes the gutter
        // counts say which way to widen a tap's scope.
        assert!(900 < proj.window_from_ms());
        assert!(2_100 > proj.window_to_ms());
    }

    /// A mark's id and a table row are the same object by construction.
    #[test]
    fn a_mark_id_round_trips_to_its_lane_and_its_row() {
        for (lane, row) in [(0usize, 0usize), (3, 7), (17, 199_999), (1, ROW_MASK as usize)] {
            assert_eq!(split_mark_id(mark_id(lane, row)), (lane, row));
        }
    }

    /// A bin holding exactly one mark carries it and is clickable; a merged
    /// bin carries a count and is not. The rule `BinRun::one` already holds
    /// about never reporting a merged block as one thing.
    #[test]
    fn a_cluster_says_it_is_a_cluster_and_a_single_mark_carries_itself() {
        let mark = |t: u64, id: u64| Mark {
            t: Some(t),
            core_rx_utc_ms: Some(t),
            kind: "gatt",
            sub: String::new(),
            uncertain: false,
            id,
            label: String::new(),
        };
        let lane = MarkLane {
            key: "gatt:gatt".to_string(),
            label: "gatt".to_string(),
            kind: "gatt",
            source: "rows",
            tap: "gatt".to_string(),
            total: 3,
            placed: 3,
            before: 0,
            after: 0,
            dropped_by_cap: 0,
            note: None,
            marks: vec![mark(0, 0), mark(1, 1), mark(90, 2)],
        };
        let binned = bin_marks(&lane, 0, 100, 10);
        assert_eq!(binned.visible, 3);
        assert_eq!(binned.runs.len(), 2);
        assert_eq!(binned.runs[0].count, 2);
        assert!(binned.runs[0].one.is_none(), "a merged bin must not present itself as one mark");
        assert_eq!(binned.runs[1].count, 1);
        assert_eq!(binned.runs[1].one.as_ref().unwrap().id, 2);
    }

    /// The gutter counts: marks that exist and fall outside the window a
    /// reader is looking at, on each side separately.
    #[test]
    fn marks_outside_the_drawn_window_are_counted_at_each_end() {
        let mark = |t: u64| Mark {
            t: Some(t),
            core_rx_utc_ms: Some(t),
            kind: "gatt",
            sub: String::new(),
            uncertain: false,
            id: t,
            label: String::new(),
        };
        let lane = MarkLane {
            key: "k".to_string(),
            label: "k".to_string(),
            kind: "gatt",
            source: "rows",
            tap: "t".to_string(),
            total: 4,
            placed: 4,
            before: 0,
            after: 0,
            dropped_by_cap: 0,
            note: None,
            marks: vec![mark(5), mark(50), mark(60), mark(500)],
        };
        let binned = bin_marks(&lane, 40, 100, 20);
        assert_eq!(binned.before, 1);
        assert_eq!(binned.after, 1);
        assert_eq!(binned.visible, 2);
    }

    /// A struct tap's rows become marks that say whether their payload fitted
    /// the layout — `decode_note` is populated exactly when it did not
    /// (`embarch-study-designer` decision 52), and a chart that drew both the
    /// same would hide the rows that say what went wrong.
    #[test]
    fn a_struct_row_that_did_not_fit_its_layout_says_so_on_its_mark() {
        let table = Table::parse(
            "rx_utc_ms,step_index,step_name,temperature,payload_hex,decode_note,core_rx_utc_ms\n\
             10,0,drain,21.5,aabb,,1700000000010\n\
             11,0,drain,,aa,short header: need 4 have 2,1700000000011\n",
        );
        let lane = rows_lane("bds", "struct", &table);
        assert_eq!(lane.marks.len(), 2);
        assert_eq!(lane.marks[0].sub, "decoded");
        assert_eq!(lane.marks[1].sub, "undecoded");
        assert!(lane.marks[1].label.contains("short header"));
    }

    /// A sample tap becomes a strip, projected onto the axis — and the points
    /// that fall outside it are counted at the end they fell off rather than
    /// dragged to the edge.
    #[test]
    fn a_sample_tap_projects_onto_the_axis_and_counts_what_falls_outside_it() {
        let table = Table::parse(
            "rx_utc_ms,step_name,value,unit,channel_id,core_rx_utc_ms\n\
             1,c,1.0,Milliamps,0,900\n\
             2,c,2.0,Milliamps,0,1100\n\
             3,c,9.0,Milliamps,0,1500\n\
             4,c,3.0,Milliamps,0,2500\n",
        );
        let lane = series_lane("rail", &table, &Projection::core_clock(1_000, 2_000));
        assert_eq!(lane.column, "value");
        assert_eq!(lane.unit.as_deref(), Some("Milliamps"));
        assert_eq!(lane.placed, 2);
        assert_eq!(lane.before, 1);
        assert_eq!(lane.after, 1);
        // Min and max per bin, never an average — the spike is usually the
        // reason someone is looking.
        let binned = bin_series(&lane, 1_000, 2_000, 1);
        assert_eq!(binned.bins.len(), 1);
        assert_eq!(binned.bins[0].min, 2.0);
        assert_eq!(binned.bins[0].max, 9.0);
        assert_eq!(binned.bins[0].count, 2);
    }

    /// A study with nothing stamped has **no axis**, and says so rather than
    /// drawing one from row order — which would look exactly like a time.
    #[test]
    fn nothing_stamped_is_no_axis_rather_than_an_invented_one() {
        let mut notes = Vec::new();
        let axis = describe_axis(&Projection::default(), None, &mut notes);
        assert!(!axis.placeable);
        assert_eq!(axis.axis_clock, "none");
        assert!(axis.note.contains("no axis"));
    }

    /// A zero or oversized width is a malformed request, not a window.
    #[test]
    fn a_zero_or_oversized_width_is_refused() {
        let view = TimeChartView {
            study_id: "s".to_string(),
            axis: Axis {
                unit: "ms",
                axis_clock: "core-clock",
                t_from: 0,
                t_to: 100,
                projected: false,
                accuracy_ms: None,
                placeable: true,
                source: "embarch-core".to_string(),
                window_from_ms: 0,
                window_to_ms: 100,
                note: String::new(),
            },
            bands: Vec::new(),
            steps_placeable: false,
            steps_note: String::new(),
            lanes: Vec::new(),
            series: Vec::new(),
            trace: None,
            axis_epoch: 0,
            marks_dropped_by_cap: 0,
            notes: Vec::new(),
        };
        assert!(bin_window(&view, 0, 100, 0).is_err());
        assert!(bin_window(&view, 0, 100, MAX_BINS + 1).is_err());
        // And a window past the axis is clamped and says what it clamped to.
        let bins = bin_window(&view, 0, 10_000, 50).expect("clamped");
        assert_eq!(bins.to, 100);
    }
}
