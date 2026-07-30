//! The wire format shared by `dlog export` and `dlog import` (design §7, #64).
//!
//! JSONL: one JSON object per line, discriminated by `type`. A header line comes
//! first; then tasks, decisions and invariants, each ascending by id — which
//! approximates foreign-key order, since a referenced row always existed first
//! and ULIDs sort chronologically. Only approximates: ULIDs minted in the same
//! millisecond sort randomly, so `import` defers foreign keys to commit rather
//! than depending on the order it reads.
//!
//! Only **sealed** decisions cross this boundary (§8.2). Staging is one agent's
//! live work area, and a decision arriving from another machine was never this
//! store's to seal, so `export` never emits a staged row and `import` refuses a
//! file that contains one.
//!
//! The decision record is [`StoredDecision`]'s own serialization plus the `type`
//! tag, so the file shape cannot drift from what `dlog show` returns.

use serde::{Deserialize, Serialize};

use crate::commands::AppError;
use crate::model::StoredDecision;
use crate::store::{InvariantRecord, SinceBound, TaskRecord};

/// Version of the *file format*, independent of the store's schema version.
/// An importer that meets a format it doesn't know refuses the file rather than
/// guessing at it — the same posture `schema_too_new` takes for stores (#60).
pub(crate) const FORMAT_VERSION: u32 = 1;

/// The first line of an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Header {
    pub format: u32,
    /// The exporting store's schema version. Diagnostic only — the format
    /// version is what gates compatibility.
    pub schema_version: i64,
    pub exported_at: i64,
}

/// One line of an export file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Record {
    Header(Header),
    Task(TaskRecord),
    Decision(Box<StoredDecision>),
    Invariant(InvariantRecord),
}

/// Serialize one record as a JSONL line (no trailing newline).
pub(crate) fn to_line(record: &Record) -> Result<String, AppError> {
    serde_json::to_string(record)
        .map_err(|e| AppError::new("serialize", format!("could not serialize record: {e}")))
}

/// Parse a `--since` bound: a ULID (that decision onwards) or a `YYYY-MM-DD`
/// date in UTC (everything recorded on or after midnight).
pub(crate) fn parse_since(raw: &str) -> Result<SinceBound, AppError> {
    let raw = raw.trim();
    if ulid::Ulid::from_string(raw).is_ok() {
        return Ok(SinceBound::Id(raw.to_string()));
    }
    if let Some(ms) = parse_utc_date_ms(raw) {
        return Ok(SinceBound::Ms(ms));
    }
    Err(AppError::new(
        "invalid_since",
        format!("--since must be a decision id (ULID) or a YYYY-MM-DD date, got {raw:?}"),
    ))
}

/// `YYYY-MM-DD` → epoch milliseconds at UTC midnight. `None` when the string
/// isn't that shape or isn't a real date.
fn parse_utc_date_ms(raw: &str) -> Option<i64> {
    let parts: Vec<&str> = raw.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400_000)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's `days_from_civil`).
/// Written out rather than pulling in a date crate for one conversion.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = month as i64;
    let d = day as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Anchor, Binding};

    fn sealed(id: &str) -> StoredDecision {
        StoredDecision {
            id: id.into(),
            task_id: None,
            agent: Agent {
                role: "implementer".into(),
                model: "claude-test".into(),
                session_id: None,
                author: None,
            },
            conversation_id: None,
            rationale: "retry three times".into(),
            rejected: vec![],
            caused_by: vec![],
            supersedes: None,
            anchors: vec![],
            staged: false,
            binding: Some(Binding::None),
            created_at_ms: 1,
        }
    }

    #[test]
    fn a_decision_round_trips_through_the_wire_format() {
        // Every field that is skipped when empty must also be `default`, or a
        // minimal decision fails to parse back.
        let mut d = sealed("01AAA");
        d.anchors = vec![Anchor {
            file: "src/net/client.rs".into(),
            symbol_path: Some("Client::send".into()),
            node_kind: Some("function_item".into()),
            structural_hash: Some("h_1".into()),
            line_span: Some((40, 58)),
            recorded_at_sha: Some("a3f".into()),
        }];

        for decision in [sealed("01AAA"), d] {
            let line = to_line(&Record::Decision(Box::new(decision.clone()))).unwrap();
            let back: Record = serde_json::from_str(&line).unwrap();
            let Record::Decision(parsed) = back else {
                panic!("expected a decision record");
            };
            assert_eq!(parsed.id, decision.id);
            assert_eq!(parsed.rationale, decision.rationale);
            assert_eq!(parsed.anchors, decision.anchors);
            assert_eq!(parsed.binding, decision.binding);
            assert_eq!(parsed.created_at_ms, decision.created_at_ms);
        }
    }

    #[test]
    fn every_record_carries_its_type_tag() {
        let header = to_line(&Record::Header(Header {
            format: FORMAT_VERSION,
            schema_version: 3,
            exported_at: 7,
        }))
        .unwrap();
        assert!(header.contains(r#""type":"header""#));

        let task = to_line(&Record::Task(TaskRecord {
            id: "01T".into(),
            parent_task_id: None,
            instruction: None,
            created_at_ms: 1,
            completed_at_ms: None,
        }))
        .unwrap();
        assert!(task.contains(r#""type":"task""#));

        let invariant = to_line(&Record::Invariant(InvariantRecord {
            id: "01I".into(),
            declared_by: "01AAA".into(),
            statement: "tokens never persist".into(),
            scope: None,
            retired: false,
            created_at_ms: 1,
        }))
        .unwrap();
        assert!(invariant.contains(r#""type":"invariant""#));

        assert!(
            to_line(&Record::Decision(Box::new(sealed("01AAA"))))
                .unwrap()
                .contains(r#""type":"decision""#)
        );
    }

    #[test]
    fn since_accepts_a_ulid_or_a_date() {
        let id = ulid::Ulid::new().to_string();
        assert_eq!(parse_since(&id).unwrap(), SinceBound::Id(id.clone()));

        // 2026-07-01T00:00:00Z.
        assert_eq!(
            parse_since("2026-07-01").unwrap(),
            SinceBound::Ms(1_782_864_000_000)
        );
        assert_eq!(parse_since("1970-01-01").unwrap(), SinceBound::Ms(0));
        // Leap day is a real date; the 30th of February is not.
        assert!(matches!(
            parse_since("2024-02-29").unwrap(),
            SinceBound::Ms(_)
        ));

        for bad in ["july", "2026-13-01", "2026-02-30", "2026-7-1", "", "01ZZ"] {
            assert!(parse_since(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
