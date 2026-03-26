//! RocksDB-backed session store for persistent agent sessions.
//!
//! Sessions are stored in the `cf_sessions` column family with
//! key = `session_id` (32 bytes Blake3Digest) and value = BCS(AgentSession).
//!
//! On node startup, non-terminal sessions can be recovered by scanning
//! the entire column family and filtering by [`SessionState::is_terminal`].

use std::sync::Mutex;

use nexus_primitives::Blake3Digest;
use nexus_storage::{ColumnFamily, StateStorage, WriteBatchOps};

use crate::agent_core::session::AgentSession;
#[cfg(test)]
use crate::agent_core::session::SessionState;

const CF: &str = "cf_sessions";

/// Persistent session store backed by a [`StateStorage`] implementation.
///
/// Thread-safe via internal `Mutex` for in-memory index consistency.
/// The RocksDB writes are authoritative; the in-memory map is a
/// read cache rebuilt from the DB on [`Self::recover`].
pub struct RocksSessionStore<S: StateStorage> {
    store: S,
    /// In-memory cache: session_id → AgentSession.
    cache: Mutex<std::collections::HashMap<Blake3Digest, AgentSession>>,
}

impl<S: StateStorage> RocksSessionStore<S> {
    /// Create a new session store backed by the given storage.
    ///
    /// Call [`recover`](Self::recover) after construction to populate
    /// the in-memory cache from the database.
    pub fn new(store: S) -> Self {
        Self {
            store,
            cache: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Persist a new session.
    pub async fn put(&self, session: &AgentSession) -> Result<(), crate::error::IntentError> {
        let key = session.session_id.0.to_vec();
        let value =
            bcs::to_bytes(session).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;

        let mut batch = self.store.new_batch();
        batch.put_cf(CF, key, value);
        self.store
            .write_batch(batch)
            .await
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;

        // Update cache.
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(session.session_id, session.clone());
        Ok(())
    }

    /// Update an existing session (e.g. after a state transition).
    pub async fn update(&self, session: &AgentSession) -> Result<(), crate::error::IntentError> {
        self.put(session).await
    }

    /// Retrieve a session by ID (from cache, falling back to DB).
    pub fn get(&self, session_id: &Blake3Digest) -> Option<AgentSession> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = cache.get(session_id) {
            return Some(s.clone());
        }
        drop(cache);

        // Fallback to DB (sync path).
        let key = session_id.0.to_vec();
        match self.store.get_sync(CF, &key) {
            Ok(Some(bytes)) => {
                if let Ok(session) = bcs::from_bytes::<AgentSession>(&bytes) {
                    let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                    cache.insert(*session_id, session.clone());
                    Some(session)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Delete a session by ID (e.g. TTL expiration cleanup).
    pub async fn delete(&self, session_id: &Blake3Digest) -> Result<(), crate::error::IntentError> {
        let key = session_id.0.to_vec();
        let mut batch = self.store.new_batch();
        batch.delete_cf(CF, key);
        self.store
            .write_batch(batch)
            .await
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;

        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.remove(session_id);
        Ok(())
    }

    /// Recover all sessions from the database into the in-memory cache.
    ///
    /// Returns the number of sessions recovered (both terminal and non-terminal).
    pub fn recover(&self) -> Result<RecoveryResult, crate::error::IntentError> {
        // Scan the full CF range.
        let start = [0u8; 0].to_vec();
        let end = [0xFFu8; 33].to_vec();
        let entries = self
            .store
            .scan(ColumnFamily::Sessions.as_str(), &start, &end)
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;

        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut total = 0u64;
        let mut active = 0u64;
        let mut terminal = 0u64;

        for (_key, value) in entries {
            if let Ok(session) = bcs::from_bytes::<AgentSession>(&value) {
                if session.current_state.is_terminal() {
                    terminal += 1;
                } else {
                    active += 1;
                }
                cache.insert(session.session_id, session);
                total += 1;
            }
        }

        Ok(RecoveryResult {
            total,
            active,
            terminal,
        })
    }

    /// Return all non-terminal (active) sessions.
    pub fn active_sessions(&self) -> Vec<AgentSession> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .values()
            .filter(|s| !s.current_state.is_terminal())
            .cloned()
            .collect()
    }

    /// Return all sessions (both active and terminal).
    pub fn all_sessions(&self) -> Vec<AgentSession> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.values().cloned().collect()
    }

    /// Total session count in cache.
    pub fn len(&self) -> usize {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove all terminal sessions older than `cutoff_ms` from both DB and cache.
    ///
    /// Returns the number of sessions cleaned up.
    pub async fn cleanup_expired(&self, cutoff_ms: u64) -> Result<u64, crate::error::IntentError> {
        let to_remove: Vec<Blake3Digest> = {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .values()
                .filter(|s| s.current_state.is_terminal() && s.created_at_ms.0 < cutoff_ms)
                .map(|s| s.session_id)
                .collect()
        };

        if to_remove.is_empty() {
            return Ok(0);
        }

        let mut batch = self.store.new_batch();
        for id in &to_remove {
            batch.delete_cf(CF, id.0.to_vec());
        }
        self.store
            .write_batch(batch)
            .await
            .map_err(|e| crate::error::IntentError::Storage(e.to_string()))?;

        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        for id in &to_remove {
            cache.remove(id);
        }

        Ok(to_remove.len() as u64)
    }
}

/// Summary of a session recovery operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryResult {
    /// Total sessions loaded from the database.
    pub total: u64,
    /// Sessions in non-terminal states (can be resumed).
    pub active: u64,
    /// Sessions in terminal states (Finalized/Aborted/Expired).
    pub terminal: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::TimestampMs;
    use nexus_storage::MemoryStore;

    fn make_session(id: u8, state: SessionState) -> AgentSession {
        let mut s = AgentSession::new(Blake3Digest([id; 32]), TimestampMs(1_000_000));
        s.current_state = state;
        s
    }

    #[tokio::test]
    async fn put_and_get() {
        let store = RocksSessionStore::new(MemoryStore::new());
        let session = make_session(0x01, SessionState::Received);
        store.put(&session).await.unwrap();

        let retrieved = store.get(&Blake3Digest([0x01; 32]));
        assert_eq!(retrieved, Some(session));
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let store = RocksSessionStore::new(MemoryStore::new());
        assert!(store.get(&Blake3Digest([0xFF; 32])).is_none());
    }

    #[tokio::test]
    async fn update_session() {
        let store = RocksSessionStore::new(MemoryStore::new());
        let mut session = make_session(0x01, SessionState::Received);
        store.put(&session).await.unwrap();

        session.current_state = SessionState::Simulated;
        store.update(&session).await.unwrap();

        let retrieved = store.get(&Blake3Digest([0x01; 32])).unwrap();
        assert_eq!(retrieved.current_state, SessionState::Simulated);
    }

    #[tokio::test]
    async fn delete_session() {
        let store = RocksSessionStore::new(MemoryStore::new());
        let session = make_session(0x01, SessionState::Received);
        store.put(&session).await.unwrap();
        assert_eq!(store.len(), 1);

        store.delete(&Blake3Digest([0x01; 32])).await.unwrap();
        assert!(store.get(&Blake3Digest([0x01; 32])).is_none());
        assert_eq!(store.len(), 0);
    }

    #[tokio::test]
    async fn active_sessions_filter() {
        let store = RocksSessionStore::new(MemoryStore::new());
        store
            .put(&make_session(0x01, SessionState::Received))
            .await
            .unwrap();
        store
            .put(&make_session(0x02, SessionState::Executing))
            .await
            .unwrap();
        store
            .put(&make_session(0x03, SessionState::Finalized))
            .await
            .unwrap();
        store
            .put(&make_session(0x04, SessionState::Aborted))
            .await
            .unwrap();

        let active = store.active_sessions();
        assert_eq!(active.len(), 2);
    }

    #[tokio::test]
    async fn cleanup_expired() {
        let store = RocksSessionStore::new(MemoryStore::new());

        // Old terminal session
        let mut s1 = AgentSession::new(Blake3Digest([0x01; 32]), TimestampMs(500));
        s1.current_state = SessionState::Finalized;
        store.put(&s1).await.unwrap();

        // Recent terminal session
        let mut s2 = AgentSession::new(Blake3Digest([0x02; 32]), TimestampMs(2_000));
        s2.current_state = SessionState::Aborted;
        store.put(&s2).await.unwrap();

        // Active session (should not be cleaned)
        let s3 = AgentSession::new(Blake3Digest([0x03; 32]), TimestampMs(500));
        store.put(&s3).await.unwrap();

        // Cleanup sessions created before 1000ms
        let cleaned = store.cleanup_expired(1_000).await.unwrap();
        assert_eq!(cleaned, 1);
        assert_eq!(store.len(), 2);
        assert!(store.get(&Blake3Digest([0x01; 32])).is_none()); // cleaned
        assert!(store.get(&Blake3Digest([0x02; 32])).is_some()); // recent
        assert!(store.get(&Blake3Digest([0x03; 32])).is_some()); // active
    }

    // ── Z-4: Cold-start verification ────────────────────────────────

    #[tokio::test]
    async fn cold_start_recovers_active_sessions() {
        // Simulate a node lifecycle: store sessions, "restart" the store,
        // then verify recover() restores the correct state.
        let backing = MemoryStore::new();

        // Phase 1: Write sessions to the original store instance.
        {
            let store = RocksSessionStore::new(backing.clone());
            store
                .put(&make_session(0x01, SessionState::Received))
                .await
                .unwrap();
            store
                .put(&make_session(0x02, SessionState::Simulated))
                .await
                .unwrap();
            store
                .put(&make_session(0x03, SessionState::Executing))
                .await
                .unwrap();
            store
                .put(&make_session(0x04, SessionState::Finalized))
                .await
                .unwrap();
            store
                .put(&make_session(0x05, SessionState::Aborted))
                .await
                .unwrap();
            assert_eq!(store.len(), 5);
            // Drop `store` — simulates node shutdown.
        }

        // Phase 2: Create a fresh store from the same backing DB
        // (simulates node cold-start).
        let store2 = RocksSessionStore::new(backing);
        assert_eq!(store2.len(), 0, "cache should be empty before recover");

        let result = store2.recover().unwrap();
        assert_eq!(result.total, 5);
        assert_eq!(result.active, 3); // Received + Simulated + Executing
        assert_eq!(result.terminal, 2); // Finalized + Aborted

        // Verify all sessions are present in cache.
        assert_eq!(store2.len(), 5);
        assert!(store2.get(&Blake3Digest([0x01; 32])).is_some());
        assert!(store2.get(&Blake3Digest([0x04; 32])).is_some());

        // Verify active_sessions only returns non-terminal.
        let active = store2.active_sessions();
        assert_eq!(active.len(), 3);
        for s in &active {
            assert!(
                !s.current_state.is_terminal(),
                "active_sessions must not contain terminal states"
            );
        }
    }

    #[tokio::test]
    async fn cold_start_with_state_transitions_preserves_latest() {
        let backing = MemoryStore::new();

        // Phase 1: Create a session and advance it through multiple states.
        {
            let store = RocksSessionStore::new(backing.clone());
            let mut session = make_session(0x01, SessionState::Received);
            store.put(&session).await.unwrap();

            session.current_state = SessionState::Simulated;
            store.update(&session).await.unwrap();

            session.current_state = SessionState::AwaitingConfirmation;
            store.update(&session).await.unwrap();
        }

        // Phase 2: Recover and verify the latest state is preserved.
        let store2 = RocksSessionStore::new(backing);
        store2.recover().unwrap();

        let recovered = store2.get(&Blake3Digest([0x01; 32])).unwrap();
        assert_eq!(
            recovered.current_state,
            SessionState::AwaitingConfirmation,
            "recovery must reflect the last persisted state"
        );
    }

    #[tokio::test]
    async fn cold_start_empty_db_recovers_zero() {
        let backing = MemoryStore::new();
        let store = RocksSessionStore::new(backing);
        let result = store.recover().unwrap();
        assert_eq!(result.total, 0);
        assert_eq!(result.active, 0);
        assert_eq!(result.terminal, 0);
        assert!(store.is_empty());
    }
}
