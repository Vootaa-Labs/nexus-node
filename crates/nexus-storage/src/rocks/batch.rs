//! [`RocksWriteBatch`] — atomic write batch for the RocksDB backend.

use crate::traits::WriteBatchOps;

/// A write batch that accumulates mutations before atomic commit.
///
/// Created via [`RocksStore::new_batch`](super::RocksStore::new_batch).
pub struct RocksWriteBatch {
    /// Ordered list of (cf_name, key, optional_value).
    /// `None` value = delete, `Some` = put.
    pub(super) ops: Vec<(String, Vec<u8>, Option<Vec<u8>>)>,
}

impl RocksWriteBatch {
    pub(super) fn new() -> Self {
        Self { ops: Vec::new() }
    }
}

impl WriteBatchOps for RocksWriteBatch {
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> &mut Self {
        // Default CF — callers use `RocksStore::put_cf` helper methods that
        // internally build a batch with the correct CF name. For the trait
        // interface, we use the state CF as default.
        self.ops.push(("cf_state".to_owned(), key, Some(value)));
        self
    }

    fn delete(&mut self, key: Vec<u8>) -> &mut Self {
        self.ops.push(("cf_state".to_owned(), key, None));
        self
    }

    fn put_cf(&mut self, cf_name: &str, key: Vec<u8>, value: Vec<u8>) -> &mut Self {
        self.ops.push((cf_name.to_owned(), key, Some(value)));
        self
    }

    fn delete_cf(&mut self, cf_name: &str, key: Vec<u8>) -> &mut Self {
        self.ops.push((cf_name.to_owned(), key, None));
        self
    }

    fn size_hint(&self) -> usize {
        self.ops.len()
    }
}
