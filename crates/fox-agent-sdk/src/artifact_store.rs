use chrono::{DateTime, Duration, Utc};
use fox_agent_core::{
    ArtifactProducer, ArtifactRecord, ArtifactRetentionClass, ArtifactStoreConfig, ArtifactType,
};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct ArtifactStoreGcReport {
    pub deleted: u64,
    pub kept: u64,
    pub bytes_freed: u64,
    pub session_quota_evictions: u64,
    pub store_quota_evictions: u64,
}

#[derive(Debug, Clone)]
pub struct ArtifactPutResult {
    pub record: ArtifactRecord,
    pub gc_report: Option<ArtifactStoreGcReport>,
}

#[async_trait::async_trait]
pub trait ArtifactStore: Send + Sync {
    async fn put_text(
        &self,
        session_id: &str,
        producer: ArtifactProducer,
        artifact_type: ArtifactType,
        class: ArtifactRetentionClass,
        text: String,
        metadata: Value,
    ) -> Result<ArtifactPutResult, String>;

    async fn get_record(&self, artifact_id: &str) -> Result<Option<ArtifactRecord>, String>;

    async fn get_text(&self, artifact_id: &str) -> Result<Option<String>, String>;

    async fn delete(&self, artifact_id: &str) -> Result<(), String>;

    async fn list_by_session(&self, session_id: &str) -> Result<Vec<ArtifactRecord>, String>;

    async fn gc_expired(&self, session_id: Option<&str>) -> Result<ArtifactStoreGcReport, String>;

    /// Return aggregate statistics grouped by artifact type and total count.
    async fn stats_by_type(&self, session_id: &str) -> Result<ArtifactTypeStats, String>;
}

/// Aggregate statistics for artifacts in a session.
#[derive(Debug, Clone, Default)]
pub struct ArtifactTypeStats {
    pub total_count: u64,
    pub total_bytes: u64,
    pub by_type: std::collections::HashMap<String, TypeCount>,
}

impl ArtifactTypeStats {
    pub fn format_summary(&self) -> String {
        let mut lines = vec![format!(
            "{} artifacts, {} bytes total",
            self.total_count, self.total_bytes
        )];
        for (kind, tc) in &self.by_type {
            lines.push(format!("  {kind}: {} items, {} bytes", tc.count, tc.bytes));
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone, Default)]
pub struct TypeCount {
    pub count: u64,
    pub bytes: u64,
}

pub struct DisabledArtifactStore;

#[async_trait::async_trait]
impl ArtifactStore for DisabledArtifactStore {
    async fn put_text(
        &self,
        _session_id: &str,
        _producer: ArtifactProducer,
        _artifact_type: ArtifactType,
        _class: ArtifactRetentionClass,
        _text: String,
        _metadata: Value,
    ) -> Result<ArtifactPutResult, String> {
        Err("artifact store is disabled".to_string())
    }

    async fn get_record(&self, _artifact_id: &str) -> Result<Option<ArtifactRecord>, String> {
        Ok(None)
    }

    async fn get_text(&self, _artifact_id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    async fn delete(&self, _artifact_id: &str) -> Result<(), String> {
        Ok(())
    }

    async fn list_by_session(&self, _session_id: &str) -> Result<Vec<ArtifactRecord>, String> {
        Ok(Vec::new())
    }

    async fn gc_expired(&self, _session_id: Option<&str>) -> Result<ArtifactStoreGcReport, String> {
        Ok(ArtifactStoreGcReport {
            deleted: 0,
            kept: 0,
            bytes_freed: 0,
            session_quota_evictions: 0,
            store_quota_evictions: 0,
        })
    }

    async fn stats_by_type(&self, _session_id: &str) -> Result<ArtifactTypeStats, String> {
        Ok(ArtifactTypeStats::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_artifact_store_roundtrip_put_get_delete() {
        let root =
            std::env::temp_dir().join(format!("fox-artifact-store-{}", uuid::Uuid::new_v4()));
        let mut cfg = ArtifactStoreConfig::default();
        cfg.enabled = true;
        cfg.max_artifact_bytes = 4096;
        cfg.gc_after_write = false;

        let store = FileArtifactStore::new(cfg, root.join("artifacts"));

        let record = store
            .put_text(
                "s1",
                ArtifactProducer::Tool {
                    tool_name: "read".to_string(),
                },
                ArtifactType::FileChunk,
                ArtifactRetentionClass::Ephemeral,
                "hello".to_string(),
                serde_json::json!({"k":"v"}),
            )
            .await
            .unwrap()
            .record;

        let loaded = store.get_text(&record.artifact_id).await.unwrap().unwrap();
        assert_eq!(loaded, "hello");

        let listed = store.list_by_session("s1").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].artifact_id, record.artifact_id);

        store.delete(&record.artifact_id).await.unwrap();
        let missing = store.get_text(&record.artifact_id).await.unwrap();
        assert!(missing.is_none());

        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn file_artifact_store_enforces_session_quota() {
        let root =
            std::env::temp_dir().join(format!("fox-artifact-store-quota-{}", uuid::Uuid::new_v4()));
        let mut cfg = ArtifactStoreConfig::default();
        cfg.enabled = true;
        cfg.max_artifact_bytes = 4096;
        cfg.max_session_bytes = 10;
        cfg.gc_after_write = true;

        let store = FileArtifactStore::new(cfg, root.join("artifacts"));

        let first = store
            .put_text(
                "s1",
                ArtifactProducer::Tool {
                    tool_name: "read".to_string(),
                },
                ArtifactType::FileChunk,
                ArtifactRetentionClass::Ephemeral,
                "12345".to_string(),
                serde_json::json!({}),
            )
            .await
            .unwrap()
            .record;

        let second = store
            .put_text(
                "s1",
                ArtifactProducer::Tool {
                    tool_name: "grep".to_string(),
                },
                ArtifactType::SearchResults,
                ArtifactRetentionClass::Ephemeral,
                "67890".to_string(),
                serde_json::json!({}),
            )
            .await
            .unwrap()
            .record;

        let third = store
            .put_text(
                "s1",
                ArtifactProducer::Tool {
                    tool_name: "web_fetch".to_string(),
                },
                ArtifactType::WebPage,
                ArtifactRetentionClass::Ephemeral,
                "abcde".to_string(),
                serde_json::json!({}),
            )
            .await
            .unwrap()
            .record;

        let listed = store.list_by_session("s1").await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|r| r.artifact_id != first.artifact_id));
        assert!(listed.iter().any(|r| r.artifact_id == second.artifact_id));
        assert!(listed.iter().any(|r| r.artifact_id == third.artifact_id));

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}

#[derive(Clone)]
pub struct FileArtifactStore {
    cfg: ArtifactStoreConfig,
    root: PathBuf,
    lock: Arc<RwLock<()>>,
}

impl FileArtifactStore {
    pub fn new(cfg: ArtifactStoreConfig, root: PathBuf) -> Self {
        Self {
            cfg,
            root,
            lock: Arc::new(RwLock::new(())),
        }
    }

    fn records_dir(&self) -> PathBuf {
        self.root.join("records")
    }

    fn payload_dir(&self) -> PathBuf {
        self.root.join("payload")
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn record_path(&self, artifact_id: &str) -> PathBuf {
        self.records_dir().join(format!("{artifact_id}.json"))
    }

    fn payload_path(&self, artifact_id: &str) -> PathBuf {
        self.payload_dir().join(format!("{artifact_id}.txt"))
    }

    fn session_index_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(session_id).join("index.json")
    }

    async fn ensure_dirs(&self) -> Result<(), String> {
        tokio::fs::create_dir_all(self.records_dir())
            .await
            .map_err(|e| format!("failed to create artifact records dir: {e}"))?;
        tokio::fs::create_dir_all(self.payload_dir())
            .await
            .map_err(|e| format!("failed to create artifact payload dir: {e}"))?;
        Ok(())
    }

    async fn read_index(&self, session_id: &str) -> Result<Vec<String>, String> {
        let path = self.session_index_path(session_id);
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(format!(
                    "failed to read session artifact index {}: {e}",
                    path.display()
                ));
            }
        };
        serde_json::from_str(&content).map_err(|e| {
            format!(
                "failed to parse session artifact index {}: {e}",
                path.display()
            )
        })
    }

    async fn write_index(&self, session_id: &str, ids: &[String]) -> Result<(), String> {
        let path = self.session_index_path(session_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                format!(
                    "failed to create session artifact dir {}: {e}",
                    parent.display()
                )
            })?;
        }
        let payload = serde_json::to_string_pretty(ids)
            .map_err(|e| format!("failed to serialize session artifact index: {e}"))?;
        tokio::fs::write(&path, payload).await.map_err(|e| {
            format!(
                "failed to write session artifact index {}: {e}",
                path.display()
            )
        })
    }

    fn compute_hash(text: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn compute_expiry(
        &self,
        class: ArtifactRetentionClass,
        metadata: &Value,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let hours = metadata
            .get("ttl_hours_override")
            .and_then(|v| v.as_u64())
            .unwrap_or(match class {
                ArtifactRetentionClass::Ephemeral => self.cfg.ephemeral_ttl_hours,
                ArtifactRetentionClass::Referenced => self.cfg.referenced_ttl_hours,
                ArtifactRetentionClass::Pinned => self.cfg.pinned_ttl_hours,
            });
        if hours == 0 {
            None
        } else {
            Some(now + Duration::hours(hours as i64))
        }
    }

    async fn write_record(&self, record: &ArtifactRecord) -> Result<(), String> {
        let path = self.record_path(&record.artifact_id);
        let payload = serde_json::to_string_pretty(record)
            .map_err(|e| format!("failed to serialize artifact record: {e}"))?;
        tokio::fs::write(&path, payload)
            .await
            .map_err(|e| format!("failed to write artifact record {}: {e}", path.display()))
    }

    async fn read_record_file(&self, artifact_id: &str) -> Result<Option<ArtifactRecord>, String> {
        let path = self.record_path(artifact_id);
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "failed to read artifact record {}: {e}",
                    path.display()
                ));
            }
        };
        let record: ArtifactRecord = serde_json::from_str(&content)
            .map_err(|e| format!("failed to parse artifact record {}: {e}", path.display()))?;
        Ok(Some(record))
    }

    async fn remove_file_if_exists(path: &Path) -> Result<(), String> {
        match tokio::fs::remove_file(path).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("failed to delete {}: {e}", path.display())),
        }
    }

    async fn session_ids(&self) -> Result<Vec<String>, String> {
        let dir = self.sessions_dir();
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(format!(
                    "failed to read artifact sessions dir {}: {e}",
                    dir.display()
                ));
            }
        };
        let mut ids = Vec::new();
        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| format!("failed to iterate artifact sessions dir: {e}"))?
        {
            if entry
                .file_type()
                .await
                .map_err(|e| format!("failed to stat artifact sessions entry: {e}"))?
                .is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                ids.push(name.to_string());
            }
        }
        Ok(ids)
    }

    async fn delete_record_unlocked(&self, record: &ArtifactRecord) -> Result<(), String> {
        Self::remove_file_if_exists(&record.storage_path).await?;
        Self::remove_file_if_exists(&self.record_path(&record.artifact_id)).await?;

        let mut index = self.read_index(&record.session_id).await?;
        index.retain(|id| id != &record.artifact_id);
        let _ = self.write_index(&record.session_id, &index).await;
        Ok(())
    }

    async fn touch_record_unlocked(&self, record: &mut ArtifactRecord) -> Result<(), String> {
        record.last_access_at = Utc::now();
        self.write_record(record).await
    }

    fn store_quota_bytes(&self) -> Option<u64> {
        [self.cfg.max_project_bytes, self.cfg.max_global_bytes]
            .into_iter()
            .filter(|v| *v > 0)
            .min()
    }

    fn eviction_rank(record: &ArtifactRecord) -> (u8, DateTime<Utc>, u32) {
        let class_rank = match record.class {
            ArtifactRetentionClass::Ephemeral => 0,
            ArtifactRetentionClass::Referenced => 1,
            ArtifactRetentionClass::Pinned => 2,
        };
        (class_rank, record.last_access_at, record.ref_count)
    }

    async fn enforce_session_quota_unlocked(&self, session_id: &str) -> Result<(u64, u64), String> {
        if self.cfg.max_session_bytes == 0 {
            return Ok((0, 0));
        }
        let mut records = self.list_by_session(session_id).await?;
        let mut total: u64 = records.iter().map(|r| r.size_bytes).sum();
        if total <= self.cfg.max_session_bytes {
            return Ok((0, 0));
        }

        records.sort_by_key(Self::eviction_rank);
        let mut deleted = 0u64;
        let mut bytes_freed = 0u64;
        for record in records {
            if total <= self.cfg.max_session_bytes {
                break;
            }
            total = total.saturating_sub(record.size_bytes);
            bytes_freed += record.size_bytes;
            deleted += 1;
            self.delete_record_unlocked(&record).await?;
        }
        Ok((deleted, bytes_freed))
    }

    async fn enforce_store_quota_unlocked(&self) -> Result<(u64, u64), String> {
        let Some(limit) = self.store_quota_bytes() else {
            return Ok((0, 0));
        };

        let mut records = Vec::new();
        for session_id in self.session_ids().await? {
            records.extend(self.list_by_session(&session_id).await?);
        }
        let mut total: u64 = records.iter().map(|r| r.size_bytes).sum();
        if total <= limit {
            return Ok((0, 0));
        }

        records.sort_by_key(Self::eviction_rank);
        let mut deleted = 0u64;
        let mut bytes_freed = 0u64;
        for record in records {
            if total <= limit {
                break;
            }
            total = total.saturating_sub(record.size_bytes);
            bytes_freed += record.size_bytes;
            deleted += 1;
            self.delete_record_unlocked(&record).await?;
        }
        Ok((deleted, bytes_freed))
    }

    async fn gc_unlocked(&self, session_id: Option<&str>) -> Result<ArtifactStoreGcReport, String> {
        let now = Utc::now();
        let targets = if let Some(sid) = session_id {
            vec![sid.to_string()]
        } else {
            self.session_ids().await?
        };

        let mut deleted = 0u64;
        let mut kept = 0u64;
        let mut bytes_freed = 0u64;
        let mut session_quota_evictions = 0u64;
        let mut store_quota_evictions = 0u64;

        for sid in &targets {
            let mut ids = self.read_index(sid).await?;
            let mut kept_ids = Vec::new();
            let mut seen = HashSet::new();
            for id in ids.drain(..) {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let record = match self.read_record_file(&id).await? {
                    Some(r) => r,
                    None => continue,
                };
                let expired = record.expires_at.map(|t| t <= now).unwrap_or(false);
                if expired {
                    bytes_freed += record.size_bytes;
                    self.delete_record_unlocked(&record).await?;
                    deleted += 1;
                } else {
                    kept_ids.push(id);
                    kept += 1;
                }
            }
            let _ = self.write_index(sid, &kept_ids).await;

            let (quota_deleted, quota_freed) = self.enforce_session_quota_unlocked(sid).await?;
            deleted += quota_deleted;
            bytes_freed += quota_freed;
            session_quota_evictions += quota_deleted;
        }

        let (store_deleted, store_freed) = self.enforce_store_quota_unlocked().await?;
        deleted += store_deleted;
        bytes_freed += store_freed;
        store_quota_evictions += store_deleted;

        kept = kept.saturating_sub(session_quota_evictions + store_quota_evictions);

        Ok(ArtifactStoreGcReport {
            deleted,
            kept,
            bytes_freed,
            session_quota_evictions,
            store_quota_evictions,
        })
    }
}

#[async_trait::async_trait]
impl ArtifactStore for FileArtifactStore {
    async fn put_text(
        &self,
        session_id: &str,
        producer: ArtifactProducer,
        artifact_type: ArtifactType,
        class: ArtifactRetentionClass,
        text: String,
        metadata: Value,
    ) -> Result<ArtifactPutResult, String> {
        if !self.cfg.enabled {
            return Err("artifact store is disabled".to_string());
        }
        if (text.len() as u64) > self.cfg.max_artifact_bytes {
            return Err("artifact exceeds max_artifact_bytes".to_string());
        }

        let _guard = self.lock.write().await;
        self.ensure_dirs().await?;

        let now = Utc::now();
        let artifact_id = uuid::Uuid::new_v4().to_string();
        let content_hash = Self::compute_hash(&text);
        let payload_path = self.payload_path(&artifact_id);

        tokio::fs::write(&payload_path, text).await.map_err(|e| {
            format!(
                "failed to write artifact payload {}: {e}",
                payload_path.display()
            )
        })?;

        let size_bytes = tokio::fs::metadata(&payload_path)
            .await
            .map_err(|e| {
                format!(
                    "failed to stat artifact payload {}: {e}",
                    payload_path.display()
                )
            })?
            .len();

        let record = ArtifactRecord {
            artifact_id: artifact_id.clone(),
            session_id: session_id.to_string(),
            producer,
            artifact_type,
            size_bytes,
            content_hash,
            class,
            ref_count: 0,
            last_access_at: now,
            expires_at: self.compute_expiry(class, &metadata, now),
            metadata,
            storage_path: payload_path,
        };

        if self.cfg.deduplicate_by_content_hash {
            let existing = self.list_by_session(session_id).await?;
            if let Some(hit) = existing.into_iter().find(|r| {
                r.content_hash == record.content_hash && r.size_bytes == record.size_bytes
            }) {
                Self::remove_file_if_exists(&record.storage_path).await?;
                return Ok(ArtifactPutResult {
                    record: hit,
                    gc_report: None,
                });
            }
        }

        self.write_record(&record).await?;

        let mut index = self.read_index(session_id).await?;
        index.push(artifact_id);
        self.write_index(session_id, &index).await?;

        let gc_report = if self.cfg.gc_after_write {
            Some(self.gc_unlocked(Some(session_id)).await?)
        } else {
            None
        };

        Ok(ArtifactPutResult { record, gc_report })
    }

    async fn get_record(&self, artifact_id: &str) -> Result<Option<ArtifactRecord>, String> {
        let _guard = self.lock.write().await;
        let mut record = match self.read_record_file(artifact_id).await? {
            Some(r) => r,
            None => return Ok(None),
        };
        self.touch_record_unlocked(&mut record).await?;
        Ok(Some(record))
    }

    async fn get_text(&self, artifact_id: &str) -> Result<Option<String>, String> {
        let record = match self.get_record(artifact_id).await? {
            Some(r) => r,
            None => return Ok(None),
        };
        let content = match tokio::fs::read_to_string(&record.storage_path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "failed to read artifact payload {}: {e}",
                    record.storage_path.display()
                ));
            }
        };
        Ok(Some(content))
    }

    async fn delete(&self, artifact_id: &str) -> Result<(), String> {
        let _guard = self.lock.write().await;
        let record = match self.read_record_file(artifact_id).await? {
            Some(r) => r,
            None => return Ok(()),
        };
        self.delete_record_unlocked(&record).await
    }

    async fn list_by_session(&self, session_id: &str) -> Result<Vec<ArtifactRecord>, String> {
        let ids = self.read_index(session_id).await?;
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(r) = self.read_record_file(&id).await? {
                records.push(r);
            }
        }
        Ok(records)
    }

    async fn gc_expired(&self, session_id: Option<&str>) -> Result<ArtifactStoreGcReport, String> {
        let _guard = self.lock.write().await;
        self.gc_unlocked(session_id).await
    }

    async fn stats_by_type(&self, session_id: &str) -> Result<ArtifactTypeStats, String> {
        let records = self.list_by_session(session_id).await?;
        let mut stats = ArtifactTypeStats {
            total_count: records.len() as u64,
            total_bytes: records.iter().map(|r| r.size_bytes).sum(),
            ..Default::default()
        };
        for record in &records {
            let type_key = format!("{:?}", record.artifact_type);
            let entry = stats.by_type.entry(type_key).or_default();
            entry.count += 1;
            entry.bytes += record.size_bytes;
        }
        Ok(stats)
    }
}
