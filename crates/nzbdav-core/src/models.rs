use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Virtual filesystem node
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DavItem {
    pub id: Uuid,
    pub id_prefix: String,
    pub created_at: chrono::NaiveDateTime,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub file_size: Option<i64>,
    pub item_type: ItemType,
    pub sub_type: ItemSubType,
    pub path: String,
    pub release_date: Option<chrono::DateTime<chrono::Utc>>,
    pub last_health_check: Option<chrono::DateTime<chrono::Utc>>,
    pub next_health_check: Option<chrono::DateTime<chrono::Utc>>,
    pub history_item_id: Option<Uuid>,
    pub file_blob_id: Option<Uuid>,
    pub nzb_blob_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum ItemType {
    Directory = 1,
    UsenetFile = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum ItemSubType {
    // Directory subtypes
    Directory = 101,
    WebdavRoot = 102,
    NzbsRoot = 103,
    ContentRoot = 104,
    SymlinkRoot = 105,
    IdsRoot = 106,
    // File subtypes
    NzbFile = 201,
    RarFile = 202,
    MultipartFile = 203,
    ReadmeFile = 204,
}

impl TryFrom<i32> for ItemType {
    type Error = String;
    fn try_from(value: i32) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Directory),
            2 => Ok(Self::UsenetFile),
            _ => Err(format!("invalid ItemType: {value}")),
        }
    }
}

impl TryFrom<i32> for ItemSubType {
    type Error = String;
    fn try_from(value: i32) -> std::result::Result<Self, Self::Error> {
        match value {
            101 => Ok(Self::Directory),
            102 => Ok(Self::WebdavRoot),
            103 => Ok(Self::NzbsRoot),
            104 => Ok(Self::ContentRoot),
            105 => Ok(Self::SymlinkRoot),
            106 => Ok(Self::IdsRoot),
            201 => Ok(Self::NzbFile),
            202 => Ok(Self::RarFile),
            203 => Ok(Self::MultipartFile),
            204 => Ok(Self::ReadmeFile),
            _ => Err(format!("invalid ItemSubType: {value}")),
        }
    }
}

impl TryFrom<i32> for DownloadStatus {
    type Error = String;
    fn try_from(value: i32) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Completed),
            2 => Ok(Self::Failed),
            _ => Err(format!("invalid DownloadStatus: {value}")),
        }
    }
}

// ---------------------------------------------------------------------------
// File metadata (stored as blobs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DavNzbFile {
    pub segment_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DavMultipartFile {
    pub aes_params: Option<AesParams>,
    pub file_parts: Vec<FilePart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePart {
    pub segment_ids: Vec<String>,
    pub segment_id_byte_range: LongRange,
    pub file_part_byte_range: LongRange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AesParams {
    pub iv: Vec<u8>,
    pub key: Vec<u8>,
    pub decoded_size: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LongRange {
    pub start: i64,
    pub end: i64,
}

impl LongRange {
    pub fn new(start: i64, end: i64) -> Self {
        Self { start, end }
    }

    pub fn from_start_and_size(start: i64, size: i64) -> Self {
        Self {
            start,
            end: start + size,
        }
    }

    pub fn size(&self) -> i64 {
        self.end - self.start
    }

    pub fn contains(&self, value: i64) -> bool {
        value >= self.start && value < self.end
    }
}

// ---------------------------------------------------------------------------
// Queue and history
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: Uuid,
    pub created_at: chrono::NaiveDateTime,
    pub file_name: String,
    pub job_name: String,
    pub nzb_file_size: i64,
    pub total_segment_bytes: i64,
    pub category: String,
    pub priority: i32,
    pub post_processing: i32,
    pub pause_until: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryItem {
    pub id: Uuid,
    pub created_at: chrono::NaiveDateTime,
    pub file_name: String,
    pub job_name: String,
    pub category: String,
    pub download_status: DownloadStatus,
    pub total_segment_bytes: i64,
    pub download_time_seconds: i32,
    pub fail_message: Option<String>,
    pub download_dir_id: Option<Uuid>,
    pub nzb_blob_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum DownloadStatus {
    Completed = 1,
    Failed = 2,
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckResult {
    pub id: Uuid,
    pub dav_item_id: Uuid,
    pub path: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub result: HealthResult,
    pub repair_status: RepairStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum HealthResult {
    Healthy = 0,
    Unhealthy = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum RepairStatus {
    None = 0,
    Repaired = 1,
    Deleted = 2,
    ActionNeeded = 3,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_from_item_type() {
        assert_eq!(ItemType::try_from(1).unwrap(), ItemType::Directory);
        assert_eq!(ItemType::try_from(2).unwrap(), ItemType::UsenetFile);
        assert!(ItemType::try_from(0).is_err());
        assert!(ItemType::try_from(99).is_err());
    }

    #[test]
    fn test_try_from_item_sub_type() {
        assert_eq!(ItemSubType::try_from(101).unwrap(), ItemSubType::Directory);
        assert_eq!(ItemSubType::try_from(102).unwrap(), ItemSubType::WebdavRoot);
        assert_eq!(ItemSubType::try_from(103).unwrap(), ItemSubType::NzbsRoot);
        assert_eq!(
            ItemSubType::try_from(104).unwrap(),
            ItemSubType::ContentRoot
        );
        assert_eq!(
            ItemSubType::try_from(105).unwrap(),
            ItemSubType::SymlinkRoot
        );
        assert_eq!(ItemSubType::try_from(106).unwrap(), ItemSubType::IdsRoot);
        assert_eq!(ItemSubType::try_from(201).unwrap(), ItemSubType::NzbFile);
        assert_eq!(ItemSubType::try_from(202).unwrap(), ItemSubType::RarFile);
        assert_eq!(
            ItemSubType::try_from(203).unwrap(),
            ItemSubType::MultipartFile
        );
        assert_eq!(ItemSubType::try_from(204).unwrap(), ItemSubType::ReadmeFile);
        assert!(ItemSubType::try_from(0).is_err());
        assert!(ItemSubType::try_from(999).is_err());
    }

    #[test]
    fn test_try_from_download_status() {
        assert_eq!(
            DownloadStatus::try_from(1).unwrap(),
            DownloadStatus::Completed
        );
        assert_eq!(DownloadStatus::try_from(2).unwrap(), DownloadStatus::Failed);
        assert!(DownloadStatus::try_from(0).is_err());
        assert!(DownloadStatus::try_from(3).is_err());
    }

    #[test]
    fn test_long_range() {
        let r = LongRange::new(10, 50);
        assert_eq!(r.start, 10);
        assert_eq!(r.end, 50);
        assert_eq!(r.size(), 40);

        let r2 = LongRange::from_start_and_size(100, 200);
        assert_eq!(r2.start, 100);
        assert_eq!(r2.end, 300);
        assert_eq!(r2.size(), 200);

        assert!(r.contains(10));
        assert!(r.contains(25));
        assert!(r.contains(49));
        assert!(!r.contains(50)); // end is exclusive
        assert!(!r.contains(9));
    }
}
