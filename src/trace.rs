//! The Trace view's backend (`embarch-ui/design.md` §3 decision 10's second
//! half): turning a completed study's recorded outpost timeline into
//! something a browser can draw, **without any of the five lies a timeline
//! makes easy**.
//!
//! Post-hoc, deliberately: outpost capture is study-scoped with no live feed
//! (`embarch-outpost/design.md` §3 decision 10), so this renders a finished
//! study's recorded stream and is the one place in this UI that is not live.
//!
//! # What this reads, and why it isn't the raw bytes
//!
//! Core writes three files per outpost tap (`embarch-outpost/design.md` §4):
//! `<tap>.bin`, the framed stream verbatim; `<tap>.arrival.csv`, Core's own
//! receipt time per frame; and `<tap>.trace.csv`, the decoded rows with names
//! resolved **through the manifest the flash bound** and those receipt times
//! joined on. This reads the CSV.
//!
//! Decoding the raw bytes here instead was the other option, and it is worse
//! for two reasons now. The manifest lives on Core's side (it arrived with the
//! flash, and the study snapshotted its own copy beside its results), so a
//! decode done here would produce an *unnamed* trace every single time — and
//! the arrival stamps live on Core's side too, for the same structural reason:
//! **Core is the process that received the bytes**, so Core is the only one
//! that could have timed them. A decode here would be unnamed *and* untimed.
//!
//! What this deliberately does **not** do is re-derive trace knowledge: the
//! column list is checked against
//! [`embarch_study_designer::outpost::csv_header`] and refused if it differs,
//! the kind vocabulary is read out of [`RecordKind`] rather than written out
//! here, and `IRQ_UNKNOWN` comes from that crate too.
//!
//! # The clock, and what it can and cannot say
//!
//! **An outpost record carries no timestamp at all** (`embarch-outpost`'s §3
//! decision 4, reworked 2026-08-26: reading the DUT's cycle counter inside the
//! context switch and inside `_isr_wrapper()` was the instrument charging its
//! cost to the code it measures). The only time a trace has is the arrival
//! stamp of the **frame** that carried each record, which means:
//!
//! - Every record in a frame has the same time. A frame is this view's
//!   resolution, [`TraceView::resolution_ms`] says how coarse that is in
//!   milliseconds, and **nothing here spreads a frame's records across an
//!   interval** to make a smoother picture (decision 17).
//! - A span whose two ends are in the same frame has **no measurable
//!   duration** — [`Span::same_frame`] — and is excluded from every total. Most
//!   ISR spans are this, and saying so is the honest answer for a wire whose
//!   frames are milliseconds and whose ISRs are microseconds.
//! - The axis is milliseconds only when **every** row is stamped. One
//!   unstamped frame drops the whole view to [`TraceView::unit`] `"frame"` —
//!   frame index, a complete and real coordinate — rather than drawing two
//!   different axes as one.
//!
//! # The five lies
//!
//! 1. **A dropped-record gap is drawn as a gap, never bridged.** Every
//!    `Gap` record becomes a [`Gap`] band, and the band is **a bound, not a
//!    measurement**: a gap record is always the first record of its frame, so
//!    the losses fall somewhere between the previous frame's arrival and its
//!    own. That is one frame wide, and it is drawn as one frame wide.
//!
//!    A gap band is **not** an empty interval: the records at both its ends
//!    survived. It is drawn as an overlay over what survived, never as a hole
//!    punched through it — erasing real records to make the picture tidier
//!    would be its own lie.
//!
//! 2. **A trace whose manifest did not apply is never presented as a named
//!    one**, and **a trace nobody timed is never presented as a timed one.**
//!    Both come from Core's own `streams/index.json` — `named` and `timed` on
//!    `GET /study/{id}/streams` — not from inspecting whether the rows happen
//!    to carry names or times. Two facts, carried as two, because a trace can
//!    be either without the other.
//!
//! 3. **An unnamed thread or vector renders as the number it is.** No
//!    interpolation, no "probably the worker thread". Most of a real build's
//!    threads have no distinguishing symbol and resolve to raw pointers
//!    (`embarch-outpost/design.md` §3 decision 8), so [`Lane::unnamed`] is a
//!    first-class state, not an error path.
//!
//! 4. **A span with no closing record is open-ended and says so.** It is
//!    drawn out to the next traced event because a shape needs an extent, and
//!    [`Span::open_end`] is what stops that extent reading as a measurement.
//!
//! 5. **An extent below the capture's resolution is not a duration.** See
//!    `same_frame` above. This is the lie the timestamp change introduced, and
//!    it is the one a reader of a load table would otherwise never suspect.

use embarch_study_designer::outpost::{self, RecordKind};
use serde::Serialize;

/// How many rows one view will parse. A study long enough to overflow this is
/// a real thing (the wire's whole point is to run under load), and the answer
/// is to say so rather than to render a silently-shortened timeline: see
/// [`TraceView::rows_dropped_by_cap`], which the tab renders as a banner.
const MAX_ROWS: usize = 250_000;

/// One traced subject over time — a thread, the CPU's idle state, or one
/// interrupt vector.
#[derive(Debug, Clone, Serialize)]
pub struct Lane {
    /// Stable identity: `0x…` for a thread pointer, the vector number for an
    /// ISR, or a fixed key for the singleton lanes.
    pub key: String,
    /// What to show. Equals `key` when nothing named it.
    pub label: String,
    /// True when the manifest did not name this subject, so `label` is a raw
    /// number. Rendered visibly differently — a pointer that looks like a name
    /// is the defect decision 35 exists to prevent.
    pub unnamed: bool,
    /// `"thread"`, `"idle"`, `"isr"`, or `"gpio"`.
    pub kind: &'static str,
    pub spans: Vec<Span>,
    /// Point-in-time records on this lane — `thread_create`/`thread_name`.
    pub points: Vec<PointEvent>,
}

/// One interval a subject was running, in [`TraceView::unit`]s.
#[derive(Debug, Clone, Serialize)]
pub struct Span {
    pub from: u64,
    pub to: u64,
    /// The opening record was never seen (its own switch-in was among the
    /// losses). `from` is where this became observable, not where it started.
    pub open_start: bool,
    /// No closing record was seen. `to` is the next traced event, or the end
    /// of the capture — an extent to draw, never a measurement.
    pub open_end: bool,
    /// This interval overlaps a [`Gap`], so events inside it were dropped.
    /// Drawn hatched: the span is real, its continuity is not established.
    pub crosses_gap: bool,
    /// Both ends arrived in the **same frame**, so this span is real and its
    /// duration is below what the capture can resolve. `to == from`, and the
    /// span contributes nothing to any total.
    ///
    /// The normal state of an ISR span: a frame is milliseconds and an
    /// interrupt is microseconds. A view that quietly totalled these as zero
    /// would report an ISR load of 0% and look like a measurement.
    pub same_frame: bool,
}

/// A record with no duration — a marker, a thread creation, a name-set.
#[derive(Debug, Clone, Serialize)]
pub struct PointEvent {
    /// Position on the axis, in [`TraceView::unit`]s.
    pub t: u64,
    /// Which frame carried it. Two point events sharing a `t` and a
    /// `frame_index` are simultaneous *as far as this capture knows*, which is
    /// not the same as simultaneous.
    pub frame_index: u64,
    pub kind: String,
    pub label: String,
    pub unnamed: bool,
    /// `b` — an engineer's own marker argument, meaningless to this crate and
    /// passed through as the number it is.
    pub arg: u32,
}

/// Records the firmware itself reported dropping, and the interval they were
/// lost somewhere inside.
///
/// **A bound, and sometimes a loose one.** The band reaches back to the
/// previous frame *this file carries*, and the file carries only frames that
/// held records — a header frame arriving in between is a real arrival that
/// would have narrowed the bound, and it is not in the rendered CSV to be
/// seen. So a band can be two or three frame intervals wide where the true
/// bound was one. Too wide is still a bound; too narrow would be a claim.
#[derive(Debug, Clone, Serialize)]
pub struct Gap {
    /// The previous record-carrying frame's arrival — the earliest the losses
    /// can have begun.
    pub from: u64,
    /// The arrival of the frame reporting them — the latest they can have
    /// ended.
    pub to: u64,
    pub records_lost: u32,
    /// Which frame reported the loss, and where that row sat in the file.
    /// Surfaced so the view can say a band is a bound rather than look
    /// inconsistent.
    pub frame_index: u64,
    pub row_index: usize,
    /// The gap was reported by the first frame in the capture, so there is no
    /// earlier arrival to bound it with: `from == to`, and its extent is
    /// unknown rather than zero.
    pub unbounded_start: bool,
}

/// One traced subject's share of the capture window — the "load repartition"
/// the Trace view exists to produce (`embarch-ui/design.md` §3 decision 10).
///
/// **A total here is deliberately not the sum of everything drawn.** Four
/// classes of span are excluded from `total_extent` because their extent is
/// not a duration, and each is counted separately rather than quietly folded
/// in or quietly dropped:
///
/// - a span that **crosses a gap** ([`Span::crosses_gap`]) spent an unknown
///   part of its extent doing something nobody recorded;
/// - a span with **no closing record** ([`Span::open_end`]) was drawn out to
///   the next event so it had a shape, which is not the same as having lasted
///   that long;
/// - a span with **no opening record** ([`Span::open_start`]) began before it
///   became observable;
/// - a span **inside one frame** ([`Span::same_frame`]) is shorter than the
///   capture can resolve, so its extent is zero and its duration is unknown.
///
/// `entries` counts every span regardless, because "this subject ran N times"
/// survives all four doubts.
#[derive(Debug, Clone, Serialize)]
pub struct LoadSubject {
    /// Mirrors [`Lane::key`], so a row here and a lane there are the same
    /// subject without the caller matching on labels.
    pub key: String,
    pub label: String,
    /// Same first-class state as [`Lane::unnamed`]: this row's `label` is a
    /// raw pointer or vector number, and must not render as though it were a
    /// name.
    pub unnamed: bool,
    /// `"thread"`, `"idle"`, `"isr"`, or `"gpio"`.
    pub kind: &'static str,
    /// How many times this subject was entered, counting every span — the one
    /// figure none of the four exclusions above can invalidate.
    pub entries: usize,
    /// Spans that contributed to `total_extent`.
    pub measured_spans: usize,
    /// Summed extent of the measured spans only, in [`LoadSummary::unit`]s.
    pub total_extent: u64,
    /// `total_extent` as a fraction of the capture window, in `0.0..=1.0`.
    /// Of the **window**, not of the accounted time — see
    /// [`LoadSummary::isr_extent`] for why these do not sum to 1.
    pub share: f64,
    /// Spans left out of `total_extent`, by reason. A subject whose
    /// `excluded_spans` rivals its `measured_spans` has a total worth
    /// distrusting, and these are what let a reader see that.
    pub excluded_spans: usize,
    /// Summed extent of the excluded spans. Reported so the time is visible
    /// as unaccounted rather than absent.
    pub excluded_extent: u64,
    pub gap_crossing_spans: usize,
    pub open_ended_spans: usize,
    pub open_started_spans: usize,
    /// Spans wholly inside one frame. On a wire whose frames are
    /// milliseconds, this is where an ISR's entire life goes.
    pub same_frame_spans: usize,
}

/// The whole capture's load repartition, plus everything a reader needs to
/// know how much of it to believe.
///
/// **The headline honesty constraint** (`embarch-ui/design.md` §3 decision
/// 10): a repartition computed across an interval where records were dropped
/// is not a measurement, and neither is one whose subjects live below the
/// capture's resolution. [`Self::gap_fraction`] and
/// [`Self::same_frame_spans`] are what say how much of this window is in each
/// state, and both are meant to be rendered *beside* the numbers, not in a
/// footnote.
#[derive(Debug, Clone, Serialize)]
pub struct LoadSummary {
    /// `"ms"` or `"frame"` — mirrors [`TraceView::unit`], so a caller
    /// formatting this table needs nothing else.
    pub unit: &'static str,
    /// `t_to - t_from`. Zero for a capture too short to have a window, in
    /// which case every `share` is zero rather than a division by zero.
    pub window_extent: u64,
    /// Extent covered by at least one gap band, counted as a **union** — two
    /// overlapping bands cover their union, not the sum of their widths, and
    /// summing would let `gap_fraction` exceed 1 and read as nonsense.
    pub gap_extent: u64,
    /// `gap_extent / window_extent`, in `0.0..=1.0`. **The number that decides
    /// whether the rest of this struct is a measurement.**
    pub gap_fraction: f64,
    /// Total records the firmware itself said it lost, mirroring
    /// [`TraceView::records_lost`] so a summary row can carry it without the
    /// caller reaching back out to the view.
    pub records_lost: u64,
    /// Whether the extents here are milliseconds at all. False means every
    /// number below counts **frames**, and must be said as such.
    pub has_time_base: bool,
    /// Measured extent across **thread lanes only**, which are mutually
    /// exclusive — exactly one thread is the running context at any instant —
    /// so this is the one total that is meaningful to compare against the
    /// window. Zephyr's idle thread is a thread, and is included here.
    pub thread_extent: u64,
    /// The `cpu-idle` lane's measured extent — a **corroborating** figure,
    /// deliberately **not** added to [`Self::thread_extent`].
    ///
    /// Found by asserting the opposite and watching it fail against a real
    /// capture: idle is reported twice by construction, once as
    /// `RecordKind::Idle` records and once as ordinary switch in/out of the
    /// thread the manifest names `idle`. Adding them claimed nearly twice the
    /// window. They are also allowed to *disagree*, and the disagreement is
    /// worth seeing rather than averaging away.
    pub idle_record_extent: u64,
    /// ISR extent, kept **separate and deliberately not added** to the above:
    /// an ISR runs *inside* whatever it interrupted, so its extent is counted
    /// twice by construction. Adding these would produce a repartition
    /// summing past 100% and reading as a bug rather than as the nesting it
    /// is.
    ///
    /// Expect **zero**, and expect that to stay true on real silicon: an
    /// interrupt begins and ends inside one frame, so almost every ISR span is
    /// [`Span::same_frame`] and excluded. That is a property of what this wire
    /// can resolve, not of this arithmetic — the count in
    /// [`LoadSubject::same_frame_spans`] is where an ISR's activity shows up.
    pub isr_extent: u64,
    /// `window_extent - thread_extent`, floored at zero: window time no
    /// measured thread span accounts for. Large values mean the exclusions
    /// above ate the picture, not that the CPU was idle — idle is a thread and
    /// is already counted.
    pub unaccounted_extent: u64,
    /// Spans excluded across every subject for being inside one frame. A large
    /// number against a small `thread_extent` is the signature of a capture
    /// whose activity is finer than its frames.
    pub same_frame_spans: usize,
    /// Per-subject rows, sorted by `total_extent` descending so the heaviest
    /// subject is first. Ties break by `key` so the order is stable across
    /// runs of the same capture.
    pub subjects: Vec<LoadSubject>,
}

/// A study's outpost tap, decoded into something drawable.
#[derive(Debug, Clone, Serialize)]
pub struct TraceView {
    pub study_id: String,
    pub tap: String,
    /// Whether Core resolved names for this trace. Core's finding, from
    /// `streams/index.json`, never re-derived here.
    pub named: bool,
    /// Whether Core's arrival stamps reached this trace's frames. Also Core's
    /// finding — and note it is **not** the same as [`Self::has_time_base`]:
    /// Core can have stamped some frames and not others, which this view will
    /// not draw as milliseconds (see [`Self::unstamped_rows`]).
    pub timed: bool,
    /// Core's own reason, verbatim, when it had one. Rendered as given — this
    /// tab does not paraphrase a refusal.
    pub note: Option<String>,
    pub rows: usize,
    /// Rows past [`MAX_ROWS`], never silently discarded.
    pub rows_dropped_by_cap: usize,
    /// `"ms"` when every row carries an arrival stamp, `"frame"` otherwise.
    /// Every `t`, `from`, `to` and extent in this struct is in these units.
    pub unit: &'static str,
    pub t_from: u64,
    pub t_to: u64,
    /// True when [`Self::unit`] is `"ms"`. The axis is then Core's own wall
    /// clock — the same one `core_rx_utc_ms` carries on a sample or transcript
    /// row, which is what makes laying a trace beside a power capture an
    /// alignment rather than a guess.
    pub has_time_base: bool,
    /// Rows whose frame carried no arrival stamp. Non-zero forces `unit` to
    /// `"frame"` for the whole view: a timeline drawn half in milliseconds and
    /// half in frame indices is one axis pretending to be another.
    pub unstamped_rows: usize,
    /// How many frames this capture holds — the number of distinct arrival
    /// instants, and so the number of distinguishable moments in it.
    pub frames: usize,
    /// Median milliseconds between consecutive record-carrying frames, when
    /// there is a time base. **This is the resolution of everything above**:
    /// two records this far apart may be adjacent, and two records in one frame
    /// are indistinguishable in time.
    ///
    /// Median rather than mean, and over the frames this file has rather than
    /// every frame that arrived — a header frame carries no records and so
    /// leaves a double-width interval here, which the median absorbs and a
    /// mean would not.
    pub resolution_ms: Option<f64>,
    pub records_lost: u64,
    /// Rows whose arrival stamp went backwards. Expected to be zero — frames
    /// are stamped in arrival order — so a non-zero count means the host clock
    /// stepped backwards mid-capture (an NTP correction is the realistic
    /// cause), and it says so instead of drawing confidently.
    pub out_of_order_rows: usize,
    pub gaps: Vec<Gap>,
    pub lanes: Vec<Lane>,
    pub markers: Vec<PointEvent>,
    /// The load repartition over `lanes` (§3 decision 10). Arithmetic over the
    /// spans above, not a second decode — every doubt it reports is one the
    /// spans already carried.
    pub summary: LoadSummary,
}

/// The `idle` record's own lane. Zephyr traces idle **entry only** — there is
/// no `sys_trace_idle_exit_user()` to define at all, so idle ends at the next
/// thread-switch-in, which is what actually ends it
/// (`embarch-outpost/src/outpost_hooks.c`'s own comment, stated by the
/// firmware rather than inferred here).
const IDLE_LANE: &str = "cpu-idle";

fn kind_of(name: &str) -> Option<RecordKind> {
    // The vocabulary comes from the shared crate rather than a list written
    // out here, so a new record kind cannot be silently unknown to this view
    // while being known to the decoder that produced the row.
    (0u8..=u8::MAX).find_map(|b| match RecordKind::from_byte(b) {
        Some(k) if k.as_str() == name => Some(k),
        _ => None,
    })
}

struct Row {
    frame_index: u64,
    rx_utc_ms: Option<u64>,
    kind: Option<RecordKind>,
    a: u32,
    b: u32,
    name: String,
    /// Filled in once the axis unit is known: `rx_utc_ms` or `frame_index`.
    t: u64,
}

/// Splits one CSV line, honouring the double-quoting `name` may carry: a
/// resolved ISR label is `handler(inner_handler)` and a marker name comes from
/// an application's own macro, so a comma is not impossible.
fn split_row(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field)),
            other => field.push(other),
        }
    }
    out.push(field);
    out
}

/// Merges gap bands into a set of disjoint intervals clamped to the capture
/// window, so overlapping bands are counted once. Summing raw band widths
/// instead would let `gap_fraction` exceed 1 — and gap bands can overlap: two
/// consecutive frames can each report losses, and each band reaches back to
/// its predecessor's arrival.
fn merged_gap_extent(gaps: &[Gap], from: u64, to: u64) -> u64 {
    let mut bands: Vec<(u64, u64)> = gaps
        .iter()
        .filter_map(|g| {
            let lo = g.from.max(from);
            let hi = g.to.min(to);
            (lo < hi).then_some((lo, hi))
        })
        .collect();
    bands.sort_unstable();
    let mut total = 0u64;
    let mut cur: Option<(u64, u64)> = None;
    for (lo, hi) in bands {
        match cur {
            Some((clo, chi)) if lo <= chi => cur = Some((clo, chi.max(hi))),
            Some((clo, chi)) => {
                total += chi - clo;
                cur = Some((lo, hi));
            }
            None => cur = Some((lo, hi)),
        }
    }
    if let Some((clo, chi)) = cur {
        total += chi - clo;
    }
    total
}

/// Computes the load repartition. Pure arithmetic over already-built lanes —
/// it re-derives nothing about the trace, which is why every caveat it reports
/// is one [`Span`] already carried.
fn summarize(
    lanes: &[Lane],
    gaps: &[Gap],
    unit: &'static str,
    t_from: u64,
    t_to: u64,
    records_lost: u64,
) -> LoadSummary {
    let window_extent = t_to.saturating_sub(t_from);
    // Guarded rather than assumed non-zero: a capture of one frame has a
    // zero-width window, and a share of 0.0 is the honest answer there.
    let share_of =
        |c: u64| if window_extent == 0 { 0.0 } else { c as f64 / window_extent as f64 };

    let mut subjects: Vec<LoadSubject> = lanes
        .iter()
        .map(|lane| {
            let mut total_extent = 0u64;
            let mut excluded_extent = 0u64;
            let (mut measured, mut excluded) = (0usize, 0usize);
            let (mut crossing, mut open_end, mut open_start, mut same_frame) =
                (0usize, 0usize, 0usize, 0usize);
            for span in &lane.spans {
                let extent = span.to.saturating_sub(span.from);
                if span.crosses_gap {
                    crossing += 1;
                }
                if span.open_end {
                    open_end += 1;
                }
                if span.open_start {
                    open_start += 1;
                }
                if span.same_frame {
                    same_frame += 1;
                }
                if span.crosses_gap || span.open_end || span.open_start || span.same_frame {
                    excluded += 1;
                    excluded_extent += extent;
                } else {
                    measured += 1;
                    total_extent += extent;
                }
            }
            LoadSubject {
                key: lane.key.clone(),
                label: lane.label.clone(),
                unnamed: lane.unnamed,
                kind: lane.kind,
                entries: lane.spans.len(),
                measured_spans: measured,
                total_extent,
                share: share_of(total_extent),
                excluded_spans: excluded,
                excluded_extent,
                gap_crossing_spans: crossing,
                open_ended_spans: open_end,
                open_started_spans: open_start,
                same_frame_spans: same_frame,
            }
        })
        .collect();
    subjects.sort_by(|a, b| b.total_extent.cmp(&a.total_extent).then_with(|| a.key.cmp(&b.key)));

    // Threads only. The `idle` *lane* is the same time seen a second way, so
    // adding it double-counts — see `LoadSummary::idle_record_extent`.
    let thread_extent: u64 =
        subjects.iter().filter(|s| s.kind == "thread").map(|s| s.total_extent).sum();
    let idle_record_extent: u64 =
        subjects.iter().filter(|s| s.kind == "idle").map(|s| s.total_extent).sum();
    let isr_extent: u64 = subjects.iter().filter(|s| s.kind == "isr").map(|s| s.total_extent).sum();
    let same_frame_spans: usize = subjects.iter().map(|s| s.same_frame_spans).sum();
    let gap_extent = merged_gap_extent(gaps, t_from, t_to);

    LoadSummary {
        unit,
        window_extent,
        gap_extent,
        gap_fraction: share_of(gap_extent),
        records_lost,
        has_time_base: unit == "ms",
        thread_extent,
        idle_record_extent,
        isr_extent,
        unaccounted_extent: window_extent.saturating_sub(thread_extent),
        same_frame_spans,
        subjects,
    }
}

/// Parses a rendered `*.trace.csv` into a drawable view.
///
/// `note`, `named` and `timed` come from the caller (Core's own stream index),
/// never from these bytes: whether a manifest applied and whether the frames
/// were stamped are Core's findings, and re-deriving either from whether a
/// column happens to be populated would be a guess dressed as a check.
pub fn parse(
    study_id: &str,
    tap: &str,
    csv: &str,
    named: bool,
    timed: bool,
    note: Option<String>,
) -> Result<TraceView, String> {
    let mut lines = csv.split('\n');
    let header = lines.next().unwrap_or_default().trim_end_matches('\r');
    if header != outpost::csv_header() {
        return Err(format!(
            "this capture's columns are {header:?}, and this build reads {:?} — refusing to guess \
             which column moved, for the same reason a manifest from another build is refused",
            outpost::csv_header()
        ));
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut rows_dropped_by_cap = 0usize;
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if rows.len() >= MAX_ROWS {
            rows_dropped_by_cap += 1;
            continue;
        }
        let f = split_row(line);
        if f.len() < 9 {
            // A short line is a truncated write, not a row shape to interpret.
            continue;
        }
        // Positional access is safe only because the header check above already
        // refused anything whose columns are not exactly `outpost::csv_header()`
        // — that check is what stands in for parsing the header into a name
        // map. The indices moved by two when record layout 3 restored the DUT's
        // `cycles`/`us` columns ahead of `kind`; they are:
        //   0 frame_index, 1 frame_seq, 2 rx_utc_ms, 3 cycles, 4 us,
        //   5 kind, 6 a, 7 b, 8 name
        let Ok(frame_index) = f[0].parse::<u64>() else { continue };
        rows.push(Row {
            frame_index,
            rx_utc_ms: if f[2].is_empty() { None } else { f[2].parse::<u64>().ok() },
            kind: kind_of(&f[5]),
            a: f[6].parse::<u32>().unwrap_or(0),
            b: f[7].parse::<u32>().unwrap_or(0),
            name: f[8].clone(),
            t: 0,
        });
    }

    // The axis unit, decided once for the whole view. Milliseconds require
    // *every* row to be stamped: a mixed axis is the one thing worse than a
    // coarse one.
    let unstamped_rows = rows.iter().filter(|r| r.rx_utc_ms.is_none()).count();
    let unit = if !rows.is_empty() && unstamped_rows == 0 { "ms" } else { "frame" };
    for row in &mut rows {
        row.t = match (unit, row.rx_utc_ms) {
            ("ms", Some(ms)) => ms,
            _ => row.frame_index,
        };
    }

    // Frames, in the order the file mentions them. This is both the count of
    // distinguishable instants in the capture and what a gap band reaches back
    // through — a gap is bounded by the frame *before* the one reporting it.
    //
    // Deliberately **not** treating a jump in `frame_index` as a lost frame:
    // header frames carry no records, so they occupy an index and contribute
    // no row. A hole here is usually a header, not a loss. Frames the wire
    // actually lost are Core's finding, from the `seq` bytes, and are in the
    // note.
    let mut frame_order: Vec<(u64, u64)> = Vec::new();
    let mut frame_pos: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for r in &rows {
        if let std::collections::hash_map::Entry::Vacant(slot) = frame_pos.entry(r.frame_index) {
            slot.insert(frame_order.len());
            frame_order.push((r.frame_index, r.t));
        }
    }

    // Median inter-frame interval: the capture's own statement of how finely
    // it can place anything. Median rather than mean because one long pause in
    // a study — a step waiting on a DUT — would drag a mean and misreport the
    // resolution of everything else.
    let resolution_ms = (unit == "ms" && frame_order.len() > 1).then(|| {
        let mut deltas: Vec<u64> =
            frame_order.windows(2).map(|w| w[1].1.saturating_sub(w[0].1)).collect();
        deltas.sort_unstable();
        let mid = deltas.len() / 2;
        if deltas.len().is_multiple_of(2) {
            (deltas[mid - 1] + deltas[mid]) as f64 / 2.0
        } else {
            deltas[mid] as f64
        }
    });

    // Gaps first: a gap row is the only one whose extent comes from somewhere
    // other than its own record, and taking them out is what makes the rest a
    // stream this can pair switch-ins against.
    let mut gaps: Vec<Gap> = Vec::new();
    let mut records_lost = 0u64;
    for (i, r) in rows.iter().enumerate() {
        if r.kind != Some(RecordKind::Gap) {
            continue;
        }
        records_lost += u64::from(r.a);
        // A gap record is always the first record of its frame, so the losses
        // happened after the previous frame arrived and before this one did.
        // "The previous frame" here is the previous one *with records* — a
        // header frame in between is an arrival this file cannot see, so the
        // band is occasionally wider than the true bound and never narrower.
        let pos = frame_pos.get(&r.frame_index).copied().unwrap_or(0);
        let from = if pos == 0 { r.t } else { frame_order[pos - 1].1 };
        gaps.push(Gap {
            from,
            to: r.t,
            records_lost: r.a,
            frame_index: r.frame_index,
            row_index: i,
            unbounded_start: pos == 0,
        });
    }
    gaps.sort_by_key(|g| g.from);

    let timeline: Vec<&Row> = rows.iter().filter(|r| r.kind != Some(RecordKind::Gap)).collect();
    let out_of_order_rows = timeline.windows(2).filter(|w| w[1].t < w[0].t).count();

    let t_from = timeline.first().map(|r| r.t).unwrap_or(0);
    let t_to = timeline
        .iter()
        .map(|r| r.t)
        .max()
        .unwrap_or(t_from)
        .max(gaps.iter().map(|g| g.to).max().unwrap_or(0));

    // ---- lanes ------------------------------------------------------------
    //
    // Insertion-ordered, so lanes appear in the order the capture first
    // mentions them: a thread's own creation record is usually its first
    // appearance, which puts the lanes in a stable, meaningful order without
    // sorting by a number a reader has no reason to care about.
    struct Building {
        lane: Lane,
        /// `(t, frame_index, open_start)` for each currently-open span.
        open: Vec<(u64, u64, bool)>,
    }
    let mut order: Vec<String> = Vec::new();
    let mut building: std::collections::HashMap<String, Building> = std::collections::HashMap::new();
    let mut markers: Vec<PointEvent> = Vec::new();

    let ensure = |building: &mut std::collections::HashMap<String, Building>,
                      order: &mut Vec<String>,
                      key: String,
                      label: String,
                      unnamed: bool,
                      kind: &'static str| {
        if !building.contains_key(&key) {
            order.push(key.clone());
            building.insert(
                key.clone(),
                Building {
                    lane: Lane { key, label, unnamed, kind, spans: Vec::new(), points: Vec::new() },
                    open: Vec::new(),
                },
            );
        } else if let Some(b) = building.get_mut(&key) {
            // A later record may carry a name the first one did not (a
            // `thread_name` record, or simply a manifest hit on a different
            // field). Upgrading is safe; downgrading a name back to a pointer
            // is not, so it never happens.
            if b.lane.unnamed && !unnamed {
                b.lane.label = label;
                b.lane.unnamed = false;
            }
        }
    };

    /// One closed span. `same_frame` is computed here, in the one place that
    /// knows both ends' frames, so no caller can forget it.
    fn close(from: (u64, u64, bool), to_t: u64, to_frame: u64, open_end: bool) -> Span {
        let (from_t, from_frame, open_start) = from;
        Span {
            from: from_t,
            to: to_t,
            open_start,
            open_end,
            crosses_gap: false,
            same_frame: from_frame == to_frame,
        }
    }

    let thread_key = |a: u32| format!("0x{a:08x}");

    for r in timeline.iter() {
        let Some(kind) = r.kind else {
            // A kind this build does not know decodes as itself rather than
            // failing the row (`OutpostRecord::kind`'s own doc comment) — it
            // has no lane, so it lands as a point event nobody has to
            // interpret.
            markers.push(PointEvent {
                t: r.t,
                frame_index: r.frame_index,
                kind: "unknown".to_string(),
                label: String::new(),
                unnamed: true,
                arg: r.b,
            });
            continue;
        };

        match kind {
            RecordKind::ThreadSwitchIn => {
                let key = thread_key(r.a);
                let named_here = !r.name.is_empty();
                let label = if named_here { r.name.clone() } else { key.clone() };
                ensure(&mut building, &mut order, key.clone(), label, !named_here, "thread");
                if let Some(b) = building.get_mut(&key) {
                    b.open.push((r.t, r.frame_index, false));
                }
                // A switch-in is also what ends idle, per the firmware's own
                // note: there is no idle-exit hook to define.
                if let Some(idle) = building.get_mut(IDLE_LANE) {
                    if let Some(open) = idle.open.pop() {
                        idle.lane.spans.push(close(open, r.t, r.frame_index, false));
                    }
                }
            }
            RecordKind::ThreadSwitchOut => {
                let key = thread_key(r.a);
                let named_here = !r.name.is_empty();
                let label = if named_here { r.name.clone() } else { key.clone() };
                ensure(&mut building, &mut order, key.clone(), label, !named_here, "thread");
                if let Some(b) = building.get_mut(&key) {
                    match b.open.pop() {
                        Some(open) => {
                            b.lane.spans.push(close(open, r.t, r.frame_index, false));
                        }
                        // Its switch-in was among the losses. The run is real
                        // and its start is not known, which is exactly what
                        // `open_start` says.
                        None => b.lane.spans.push(Span {
                            from: r.t,
                            to: r.t,
                            open_start: true,
                            open_end: false,
                            crosses_gap: false,
                            same_frame: true,
                        }),
                    }
                }
            }
            RecordKind::IsrEnter | RecordKind::IsrExit => {
                let unidentified = r.a == outpost::IRQ_UNKNOWN;
                let key = if unidentified {
                    "isr-unidentified".to_string()
                } else {
                    format!("irq-{}", r.a)
                };
                let named_here = !r.name.is_empty();
                let label = if unidentified {
                    // The firmware said it could not name the active vector.
                    // Reporting that is the answer; picking a vector is not.
                    "ISR (vector not reported)".to_string()
                } else if named_here {
                    r.name.clone()
                } else {
                    format!("IRQ {}", r.a)
                };
                ensure(
                    &mut building,
                    &mut order,
                    key.clone(),
                    label,
                    unidentified || !named_here,
                    "isr",
                );
                if let Some(b) = building.get_mut(&key) {
                    if kind == RecordKind::IsrEnter {
                        b.open.push((r.t, r.frame_index, false));
                    } else {
                        match b.open.pop() {
                            Some(open) => {
                                b.lane.spans.push(close(open, r.t, r.frame_index, false));
                            }
                            None => b.lane.spans.push(Span {
                                from: r.t,
                                to: r.t,
                                open_start: true,
                                open_end: false,
                                crosses_gap: false,
                                same_frame: true,
                            }),
                        }
                    }
                }
            }
            RecordKind::Idle => {
                ensure(
                    &mut building,
                    &mut order,
                    IDLE_LANE.to_string(),
                    "cpu idle".to_string(),
                    false,
                    "idle",
                );
                if let Some(b) = building.get_mut(IDLE_LANE) {
                    // An idle entry with one already open means the switch-in
                    // that closes it was lost. Close the old one where it
                    // stopped being observable rather than nesting idle
                    // inside itself.
                    if let Some(open) = b.open.pop() {
                        b.lane.spans.push(close(open, r.t, r.frame_index, true));
                    }
                    b.open.push((r.t, r.frame_index, false));
                }
            }
            RecordKind::ThreadCreate | RecordKind::ThreadName => {
                let key = thread_key(r.a);
                let named_here = !r.name.is_empty();
                let label = if named_here { r.name.clone() } else { key.clone() };
                ensure(&mut building, &mut order, key.clone(), label.clone(), !named_here, "thread");
                if let Some(b) = building.get_mut(&key) {
                    b.lane.points.push(PointEvent {
                        t: r.t,
                        frame_index: r.frame_index,
                        kind: kind.as_str().to_string(),
                        label,
                        unnamed: !named_here,
                        arg: r.b,
                    });
                }
            }
            RecordKind::Marker => {
                let named_here = !r.name.is_empty();
                markers.push(PointEvent {
                    t: r.t,
                    frame_index: r.frame_index,
                    kind: kind.as_str().to_string(),
                    // A marker with no name in the manifest is its ID. The ID
                    // is a real answer; a made-up name would not be.
                    label: if named_here { r.name.clone() } else { format!("marker {}", r.a) },
                    unnamed: !named_here,
                    arg: r.b,
                });
            }
            RecordKind::GpioDispatch | RecordKind::GpioCallbackDone => {
                // Point events on a per-subject lane, and deliberately **not**
                // spans.
                //
                // A handler's span is recoverable — it runs from the record
                // before its `gpio_callback_done` to that record, because
                // Zephyr places the hook *after* `cb->handler()` returns. That
                // is exactly why it is not drawn here: reading
                // `gpio_callback_done` as an *entry* marker attributes every
                // handler's time to the wrong handler, and the picture stays
                // entirely readable while it does. Drawing the points is a
                // true statement about when each dispatch and each completion
                // happened; drawing spans is a claim this view has not earned
                // yet, and a wrong span is worse than an honest point.
                //
                // `b` is the pin mask on a completion and 0 on a dispatch —
                // carried as `arg` either way, since Zephyr truncates the mask
                // through a uint8_t parameter before the firmware ever sees a
                // dispatch's copy of it.
                let key = format!("gpio:0x{:08x}", r.a);
                let named_here = !r.name.is_empty();
                let label =
                    if named_here { r.name.clone() } else { format!("0x{:08x}", r.a) };
                ensure(&mut building, &mut order, key.clone(), label.clone(), !named_here, "gpio");
                if let Some(bl) = building.get_mut(&key) {
                    bl.lane.points.push(PointEvent {
                        t: r.t,
                        frame_index: r.frame_index,
                        kind: kind.as_str().to_string(),
                        label,
                        unnamed: !named_here,
                        arg: r.b,
                    });
                }
            }
            RecordKind::Gap => unreachable!("gap rows are filtered out of `timeline`"),
        }
    }

    // Whatever is still open at the end never got a closing record. Drawn out
    // to the end of the capture, flagged, so the extent is a shape and not a
    // duration.
    let mut lanes: Vec<Lane> = Vec::new();
    for key in order {
        if let Some(mut b) = building.remove(&key) {
            let open: Vec<(u64, u64, bool)> = b.open.drain(..).collect();
            // `_from_frame` goes unused on purpose: an open span has no closing
            // frame to compare against, so `same_frame` is not the reason it is
            // excluded — `open_end` is. Claiming both would double-count it in
            // the reason columns a reader uses to judge a total.
            for (from_t, _from_frame, open_start) in open {
                b.lane.spans.push(Span {
                    from: from_t,
                    to: t_to,
                    open_start,
                    open_end: true,
                    crosses_gap: false,
                    same_frame: false,
                });
            }
            b.lane.spans.sort_by_key(|s| s.from);
            for span in &mut b.lane.spans {
                span.crosses_gap = gaps.iter().any(|g| span.from < g.to && g.from < span.to);
            }
            lanes.push(b.lane);
        }
    }

    let summary = summarize(&lanes, &gaps, unit, t_from, t_to, records_lost);

    Ok(TraceView {
        study_id: study_id.to_string(),
        tap: tap.to_string(),
        named,
        timed,
        note,
        rows: rows.len(),
        rows_dropped_by_cap,
        unit,
        t_from,
        t_to,
        has_time_base: unit == "ms",
        unstamped_rows,
        frames: frame_order.len(),
        resolution_ms,
        records_lost,
        out_of_order_rows,
        gaps,
        lanes,
        markers,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real `native_sim` capture, run through **Core's own renderer**
    /// against the manifest its own build produced — regenerated by
    /// `embarch-core`'s `outpost_manifest::regenerate_the_ui_trace_fixtures`
    /// from `embarch-core/tests/fixtures/outpost-native-sim.bin`, whose bytes
    /// came out of the firmware encoder rather than out of anything in Rust.
    ///
    /// Committed as the rendered CSV rather than as the raw stream on purpose:
    /// this module's job *starts* at Core's output, so a fixture that went
    /// through the firmware encoder and then Core's decoder exercises the
    /// actual seam.
    ///
    /// **This half carries no arrival stamps, and could not.** That capture
    /// went to a `native_sim` process's stdout; no receiver ever saw it, and
    /// no outpost byte has crossed a real UART on any board. So it is the
    /// fixture for the untimed axis, which is a state the product will really
    /// produce (a study whose arrival log failed to write, or a `--raw` file
    /// decoded by hand).
    const REAL_TRACE: &str = include_str!("../tests/fixtures/outpost-native-sim.trace.csv");

    /// The same real frames with **synthesised** 20 ms arrival stamps — real
    /// frame boundaries, invented pacing, said out loud here because that is
    /// the only way a stamped fixture can exist until the wire has run on
    /// hardware.
    ///
    /// It is worth having anyway: everything about the millisecond axis — the
    /// resolution figure, `same_frame` exclusion, a gap band bounded by two
    /// arrivals — is arithmetic over frame stamps, and this exercises all of
    /// it against frames a DUT really produced.
    const STAMPED_TRACE: &str =
        include_str!("../tests/fixtures/outpost-native-sim-stamped.trace.csv");

    pub(super) fn real() -> TraceView {
        parse("study-1", "outpost", REAL_TRACE, true, false, None).expect("the real trace parses")
    }

    pub(super) fn stamped() -> TraceView {
        parse("study-1", "outpost", STAMPED_TRACE, true, true, None)
            .expect("the stamped trace parses")
    }

    #[test]
    fn a_real_capture_decodes_into_lanes() {
        let view = real();
        assert_eq!(view.rows, 831, "the committed capture is 831 records");
        assert_eq!(view.rows_dropped_by_cap, 0);
        assert_eq!(view.frames, 32, "32 of its 41 frames carry records");
        assert_eq!(view.out_of_order_rows, 0);

        let threads: Vec<&Lane> = view.lanes.iter().filter(|l| l.kind == "thread").collect();
        assert_eq!(threads.len(), 7, "this capture mentions seven distinct thread pointers");
        assert!(view.lanes.iter().any(|l| l.kind == "idle"));
        assert!(view.lanes.iter().any(|l| l.kind == "isr"));
    }

    /// **An unstamped capture is drawn against frames, and says so.** The
    /// alternative — millisecond labels over frame indices — is the one lie
    /// this axis can tell.
    #[test]
    fn an_unstamped_capture_is_drawn_against_frames() {
        let view = real();
        assert_eq!(view.unit, "frame");
        assert!(!view.has_time_base);
        assert!(!view.timed, "Core said it stamped nothing");
        assert_eq!(view.unstamped_rows, view.rows, "no row in this capture has a time");
        assert_eq!(view.resolution_ms, None, "a resolution in ms with no ms to measure");
        // And the axis is still a real coordinate: frame indices, in order.
        assert!(view.t_to > view.t_from);
        assert_eq!(view.summary.unit, "frame");
        assert!(!view.summary.has_time_base);
    }

    /// The stamped half: every extent is milliseconds of Core's own wall
    /// clock, which is the clock every other stream in the study is on.
    #[test]
    fn a_stamped_capture_is_drawn_against_core_s_own_clock() {
        let view = stamped();
        assert_eq!(view.unit, "ms");
        assert!(view.has_time_base && view.timed);
        assert_eq!(view.unstamped_rows, 0);
        assert_eq!(view.rows, 831, "the same records, differently placed");
        // Absolute UTC milliseconds, so a row here and a `core_rx_utc_ms` on a
        // power sample are directly comparable.
        assert!(view.t_from >= 1_700_000_000_000);
        // 20 ms per *frame*, and the median interval between record-carrying
        // frames is still 20: header frames repeat every ~100 ms, so only a few
        // intervals are double-width.
        assert_eq!(view.resolution_ms, Some(20.0));
        // The span is 38 frame intervals, not 31: this capture's 32
        // record-carrying frames sit at indices 1..=39, with nine header frames
        // interleaved. The axis is arrival time, so the headers' 20 ms each are
        // part of it.
        assert_eq!(view.t_to - view.t_from, 20 * 38);
    }

    /// **The resolution claim, tested where it actually bites.** Records in
    /// one frame share a time, so a view must not present them as ordered in
    /// time — only as ordered.
    #[test]
    fn records_in_one_frame_share_one_instant() {
        let view = stamped();
        let mut by_t: std::collections::BTreeMap<u64, usize> = std::collections::BTreeMap::new();
        for lane in &view.lanes {
            for p in &lane.points {
                *by_t.entry(p.t).or_default() += 1;
            }
        }
        for m in &view.markers {
            *by_t.entry(m.t).or_default() += 1;
        }
        assert!(
            by_t.values().any(|n| *n > 1),
            "no two events share an instant, which cannot be true of a frame-stamped capture"
        );
        // Distinct instants can never exceed the number of frames.
        assert!(by_t.len() <= view.frames);
    }

    /// The requirement most of a real build depends on: three of this
    /// capture's seven threads have no distinguishing symbol, and the real
    /// reference-dut image resolves only five of its threads at all. A view that
    /// assumed a name would look broken against real data.
    #[test]
    fn unnamed_threads_stay_visibly_unnamed() {
        let view = real();
        let threads: Vec<&Lane> = view.lanes.iter().filter(|l| l.kind == "thread").collect();
        let unnamed: Vec<&&Lane> = threads.iter().filter(|l| l.unnamed).collect();
        assert_eq!(unnamed.len(), 3, "three of this capture's threads resolve to no name");
        for lane in unnamed {
            assert_eq!(lane.label, lane.key, "an unnamed thread must render as the pointer it is");
            assert!(lane.label.starts_with("0x"));
        }
        for lane in threads.iter().filter(|l| !l.unnamed) {
            assert!(!lane.label.starts_with("0x"), "a named thread kept its pointer as a label");
        }
    }

    /// This capture's ISRs all report `IRQ_UNKNOWN` — there is no
    /// `_sw_isr_table` in the `native_sim` ELF at all, which the manifest says
    /// out loud in its own notes. The lane must say the vector was not
    /// reported rather than invent one, and must not present `4294967295` as a
    /// vector number either.
    #[test]
    fn an_unreported_vector_is_reported_as_unreported() {
        let view = real();
        let isr: &Lane = view.lanes.iter().find(|l| l.kind == "isr").expect("an isr lane");
        assert_eq!(isr.key, "isr-unidentified");
        assert!(isr.unnamed);
        assert!(isr.label.contains("not reported"), "{}", isr.label);
        assert!(!isr.label.contains("4294967295"));
        assert_eq!(isr.spans.len(), 75, "75 enter/exit pairs");
    }

    /// **A frame boundary is not a time boundary**, and layout 3 is what lets
    /// this view say so.
    ///
    /// Under layout 2 the DUT carried no clock and a record's only time was its
    /// frame's arrival stamp, so an `isr_enter`/`isr_exit` pair landing in two
    /// different frames was indistinguishable from an ISR that really ran for a
    /// frame interval. This capture has five such pairs — and every one of them
    /// has **identical enter and exit `cycles`**. They took no measurable time
    /// at all; the ring simply drained between the two records. Reading those
    /// five as frame-long ISRs would be a load figure invented out of framing.
    ///
    /// The view still places spans on a frame/ms axis, so it draws these five
    /// as crossing — which is honest about what it plotted. What it must not do
    /// is claim they are all confined to one frame, which is what this test
    /// asserted while the fixture was a layout-2 capture.
    #[test]
    fn a_frame_boundary_is_not_a_time_boundary() {
        let view = stamped();
        let isr: &Lane = view.lanes.iter().find(|l| l.kind == "isr").expect("an isr lane");

        let crossing = isr.spans.iter().filter(|s| !s.same_frame).count();
        let confined = isr.spans.iter().filter(|s| s.same_frame).count();
        assert_eq!(confined, 69, "most of this capture's ISRs open and close inside one frame");
        // Six, not five: this capture has 75 `isr_enter` records and 74
        // `isr_exit`, so five are genuine straddles and the sixth is the ISR
        // still open when the capture ended, drawn out to the end and flagged.
        // Both are "not confined to one frame"; only the five are about timing.
        assert_eq!(crossing, 6, "five straddle a ring drain, and one never closed");
        assert_eq!(confined + crossing, isr.spans.len());

        // A span confined to one frame still has zero extent on this axis:
        // the axis counts frames, and both ends are the same frame.
        assert!(isr.spans.iter().filter(|s| s.same_frame).all(|s| s.to == s.from));

        let row = view.summary.subjects.iter().find(|s| s.kind == "isr").expect("an isr row");

        assert_eq!(row.entries, isr.spans.len(), "every ISR span is still counted");
        // Three of the six non-confined spans land on *distinct* arrival
        // stamps and so get a measured extent on the ms axis; the rest close
        // inside their own frame or never close. That this is 3 and not 0 is
        // the whole difference from the layout-2 fixture, where the summary
        // could only ever report an ISR load of zero -- and it is a number to
        // read carefully, because those extents are ring-drain intervals, not
        // ISR durations. The `cycles` column says the ISRs themselves took no
        // measurable time; see this test's own doc comment.
        assert_eq!(row.measured_spans, 3);
        assert_eq!(row.same_frame_spans, confined);
        assert!(row.total_extent > 0);
        assert!(view.summary.isr_extent > 0);
        assert!(view.summary.same_frame_spans >= confined);
    }

    /// **A gap band is a bound, not a measurement**: a gap record is the first
    /// record of its frame, so the losses fall between the previous frame's
    /// arrival and its own.
    #[test]
    fn a_gap_is_bounded_by_the_frames_around_it() {
        let view = stamped();
        assert_eq!(view.gaps.len(), 3, "this capture overflowed its ring three times");
        let gap = &view.gaps[0];
        assert_eq!(gap.records_lost, 19_991, "the first of three gaps; 20,001 lost in total");
        assert!(!gap.unbounded_start);
        // 40 ms, not 20: a header frame arrived between the two record frames
        // bracketing this gap, and the rendered CSV does not carry header
        // frames — so the band is two frame intervals wide where the true bound
        // was one. Wider than necessary, never narrower, and that asymmetry is
        // the point.
        assert_eq!(gap.to - gap.from, 40);
        assert_eq!(view.records_lost, 20_001);

        // The untimed half draws the identical bound in frame indices: the
        // same two-index reach, for the same reason.
        let untimed = real();
        assert_eq!(untimed.gaps.len(), 3);
        assert_eq!(untimed.gaps[0].to - untimed.gaps[0].from, 2);
    }

    /// The finding that survived the reshape: a gap band is **not** an empty
    /// interval. The records at both its ends survived, so drawing it as a
    /// hole would erase real data. The band is an overlay, and every span it
    /// touches is flagged instead.
    #[test]
    fn a_gap_marks_spans_incomplete_rather_than_erasing_them() {
        let view = stamped();
        let crossing: usize = view
            .lanes
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.crosses_gap)
            .count();
        assert!(
            crossing > 0,
            "this capture has spans overlapping its gap; none were flagged, so the view would \
             present an interrupted run as a continuous one"
        );
    }

    #[test]
    fn markers_keep_the_engineers_own_argument_and_name() {
        let view = real();
        assert_eq!(view.markers.len(), 155);
        let names: std::collections::BTreeSet<&str> =
            view.markers.iter().map(|m| m.label.as_str()).collect();
        assert!(names.contains("WORK_BEGIN"), "{names:?}");
        assert!(names.contains("BURST"), "{names:?}");
        assert!(view.markers.iter().all(|m| !m.unnamed), "this build's markers all resolve");
    }

    /// A trace with no manifest still decodes into a timeline — it just has no
    /// names in it — and every lane must then be visibly unnamed. This is the
    /// shape a refused trace arrives in, and the reason it must never be
    /// presented as a named one.
    #[test]
    fn a_trace_with_no_names_is_all_unnamed_lanes() {
        let stripped: String = REAL_TRACE
            .lines()
            .enumerate()
            .map(|(i, line)| {
                if i == 0 {
                    line.to_string()
                } else {
                    let mut f = split_row(line);
                    // `name` is the last column, index 8 since record layout 3
                    // put `cycles`/`us` back ahead of `kind`.
                    if f.len() >= 9 {
                        f[8] = String::new();
                    }
                    f.join(",")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let view = parse(
            "study-1",
            "outpost",
            &stripped,
            false,
            false,
            Some("decoded but NOT named: manifest build_id \"a\" != firmware build_id \"b\"".to_string()),
        )
        .expect("an unnamed trace still parses");

        assert!(!view.named);
        assert!(view.note.as_deref().is_some_and(|n| n.contains("NOT named")));
        assert!(
            view.lanes.iter().filter(|l| l.kind == "thread").all(|l| l.unnamed),
            "a nameless trace produced a lane claiming a name"
        );
        assert!(view.markers.iter().all(|m| m.unnamed));
        // And it is still a real timeline: the structure survives the refusal.
        assert_eq!(view.rows, 831);
        assert_eq!(view.gaps.len(), 3);
    }

    /// Named and timed are independent, and a trace can be named and untimed —
    /// which is exactly what the committed unstamped fixture is.
    #[test]
    fn named_and_timed_are_reported_separately() {
        let view = parse(
            "s",
            "outpost",
            REAL_TRACE,
            true,
            false,
            Some("decoded but NOT timed: no arrival stamps were recorded for this capture."
                .to_string()),
        )
        .expect("parses");
        assert!(view.named, "an untimed trace is still a named one");
        assert!(!view.timed);
        assert!(view.note.as_deref().is_some_and(|n| n.contains("NOT timed")));
    }

    /// Columns this build does not recognize are refused rather than guessed
    /// at — the same posture as a manifest from another record layout. The
    /// layout-1 column list is the case that matters: it is a real shape this
    /// code will meet in old results directories.
    #[test]
    fn an_unfamiliar_column_list_is_refused() {
        let err = parse("s", "t", "cycles,us,kind,a,b,name\n0,0,idle,0,0,\n", true, true, None)
            .expect_err("must refuse");
        assert!(err.contains("refusing to guess"), "{err}");
    }

    /// One unstamped frame drops the whole view to the frame axis. Half a
    /// timeline in milliseconds and half in frame indices is one axis
    /// pretending to be another.
    #[test]
    fn a_partly_stamped_capture_falls_back_to_frames_rather_than_mixing_axes() {
        let header = outpost::csv_header();
        let csv = format!(
            "{header}\n\
             0,0,1700000000000,10,10.000,thread_switch_in,4096,0,worker\n\
             1,1,,20,20.000,thread_switch_out,4096,0,worker\n"
        );
        let view = parse("s", "t", &csv, true, true, None).expect("parses");
        assert_eq!(view.unit, "frame");
        assert!(!view.has_time_base);
        assert_eq!(view.unstamped_rows, 1);
        assert!(view.timed, "Core did stamp some of it, and that is worth reporting");
        assert_eq!(view.t_from, 0);
        assert_eq!(view.t_to, 1);
    }

    /// Dumps a parsed view as the JSON `assets/app.js` actually receives, so
    /// the browser half can be driven against real data instead of a
    /// hand-written object.
    ///
    /// `#[ignore]`d: it writes a file, and its only consumer is a manual
    /// check. There is no `node` on this bench, so the render path is exercised
    /// by re-evaluating `app.js`'s IIFE body in headless Firefox against these
    /// bytes — the alternative being to change 300 lines of chart code and
    /// hope.
    ///
    /// ```text
    /// EMBARCH_TRACE_VIEW_JSON=$HOME/.embarch-uicheck/view.json \
    ///     cargo test dump_a_view_for_the_browser_harness -- --ignored
    /// ```
    #[test]
    #[ignore = "writes a JSON file for the manual browser check"]
    fn dump_a_view_for_the_browser_harness() {
        let path = std::env::var("EMBARCH_TRACE_VIEW_JSON")
            .expect("set EMBARCH_TRACE_VIEW_JSON to an output path");
        let json = serde_json::to_string(&stamped()).expect("serializes");
        std::fs::write(&path, json).expect("writes");
    }

    #[test]
    fn the_kind_vocabulary_comes_from_the_shared_crate() {
        assert_eq!(kind_of("thread_switch_in"), Some(RecordKind::ThreadSwitchIn));
        assert_eq!(kind_of("gap"), Some(RecordKind::Gap));
        assert_eq!(kind_of("unknown_42"), None);
    }
}

#[cfg(test)]
mod load_summary_tests {
    use super::tests::{real, stamped};
    use super::*;

    /// The summary is arithmetic over the same spans the timeline draws, so
    /// every subject the view has a lane for has a row here — no filtering,
    /// no top-N.
    #[test]
    fn every_lane_gets_a_row() {
        let view = real();
        assert_eq!(view.summary.subjects.len(), view.lanes.len());
        for lane in &view.lanes {
            let row = view
                .summary
                .subjects
                .iter()
                .find(|s| s.key == lane.key)
                .expect("every lane has a summary row");
            assert_eq!(row.label, lane.label);
            assert_eq!(row.unnamed, lane.unnamed);
            assert_eq!(row.kind, lane.kind);
            assert_eq!(row.entries, lane.spans.len());
        }
    }

    /// §3 decision 10's headline constraint: this capture really did lose
    /// records, so the summary must report the affected fraction rather than
    /// present its totals as a clean measurement.
    #[test]
    fn a_lossy_capture_reports_its_gap_fraction() {
        let view = stamped();
        assert_eq!(view.records_lost, 20_001, "the committed capture lost 20,001 records");
        assert_eq!(view.summary.records_lost, view.records_lost);
        assert!(view.summary.gap_extent > 0, "the gap band covers a non-zero interval");
        assert!(
            view.summary.gap_fraction > 0.0 && view.summary.gap_fraction <= 1.0,
            "a fraction of the window, never more than all of it: {}",
            view.summary.gap_fraction
        );
    }

    /// Overlapping bands are counted once. Summing raw widths instead is the
    /// bug that lets a fraction exceed 1.
    #[test]
    fn overlapping_gap_bands_are_counted_as_a_union() {
        let gap = |from, to, row_index| Gap {
            from,
            to,
            records_lost: 1,
            frame_index: row_index as u64,
            row_index,
            unbounded_start: false,
        };
        let gaps = vec![gap(100, 200, 0), gap(150, 250, 1), gap(400, 450, 2)];
        // Union is 100..250 (150) plus 400..450 (50), not 100+100+50.
        assert_eq!(merged_gap_extent(&gaps, 0, 1_000), 200);
        // And it clamps to the window rather than counting outside it.
        assert_eq!(merged_gap_extent(&gaps, 0, 120), 20);
    }

    /// An extent is not a duration. A span that is open at either end, that
    /// crosses a gap, or that lives inside one frame must not contribute its
    /// drawn width to a total — but it must still be counted, because "this
    /// ran" is not in doubt.
    #[test]
    fn untrustworthy_spans_are_excluded_but_still_counted() {
        let view = stamped();
        for s in &view.summary.subjects {
            assert_eq!(s.entries, s.measured_spans + s.excluded_spans);
            assert!(
                s.total_extent <= view.summary.window_extent,
                "{} claims more measured time than the window holds",
                s.label
            );
        }
        let with_exclusions: Vec<&LoadSubject> =
            view.summary.subjects.iter().filter(|s| s.excluded_spans > 0).collect();
        assert!(
            !with_exclusions.is_empty(),
            "this capture has a gap, open spans and sub-frame spans, so something must be excluded"
        );
    }

    /// Threads are mutually exclusive, so their measured time cannot exceed
    /// the window. Idle records and ISRs both overlap that set and are
    /// deliberately kept out of the sum.
    #[test]
    fn thread_time_fits_the_window_and_overlapping_kinds_stay_separate() {
        let view = stamped();
        let s = &view.summary;
        assert!(
            s.thread_extent <= s.window_extent,
            "{} ms of threads in a {} ms window",
            s.thread_extent,
            s.window_extent
        );
        assert_eq!(s.unaccounted_extent, s.window_extent - s.thread_extent);
        let isr_sum: u64 =
            s.subjects.iter().filter(|x| x.kind == "isr").map(|x| x.total_extent).sum();
        assert_eq!(s.isr_extent, isr_sum);
    }

    /// The double count this design exists to avoid: idle is reported both as
    /// `RecordKind::Idle` records and as switches of the thread the manifest
    /// names `idle`. The two are kept apart, and the `idle` lane is never
    /// added to the thread total.
    #[test]
    fn idle_is_not_counted_twice() {
        let view = stamped();
        let s = &view.summary;
        let idle_thread = s
            .subjects
            .iter()
            .find(|x| x.kind == "thread" && x.label == "idle")
            .expect("this capture's manifest names an idle thread");
        assert!(idle_thread.total_extent > 0, "the idle thread ran and was measured");
        assert!(
            s.subjects.iter().any(|x| x.kind == "idle"),
            "the idle *record* lane exists as its own subject"
        );
        assert!(
            !s.subjects.iter().filter(|x| x.kind == "thread").any(|x| x.key == "cpu-idle"),
            "the idle record lane leaked into the thread total"
        );
        // The kept total is the one that fits.
        assert!(s.thread_extent <= s.window_extent);
    }

    /// Sorted heaviest-first, so the load repartition reads as one.
    #[test]
    fn subjects_are_sorted_by_measured_time() {
        let view = stamped();
        let totals: Vec<u64> = view.summary.subjects.iter().map(|s| s.total_extent).collect();
        let mut sorted = totals.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(totals, sorted);
    }

    /// With no arrival stamps there is no time base, so every extent counts
    /// **frames** and the summary says which unit it is in rather than letting
    /// a caller assume milliseconds.
    #[test]
    fn an_untimed_capture_reports_its_totals_in_frames() {
        let view = real();
        assert_eq!(view.summary.unit, "frame");
        assert!(!view.summary.has_time_base);
        assert!(view.summary.window_extent > 0);
        for s in &view.summary.subjects {
            assert!(s.share >= 0.0 && s.share <= 1.0);
        }
    }

    /// An unnamed subject stays unnamed in the summary too. A load table is
    /// exactly where a raw pointer is most tempting to dress up.
    #[test]
    fn unnamed_subjects_stay_unnamed_in_the_summary() {
        let view = real();
        let unnamed: Vec<&LoadSubject> = view.summary.subjects.iter().filter(|s| s.unnamed).collect();
        assert!(!unnamed.is_empty(), "this capture has unnamed threads");
        for s in unnamed {
            if s.kind == "thread" {
                assert_eq!(s.label, s.key, "an unnamed thread must render as the pointer it is");
                assert!(s.label.starts_with("0x"));
            } else {
                // The one non-pointer unnamed subject: a vector the firmware
                // could not report at all. Its label says exactly that and
                // names nothing.
                assert_eq!(s.key, "isr-unidentified");
                assert_eq!(s.label, "ISR (vector not reported)");
            }
        }
    }
}
