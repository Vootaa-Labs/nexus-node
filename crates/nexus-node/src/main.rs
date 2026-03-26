//! `nexus-node` — Nexus validator node entry point.
//!
//! This is a **thin assembly binary**: it parses configuration, initialises
//! the tracing subscriber, and wires all subsystem handles together.
//! Business logic lives in the domain crates, not here.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use nexus_config::NodeConfig;
use nexus_consensus::ValidatorRegistry;
use nexus_intent::{RocksProvenanceStore, RocksSessionStore};
use nexus_network::NetworkService;
use nexus_node::backends::{
    GossipBroadcaster, LiveConsensusBackend, LiveIntentBackend, LiveNetworkBackend,
    LiveSessionProvenanceBackend, McpDispatchBackend, SharedChainHead, StorageQueryBackend,
    StorageStateView,
};
use nexus_node::batch_proposer::BatchProposerConfig;
use nexus_node::batch_store::BatchStore;
use nexus_node::execution_bridge::ExecutionBridgeConfig;
use nexus_node::genesis_boot;
use nexus_node::mempool::{Mempool, MempoolConfig};
use nexus_node::readiness::NodeReadiness;
use nexus_node::validator_discovery;
use nexus_primitives::ValidatorIndex;
use nexus_rpc::RpcService;
use nexus_storage::RocksStore;
use tracing_subscriber::EnvFilter;

fn resolve_local_validator_index(
    committee: &nexus_consensus::Committee,
    verify_key: &nexus_crypto::falcon::FalconVerifyKey,
) -> anyhow::Result<ValidatorIndex> {
    committee
        .active_validators()
        .into_iter()
        .find(|validator| validator.falcon_pub_key.as_bytes() == verify_key.as_bytes())
        .map(|validator| validator.index)
        .ok_or_else(|| anyhow::anyhow!("local Falcon verify key not found in active committee"))
}

fn seed_genesis_certificates(
    engine: &mut nexus_consensus::ConsensusEngine,
) -> anyhow::Result<usize> {
    let epoch = engine.epoch();
    let round = nexus_primitives::RoundNumber(0);
    let validator_count = engine.committee().active_validators().len() as u32;
    let validator_indices: Vec<_> = engine
        .committee()
        .active_validators()
        .iter()
        .map(|validator| validator.index)
        .collect();

    for validator_index in &validator_indices {
        let payload = bcs::to_bytes(&(epoch, validator_index, round))
            .context("failed to serialize genesis certificate seed")?;
        let batch_digest =
            nexus_crypto::Blake3Hasher::digest(nexus_consensus::types::BATCH_DOMAIN, &payload);
        let cert_digest = nexus_consensus::compute_cert_digest(
            epoch,
            &batch_digest,
            *validator_index,
            round,
            &[],
        )
        .context("failed to compute genesis certificate digest")?;

        engine
            .insert_verified_certificate(nexus_consensus::NarwhalCertificate {
                epoch,
                batch_digest,
                origin: *validator_index,
                round,
                parents: Vec::new(),
                signatures: Vec::new(),
                signers: nexus_consensus::ValidatorBitset::new(validator_count),
                cert_digest,
            })
            .context("failed to seed genesis certificate into consensus engine")?;
    }

    Ok(validator_indices.len())
}

fn main() -> anyhow::Result<()> {
    // ── 1. Load configuration ───────────────────────────────────────────
    let config = if let Some(path) = std::env::args().nth(1) {
        NodeConfig::load(Some(std::path::Path::new(&path))).context("failed to load node config")?
    } else {
        tracing::info!("no config file specified, using defaults");
        NodeConfig::default()
    };

    // ── 2. Validate configuration ────────────────────────────────────────
    config
        .rpc
        .validate()
        .map_err(|e| anyhow::anyhow!("RPC config validation failed: {e}"))?;

    // ── 2a. Fail-closed: require genesis in production mode ─────────────
    if config.genesis_path.is_none() && !config.dev_mode {
        anyhow::bail!(
            "genesis_path is required when dev_mode is false. \
             Set dev_mode = true in your config or NEXUS_DEV_MODE=1 for local development."
        );
    }

    // ── 3. Initialise tracing ───────────────────────────────────────────
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.telemetry.log_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        rest_addr = %config.rpc.rest_listen_addr,
        "starting nexus-node"
    );

    // ── 4. Build tokio runtime and run ──────────────────────────────────
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    rt.block_on(run(config))
}

async fn run(config: NodeConfig) -> anyhow::Result<()> {
    // ── Readiness tracker ───────────────────────────────────────────────
    let readiness = NodeReadiness::new();

    if config.dev_mode {
        tracing::warn!(
            "running in DEV MODE — empty committee allowed, NOT suitable for production"
        );
    }

    // ── 5. Initialise storage ───────────────────────────────────────────
    let store = RocksStore::open(&config.storage).context("failed to open RocksDB storage")?;
    tracing::info!(path = %config.storage.rocksdb_path.display(), "storage initialised (RocksDB)");
    readiness.storage_handle().set_ready();

    // ── 5a. Initialise session & provenance stores ──────────────────────
    let session_store = Arc::new(RocksSessionStore::new(store.clone()));
    let provenance_store = Arc::new(RocksProvenanceStore::new(store.clone()));

    // Recover sessions from previous run.
    let session_recovery = match session_store.recover() {
        Ok(result) => {
            tracing::info!(
                total = result.total,
                active = result.active,
                terminal = result.terminal,
                "session recovery complete"
            );
            nexus_node::startup_report::RecoveryOutcome {
                success: true,
                count: result.total,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "session recovery failed (starting with empty session store)");
            nexus_node::startup_report::RecoveryOutcome {
                success: false,
                count: 0,
            }
        }
    };

    // Recover provenance record count.
    let provenance_recovery = match provenance_store.recover_count() {
        Ok(count) => {
            tracing::info!(records = count, "provenance recovery complete");
            nexus_node::startup_report::RecoveryOutcome {
                success: true,
                count,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "provenance count recovery failed");
            nexus_node::startup_report::RecoveryOutcome {
                success: false,
                count: 0,
            }
        }
    };

    // ── 5. Load genesis and initialise consensus engine ─────────────────
    let genesis_already_applied = genesis_boot::is_genesis_applied(&store).unwrap_or(false);

    let (committee, num_shards, chain_id_str, genesis_outcome) = if let Some(ref genesis_path) =
        config.genesis_path
    {
        tracing::info!(path = %genesis_path.display(), "loading genesis");
        let boot =
            genesis_boot::boot_from_genesis(genesis_path, &store, nexus_primitives::ShardId(0))
                .context("genesis boot failed")?;
        tracing::info!(
            chain_id = %boot.chain_id,
            validators = boot.committee.active_validators().len(),
            shards = boot.num_shards,
            "genesis loaded"
        );

        // ── 5a. Validate chain identity (genesis hash + chain ID) ───
        //  Reads the genesis file and compares its hash against the stored
        //  marker in the data directory. Fails fast on mismatch to prevent
        //  split-brain. On first boot, writes the marker for future checks.
        let data_dir = &config.storage.rocksdb_path;
        if !data_dir.exists() {
            std::fs::create_dir_all(data_dir)
                .context("failed to create data directory for chain identity")?;
        }
        let content = std::fs::read_to_string(genesis_path)
            .context("failed to re-read genesis for identity check")?;
        let genesis: nexus_config::genesis::GenesisConfig =
            serde_json::from_str(&content).context("failed to parse genesis for identity check")?;
        let genesis_hash = nexus_node::chain_identity::validate_chain_identity(data_dir, &genesis)
            .context("chain identity validation failed")?;
        tracing::info!(
            genesis_hash = hex::encode(genesis_hash.as_bytes()),
            chain_id = %boot.chain_id,
            "chain identity validated"
        );

        readiness.genesis_handle().set_ready();
        let outcome = if genesis_already_applied {
            nexus_node::startup_report::GenesisOutcome::AlreadyApplied
        } else {
            nexus_node::startup_report::GenesisOutcome::Applied
        };
        (boot.committee, boot.num_shards, boot.chain_id, outcome)
    } else {
        // dev_mode guard enforced in main(); reaching here means dev_mode is true.
        tracing::warn!("no genesis file — starting with empty committee (dev mode)");
        let committee =
            nexus_consensus::Committee::new(nexus_primitives::EpochNumber(0), Vec::new())
                .context("failed to create empty committee")?;
        readiness.genesis_handle().set_ready();
        (
            committee,
            config.execution.shard_count,
            "nexus-devnet".to_owned(),
            nexus_node::startup_report::GenesisOutcome::Skipped,
        )
    };

    // ── 5b. Build network service ───────────────────────────────────────
    let (net_handle, net_service) =
        NetworkService::build(&config.network).context("failed to build network service")?;
    let net_shutdown_handle = net_handle.transport.clone();
    tracing::info!("network service built");

    // ── 6. Spawn execution services (one per shard) ───────────────────
    let mut shard_exec_handles = std::collections::HashMap::new();
    let mut shard_chain_heads = std::collections::HashMap::new();
    for shard_idx in 0..num_shards {
        let shard_id = nexus_primitives::ShardId(shard_idx);
        let state_view = StorageStateView::new(store.clone(), shard_id);
        let handle = nexus_execution::spawn_execution_service(
            config.execution.clone(),
            shard_id,
            Arc::new(state_view),
        );
        shard_exec_handles.insert(shard_id, handle);
        shard_chain_heads.insert(shard_id, SharedChainHead::new());
        tracing::info!(shard = shard_id.0, "execution service spawned");
    }
    let shard_router = nexus_node::execution_bridge::ShardRouter::new(shard_exec_handles);
    tracing::info!(shards = num_shards, "all execution services spawned");
    readiness.execution_handle().set_ready();

    // ── 6b. Create mempool (shard-partitioned) ────────────────────────
    let mempool = Arc::new(Mempool::new(&MempoolConfig {
        num_shards,
        ..MempoolConfig::default()
    }));
    tracing::info!(capacity = 10_000, num_shards, "shard-aware mempool created");

    // ── 6c. Create batch store (with disk persistence) ─────────────────
    let batch_persist = nexus_node::batch_persist::BatchPersistence::new(store.clone());
    let batch_store = Arc::new(BatchStore::new_with_persistence(Box::new(batch_persist)));
    let restored_batches = batch_store.restore_from_disk();
    if restored_batches > 0 {
        tracing::info!(
            count = restored_batches,
            "batch store restored from disk (cold restart)"
        );
    } else {
        tracing::info!("batch store created (empty)");
    }

    // ── 6d. Load or generate validator signing key ────────────────────
    let key_pair = if let Some(ref key_path) = config.validator_key_path {
        tracing::info!(path = %key_path.display(), "loading validator keys from persistent storage");
        nexus_node::validator_keys::load_validator_keys(key_path)
            .context("failed to load validator keys")?
    } else {
        tracing::warn!("no validator_key_path configured — using ephemeral dev signing key");
        tracing::warn!("this is NOT suitable for production or devnet deployments");
        nexus_node::validator_keys::generate_dev_keys()
    };
    let local_validator_index = if committee.active_validators().is_empty() {
        ValidatorIndex(0)
    } else {
        resolve_local_validator_index(&committee, &key_pair.verify_key)
            .context("failed to resolve local validator index from committee")?
    };
    let committee_size = committee.active_validators().len();
    let dev_signing_key = Arc::new(key_pair.signing_key);
    tracing::info!(
        validator = local_validator_index.0,
        "validator signing key ready"
    );

    // ── DAG persistence layer ─────────────────────────────────────────
    let dag_persist = nexus_consensus::DagPersistence::new(store.clone());

    // Attempt cold-restart restore from cf_certificates.
    let restored_certs = dag_persist.restore_certificates().unwrap_or_else(|e| {
        tracing::warn!("DAG restore failed, starting fresh: {e}");
        Vec::new()
    });

    let mut engine = nexus_consensus::ConsensusEngine::new_with_persistence_and_retention(
        nexus_primitives::EpochNumber(0),
        committee,
        Box::new(dag_persist),
        config.storage.epoch_retention_count,
    );

    if restored_certs.is_empty() {
        let seeded_genesis_certs = seed_genesis_certificates(&mut engine)
            .context("failed to seed genesis certificates into consensus engine")?;
        tracing::info!(
            count = seeded_genesis_certs,
            "seeded genesis certificates into consensus DAG"
        );
    } else {
        let count = restored_certs.len();
        for cert in restored_certs {
            engine
                .insert_verified_certificate(cert)
                .context("failed to replay restored certificate")?;
        }
        tracing::info!(count, "restored DAG certificates from disk (cold restart)");
    }

    let engine = Arc::new(std::sync::Mutex::new(engine));
    tracing::info!("consensus engine initialised");

    // ── Epoch manager ───────────────────────────────────────────────────
    let epoch_config = nexus_consensus::EpochConfig::default();
    let epoch_manager = if genesis_already_applied {
        // Cold restart: try to recover epoch state from storage.
        match nexus_node::epoch_store::load_epoch_state(&store) {
            Ok(Some(state)) => {
                tracing::info!(
                    epoch = state.epoch.0,
                    transitions = state.transitions.len(),
                    "epoch state recovered from storage"
                );
                // Validate election result consistency (R-3).
                if let Some(ref er) = state.election_result {
                    if er.for_epoch == state.epoch {
                        let committee_size = state.committee.active_validators().len();
                        let elected_count = er.elected.len();
                        if elected_count == committee_size {
                            tracing::info!(
                                epoch = state.epoch.0,
                                elected = elected_count,
                                total_stake = er.total_effective_stake,
                                is_fallback = er.is_fallback,
                                "election result validated: committee size matches"
                            );
                        } else {
                            tracing::warn!(
                                epoch = state.epoch.0,
                                elected = elected_count,
                                committee_size = committee_size,
                                "election result size mismatch (non-fatal — committee from storage is authoritative)"
                            );
                        }
                    } else {
                        tracing::debug!(
                            election_epoch = er.for_epoch.0,
                            current_epoch = state.epoch.0,
                            "election result is for a different epoch (carry-forward interval)"
                        );
                    }
                }
                nexus_consensus::EpochManager::recover(
                    epoch_config,
                    state.epoch,
                    state.epoch_started_at,
                    state.transitions,
                )
            }
            Ok(None) => {
                tracing::info!("no persisted epoch state found, starting at epoch 0");
                nexus_consensus::EpochManager::new(epoch_config, nexus_primitives::EpochNumber(0))
            }
            Err(e) => {
                tracing::warn!(error = %e, "epoch state recovery failed, starting at epoch 0");
                nexus_consensus::EpochManager::new(epoch_config, nexus_primitives::EpochNumber(0))
            }
        }
    } else {
        // First boot: persist initial epoch.
        let mgr =
            nexus_consensus::EpochManager::new(epoch_config, nexus_primitives::EpochNumber(0));
        let initial_committee = {
            let eng = engine
                .lock()
                .expect("consensus engine lock poisoned at first-boot init");
            eng.committee().clone()
        };
        if let Err(e) = nexus_node::epoch_store::persist_initial_epoch(&store, &initial_committee) {
            tracing::warn!(error = %e, "failed to persist initial epoch state (non-fatal)");
        } else {
            tracing::info!("initial epoch state persisted to storage");
        }
        mgr
    };
    let epoch_manager = Arc::new(std::sync::Mutex::new(epoch_manager));
    readiness.consensus_handle().set_ready();

    // ── 7. Build real backend adapters ───────────────────────────────────
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let commit_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));

    // RPC uses the shard-0 chain head for backward-compatible /v2/chain/head.
    let rpc_chain_head = shard_chain_heads
        .get(&nexus_primitives::ShardId(0))
        .cloned()
        .unwrap_or_else(SharedChainHead::new);

    let query_backend = Arc::new(
        StorageQueryBackend::new(
            store.clone(),
            nexus_primitives::ShardId(0),
            epoch.clone(),
            commit_seq.clone(),
        )
        .with_peer_count({
            let disc = net_handle.discovery.clone();
            Arc::new(move || disc.routing_health().known_peers)
        })
        .with_chain_head(rpc_chain_head)
        .with_readiness(readiness.clone())
        .with_gas_budget(config.rpc.query_gas_budget),
    );

    let mut consensus_backend = LiveConsensusBackend::new(engine)
        .with_epoch_manager(epoch_manager.clone())
        .with_store(store.clone());
    let engine_handle = consensus_backend.engine(); // shared with consensus bridge

    // ── 8. Spawn intent service ─────────────────────────────────────────
    let resolver = Arc::new(nexus_intent::AccountResolverImpl::new(num_shards));
    let intent_config = nexus_intent::IntentConfig::default();
    let compiler = nexus_intent::IntentCompilerImpl::new(intent_config.clone());
    let intent_handle = nexus_intent::IntentService::spawn(compiler, 256);
    let intent_backend = LiveIntentBackend::new(intent_handle, resolver.clone());

    let mcp_compiler = Arc::new(nexus_intent::IntentCompilerImpl::new(intent_config));
    let mcp_planner = nexus_intent::agent_core::intent_planner_bridge::IntentPlannerBridge::new(
        mcp_compiler,
        resolver.clone(),
    );
    let ace_dispatcher: Arc<dyn nexus_intent::agent_core::dispatcher::DispatchBackend> =
        Arc::new(nexus_intent::agent_core::engine::AgentCoreEngine::new(
            mcp_planner,
            nexus_intent::agent_core::policy::ConfirmationThreshold::default(),
        ));
    tracing::info!("intent service spawned");

    // ── 9. Build RPC service ────────────────────────────────────────────
    let (events_tx, _events_rx) = nexus_rpc::event_channel();
    let events_tx_bridge = events_tx.clone(); // clone for execution bridge

    // Create intent lifecycle tracker (shared with RPC + watcher)
    let intent_tracker = Arc::new(nexus_rpc::IntentTracker::new());

    // Benchmark lifecycle tracker – bounded in-memory LRU for tx latency observation.
    let tx_lifecycle = Arc::new(nexus_rpc::TxLifecycleRegistry::new(100_000));

    // Create commitment tracker early so it can be shared with both
    // the RPC layer (proof queries) and the execution bridge (state updates).
    let commitment_tracker = nexus_node::commitment_tracker::new_shared_tracker_with_persistence(
        store.clone(),
        config.storage.commitment_cache_size,
    )
    .context("failed to initialise persistent commitment tracker")?;

    // Install Prometheus metrics recorder so metrics::counter! et al.
    // are captured and can be scraped via GET /metrics.
    let prom_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus metrics recorder");

    let tx_broadcaster = GossipBroadcaster::new(net_handle.gossip.clone());
    let network_backend = LiveNetworkBackend::new(net_handle.discovery.clone());
    let session_provenance_backend = Arc::new(LiveSessionProvenanceBackend::new(
        session_store.clone(),
        provenance_store.clone(),
    ));
    let mcp_dispatcher: Arc<dyn nexus_intent::agent_core::dispatcher::DispatchBackend> =
        Arc::new(McpDispatchBackend::new(
            ace_dispatcher,
            query_backend.clone(),
            session_provenance_backend.clone(),
            Some(Arc::clone(&intent_tracker)),
        ));

    let mut builder = RpcService::builder(config.rpc.rest_listen_addr)
        .query_backend(query_backend)
        .tx_broadcaster(Arc::new(tx_broadcaster))
        .network_backend(Arc::new(network_backend))
        .event_sender(events_tx.clone())
        .metrics_handle(prom_handle)
        .faucet(config.rpc.faucet_enabled, config.rpc.faucet_amount)
        .api_keys(config.rpc.api_keys.clone())
        .mcp_dispatcher(mcp_dispatcher)
        .quota_manager(Arc::new(
            nexus_rpc::middleware::QuotaManager::new_per_class(
                [
                    config.rpc.query_rate_limit_anonymous_rpm,
                    config.rpc.query_rate_limit_authenticated_rpm,
                    config.rpc.query_rate_limit_whitelisted_rpm,
                ],
                [
                    config.rpc.intent_rate_limit_anonymous_rpm,
                    config.rpc.intent_rate_limit_authenticated_rpm,
                    config.rpc.intent_rate_limit_whitelisted_rpm,
                ],
                [
                    config.rpc.mcp_rate_limit_anonymous_rpm,
                    config.rpc.mcp_rate_limit_authenticated_rpm,
                    config.rpc.mcp_rate_limit_whitelisted_rpm,
                ],
                config.rpc.whitelisted_api_keys.clone(),
            ),
        ))
        .query_gas_budget(config.rpc.query_gas_budget)
        .query_timeout_ms(config.rpc.query_timeout_ms)
        .num_shards(num_shards)
        .tx_lifecycle(Arc::clone(&tx_lifecycle))
        .faucet_per_addr_limit(config.rpc.faucet_per_addr_limit_per_hour)
        .cors_allowed_origins(config.rpc.cors_allowed_origins.clone())
        .max_ws_connections(config.rpc.max_ws_connections)
        .intent_tracker(Arc::clone(&intent_tracker))
        .session_provenance(session_provenance_backend)
        .state_proof(Arc::new(nexus_node::backends::LiveStateProofBackend::new(
            commitment_tracker.clone(),
        )));

    if config.rpc.rate_limit_enabled {
        builder = builder.rate_limit(
            config.rpc.rate_limit_per_ip_rps,
            std::time::Duration::from_secs(1),
        );
    }

    // ── 9b. Build validators identity registry + snapshot provider ──────
    let rotation_policy = nexus_node::staking_snapshot::CommitteeRotationPolicy::with_interval(
        config.consensus.validator_election_epoch_interval,
    );

    let identity_registry = if genesis_already_applied {
        match nexus_node::validator_identity::load_identity_registry(&store) {
            Ok(reg) if !reg.is_empty() => {
                tracing::info!(
                    identities = reg.len(),
                    "validator identity registry loaded from storage"
                );
                Arc::new(reg)
            }
            _ => {
                let reg = nexus_node::validator_identity::ValidatorIdentityRegistry::new();
                {
                    let eng = engine_handle
                        .lock()
                        .expect("consensus engine lock poisoned at identity registry init");
                    reg.seed_from_committee(eng.committee());
                }
                if let Err(e) =
                    nexus_node::validator_identity::persist_identity_registry(&store, &reg)
                {
                    tracing::warn!(error = %e, "failed to persist identity registry (non-fatal)");
                }
                tracing::info!(
                    identities = reg.len(),
                    "validator identity registry seeded from committee (cold restart fallback)"
                );
                Arc::new(reg)
            }
        }
    } else {
        let reg = nexus_node::validator_identity::ValidatorIdentityRegistry::new();
        {
            let eng = engine_handle
                .lock()
                .expect("consensus engine lock poisoned at identity registry init");
            reg.seed_from_committee(eng.committee());
        }
        if let Err(e) = nexus_node::validator_identity::persist_identity_registry(&store, &reg) {
            tracing::warn!(error = %e, "failed to persist identity registry (non-fatal)");
        }
        tracing::info!(
            identities = reg.len(),
            "validator identity registry initialised from genesis"
        );
        Arc::new(reg)
    };

    let snapshot_provider = nexus_node::snapshot_provider::build_snapshot_provider(
        engine_handle.clone(),
        store.clone(),
        identity_registry,
    );

    // Wire rotation policy and snapshot provider into consensus backend.
    consensus_backend = consensus_backend
        .with_rotation_policy(rotation_policy.clone())
        .with_snapshot_provider(snapshot_provider.clone());

    builder = builder
        .intent_backend(Arc::new(intent_backend))
        .consensus_backend(Arc::new(consensus_backend));

    let service = builder.build();

    // ── 10. Spawn network service ───────────────────────────────────────
    let _net_task = tokio::spawn(net_service.run());
    tracing::info!("network service spawned (P2P event loop running)");

    // ── 10b. Validator discovery (T-7005) ───────────────────────────────
    let network_discovery = if let Some(genesis_path) = config.genesis_path.as_ref() {
        let content = std::fs::read_to_string(genesis_path)
            .context("failed to re-read genesis for validator discovery")?;
        let genesis: nexus_config::genesis::GenesisConfig =
            serde_json::from_str(&content).context("failed to parse genesis for discovery")?;

        let disc_result = validator_discovery::discover_validators(
            &net_handle.discovery,
            &genesis,
            &config.network,
        )
        .await
        .context("validator discovery failed")?;

        tracing::info!(
            validators = disc_result.validators_seeded,
            boot_nodes = disc_result.boot_nodes_added,
            bootstrap = disc_result.bootstrap_initiated,
            "validator discovery complete"
        );
        Some(nexus_node::startup_report::DiscoveryOutcome {
            validators_seeded: disc_result.validators_seeded,
            boot_nodes_added: disc_result.boot_nodes_added,
            bootstrap_initiated: disc_result.bootstrap_initiated,
        })
    } else {
        None
    };
    readiness.network_handle().set_ready();

    // ── 10c. Spawn consensus inbound bridge (T-7004) ────────────────────
    let _consensus_bridge = nexus_node::consensus_bridge::spawn_consensus_inbound_bridge(
        net_handle.gossip.clone(),
        engine_handle.clone(),
        epoch.clone(),
        readiness.consensus_handle(),
    )
    .await
    .context("failed to spawn consensus inbound bridge")?;
    tracing::info!("consensus inbound bridge spawned");

    // ── 10d. Spawn state sync service (T-7006) ─────────────────────────
    let _state_sync = nexus_node::state_sync::spawn_state_sync_service(
        net_handle.gossip.clone(),
        net_handle.transport.clone(),
        store.clone(),
    )
    .await
    .context("failed to spawn state sync service")?;
    tracing::info!("state sync service spawned");

    // ── 10e. Spawn gossip → mempool bridge (T-9000) ─────────────────────
    let _gossip_bridge = nexus_node::gossip_bridge::spawn_gossip_mempool_bridge(
        net_handle.gossip.clone(),
        mempool.clone(),
        epoch.clone(),
    )
    .await
    .context("failed to spawn gossip→mempool bridge")?;
    tracing::info!("gossip→mempool bridge spawned");

    // ── 10f. Spawn cert aggregator + batch proposer (T-9002) ──────────
    let (proposal_tx, proposal_rx) = nexus_node::cert_aggregator::proposal_channel();

    let _cert_aggregator = nexus_node::cert_aggregator::spawn_cert_aggregator(
        net_handle.gossip.clone(),
        engine_handle.clone(),
        batch_store.clone(),
        Arc::clone(&dev_signing_key),
        local_validator_index,
        epoch.clone(),
        proposal_rx,
    )
    .await
    .context("failed to spawn cert aggregator")?;
    tracing::info!("cert aggregator spawned");

    let _batch_proposer = nexus_node::batch_proposer::spawn_batch_proposer(
        BatchProposerConfig {
            empty_proposal_interval: Duration::from_millis(
                config.consensus.empty_proposal_interval_ms,
            ),
            ..Default::default()
        },
        mempool.clone(),
        batch_store.clone(),
        engine_handle.clone(),
        local_validator_index,
        epoch.clone(),
        proposal_tx,
    );
    tracing::info!("batch proposer spawned");

    // ── 10g. Spawn execution bridge (T-9003) ────────────────────────────
    let commit_seq_maint = commit_seq.clone(); // clone for storage maintenance

    let _exec_bridge = nexus_node::execution_bridge::spawn_execution_bridge(
        ExecutionBridgeConfig {
            num_shards,
            ..Default::default()
        },
        engine_handle,
        nexus_node::execution_bridge::BridgeContext {
            shard_router,
            batch_store,
            store: store.clone(),
            commit_seq,
            events_tx: Some(events_tx_bridge),
            shard_chain_heads,
            provenance_store: Some(provenance_store.clone()),
            commitment_tracker: Some(commitment_tracker.clone()),
        },
        nexus_node::execution_bridge::EpochContext {
            epoch_manager: Some(epoch_manager),
            epoch_counter: Some(epoch.clone()),
            rotation_policy: Some(rotation_policy),
            staking_snapshot_provider: Some(snapshot_provider),
        },
        readiness.execution_handle(),
    );
    tracing::info!(
        num_shards,
        "execution bridge spawned (multi-shard + commitment tracker + epoch manager + rotation policy)"
    );

    // ── 10g-b. Spawn provenance anchor batch task ───────────────────────
    let _anchor_batch = nexus_node::anchor_batch::spawn_anchor_batch_task(
        nexus_node::anchor_batch::AnchorBatchConfig::from_chain_id(&chain_id_str),
        provenance_store.clone(),
        mempool,
    );
    tracing::info!("provenance anchor batch task spawned");

    // ── 10h. Spawn intent lifecycle watcher ─────────────────────────────
    let _intent_watcher = nexus_node::intent_watcher::spawn_intent_watcher(
        events_tx.subscribe(),
        intent_tracker,
        Some(events_tx.clone()),
    );
    tracing::info!("intent watcher spawned");

    // ── 10i. Spawn session/provenance cleanup task ──────────────────────
    let _cleanup_task = nexus_node::session_cleanup::spawn_cleanup_task(
        nexus_node::session_cleanup::CleanupConfig::default(),
        session_store,
        provenance_store,
    );
    tracing::info!("session/provenance cleanup task spawned");

    // ── 10j. Spawn storage maintenance task (P5-2/P5-3) ────────────────
    let _storage_maint = nexus_node::storage_maintenance::spawn_storage_maintenance(
        nexus_node::storage_maintenance::StorageMaintenanceConfig::from_storage_config(
            &config.storage,
        ),
        store.clone(),
        commit_seq_maint,
        readiness.storage_handle(),
    );
    tracing::info!("storage maintenance task spawned");

    // net_handle kept alive for subsystem bridges
    let _net_handle = net_handle;

    // ── Startup report ──────────────────────────────────────────────────
    let startup_report = nexus_node::startup_report::StartupReport {
        version: env!("CARGO_PKG_VERSION"),
        dev_mode: config.dev_mode,
        chain_id: chain_id_str,
        genesis: genesis_outcome,
        committee_size,
        local_validator_index: local_validator_index.0,
        num_shards,
        storage_path: config.storage.rocksdb_path.display().to_string(),
        session_recovery,
        provenance_recovery,
        network_discovery,
        readiness_status: readiness.status().as_str(),
        proof_backend: nexus_node::startup_report::ProofBackendStatus {
            enabled: true,
            rpc_registered: true,
            execution_bridge_connected: true,
        },
    };
    startup_report.log();

    tracing::info!("all subsystems wired — starting RPC server");

    service
        .serve_until_ctrl_c()
        .await
        .context("RPC server error")?;

    // ── 11. Graceful shutdown ───────────────────────────────────────────
    tracing::info!("RPC server stopped — shutting down network");
    if let Err(e) = net_shutdown_handle.shutdown().await {
        tracing::warn!(error = %e, "network shutdown returned error");
    }

    tracing::info!("nexus-node shutdown complete");
    Ok(())
}
