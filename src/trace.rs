//! The Trace view's backend (`embarch-ui/design.md` §3 decision 10's second
//! half): turning a completed study's recorded outpost timeline into
//! something a browser can draw, **without any of the four lies a timeline
//! makes easy**.
//!
//! Post-hoc, deliberately: outpost capture is study-scoped with no live feed
//! (`embarch-outpost/design.md` §3 decision 10), so this renders a finished
//! study's recorded stream and is the one place in this UI that is not live.
//!
//! # What this reads, and why it isn't the raw bytes
//!
//! Core writes two files per outpost tap (`embarch-outpost/design.md` §4):
//! `<tap>.bin`, the framed stream verbatim, and `<tap>.trace.csv`, the decoded
//! rows with names resolved **through the manifest the flash bound**. This
//! reads the CSV.
//!
//! Decoding the raw bytes here instead was the other option, and it is worse
//! for one reason: the manifest lives on Core's side (it arrived with the
//! flash, and the study snapshotted its own copy beside its results), so a
//! decode done here would have no manifest at all and would produce an
//! *unnamed* trace every single time — strictly less true than the one Core
//! already produced. The build-ID check belongs where the manifest is.
//!
//! What this deliberately does **not** do is re-derive trace knowledge: the
//! column list is checked against
//! [`embarch_study_designer::outpost::csv_header`] and refused if it differs,
//! the kind vocabulary is read out of [`RecordKind`] rather than written out
//! here, and `IRQ_UNKNOWN` comes from that crate too. That is the same "column
//! knowledge lives in one crate" rule Core follows — see
//! `embarch-outpost/milestone-1.md` §4's note on reading a trace through that
//! crate rather than parsing it in JavaScript. Parsing happens here, in Rust,
//! against the shared crate's own vocabulary; nothing about a trace's shape is
//! known to `app.js`.
//!
//! # The four lies
//!
//! 1. **A dropped-record gap is drawn as a gap, never bridged.** Every
//!    `Gap` record becomes a [`Gap`] band placed *by its own timestamp*, which
//!    is not its position in the stream: a gap is stamped when the losses
//!    started and emitted when the ring next had room, and a FIFO ring cannot
//!    make those the same moment (`embarch-outpost/src/outpost_priv.h`'s own
//!    note on `OUTPOST_KIND_GAP`). Confirmed against the committed
//!    `native_sim` capture, where all four gap rows step backwards relative to
//!    their neighbours and are the *only* rows that do.
//!
//!    A gap band is **not** an empty interval, and that surprised this
//!    implementation: 16 surviving records fall inside the first band of that
//!    real capture. A gap says *records were lost across this span*, not
//!    *nothing happened here* — so the band is drawn as an overlay over the
//!    records that survived, never as a hole punched through them. Erasing
//!    real records to make the picture tidier would be its own lie.
//!
//! 2. **A trace whose manifest did not apply is never presented as a named
//!    one.** [`TraceView::named`] comes from Core's own
//!    `streams/index.json` note (`GET /study/{id}/streams`), not from
//!    inspecting whether the rows happen to carry names.
//!
//! 3. **An unnamed thread or vector renders as the number it is.** No
//!    interpolation, no "probably the worker thread". Most of a real build's
//!    threads have no distinguishing symbol and resolve to raw pointers
//!    (`embarch-outpost/design.md` §3 decision 8) — three of the seven in the
//!    `native_sim` capture, and the majority on the real reference-dut image —
//!    so [`Lane::unnamed`] is a first-class state, not an error path.
//!
//! 4. **A span with no closing record is open-ended and says so.** It is
//!    drawn out to the next traced event because a shape needs an extent, and
//!    [`Span::open_end`] is what stops that extent reading as a measurement.

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
    /// `"thread"`, `"idle"`, or `"isr"`.
    pub kind: &'static str,
    pub spans: Vec<Span>,
    /// Point-in-time records on this lane — `thread_create`/`thread_name`.
    pub points: Vec<PointEvent>,
}

/// One interval a subject was running.
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
}

/// A record with no duration — a marker, a thread creation, a name-set.
#[derive(Debug, Clone, Serialize)]
pub struct PointEvent {
    pub cycles: u64,
    pub kind: String,
    pub label: String,
    pub unnamed: bool,
    /// `b` — an engineer's own marker argument, meaningless to this crate and
    /// passed through as the number it is.
    pub arg: u32,
}

/// Records the firmware itself reported dropping, over the cycle span it
/// reported losing them across.
#[derive(Debug, Clone, Serialize)]
pub struct Gap {
    pub from: u64,
    pub to: u64,
    pub records_lost: u32,
    /// Where this record sat in the stream, which is *not* where it sits in
    /// time. Surfaced so the view can say so rather than look inconsistent.
    pub row_index: usize,
}

/// A study's outpost tap, decoded into something drawable.
#[derive(Debug, Clone, Serialize)]
pub struct TraceView {
    pub study_id: String,
    pub tap: String,
    /// Whether Core resolved names for this trace: it rendered, and Core had
    /// nothing to say about why it might not be what it looks like.
    pub named: bool,
    /// Core's own reason, verbatim, when it had one. Rendered as given — this
    /// tab does not paraphrase a refusal.
    pub note: Option<String>,
    pub rows: usize,
    /// Rows past [`MAX_ROWS`], never silently discarded.
    pub rows_dropped_by_cap: usize,
    pub cycles_from: u64,
    pub cycles_to: u64,
    /// False when every row's `us` column was empty, which means the capture
    /// carried **no header frame** and therefore no clock rate. The axis is
    /// then cycles, labelled as having no time base — not microseconds
    /// computed against a rate nobody reported.
    pub has_time_base: bool,
    pub us_per_cycle: Option<f64>,
    pub records_lost: u64,
    /// Non-gap rows that stepped backwards. Expected to be zero: gap records
    /// are the only ones allowed to, and they are excluded. A non-zero count
    /// means this view's span pairing saw a stream it cannot reason about, and
    /// it says so instead of drawing confidently.
    pub out_of_order_rows: usize,
    pub gaps: Vec<Gap>,
    pub lanes: Vec<Lane>,
    pub markers: Vec<PointEvent>,
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
    cycles: u64,
    us: Option<f64>,
    kind: Option<RecordKind>,
    a: u32,
    b: u32,
    name: String,
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

/// Parses a rendered `*.trace.csv` into a drawable view.
///
/// `note` and `named` come from the caller (Core's own stream index), never
/// from these bytes: whether a manifest applied is Core's finding, and
/// re-deriving it from whether the `name` column happens to be populated would
/// be a guess dressed as a check.
pub fn parse(study_id: &str, tap: &str, csv: &str, named: bool, note: Option<String>) -> Result<TraceView, String> {
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
        if f.len() < 6 {
            // A short line is a truncated write, not a row shape to interpret.
            continue;
        }
        let Ok(cycles) = f[0].parse::<u64>() else { continue };
        rows.push(Row {
            cycles,
            us: if f[1].is_empty() { None } else { f[1].parse::<f64>().ok() },
            kind: kind_of(&f[2]),
            a: f[3].parse::<u32>().unwrap_or(0),
            b: f[4].parse::<u32>().unwrap_or(0),
            name: f[5].clone(),
        });
    }

    let has_time_base = rows.iter().any(|r| r.us.is_some());
    // Derived from the data rather than asked for: `us` is already the DUT's
    // own clock applied by the crate that owns the column, and one ratio is
    // all a browser needs to label an axis without recomputing anything.
    let us_per_cycle = rows
        .iter()
        .find(|r| r.cycles > 0 && r.us.is_some_and(|us| us > 0.0))
        .and_then(|r| r.us.map(|us| us / r.cycles as f64));

    // Gaps first: they are the only rows whose timestamp disagrees with their
    // position, and taking them out is what makes the rest a stream this can
    // pair switch-ins against.
    let mut gaps: Vec<Gap> = Vec::new();
    let mut records_lost = 0u64;
    for (i, r) in rows.iter().enumerate() {
        if r.kind == Some(RecordKind::Gap) {
            records_lost += u64::from(r.a);
            gaps.push(Gap {
                from: r.cycles,
                to: r.cycles.saturating_add(u64::from(r.b)),
                records_lost: r.a,
                row_index: i,
            });
        }
    }
    gaps.sort_by_key(|g| g.from);

    let timeline: Vec<&Row> = rows.iter().filter(|r| r.kind != Some(RecordKind::Gap)).collect();
    let out_of_order_rows = timeline
        .windows(2)
        .filter(|w| w[1].cycles < w[0].cycles)
        .count();

    let cycles_from = timeline.first().map(|r| r.cycles).unwrap_or(0);
    let cycles_to = timeline
        .iter()
        .map(|r| r.cycles)
        .max()
        .unwrap_or(cycles_from)
        .max(gaps.iter().map(|g| g.to).max().unwrap_or(0));

    // ---- lanes ------------------------------------------------------------
    //
    // Insertion-ordered, so lanes appear in the order the capture first
    // mentions them: a thread's own creation record is usually its first
    // appearance, which puts the lanes in a stable, meaningful order without
    // sorting by a number a reader has no reason to care about.
    struct Building {
        lane: Lane,
        open: Vec<(u64, bool)>,
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

    let thread_key = |a: u32| format!("0x{a:08x}");

    for r in timeline.iter() {
        let Some(kind) = r.kind else {
            // A kind this build does not know decodes as itself rather than
            // failing the row (`OutpostRecord::kind`'s own doc comment) — it
            // has no lane, so it lands as a point event nobody has to
            // interpret.
            markers.push(PointEvent {
                cycles: r.cycles,
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
                    b.open.push((r.cycles, false));
                }
                // A switch-in is also what ends idle, per the firmware's own
                // note: there is no idle-exit hook to define.
                if let Some(idle) = building.get_mut(IDLE_LANE) {
                    if let Some((from, open_start)) = idle.open.pop() {
                        idle.lane.spans.push(Span {
                            from,
                            to: r.cycles,
                            open_start,
                            open_end: false,
                            crosses_gap: false,
                        });
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
                        Some((from, open_start)) => b.lane.spans.push(Span {
                            from,
                            to: r.cycles,
                            open_start,
                            open_end: false,
                            crosses_gap: false,
                        }),
                        // Its switch-in was among the losses. The run is real
                        // and its start is not known, which is exactly what
                        // `open_start` says.
                        None => b.lane.spans.push(Span {
                            from: r.cycles,
                            to: r.cycles,
                            open_start: true,
                            open_end: false,
                            crosses_gap: false,
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
                        b.open.push((r.cycles, false));
                    } else {
                        match b.open.pop() {
                            Some((from, open_start)) => b.lane.spans.push(Span {
                                from,
                                to: r.cycles,
                                open_start,
                                open_end: false,
                                crosses_gap: false,
                            }),
                            None => b.lane.spans.push(Span {
                                from: r.cycles,
                                to: r.cycles,
                                open_start: true,
                                open_end: false,
                                crosses_gap: false,
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
                    if let Some((from, open_start)) = b.open.pop() {
                        b.lane.spans.push(Span {
                            from,
                            to: r.cycles,
                            open_start,
                            open_end: true,
                            crosses_gap: false,
                        });
                    }
                    b.open.push((r.cycles, false));
                }
            }
            RecordKind::ThreadCreate | RecordKind::ThreadName => {
                let key = thread_key(r.a);
                let named_here = !r.name.is_empty();
                let label = if named_here { r.name.clone() } else { key.clone() };
                ensure(&mut building, &mut order, key.clone(), label.clone(), !named_here, "thread");
                if let Some(b) = building.get_mut(&key) {
                    b.lane.points.push(PointEvent {
                        cycles: r.cycles,
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
                    cycles: r.cycles,
                    kind: kind.as_str().to_string(),
                    // A marker with no name in the manifest is its ID. The ID
                    // is a real answer; a made-up name would not be.
                    label: if named_here { r.name.clone() } else { format!("marker {}", r.a) },
                    unnamed: !named_here,
                    arg: r.b,
                });
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
            for (from, open_start) in b.open.drain(..) {
                b.lane.spans.push(Span {
                    from,
                    to: cycles_to,
                    open_start,
                    open_end: true,
                    crosses_gap: false,
                });
            }
            b.lane.spans.sort_by_key(|s| s.from);
            for span in &mut b.lane.spans {
                span.crosses_gap = gaps.iter().any(|g| span.from < g.to && g.from < span.to);
            }
            lanes.push(b.lane);
        }
    }

    Ok(TraceView {
        study_id: study_id.to_string(),
        tap: tap.to_string(),
        named,
        note,
        rows: rows.len(),
        rows_dropped_by_cap,
        cycles_from,
        cycles_to,
        has_time_base,
        us_per_cycle,
        records_lost,
        out_of_order_rows,
        gaps,
        lanes,
        markers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real `native_sim` capture, run through **Core's own renderer**
    /// against the manifest its own build produced — generated by
    /// `embarch-core`'s `outpost_manifest::render` from
    /// `embarch-core/tests/fixtures/outpost-native-sim.bin`, whose bytes came
    /// out of the firmware encoder rather than out of anything in Rust.
    ///
    /// Committed as the rendered CSV rather than as the raw stream on purpose:
    /// this module's job *starts* at Core's output, so a fixture that went
    /// through the firmware encoder and then Core's decoder exercises the
    /// actual seam. See this module's own header for why the raw bytes are
    /// Core's to decode and not this crate's.
    ///
    /// A useful side finding while producing it: Core's Rust renderer and
    /// `embarch-outpost/scripts/decode_outpost.py` agree on every column of
    /// all 848 rows, differing only in how many decimals they print for `us`.
    /// Three independent implementations of this wire, two of them checked
    /// against each other here.
    const REAL_TRACE: &str = include_str!("../tests/fixtures/outpost-native-sim.trace.csv");

    fn real() -> TraceView {
        parse("study-1", "outpost", REAL_TRACE, true, None).expect("the real trace parses")
    }

    #[test]
    fn a_real_capture_decodes_into_lanes() {
        let view = real();
        assert_eq!(view.rows, 848, "the committed capture is 848 records");
        assert_eq!(view.rows_dropped_by_cap, 0);
        assert!(view.has_time_base, "this capture has a header frame, so it has a clock rate");
        assert_eq!(view.out_of_order_rows, 0, "only gap rows may step backwards, and they are excluded");

        let threads: Vec<&Lane> = view.lanes.iter().filter(|l| l.kind == "thread").collect();
        assert_eq!(threads.len(), 7, "this capture mentions seven distinct thread pointers");
        assert!(view.lanes.iter().any(|l| l.kind == "idle"));
        assert!(view.lanes.iter().any(|l| l.kind == "isr"));
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
        assert_eq!(isr.spans.len(), 72, "72 enter/exit pairs");
    }

    /// **Gaps are placed by timestamp, not by position**, and this capture is
    /// what proves the distinction is real: all four of its gap rows sit
    /// between neighbours with *lower* timestamps, because a gap is stamped
    /// when the losses began and emitted when the ring next had room.
    #[test]
    fn gaps_are_placed_by_their_own_timestamp() {
        let view = real();
        assert_eq!(view.gaps.len(), 4);
        assert_eq!(view.records_lost, 20_009 + 15 + 16 + 17);
        for gap in &view.gaps {
            assert!(gap.to > gap.from, "a gap must span the cycles it reports losing");
            assert!(gap.records_lost > 0);
        }
        // The first gap is stamped at 410000 and the row after it is at
        // 370000 — the whole reason position is not time.
        assert_eq!(view.gaps[0].from, 410_000);
        assert_eq!(view.gaps[0].to, 420_000);
        assert!(view.gaps.windows(2).all(|w| w[0].from <= w[1].from), "gaps come out in time order");
    }

    /// The finding that changed this module's design: a gap band is **not** an
    /// empty interval. Sixteen surviving records fall inside the first band of
    /// this real capture, so drawing the band as a hole would erase real data.
    /// The band is an overlay, and every span it touches is flagged instead.
    #[test]
    fn a_gap_marks_spans_incomplete_rather_than_erasing_them() {
        let view = real();
        let crossing: usize = view
            .lanes
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.crosses_gap)
            .count();
        assert!(
            crossing > 0,
            "this capture has spans overlapping its gaps; none were flagged, so the view would \
             present an interrupted run as a continuous one"
        );
    }

    #[test]
    fn markers_keep_the_engineers_own_argument_and_name() {
        let view = real();
        assert_eq!(view.markers.len(), 132);
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
                    if f.len() >= 6 {
                        f[5] = String::new();
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
        assert_eq!(view.rows, 848);
        assert_eq!(view.gaps.len(), 4);
    }

    /// Columns this build does not recognize are refused rather than guessed
    /// at — the same posture as a manifest from another record layout.
    #[test]
    fn an_unfamiliar_column_list_is_refused() {
        let err = parse("s", "t", "cycles,us,kind,a,b\n0,0,idle,0,0\n", true, None)
            .expect_err("must refuse");
        assert!(err.contains("refusing to guess"), "{err}");
    }

    /// A capture with no header frame has no clock rate, so its rows have no
    /// microseconds. Saying so is the answer; computing microseconds against a
    /// rate nobody reported is not.
    #[test]
    fn a_capture_with_no_time_base_says_so() {
        let csv = format!("{}\n0,,idle,0,0,\n1000,,thread_switch_in,16,0,\n", outpost::csv_header());
        let view = parse("s", "t", &csv, true, None).unwrap();
        assert!(!view.has_time_base);
        assert_eq!(view.us_per_cycle, None);
    }

    #[test]
    fn the_kind_vocabulary_comes_from_the_shared_crate() {
        assert_eq!(kind_of("thread_switch_in"), Some(RecordKind::ThreadSwitchIn));
        assert_eq!(kind_of("gap"), Some(RecordKind::Gap));
        assert_eq!(kind_of("unknown_42"), None);
    }
}
