//! Reading a past study back: the listing, one study's record, and the three
//! ways the Live Study tab renders a tap's capture.
//!
//! # The browser parses no CSV
//!
//! The same rule the Trace view already holds (`embarch-ui` decision 18, and
//! suite decision 4): **decode lives in Rust, drawing lives in `app.js`.** A
//! tap's rendered file is a CSV whose column order is
//! `embarch-study-designer`'s business, and a browser that learned that order
//! would be a second place the suite's column knowledge lives. So these
//! routes hand back columns and rows by name, and bins with numbers in them —
//! never a file for the browser to split.
//!
//! # Why the CSV split is a plain `split(',')`
//!
//! Because the crate that writes these files refuses a value containing a
//! comma rather than quoting it (`csv_escape_ok`, and `DecodeError`'s own
//! "no commas in any of these"). A quote-aware parser here would be code
//! handling a case the writer makes impossible, and would quietly start
//! accepting files this suite does not produce.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

/// Rows served in one page of `/rows`. A table this tab renders is read by a
/// person; past a few hundred rows they are scrolling, not reading, and the
/// download link beside the card is the right tool.
const MAX_ROWS_PER_PAGE: usize = 500;
/// Bins in one `/series` answer. A plot is at most a couple of thousand CSS
/// pixels wide, and a bin narrower than a pixel is not a bin.
const MAX_SERIES_BINS: usize = 4_000;
/// Console lines in one page of `/text`.
const MAX_TEXT_LINES: usize = 2_000;

/// `GET /api/studies` — every study still on Core's disk.
///
/// A proxy rather than a browser-to-Core call, the same two reasons
/// `/api/enroll` is one: this handler holds the bearer token and the browser
/// does not.
pub async fn api_studies(State(state): State<AppState>) -> Response {
    match state.core.list_studies().await {
        Ok(listing) => (
            StatusCode::OK,
            Json(json!({
                "keep": listing.keep,
                "studies": listing
                    .studies
                    .iter()
                    .map(|s| json!({
                        "study_id": s.study_id,
                        "study_name": s.study_name,
                        // Core's own word, unchanged. `interrupted` in
                        // particular is neither `completed` nor `failed`
                        // (`embarch-core` decision 69) and this tab renders
                        // it as its own thing.
                        "status": s.status,
                        "started_utc_ms": s.started_utc_ms,
                        "ended_utc_ms": s.ended_utc_ms,
                        // Absent, not zeroed, when Core could not read the
                        // record — the two are opposite facts.
                        "steps": s.steps.as_ref().map(|t| json!({
                            "total": t.total,
                            "passed": t.passed,
                            "failed": t.failed,
                            "timed_out": t.timed_out,
                            "unknown": t.unknown,
                        })),
                        "taps": s.taps.as_ref().map(|taps| taps
                            .iter()
                            .map(|t| json!({
                                "name": t.name,
                                "encoding": t.encoding,
                                "rendered": t.rendered,
                                "named": t.named,
                                "timed": t.timed,
                                "self_excluded": t.self_excluded,
                                "source_deferred": t.source_deferred,
                            }))
                            .collect::<Vec<_>>()),
                        "note": s.note,
                    }))
                    .collect::<Vec<_>>(),
            })),
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// `GET /api/studies/{id}` — one call for opening a past study.
///
/// Three of Core's routes in one answer, because opening a study needs all
/// three and three round trips from a browser would render the card in three
/// stages: its steps, its taps, and — only where Core still has the job or
/// the finished record — its status and provenance.
///
/// **A part that could not be read is reported as a part that could not be
/// read.** Each of the three carries its own `note` rather than the whole
/// call failing: a study whose `events.json` this Core cannot parse still has
/// a readable stream index, and a tab that showed nothing would be hiding it.
pub async fn api_study(State(state): State<AppState>, Path(study_id): Path<String>) -> Response {
    let steps = state.core.study_steps(&study_id).await;
    let streams = state.core.study_streams(&study_id).await;
    let status = state.core.get_study_status(&study_id).await;

    let (steps_json, steps_note) = match steps {
        Ok(Some(s)) => (
            Some(json!({
                "study_name": s.study_name,
                "timed": s.timed,
                "steps": s.steps.iter().map(|e| json!({
                    "index": e.index,
                    "step_name": e.step_name,
                    "outcome": e.outcome,
                    "reason": e.reason,
                    "delay_before_ms": e.delay_before_ms,
                    "started_utc_ms": e.started_utc_ms,
                    "ended_utc_ms": e.ended_utc_ms,
                })).collect::<Vec<_>>(),
            })),
            None,
        ),
        Ok(None) => (
            None,
            Some("this study has no events.json — it never finished, or Core has since swept it".to_string()),
        ),
        Err(e) => (None, Some(format!("{e:#}"))),
    };

    let (taps_json, taps_note) = match streams {
        Ok(Some(index)) => (
            Some(
                index
                    .streams
                    .iter()
                    .map(|e| {
                        json!({
                            "id": e.id,
                            "name": e.name,
                            "encoding": e.encoding,
                            "rendered": e.rendered,
                            "note": e.note,
                            "named": e.is_named(),
                            "timed": e.is_timed(),
                            "self_excluded": e.self_excluded,
                            "is_outpost_trace": matches!(
                                e.encoding,
                                embarch_study_designer::StreamEncoding::OutpostTrace
                            ),
                            "is_text": matches!(
                                e.encoding,
                                embarch_study_designer::StreamEncoding::Text
                            ),
                        })
                    })
                    .collect::<Vec<_>>(),
            ),
            None,
        ),
        Ok(None) => (
            None,
            Some("this study recorded no streams (it may predate streams/, or never have started)".to_string()),
        ),
        Err(e) => (None, Some(format!("{e:#}"))),
    };

    // Core `404`s a study id its job registry has forgotten, which after a
    // restart is every study that ever ran — so this arm is the ordinary
    // case for a post-hoc read, not an error. The listing is where a past
    // study's status comes from instead.
    let (status_json, provenance) = match status {
        Ok(resp) => {
            let provenance = resp
                .result
                .as_ref()
                .map(|r| {
                    serde_json::to_value(crate::study_designer::provenance_view(&r.provenance))
                        .unwrap_or(Value::Null)
                });
            let streams = resp.result.as_ref().and_then(|r| serde_json::to_value(&r.streams).ok());
            (
                Some(json!({
                    "status": resp.status,
                    "current_step": resp.current_step,
                    "total_steps": resp.total_steps,
                    "reason": resp.reason,
                    "streams": streams,
                })),
                provenance,
            )
        }
        Err(_) => (None, None),
    };

    (
        StatusCode::OK,
        Json(json!({
            "study_id": study_id,
            "steps": steps_json,
            "steps_note": steps_note,
            "taps": taps_json,
            "taps_note": taps_note,
            "job": status_json,
            "provenance": provenance,
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct RowsQuery {
    from: Option<usize>,
    limit: Option<usize>,
}

/// `GET /api/studies/{id}/stream/{name}/rows` — one page of a tap's rendered
/// table, parsed here so the browser learns no column order.
pub async fn api_stream_rows(
    State(state): State<AppState>,
    Path((study_id, name)): Path<(String, String)>,
    Query(q): Query<RowsQuery>,
) -> Response {
    let table = match table_for(&state, &study_id, &name).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let from = q.from.unwrap_or(0).min(table.rows.len());
    let limit = q.limit.unwrap_or(100).clamp(1, MAX_ROWS_PER_PAGE);
    let to = (from + limit).min(table.rows.len());

    (
        StatusCode::OK,
        Json(json!({
            "columns": table.columns,
            "rows": &table.rows[from..to],
            "from": from,
            "total": table.rows.len(),
            // Which columns hold numbers this build could parse, so the plot
            // card offers exactly those and the table offers all of them.
            "numeric_columns": table.numeric_columns(),
            "time_column": table.time_column(),
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct SeriesQuery {
    column: Option<String>,
    from: Option<f64>,
    to: Option<f64>,
    width: Option<usize>,
}

/// `GET /api/studies/{id}/stream/{name}/series` — one numeric column binned
/// for a plot, min/max/count per bin.
///
/// The same discipline `api_trace_bins` already uses: at most `width` bins
/// come back whatever the file holds, so a 600,000-sample capture costs the
/// browser a few thousand numbers rather than a few million. **Min and max
/// per bin, not an average** — an average hides the spike that is usually
/// the reason someone is looking.
pub async fn api_stream_series(
    State(state): State<AppState>,
    Path((study_id, name)): Path<(String, String)>,
    Query(q): Query<SeriesQuery>,
) -> Response {
    let table = match table_for(&state, &study_id, &name).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    let numeric = table.numeric_columns();
    let column = match q.column {
        Some(c) => c,
        None => match numeric.first() {
            Some(c) => c.clone(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "tap '{name}' has no numeric column to plot — its columns are: {}",
                        table.columns.join(", ")
                    ),
                )
                    .into_response()
            }
        },
    };
    let Some(col_index) = table.columns.iter().position(|c| *c == column) else {
        return (
            StatusCode::NOT_FOUND,
            format!(
                "tap '{name}' has no column named '{column}' — it has: {}",
                table.columns.join(", ")
            ),
        )
            .into_response();
    };

    // The x axis is the tap's own arrival stamp where it has one, and the
    // row's position where it does not. Said in the answer rather than
    // assumed by the browser: a plot against a row index is not a plot
    // against time, and the two must not look alike.
    let time_column = table.time_column();
    let time_index = time_column
        .as_ref()
        .and_then(|c| table.columns.iter().position(|col| col == c));

    let mut points: Vec<(f64, f64)> = Vec::new();
    let mut unparsed = 0usize;
    for (i, row) in table.rows.iter().enumerate() {
        let Some(raw) = row.get(col_index) else { continue };
        let Ok(value) = raw.trim().parse::<f64>() else {
            unparsed += 1;
            continue;
        };
        let x = match time_index.and_then(|ti| row.get(ti)) {
            Some(t) => match t.trim().parse::<f64>() {
                Ok(t) => t,
                Err(_) => i as f64,
            },
            None => i as f64,
        };
        points.push((x, value));
    }

    if points.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({
                "column": column,
                "time_column": time_column,
                "bins": Vec::<Value>::new(),
                "points": 0,
                "unparsed": unparsed,
                "numeric_columns": numeric,
            })),
        )
            .into_response();
    }

    let data_from = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let data_to = points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let from = q.from.unwrap_or(data_from);
    let to = q.to.unwrap_or(data_to);
    let width = q.width.unwrap_or(600).clamp(1, MAX_SERIES_BINS);

    let span = (to - from).max(f64::MIN_POSITIVE);
    let mut bins: Vec<Option<(f64, f64, u64)>> = vec![None; width];
    for (x, y) in &points {
        if *x < from || *x > to {
            continue;
        }
        let mut slot = (((x - from) / span) * width as f64) as usize;
        if slot >= width {
            slot = width - 1;
        }
        match &mut bins[slot] {
            Some((min, max, count)) => {
                if y < min {
                    *min = *y;
                }
                if y > max {
                    *max = *y;
                }
                *count += 1;
            }
            none => *none = Some((*y, *y, 1)),
        }
    }

    let step = span / width as f64;
    let out: Vec<Value> = bins
        .iter()
        .enumerate()
        .filter_map(|(i, bin)| {
            bin.map(|(min, max, count)| {
                json!({ "x": from + step * (i as f64 + 0.5), "min": min, "max": max, "count": count })
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "column": column,
            "time_column": time_column,
            "from": from,
            "to": to,
            "data_from": data_from,
            "data_to": data_to,
            "bins": out,
            "points": points.len(),
            // Rows whose value this build could not read as a number.
            // Reported, because a plot that quietly skipped them would be a
            // plot of a different dataset.
            "unparsed": unparsed,
            "numeric_columns": numeric,
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct TextQuery {
    from: Option<usize>,
    limit: Option<usize>,
}

/// `GET /api/studies/{id}/stream/{name}/text` — a `Text` tap's console, read
/// back off disk once the run is over.
///
/// Lines are split here for the same reason `live_study` assembles them
/// there: the browser is handed lines, never a byte stream it would have to
/// frame itself. Unlike the live path there is no partial remainder — the
/// file is complete — except for a last line with no trailing newline, which
/// is reported as partial rather than padded.
pub async fn api_stream_text(
    State(state): State<AppState>,
    Path((study_id, name)): Path<(String, String)>,
    Query(q): Query<TextQuery>,
) -> Response {
    let bytes = match state.core.get_study_stream(&study_id, &name, false).await {
        Ok(bytes) => bytes,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    // Lossy, deliberately: a `Text` tap that captured a byte that is not
    // valid UTF-8 still has a console worth reading, and refusing the whole
    // file over one byte would be losing the run to protect a character.
    let text = String::from_utf8_lossy(&bytes);
    let ends_complete = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<&str> = text.split('\n').collect();
    // `split` leaves a trailing empty element for a file that ends in a
    // newline — that is not a line.
    if ends_complete {
        lines.pop();
    }
    let partial = if ends_complete { None } else { lines.pop() };

    let total = lines.len();
    let from = q.from.unwrap_or(0).min(total);
    let limit = q.limit.unwrap_or(500).clamp(1, MAX_TEXT_LINES);
    let to = (from + limit).min(total);
    let page: Vec<String> = lines[from..to]
        .iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "lines": page,
            "from": from,
            "total": total,
            // The file's last line, if it never got a newline. Shown as
            // partial rather than as a line the capture did not contain.
            "partial": partial,
            "bytes": bytes.len(),
        })),
    )
        .into_response()
}


#[derive(Debug, Deserialize)]
pub struct HeadQuery {
    bytes: Option<usize>,
}

/// `GET /api/studies/{id}/stream/{name}/head` — the first bytes of a tap's
/// capture, as hex.
///
/// What a `Raw` tap gets instead of a table: nothing was declared to render
/// it as (`embarch-study-designer` decision 39), so there is no decoding to
/// offer and a hex head plus the whole file to download is the honest view.
/// **Not a sniff** — this renders bytes as bytes, and makes no guess about
/// what they mean.
pub async fn api_stream_head(
    State(state): State<AppState>,
    Path((study_id, name)): Path<(String, String)>,
    Query(q): Query<HeadQuery>,
) -> Response {
    let bytes = match state.core.get_study_stream(&study_id, &name, true).await {
        Ok(bytes) => bytes,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    let want = q.bytes.unwrap_or(512).clamp(16, 8_192);
    let head = &bytes[..want.min(bytes.len())];
    let lines: Vec<String> = head
        .chunks(16)
        .enumerate()
        .map(|(i, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            // The same bytes twice, hex and printable, for the same reason
            // `gatt.csv` carries `payload_hex` beside `payload_ascii`: one is
            // exact and the other is readable, and neither replaces the other.
            let ascii: String = chunk
                .iter()
                .map(|b| if (0x20..0x7f).contains(b) { *b as char } else { '.' })
                .collect();
            format!("{:08x}  {:<47}  {ascii}", i * 16, hex.join(" "))
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "lines": lines,
            "shown_bytes": head.len(),
            "total_bytes": bytes.len(),
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct DownloadQuery {
    raw: Option<String>,
}

/// `GET /api/studies/{id}/stream/{name}/download` — one tap's capture,
/// proxied whole.
///
/// A proxy because the browser has no bearer token and never will (`/api/
/// enroll`'s own reasoning). `?raw=1` picks the byte-for-byte capture where
/// a tap has both files, exactly as embarch-core's own flag does; this
/// forwards the choice rather than making one.
pub async fn api_stream_download(
    State(state): State<AppState>,
    Path((study_id, name)): Path<(String, String)>,
    Query(q): Query<DownloadQuery>,
) -> Response {
    let raw = matches!(q.raw.as_deref(), Some("1") | Some("true"));
    match state.core.get_study_stream(&study_id, &name, raw).await {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    // The tap's own name, sanitised: it reaches a filename,
                    // and embarch-core's own tap-name rules are not a browser
                    // header's.
                    format!(
                        "attachment; filename=\"{}-{}.dat\"",
                        safe_filename(&study_id),
                        safe_filename(&name)
                    ),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

fn safe_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

/// One tap's rendered CSV, parsed into columns and rows.
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Splits a rendered CSV. See this module's own header for why a plain
    /// `split(',')` is the right parser for these files and a quote-aware
    /// one would not be.
    ///
    /// A row with a different column count than the header is **kept**, not
    /// dropped: the raw capture is on Core's disk either way, and a row this
    /// build cannot line up is evidence rather than noise. Short rows are
    /// padded and long ones keep their extra cells, so nothing shifts
    /// columns underneath a reader.
    pub fn parse(text: &str) -> Table {
        let mut lines = text.split('\n').filter(|l| !l.trim().is_empty());
        let columns: Vec<String> = match lines.next() {
            Some(header) => header
                .strip_suffix('\r')
                .unwrap_or(header)
                .split(',')
                .map(|c| c.trim().to_string())
                .collect(),
            None => Vec::new(),
        };
        let rows: Vec<Vec<String>> = lines
            .map(|line| {
                let mut cells: Vec<String> = line
                    .strip_suffix('\r')
                    .unwrap_or(line)
                    .split(',')
                    .map(|c| c.to_string())
                    .collect();
                while cells.len() < columns.len() {
                    cells.push(String::new());
                }
                cells
            })
            .collect();
        Table { columns, rows }
    }

    /// Columns whose values parse as numbers.
    ///
    /// Decided from the data, not from a column-name table: these files come
    /// from three different renderers plus whatever `StructLayout` an
    /// engineer declared, and a hard-coded list would be this tab holding a
    /// copy of column knowledge that lives in `embarch-study-designer`.
    ///
    /// A column is numeric when **every** non-empty value in the sample
    /// parses — one that is numeric most of the time is a column with a
    /// note in it, and plotting it would silently drop the note.
    pub fn numeric_columns(&self) -> Vec<String> {
        // Sampled rather than exhaustive: a 600,000-row capture is uniform
        // by construction (one renderer wrote every row), and scanning it
        // all to answer a question about column *kind* costs more than it
        // settles.
        let sample = self.rows.len().min(200);
        self.columns
            .iter()
            .enumerate()
            .filter(|(i, name)| {
                // A stamp is a number and is never what someone means by
                // "plot this column" — it is the axis.
                if is_time_column(name) {
                    return false;
                }
                let mut seen = false;
                for row in self.rows.iter().take(sample) {
                    let Some(cell) = row.get(*i) else { continue };
                    let cell = cell.trim();
                    if cell.is_empty() {
                        continue;
                    }
                    if cell.parse::<f64>().is_err() {
                        return false;
                    }
                    seen = true;
                }
                seen
            })
            .map(|(_, name)| name.clone())
            .collect()
    }

    /// The column to plot against, or `None` for a tap with no stamp of its
    /// own — in which case a caller plots against the row index and says so.
    pub fn time_column(&self) -> Option<String> {
        self.columns.iter().find(|c| is_time_column(c)).cloned()
    }
}

/// Every arrival stamp this suite's renderers write. Three spellings because
/// three renderers wrote them: `Sample` and `GattTranscriptEntry` carry
/// `rx_utc_ms`, a `Struct` row gets Core's own `core_rx_utc_ms` appended, and
/// an outpost trace's own clock is `cycle`/`us` — which the Trace view draws,
/// not this one.
fn is_time_column(name: &str) -> bool {
    matches!(name, "rx_utc_ms" | "core_rx_utc_ms")
}

/// Fetches and parses one tap's rendered CSV, through a one-entry cache.
///
/// The same shape, and the same reasoning, as `trace_cache`: paging a table
/// and binning a plot are both many requests against one file, and re-fetching
/// a multi-megabyte CSV from Core per page would move the cost rather than
/// remove it.
async fn table_for(
    state: &AppState,
    study_id: &str,
    name: &str,
) -> Result<Arc<Table>, Response> {
    {
        let guard = state.table_cache.lock().await;
        if let Some(cached) = guard.as_ref() {
            if cached.study_id == study_id && cached.tap == name {
                return Ok(cached.table.clone());
            }
        }
    }

    let bytes = match state.core.get_study_stream(study_id, name, false).await {
        Ok(bytes) => bytes,
        Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()),
    };
    let text = String::from_utf8_lossy(&bytes);
    let table = Arc::new(Table::parse(&text));
    *state.table_cache.lock().await = Some(CachedTable {
        study_id: study_id.to_string(),
        tap: name.to_string(),
        table: table.clone(),
    });
    Ok(table)
}

pub struct CachedTable {
    pub study_id: String,
    pub tap: String,
    pub table: Arc<Table>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES_CSV: &str = "rx_utc_ms,step_name,value,unit,channel_id\n\
                               1000,connect,1.5,Milliamps,0\n\
                               1001,connect,2.5,Milliamps,0\n\
                               1002,connect,0.5,Milliamps,0\n";

    #[test]
    fn a_rendered_samples_csv_parses_into_columns_and_rows() {
        let table = Table::parse(SAMPLES_CSV);
        assert_eq!(table.columns, ["rx_utc_ms", "step_name", "value", "unit", "channel_id"]);
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.rows[1][2], "2.5");
    }

    #[test]
    fn the_stamp_is_the_axis_and_never_offered_as_a_series() {
        // Plotting `rx_utc_ms` against `rx_utc_ms` is a straight line, and
        // offering it in the picker is how someone ends up drawing one.
        let table = Table::parse(SAMPLES_CSV);
        assert_eq!(table.time_column().as_deref(), Some("rx_utc_ms"));
        assert_eq!(table.numeric_columns(), ["value", "channel_id"]);
    }

    #[test]
    fn a_column_that_is_numeric_most_of_the_time_is_not_a_numeric_column() {
        // `payload_hex`/`decode_note` are exactly this: a Struct row whose
        // payload did not fit its layout carries the reason in a column that
        // holds numbers on every other row (`embarch-study-designer`
        // decision 52). Plotting it would drop the rows that say what went
        // wrong.
        let table = Table::parse(
            "core_rx_utc_ms,rep_index,temperature\n\
             1,0,21.5\n\
             2,0,short header: need 4 have 2\n",
        );
        assert_eq!(table.numeric_columns(), ["rep_index"]);
        assert_eq!(table.time_column().as_deref(), Some("core_rx_utc_ms"));
    }

    #[test]
    fn a_short_row_is_padded_rather_than_dropped_or_shifted() {
        // The raw capture is on disk either way; a row this build cannot
        // line up is evidence, and dropping it would make the table quietly
        // shorter than the file.
        let table = Table::parse("a,b,c\n1,2,3\n4,5\n");
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[1], ["4", "5", ""]);
    }

    #[test]
    fn a_table_with_no_rows_is_a_table_and_not_an_error() {
        // A declared tap that captured nothing is a real, expected state
        // (`embarch-core`'s own `bytes_written: 0`), and it is not the same
        // as a tap that does not exist.
        let table = Table::parse("rx_utc_ms,step_name,value,unit,channel_id\n");
        assert_eq!(table.columns.len(), 5);
        assert!(table.rows.is_empty());
        assert!(table.numeric_columns().is_empty(), "no data means no column is numeric yet");
    }
}
