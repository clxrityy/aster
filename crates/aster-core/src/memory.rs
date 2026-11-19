//! Owner-provided memories are private data, not instructions or authority.
//! No capture, recall ranking, automatic retention, or identity edits run here.

use std::{fmt, marker::PhantomData, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_MEMORY_CONTENT_BYTES: usize = 16 * 1024;
pub const MAX_MEMORY_PROVENANCE_BYTES: usize = 1024;
pub const DEFAULT_MEMORY_PAGE_SIZE: u16 = 50;
pub const MAX_MEMORY_PAGE_SIZE: u16 = 100;

// Julian day zero relative to the Unix epoch: the shared persistence cursor
// floor. Chrono validation supplies a narrower upper bound than PostgreSQL.
const MIN_MEMORY_CURSOR_MICROS: i64 = -2_440_588 * 86_400_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InvalidMemory {
    #[error("memory content must be nonblank, NUL-free, and at most 16384 UTF-8 bytes")]
    Content,
    #[error("memory provenance must be nonblank, NUL-free, and at most 1024 UTF-8 bytes")]
    Provenance,
    #[error("memory ID must be a nonnil UUID")]
    Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Uuid", into = "Uuid")]
pub struct MemoryId(Uuid);

impl MemoryId {
    pub fn new(id: Uuid) -> Result<Self, InvalidMemory> {
        if id.is_nil() {
            Err(InvalidMemory::Id)
        } else {
            Ok(Self(id))
        }
    }

    pub fn get(self) -> Uuid {
        self.0
    }
}

impl TryFrom<Uuid> for MemoryId {
    type Error = InvalidMemory;

    fn try_from(id: Uuid) -> Result<Self, Self::Error> {
        Self::new(id)
    }
}

impl From<MemoryId> for Uuid {
    fn from(id: MemoryId) -> Self {
        id.get()
    }
}

impl FromStr for MemoryId {
    type Err = InvalidMemory;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(Uuid::parse_str(value).map_err(|_| InvalidMemory::Id)?)
    }
}

/// Immutable validated input. Provenance describes the owner's source/context;
/// it is not independently verified evidence or a grant to fetch that source.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct MemoryDraft {
    content: String,
    provenance: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryDraftFields {
    content: String,
    provenance: String,
}

impl MemoryDraft {
    pub fn new(content: String, provenance: String) -> Result<Self, InvalidMemory> {
        if !valid_text(&content, MAX_MEMORY_CONTENT_BYTES) {
            return Err(InvalidMemory::Content);
        }
        if !valid_text(&provenance, MAX_MEMORY_PROVENANCE_BYTES) {
            return Err(InvalidMemory::Provenance);
        }
        // Preserve exact submitted text; never silently truncate or normalize.
        Ok(Self {
            content,
            provenance,
        })
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn provenance(&self) -> &str {
        &self.provenance
    }
}

impl TryFrom<MemoryDraftFields> for MemoryDraft {
    type Error = InvalidMemory;

    fn try_from(fields: MemoryDraftFields) -> Result<Self, Self::Error> {
        Self::new(fields.content, fields.provenance)
    }
}

impl<'de> Deserialize<'de> for MemoryDraft {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(deserialize_object::<D, MemoryDraftFields>(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for MemoryDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryDraft").finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct MemoryRecord {
    id: MemoryId,
    created_at: DateTime<Utc>,
    content: String,
    provenance: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryRecordFields {
    id: MemoryId,
    created_at: DateTime<Utc>,
    content: String,
    provenance: String,
}

impl MemoryRecord {
    /// Restore database metadata without generating a replacement ID/timestamp.
    pub fn restore(id: MemoryId, created_at: DateTime<Utc>, draft: MemoryDraft) -> Self {
        Self {
            id,
            created_at,
            content: draft.content,
            provenance: draft.provenance,
        }
    }

    pub fn id(&self) -> MemoryId {
        self.id
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn provenance(&self) -> &str {
        &self.provenance
    }
}

impl TryFrom<MemoryRecordFields> for MemoryRecord {
    type Error = InvalidMemory;

    fn try_from(fields: MemoryRecordFields) -> Result<Self, Self::Error> {
        Ok(Self::restore(
            fields.id,
            fields.created_at,
            MemoryDraft::new(fields.content, fields.provenance)?,
        ))
    }
}

impl<'de> Deserialize<'de> for MemoryRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(deserialize_object::<D, MemoryRecordFields>(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for MemoryRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryRecord")
            .field("id", &self.id)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

// Derived Serde structs also accept positional arrays. Require an object/map,
// then retain the derived fields' unknown/duplicate/missing-field rejection.
fn deserialize_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct ObjectVisitor<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a memory object")
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(ObjectVisitor(PhantomData))
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes && !value.trim().is_empty() && !value.contains('\0')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid memory page size or cursor")]
pub struct InvalidMemoryPage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct MemoryPageSize(u16);

impl MemoryPageSize {
    pub fn new(size: u16) -> Result<Self, InvalidMemoryPage> {
        if (1..=MAX_MEMORY_PAGE_SIZE).contains(&size) {
            Ok(Self(size))
        } else {
            Err(InvalidMemoryPage)
        }
    }

    pub fn get(self) -> u16 {
        self.0
    }
}

impl Default for MemoryPageSize {
    fn default() -> Self {
        Self(DEFAULT_MEMORY_PAGE_SIZE)
    }
}

impl TryFrom<u16> for MemoryPageSize {
    type Error = InvalidMemoryPage;

    fn try_from(size: u16) -> Result<Self, Self::Error> {
        Self::new(size)
    }
}

impl From<MemoryPageSize> for u16 {
    fn from(size: MemoryPageSize) -> Self {
        size.get()
    }
}

/// Keyset boundary, independent of the continued existence of the last row.
/// This is a position only, never a credential or authorization claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryCursor {
    created_at: DateTime<Utc>,
    id: MemoryId,
}

impl MemoryCursor {
    pub fn from_record(record: &MemoryRecord) -> Self {
        Self {
            created_at: record.created_at(),
            id: record.id(),
        }
    }

    pub fn created_at(self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn id(self) -> MemoryId {
        self.id
    }

    pub fn encode(self) -> String {
        format!("{}:{}", self.created_at.timestamp_micros(), self.id.get())
    }
}

impl FromStr for MemoryCursor {
    type Err = InvalidMemoryPage;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > 80 {
            return Err(InvalidMemoryPage);
        }
        let (timestamp, id) = value.split_once(':').ok_or(InvalidMemoryPage)?;
        let micros = timestamp.parse().map_err(|_| InvalidMemoryPage)?;
        if micros < MIN_MEMORY_CURSOR_MICROS {
            return Err(InvalidMemoryPage);
        }
        Ok(Self {
            created_at: DateTime::from_timestamp_micros(micros).ok_or(InvalidMemoryPage)?,
            id: id.parse().map_err(|_| InvalidMemoryPage)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_and_provenance_are_required_and_bounded_in_utf8_bytes() {
        for text in ["", " \t\n", "\u{2003}", "\0", "private\0text"] {
            assert_eq!(
                MemoryDraft::new(text.into(), "source".into()),
                Err(InvalidMemory::Content)
            );
            assert_eq!(
                MemoryDraft::new("text".into(), text.into()),
                Err(InvalidMemory::Provenance)
            );
        }
        assert!(
            MemoryDraft::new(
                "é".repeat(MAX_MEMORY_CONTENT_BYTES / 2),
                "x".repeat(MAX_MEMORY_PROVENANCE_BYTES)
            )
            .is_ok()
        );
        assert_eq!(
            MemoryDraft::new(
                "é".repeat(MAX_MEMORY_CONTENT_BYTES / 2 + 1),
                "source".into()
            ),
            Err(InvalidMemory::Content)
        );
        assert_eq!(
            MemoryDraft::new("text".into(), "x".repeat(MAX_MEMORY_PROVENANCE_BYTES + 1)),
            Err(InvalidMemory::Provenance)
        );
    }

    #[test]
    fn records_round_trip_without_changing_submitted_text_or_metadata() {
        let draft = MemoryDraft::new("  Private text\n".into(), " Owner note ".into()).unwrap();
        assert_eq!(
            serde_json::from_value::<MemoryDraft>(serde_json::to_value(&draft).unwrap()).unwrap(),
            draft
        );
        let record =
            MemoryRecord::restore(MemoryId::new(Uuid::new_v4()).unwrap(), Utc::now(), draft);
        assert_eq!(record.content(), "  Private text\n");
        assert_eq!(record.provenance(), " Owner note ");
        assert_eq!(
            serde_json::from_value::<MemoryRecord>(serde_json::to_value(&record).unwrap()).unwrap(),
            record
        );
    }

    #[test]
    fn deserialization_enforces_validation_and_rejects_authority_fields() {
        for invalid in [
            serde_json::json!({"content":"", "provenance":"note"}),
            serde_json::json!({"content":"text"}),
            serde_json::json!({"content":"text", "provenance":null}),
            serde_json::json!({"content":"text", "provenance":"note", "actor":"owner"}),
            serde_json::json!({"content":"text", "provenance":"note", "identity_id":Uuid::new_v4()}),
        ] {
            assert!(serde_json::from_value::<MemoryDraft>(invalid).is_err());
        }
        let record = MemoryRecord::restore(
            MemoryId::new(Uuid::new_v4()).unwrap(),
            Utc::now(),
            MemoryDraft::new("text".into(), "note".into()).unwrap(),
        );
        let valid = serde_json::to_value(record).unwrap();
        for (key, value) in [
            ("id", serde_json::json!(Uuid::nil())),
            ("content", serde_json::json!("")),
            ("provenance", serde_json::json!("")),
            ("actor", serde_json::json!("owner")),
            ("created_at", serde_json::json!("invalid")),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(serde_json::from_value::<MemoryRecord>(invalid).is_err());
        }
    }

    #[test]
    fn memory_json_must_be_an_object_with_unique_fields() {
        assert!(serde_json::from_str::<MemoryDraft>(r#"["text","source"]"#).is_err());
        assert!(
            serde_json::from_str::<MemoryRecord>(
                r#"["11111111-1111-4111-8111-111111111111","2026-11-13T00:00:00Z","text","source"]"#
            )
            .is_err()
        );
        for input in [
            r#"{"content":"a","content":"b","provenance":"note"}"#,
            r#"{"content":"a","provenance":"one","provenance":"two"}"#,
        ] {
            assert!(serde_json::from_str::<MemoryDraft>(input).is_err());
        }
    }

    #[test]
    fn debug_and_errors_do_not_disclose_private_text() {
        let draft = MemoryDraft::new("private-content".into(), "private-source".into()).unwrap();
        let record = MemoryRecord::restore(
            MemoryId::new(Uuid::new_v4()).unwrap(),
            Utc::now(),
            draft.clone(),
        );
        for debug in [
            format!("{draft:?}"),
            format!("{record:?}"),
            InvalidMemory::Content.to_string(),
            InvalidMemory::Provenance.to_string(),
        ] {
            assert!(!debug.contains("private-content"));
            assert!(!debug.contains("private-source"));
        }
    }

    #[test]
    fn page_size_and_cursor_validation_are_bounded() {
        assert_eq!(MemoryPageSize::default().get(), 50);
        for size in [1, 50, 100] {
            assert_eq!(MemoryPageSize::new(size).unwrap().get(), size);
        }
        for size in [0, 101, u16::MAX] {
            assert!(MemoryPageSize::new(size).is_err());
        }
        assert!(serde_json::from_str::<MemoryPageSize>("101").is_err());
        for cursor in [
            "",
            "secret",
            "1:invalid",
            "-1000000000000000000:11111111-1111-4111-8111-111111111111",
            "9223372036854775807:11111111-1111-4111-8111-111111111111",
            "1:00000000-0000-0000-0000-000000000000",
        ] {
            assert!(cursor.parse::<MemoryCursor>().is_err());
        }
        let record = MemoryRecord::restore(
            MemoryId::new(Uuid::new_v4()).unwrap(),
            DateTime::from_timestamp_micros(1_800_000_000_123_456).unwrap(),
            MemoryDraft::new("text".into(), "source".into()).unwrap(),
        );
        let cursor = MemoryCursor::from_record(&record);
        assert_eq!(cursor.encode().parse::<MemoryCursor>().unwrap(), cursor);
        assert!(MemoryId::new(Uuid::nil()).is_err());
        assert!("secret".parse::<MemoryId>().is_err());
        let boundary = format!("{MIN_MEMORY_CURSOR_MICROS}:{}", record.id().get());
        assert_eq!(boundary.parse::<MemoryCursor>().unwrap().encode(), boundary);
        let invalid = format!("{}:{}", MIN_MEMORY_CURSOR_MICROS - 1, record.id().get());
        assert!(invalid.parse::<MemoryCursor>().is_err());
    }
}
