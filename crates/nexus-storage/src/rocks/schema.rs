// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! RocksDB column family definitions (**FROZEN-2**).
//!
//! All column families are created at DB-open time. Adding or removing
//! a CF requires an RFC and a data migration plan.

use crate::types::ColumnFamily;

/// Default RocksDB options for a column family.
pub(crate) fn cf_options(cf: ColumnFamily) -> rocksdb::Options {
    let mut opts = rocksdb::Options::default();
    match cf {
        ColumnFamily::Blocks | ColumnFamily::Transactions | ColumnFamily::State => {
            // LZ4 compression for fast decompression on reads.
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        }
        ColumnFamily::Receipts => {
            // Zstd for better compression ratio (receipts are write-heavy, read-less-often).
            opts.set_compression_type(rocksdb::DBCompressionType::Zstd);
        }
        ColumnFamily::Certificates => {
            // LZ4 — certificates are pruned by epoch retention.
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        }
        ColumnFamily::Batches => {
            // LZ4 — batches are removed after execution completes.
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        }
        ColumnFamily::Sessions => {
            // LZ4 — sessions are small and often read during recovery.
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        }
        ColumnFamily::Provenance => {
            // Zstd — provenance is write-heavy audit data, good compression ratio.
            opts.set_compression_type(rocksdb::DBCompressionType::Zstd);
        }
        ColumnFamily::CommitmentMeta
        | ColumnFamily::CommitmentLeaves
        | ColumnFamily::CommitmentNodes => {
            // LZ4 — commitment metadata and node pages are latency-sensitive.
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        }
        ColumnFamily::HtlcLocks => {
            // LZ4 — HTLC locks are small and latency-sensitive for claim/refund.
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        }
        ColumnFamily::BlockTxIndex => {
            // LZ4 — block-tx index entries are small and read-latency-sensitive.
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        }
        ColumnFamily::Events => {
            // LZ4 — event entries are small and latency-sensitive for queries.
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        }
    }
    opts
}

/// Return the ordered list of CF descriptors for DB initialization.
pub(crate) fn all_cf_descriptors() -> Vec<rocksdb::ColumnFamilyDescriptor> {
    ColumnFamily::all()
        .iter()
        .map(|cf| rocksdb::ColumnFamilyDescriptor::new(cf.as_str(), cf_options(*cf)))
        .collect()
}
