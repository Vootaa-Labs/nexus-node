//! BLAKE3-256 hasher with mandatory domain separation.
//!
//! Implements the [`CryptoHasher`] trait using BLAKE3 as the underlying
//! hash function. All operations prepend a length-prefixed domain separator
//! to prevent cross-protocol hash collisions.

use nexus_primitives::Blake3Digest;

use crate::traits::CryptoHasher;

/// BLAKE3-256 hasher implementing [`CryptoHasher`].
///
/// Domain separation encoding:
/// ```text
/// [domain_len: u32 LE] ‖ [domain bytes] ‖ [data]
/// ```
#[derive(Clone)]
pub struct Blake3Hasher {
    inner: blake3::Hasher,
}

impl Blake3Hasher {
    /// One-shot convenience: hash `data` under `domain`.
    #[inline]
    pub fn digest(domain: &[u8], data: &[u8]) -> Blake3Digest {
        <Self as CryptoHasher>::hash(domain, data)
    }
}

impl CryptoHasher for Blake3Hasher {
    type Output = Blake3Digest;

    fn hash(domain: &[u8], data: &[u8]) -> Blake3Digest {
        let mut hasher = blake3::Hasher::new();
        // Length-prefixed domain separator prevents ambiguity
        hasher.update(&(domain.len() as u32).to_le_bytes());
        hasher.update(domain);
        hasher.update(data);
        Blake3Digest::from_bytes(*hasher.finalize().as_bytes())
    }

    fn new_with_domain(domain: &[u8]) -> Self {
        let mut inner = blake3::Hasher::new();
        inner.update(&(domain.len() as u32).to_le_bytes());
        inner.update(domain);
        Self { inner }
    }

    fn update(&mut self, data: &[u8]) -> &mut Self {
        self.inner.update(data);
        self
    }

    fn finalize(self) -> Blake3Digest {
        Blake3Digest::from_bytes(*self.inner.finalize().as_bytes())
    }

    fn merkle_root(leaves: &[Blake3Digest]) -> Blake3Digest {
        const MERKLE_DOMAIN: &[u8] = b"nexus::merkle::node::v1";

        match leaves.len() {
            0 => Blake3Digest::ZERO,
            1 => leaves[0],
            _ => {
                // Pad to even count by duplicating the last leaf
                let mut current: Vec<Blake3Digest> = leaves.to_vec();
                while current.len() > 1 {
                    if current.len() % 2 != 0 {
                        let last = current[current.len() - 1];
                        current.push(last);
                    }
                    current = current
                        .chunks(2)
                        .map(|pair| {
                            let mut hasher = blake3::Hasher::new();
                            hasher.update(&(MERKLE_DOMAIN.len() as u32).to_le_bytes());
                            hasher.update(MERKLE_DOMAIN);
                            hasher.update(pair[0].as_bytes());
                            hasher.update(pair[1].as_bytes());
                            Blake3Digest::from_bytes(*hasher.finalize().as_bytes())
                        })
                        .collect();
                }
                current[0]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains;

    #[test]
    fn one_shot_deterministic() {
        let d1 = Blake3Hasher::digest(domains::USER_TX, b"hello");
        let d2 = Blake3Hasher::digest(domains::USER_TX, b"hello");
        assert_eq!(d1, d2);
    }

    #[test]
    fn different_domains_produce_different_digests() {
        let d1 = Blake3Hasher::digest(domains::USER_TX, b"same data");
        let d2 = Blake3Hasher::digest(domains::NARWHAL_BATCH, b"same data");
        assert_ne!(d1, d2, "different domains must yield different digests");
    }

    #[test]
    fn incremental_matches_one_shot() {
        let one_shot = Blake3Hasher::digest(domains::USER_TX, b"helloworld");
        let incremental = {
            let mut h = Blake3Hasher::new_with_domain(domains::USER_TX);
            h.update(b"hello").update(b"world");
            h.finalize()
        };
        assert_eq!(one_shot, incremental);
    }

    #[test]
    fn merkle_root_empty() {
        let root = Blake3Hasher::merkle_root(&[]);
        assert_eq!(root, Blake3Digest::ZERO);
    }

    #[test]
    fn merkle_root_single_leaf() {
        let leaf = Blake3Hasher::digest(b"test", b"leaf");
        assert_eq!(Blake3Hasher::merkle_root(&[leaf]), leaf);
    }

    #[test]
    fn merkle_root_two_leaves() {
        let l1 = Blake3Hasher::digest(b"t", b"a");
        let l2 = Blake3Hasher::digest(b"t", b"b");
        let root = Blake3Hasher::merkle_root(&[l1, l2]);
        assert_ne!(root, l1);
        assert_ne!(root, l2);
        // Root should be deterministic
        assert_eq!(root, Blake3Hasher::merkle_root(&[l1, l2]));
    }

    #[test]
    fn merkle_root_odd_count_deterministic() {
        let leaves: Vec<_> = (0u8..5).map(|i| Blake3Hasher::digest(b"t", &[i])).collect();
        let r1 = Blake3Hasher::merkle_root(&leaves);
        let r2 = Blake3Hasher::merkle_root(&leaves);
        assert_eq!(r1, r2);
    }

    #[test]
    fn empty_data_hashes_to_non_zero() {
        let d = Blake3Hasher::digest(domains::USER_TX, b"");
        assert_ne!(d, Blake3Digest::ZERO);
    }
}
