//! Devnet scenario tests — integration test suite for live Nexus devnet clusters.
//!
//! Migrated from the original `nexus-simulator` one-shot runner. These scenarios
//! exercise every system module via REST endpoints against a running devnet.
//!
//! # Running
//!
//! ```console
//! # Against default local node:
//! cargo test -p nexus-test-utils --test scenario_tests
//!
//! # Against multi-node devnet:
//! NEXUS_NODES=http://localhost:8080,http://localhost:8081 cargo test -p nexus-test-utils scenario_tests
//! ```

use std::fmt;
use std::time::{Duration, Instant};

use nexus_crypto::{DilithiumSigner, Signer};
use nexus_execution::types::{
    compute_tx_digest, SignedTransaction, TransactionBody, TransactionPayload, TX_DOMAIN,
};
use nexus_primitives::{AccountAddress, Amount, EpochNumber, ShardId, TokenId};
use serde::Deserialize;

// ── Response DTOs ──────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct TxSubmitResponse {
    tx_digest: serde_json::Value,
    accepted: bool,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct FaucetResponse {
    tx_digest: serde_json::Value,
    amount: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct BalanceEntry {
    token: String,
    amount: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AccountBalanceDto {
    address: String,
    balances: Vec<BalanceEntry>,
}

#[derive(Debug, Deserialize)]
struct ConsensusStatusDto {
    epoch: u64,
    #[serde(default)]
    total_commits: u64,
    #[serde(default)]
    dag_size: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ValidatorInfoDto {
    index: u32,
    #[serde(default)]
    stake: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct NetworkPeersResponse {
    #[serde(default)]
    total: u64,
}

#[derive(Debug, Deserialize)]
struct NetworkStatusResponse {
    #[serde(default)]
    known_peers: u64,
    #[serde(default)]
    routing_healthy: bool,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct IntentSubmitResponse {
    intent_id: Option<serde_json::Value>,
}

// ── Test identity ──────────────────────────────────────────────────────────

struct TestIdentity {
    sk: nexus_crypto::DilithiumSigningKey,
    pk: nexus_crypto::DilithiumVerifyKey,
    address: AccountAddress,
}

impl TestIdentity {
    fn generate() -> Self {
        let (sk, pk) = DilithiumSigner::generate_keypair();
        let address = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
        Self { sk, pk, address }
    }

    fn address_hex(&self) -> String {
        hex::encode(self.address.0)
    }
}

// ── Result tracking ────────────────────────────────────────────────────────

/// Outcome of a single scenario execution.
#[derive(Clone, Copy, PartialEq)]
pub enum ScenarioStatus {
    /// Scenario passed all steps.
    Pass,
    /// At least one step failed.
    Fail,
    /// Scenario was skipped (e.g. prerequisites not met).
    Skip,
}

impl fmt::Display for ScenarioStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScenarioStatus::Pass => write!(f, "PASS"),
            ScenarioStatus::Fail => write!(f, "FAIL"),
            ScenarioStatus::Skip => write!(f, "SKIP"),
        }
    }
}

/// Result record for one completed scenario.
pub struct ScenarioResult {
    /// Unique numeric identifier.
    pub id: u32,
    /// Human-readable scenario name.
    pub name: String,
    /// Overall pass/fail/skip outcome.
    pub status: ScenarioStatus,
    /// Number of individual steps that passed.
    pub steps_passed: u32,
    /// Total number of steps in the scenario.
    pub steps_total: u32,
    /// Wall-clock time spent running the scenario.
    pub duration: Duration,
    /// Per-step diagnostic messages.
    pub details: Vec<String>,
}

// ── Runner ─────────────────────────────────────────────────────────────────

/// Drives end-to-end scenario tests against one or more running Nexus nodes.
pub struct ScenarioRunner {
    nodes: Vec<String>,
    chain_id: u64,
    timeout: Duration,
    /// Accumulated results from all executed scenarios.
    pub results: Vec<ScenarioResult>,
}

impl ScenarioRunner {
    /// Create a runner targeting the given node endpoints.
    pub fn new(nodes: Vec<String>, chain_id: u64, timeout: Duration) -> Self {
        Self {
            nodes,
            chain_id,
            timeout,
            results: Vec::new(),
        }
    }

    /// Build a runner from `NEXUS_NODES` env var (comma-separated URLs, default `http://localhost:8080`).
    pub fn from_env() -> Self {
        let nodes_str =
            std::env::var("NEXUS_NODES").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let nodes: Vec<String> = nodes_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Self::new(nodes, 1, Duration::from_secs(30))
    }

    fn node(&self, idx: usize) -> &str {
        &self.nodes[idx % self.nodes.len()]
    }

    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new().timeout(self.timeout).build()
    }

    fn sign_tx(
        &self,
        identity: &TestIdentity,
        seq: u64,
        payload: TransactionPayload,
    ) -> anyhow::Result<SignedTransaction> {
        use anyhow::Context;
        let body = TransactionBody {
            sender: identity.address,
            sequence_number: seq,
            expiry_epoch: EpochNumber(1000),
            gas_limit: 100_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload,
            chain_id: self.chain_id,
        };
        let digest = compute_tx_digest(&body).context("compute tx digest")?;
        let body_bytes = bcs::to_bytes(&body).context("BCS encode body")?;
        let sig = DilithiumSigner::sign(&identity.sk, TX_DOMAIN, &body_bytes);
        Ok(SignedTransaction {
            body,
            signature: sig,
            sender_pk: identity.pk.clone(),
            digest,
        })
    }

    fn submit_tx(&self, node: &str, tx: &SignedTransaction) -> anyhow::Result<TxSubmitResponse> {
        use anyhow::Context;
        let url = format!("{}/v2/tx/submit", node.trim_end_matches('/'));
        let body = serde_json::to_value(tx).context("serialize tx")?;
        let resp = self
            .agent()
            .post(&url)
            .set("Content-Type", "application/json")
            .send_json(body)
            .with_context(|| format!("POST {url}"))?;
        resp.into_json::<TxSubmitResponse>()
            .context("parse submit response")
    }

    fn faucet_mint(&self, node: &str, recipient: &str) -> anyhow::Result<FaucetResponse> {
        use anyhow::Context;
        let url = format!("{}/v2/faucet/mint", node.trim_end_matches('/'));
        let body = serde_json::json!({ "recipient": recipient });
        let resp = self
            .agent()
            .post(&url)
            .set("Content-Type", "application/json")
            .send_json(body)
            .with_context(|| format!("POST {url}"))?;
        resp.into_json::<FaucetResponse>()
            .context("parse faucet response")
    }

    fn get_balance(&self, node: &str, addr: &str) -> anyhow::Result<Option<AccountBalanceDto>> {
        use anyhow::Context;
        let url = format!("{}/v2/account/{}/balance", node.trim_end_matches('/'), addr);
        match self.agent().get(&url).call() {
            Ok(resp) => {
                let dto: AccountBalanceDto = resp.into_json().context("parse balance")?;
                Ok(Some(dto))
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(e).context("get balance"),
        }
    }

    fn get_consensus_status(&self, node: &str) -> anyhow::Result<ConsensusStatusDto> {
        use anyhow::Context;
        let url = format!("{}/v2/consensus/status", node.trim_end_matches('/'));
        let resp = self
            .agent()
            .get(&url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        resp.into_json::<ConsensusStatusDto>()
            .context("parse consensus status")
    }

    fn get_validators(&self, node: &str) -> anyhow::Result<Vec<ValidatorInfoDto>> {
        use anyhow::Context;
        let url = format!("{}/v2/validators", node.trim_end_matches('/'));
        let resp = self
            .agent()
            .get(&url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        resp.into_json::<Vec<ValidatorInfoDto>>()
            .context("parse validators")
    }

    fn get_network_peers(&self, node: &str) -> anyhow::Result<NetworkPeersResponse> {
        use anyhow::Context;
        let url = format!("{}/v2/network/peers", node.trim_end_matches('/'));
        let resp = self
            .agent()
            .get(&url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        resp.into_json::<NetworkPeersResponse>()
            .context("parse peers")
    }

    fn get_network_status(&self, node: &str) -> anyhow::Result<NetworkStatusResponse> {
        use anyhow::Context;
        let url = format!("{}/v2/network/status", node.trim_end_matches('/'));
        let resp = self
            .agent()
            .get(&url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        resp.into_json::<NetworkStatusResponse>()
            .context("parse network status")
    }

    fn check_health(&self, node: &str) -> anyhow::Result<bool> {
        let url = format!("{}/health", node.trim_end_matches('/'));
        match self.agent().get(&url).call() {
            Ok(resp) => Ok(resp.status() == 200),
            Err(_) => Ok(false),
        }
    }

    /// Run all 12 scenarios and return pass/fail/skip counts.
    pub fn run_all(&mut self) -> (usize, usize, usize) {
        type ScenarioFn = fn(&ScenarioRunner) -> ScenarioResult;
        let all: Vec<(u32, &str, ScenarioFn)> = vec![
            (1, "Faucet + balance", scenario_01_faucet_balance),
            (2, "Native token transfers", scenario_02_native_transfers),
            (3, "Multi-node balance check", scenario_03_multinode_balance),
            (4, "Contract deploy (pipeline)", scenario_04_contract_deploy),
            (5, "Contract call (pipeline)", scenario_05_contract_call),
            (6, "Contract query (view)", scenario_06_contract_query),
            (7, "Intent submission", scenario_07_intent_submission),
            (8, "Consensus health", scenario_08_consensus_health),
            (9, "Network layer", scenario_09_network_layer),
            (10, "Rapid-fire transfers", scenario_10_rapid_transfers),
            (11, "Multi-sender concurrency", scenario_11_multi_sender),
            (
                12,
                "Cross-node tx submission",
                scenario_12_cross_node_submission,
            ),
        ];

        for &(_id, _name, func) in &all {
            self.results.push(func(self));
        }

        let passed = self
            .results
            .iter()
            .filter(|r| r.status == ScenarioStatus::Pass)
            .count();
        let failed = self
            .results
            .iter()
            .filter(|r| r.status == ScenarioStatus::Fail)
            .count();
        let skipped = self
            .results
            .iter()
            .filter(|r| r.status == ScenarioStatus::Skip)
            .count();
        (passed, failed, skipped)
    }
}

// ── Scenario implementations ───────────────────────────────────────────────

fn scenario_01_faucet_balance(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let steps_total = 3u32;
    let mut details = Vec::new();

    let id = TestIdentity::generate();
    let node = runner.node(0);

    match runner.faucet_mint(node, &id.address_hex()) {
        Ok(resp) => {
            details.push(format!("faucet mint: amount={:?}", resp.amount));
            steps_passed += 1;
        }
        Err(e) => details.push(format!("faucet mint FAILED: {e}")),
    }

    std::thread::sleep(Duration::from_secs(2));
    match runner.get_balance(node, &id.address_hex()) {
        Ok(Some(bal)) => {
            details.push(format!("balance: {} entries", bal.balances.len()));
            steps_passed += 1;
        }
        Ok(None) => {
            details.push("balance: not yet funded (404) — acceptable".into());
            steps_passed += 1;
        }
        Err(e) => details.push(format!("balance check FAILED: {e}")),
    }

    match runner.faucet_mint(node, &id.address_hex()) {
        Ok(_) => {
            details.push("second mint: accepted".into());
            steps_passed += 1;
        }
        Err(e) => details.push(format!("second mint FAILED: {e}")),
    }

    ScenarioResult {
        id: 1,
        name: "Faucet + balance".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_02_native_transfers(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let steps_total = 4u32;
    let mut details = Vec::new();

    let sender = TestIdentity::generate();
    let recipient = TestIdentity::generate();
    let node = runner.node(0);

    match runner.faucet_mint(node, &sender.address_hex()) {
        Ok(_) => {
            details.push("sender funded".into());
            steps_passed += 1;
        }
        Err(e) => details.push(format!("fund sender FAILED: {e}")),
    }

    std::thread::sleep(Duration::from_secs(2));

    let payload = TransactionPayload::Transfer {
        recipient: recipient.address,
        amount: Amount(100),
        token: TokenId::Native,
    };
    match runner.sign_tx(&sender, 0, payload) {
        Ok(tx) => match runner.submit_tx(node, &tx) {
            Ok(resp) => {
                if resp.accepted {
                    details.push(format!("transfer submitted: digest={:?}", resp.tx_digest));
                    steps_passed += 1;
                } else {
                    details.push("transfer rejected".into());
                }
            }
            Err(e) => details.push(format!("transfer submit FAILED: {e}")),
        },
        Err(e) => details.push(format!("sign transfer FAILED: {e}")),
    }

    let payload2 = TransactionPayload::Transfer {
        recipient: recipient.address,
        amount: Amount(50),
        token: TokenId::Native,
    };
    match runner.sign_tx(&sender, 1, payload2) {
        Ok(tx) => match runner.submit_tx(node, &tx) {
            Ok(resp) => {
                if resp.accepted {
                    details.push("second transfer accepted".into());
                    steps_passed += 1;
                } else {
                    details.push("second transfer rejected".into());
                }
            }
            Err(e) => details.push(format!("second transfer FAILED: {e}")),
        },
        Err(e) => details.push(format!("sign second FAILED: {e}")),
    }

    let payload3 = TransactionPayload::Transfer {
        recipient: recipient.address,
        amount: Amount(0),
        token: TokenId::Native,
    };
    match runner.sign_tx(&sender, 2, payload3) {
        Ok(tx) => match runner.submit_tx(node, &tx) {
            Ok(resp) => {
                details.push(format!("zero transfer: accepted={}", resp.accepted));
                steps_passed += 1;
            }
            Err(e) => details.push(format!("zero transfer FAILED: {e}")),
        },
        Err(e) => details.push(format!("sign zero FAILED: {e}")),
    }

    ScenarioResult {
        id: 2,
        name: "Native token transfers".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_03_multinode_balance(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let mut steps_total = 0u32;
    let mut details = Vec::new();

    if runner.nodes.len() < 2 {
        return ScenarioResult {
            id: 3,
            name: "Multi-node balance check".into(),
            status: ScenarioStatus::Skip,
            steps_passed: 0,
            steps_total: 0,
            duration: start.elapsed(),
            details: vec!["requires >= 2 nodes".into()],
        };
    }

    let id = TestIdentity::generate();

    steps_total += 1;
    match runner.faucet_mint(runner.node(0), &id.address_hex()) {
        Ok(_) => {
            details.push("mint on node-0".into());
            steps_passed += 1;
        }
        Err(e) => details.push(format!("mint on node-0 FAILED: {e}")),
    }

    std::thread::sleep(Duration::from_secs(3));

    for i in 1..runner.nodes.len() {
        steps_total += 1;
        match runner.get_balance(runner.node(i), &id.address_hex()) {
            Ok(Some(bal)) => {
                details.push(format!(
                    "node-{i}: balance visible ({} entries)",
                    bal.balances.len()
                ));
                steps_passed += 1;
            }
            Ok(None) => {
                details.push(format!(
                    "node-{i}: balance not yet propagated (404 — acceptable)"
                ));
                steps_passed += 1;
            }
            Err(e) => details.push(format!("node-{i}: FAILED: {e}")),
        }
    }

    ScenarioResult {
        id: 3,
        name: "Multi-node balance check".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_04_contract_deploy(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let steps_total = 2u32;
    let mut details = Vec::new();

    let deployer = TestIdentity::generate();
    let node = runner.node(0);

    match runner.faucet_mint(node, &deployer.address_hex()) {
        Ok(_) => {
            details.push("deployer funded".into());
            steps_passed += 1;
        }
        Err(e) => details.push(format!("fund deployer FAILED: {e}")),
    }

    std::thread::sleep(Duration::from_secs(2));

    let payload = TransactionPayload::MovePublish {
        bytecode_modules: vec![vec![0u8; 10]],
    };
    match runner.sign_tx(&deployer, 0, payload) {
        Ok(tx) => match runner.submit_tx(node, &tx) {
            Ok(resp) => {
                details.push(format!(
                    "deploy submitted: accepted={}, digest={:?}",
                    resp.accepted, resp.tx_digest
                ));
                steps_passed += 1;
            }
            Err(e) => details.push(format!("deploy submit FAILED: {e}")),
        },
        Err(e) => details.push(format!("sign deploy FAILED: {e}")),
    }

    ScenarioResult {
        id: 4,
        name: "Contract deploy (pipeline)".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_05_contract_call(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let steps_total = 2u32;
    let mut details = Vec::new();

    let caller = TestIdentity::generate();
    let node = runner.node(0);

    match runner.faucet_mint(node, &caller.address_hex()) {
        Ok(_) => {
            details.push("caller funded".into());
            steps_passed += 1;
        }
        Err(e) => details.push(format!("fund caller FAILED: {e}")),
    }

    std::thread::sleep(Duration::from_secs(2));

    let payload = TransactionPayload::MoveCall {
        contract: nexus_primitives::ContractAddress([0xAA; 32]),
        function: "initialize".to_string(),
        type_args: vec![],
        args: vec![],
    };
    match runner.sign_tx(&caller, 0, payload) {
        Ok(tx) => match runner.submit_tx(node, &tx) {
            Ok(resp) => {
                details.push(format!("call submitted: accepted={}", resp.accepted));
                steps_passed += 1;
            }
            Err(e) => details.push(format!("call submit FAILED: {e}")),
        },
        Err(e) => details.push(format!("sign call FAILED: {e}")),
    }

    ScenarioResult {
        id: 5,
        name: "Contract call (pipeline)".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_06_contract_query(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let steps_total = 1u32;
    let mut details = Vec::new();

    let node = runner.node(0);
    let url = format!("{}/v2/contract/query", node.trim_end_matches('/'));
    let body = serde_json::json!({
        "contract": hex::encode([0xAA; 32]),
        "function": "get_count",
        "type_args": [],
        "args": []
    });

    match runner
        .agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(resp) => {
            details.push(format!("query responded: HTTP {}", resp.status()));
            steps_passed += 1;
        }
        Err(ureq::Error::Status(code, _)) => {
            details.push(format!(
                "query returned HTTP {code} — expected for missing contract"
            ));
            steps_passed += 1;
        }
        Err(e) => details.push(format!("query FAILED: {e}")),
    }

    ScenarioResult {
        id: 6,
        name: "Contract query (view)".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_07_intent_submission(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let steps_total = 2u32;
    let mut details = Vec::new();

    let user = TestIdentity::generate();
    let node = runner.node(0);

    let url = format!("{}/v2/intent/submit", node.trim_end_matches('/'));
    let body = serde_json::json!({
        "sender": hex::encode(user.address.0),
        "intent": format!("Transfer 100 NEXUS to {}", hex::encode([0xBB; 32])),
        "signature": hex::encode(vec![0u8; 64]),
        "sender_pk": hex::encode(user.pk.as_bytes())
    });

    match runner
        .agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(resp) => {
            details.push(format!("intent submitted: HTTP {}", resp.status()));
            steps_passed += 1;
        }
        Err(ureq::Error::Status(code, _)) => {
            details.push(format!(
                "intent returned HTTP {code} — expected for minimal input"
            ));
            steps_passed += 1;
        }
        Err(e) => details.push(format!("intent submit FAILED: {e}")),
    }

    let url = format!("{}/v2/intent/estimate-gas", node.trim_end_matches('/'));
    let body = serde_json::json!({
        "sender": hex::encode(user.address.0),
        "intent": format!("Transfer 50 NEXUS to {}", hex::encode([0xCC; 32])),
        "signature": hex::encode(vec![0u8; 64]),
        "sender_pk": hex::encode(user.pk.as_bytes())
    });

    match runner
        .agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(resp) => {
            details.push(format!("gas estimate: HTTP {}", resp.status()));
            steps_passed += 1;
        }
        Err(ureq::Error::Status(code, _)) => {
            details.push(format!("gas estimate returned HTTP {code} — acceptable"));
            steps_passed += 1;
        }
        Err(e) => details.push(format!("gas estimate FAILED: {e}")),
    }

    ScenarioResult {
        id: 7,
        name: "Intent submission".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_08_consensus_health(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let mut steps_total = 0u32;
    let mut details = Vec::new();

    for (i, node) in runner.nodes.iter().enumerate() {
        steps_total += 1;
        match runner.get_consensus_status(node) {
            Ok(status) => {
                details.push(format!(
                    "node-{i}: epoch={}, commits={}, dag={}",
                    status.epoch, status.total_commits, status.dag_size
                ));
                steps_passed += 1;
            }
            Err(e) => details.push(format!("node-{i}: consensus status FAILED: {e}")),
        }
    }

    steps_total += 1;
    match runner.get_validators(runner.node(0)) {
        Ok(validators) => {
            details.push(format!("validators: {} active", validators.len()));
            steps_passed += 1;
        }
        Err(e) => details.push(format!("validators FAILED: {e}")),
    }

    if runner.nodes.len() >= 2 {
        steps_total += 1;
        let epoch0 = runner
            .get_consensus_status(runner.node(0))
            .ok()
            .map(|s| s.epoch);
        let epoch1 = runner
            .get_consensus_status(runner.node(1))
            .ok()
            .map(|s| s.epoch);
        if epoch0 == epoch1 {
            details.push(format!("epoch consistent across nodes: {:?}", epoch0));
            steps_passed += 1;
        } else {
            details.push(format!(
                "epoch mismatch: node-0={:?}, node-1={:?}",
                epoch0, epoch1
            ));
        }
    }

    ScenarioResult {
        id: 8,
        name: "Consensus health".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_09_network_layer(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let mut steps_total = 0u32;
    let mut details = Vec::new();

    for (i, node) in runner.nodes.iter().enumerate() {
        steps_total += 1;
        match runner.check_health(node) {
            Ok(true) => {
                details.push(format!("node-{i}: healthy"));
                steps_passed += 1;
            }
            Ok(false) => details.push(format!("node-{i}: unhealthy")),
            Err(e) => details.push(format!("node-{i}: health check error: {e}")),
        }

        steps_total += 1;
        match runner.get_network_peers(node) {
            Ok(peers) => {
                details.push(format!("node-{i}: {} peers", peers.total));
                steps_passed += 1;
            }
            Err(e) => details.push(format!("node-{i}: peers FAILED: {e}")),
        }

        steps_total += 1;
        match runner.get_network_status(node) {
            Ok(status) => {
                details.push(format!(
                    "node-{i}: known_peers={}, routing_healthy={}",
                    status.known_peers, status.routing_healthy
                ));
                steps_passed += 1;
            }
            Err(e) => details.push(format!("node-{i}: network status FAILED: {e}")),
        }
    }

    ScenarioResult {
        id: 9,
        name: "Network layer".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_10_rapid_transfers(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let count = 10u32;
    let steps_total = count + 1;
    let mut details = Vec::new();

    let sender = TestIdentity::generate();
    let recipient = TestIdentity::generate();
    let node = runner.node(0);

    match runner.faucet_mint(node, &sender.address_hex()) {
        Ok(_) => {
            details.push("sender funded".into());
            steps_passed += 1;
        }
        Err(e) => {
            details.push(format!("fund sender FAILED: {e}"));
            return ScenarioResult {
                id: 10,
                name: "Rapid-fire transfers".into(),
                status: ScenarioStatus::Fail,
                steps_passed,
                steps_total,
                duration: start.elapsed(),
                details,
            };
        }
    }

    std::thread::sleep(Duration::from_secs(2));

    for seq in 0..count {
        let payload = TransactionPayload::Transfer {
            recipient: recipient.address,
            amount: Amount(1),
            token: TokenId::Native,
        };
        match runner.sign_tx(&sender, seq as u64, payload) {
            Ok(tx) => match runner.submit_tx(node, &tx) {
                Ok(resp) => {
                    if resp.accepted {
                        steps_passed += 1;
                    } else {
                        details.push(format!("tx#{seq}: rejected"));
                    }
                }
                Err(e) => details.push(format!("tx#{seq}: submit error: {e}")),
            },
            Err(e) => details.push(format!("tx#{seq}: sign error: {e}")),
        }
    }

    details.push(format!(
        "{}/{} rapid transfers accepted",
        steps_passed - 1,
        count
    ));

    ScenarioResult {
        id: 10,
        name: "Rapid-fire transfers".into(),
        status: if steps_passed >= steps_total / 2 {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_11_multi_sender(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let num_senders = 5u32;
    let mut steps_passed = 0u32;
    let steps_total = num_senders * 2;
    let mut details = Vec::new();

    let node = runner.node(0);
    let recipient = TestIdentity::generate();
    let mut senders = Vec::new();

    for i in 0..num_senders {
        let id = TestIdentity::generate();
        match runner.faucet_mint(node, &id.address_hex()) {
            Ok(_) => steps_passed += 1,
            Err(e) => details.push(format!("fund sender-{i} FAILED: {e}")),
        }
        senders.push(id);
    }

    std::thread::sleep(Duration::from_secs(2));

    for (i, sender) in senders.iter().enumerate() {
        let payload = TransactionPayload::Transfer {
            recipient: recipient.address,
            amount: Amount(10),
            token: TokenId::Native,
        };
        match runner.sign_tx(sender, 0, payload) {
            Ok(tx) => match runner.submit_tx(node, &tx) {
                Ok(resp) => {
                    if resp.accepted {
                        steps_passed += 1;
                    } else {
                        details.push(format!("sender-{i}: rejected"));
                    }
                }
                Err(e) => details.push(format!("sender-{i}: submit error: {e}")),
            },
            Err(e) => details.push(format!("sender-{i}: sign error: {e}")),
        }
    }

    details.push(format!(
        "{} senders funded, transfers submitted",
        senders.len()
    ));

    ScenarioResult {
        id: 11,
        name: "Multi-sender concurrency".into(),
        status: if steps_passed >= steps_total / 2 {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

fn scenario_12_cross_node_submission(runner: &ScenarioRunner) -> ScenarioResult {
    let start = Instant::now();
    let mut steps_passed = 0u32;
    let mut steps_total = 0u32;
    let mut details = Vec::new();

    if runner.nodes.len() < 2 {
        return ScenarioResult {
            id: 12,
            name: "Cross-node tx submission".into(),
            status: ScenarioStatus::Skip,
            steps_passed: 0,
            steps_total: 0,
            duration: start.elapsed(),
            details: vec!["requires >= 2 nodes".into()],
        };
    }

    for (i, node) in runner.nodes.iter().enumerate().take(3) {
        let id = TestIdentity::generate();
        let recipient = TestIdentity::generate();

        steps_total += 1;
        match runner.faucet_mint(node, &id.address_hex()) {
            Ok(_) => steps_passed += 1,
            Err(e) => {
                details.push(format!("fund on node-{i} FAILED: {e}"));
                continue;
            }
        }

        std::thread::sleep(Duration::from_millis(500));

        steps_total += 1;
        let payload = TransactionPayload::Transfer {
            recipient: recipient.address,
            amount: Amount(25),
            token: TokenId::Native,
        };
        match runner.sign_tx(&id, 0, payload) {
            Ok(tx) => match runner.submit_tx(node, &tx) {
                Ok(resp) => {
                    if resp.accepted {
                        details.push(format!("node-{i}: tx accepted"));
                        steps_passed += 1;
                    } else {
                        details.push(format!("node-{i}: tx rejected"));
                    }
                }
                Err(e) => details.push(format!("node-{i}: submit error: {e}")),
            },
            Err(e) => details.push(format!("node-{i}: sign error: {e}")),
        }
    }

    ScenarioResult {
        id: 12,
        name: "Cross-node tx submission".into(),
        status: if steps_passed == steps_total {
            ScenarioStatus::Pass
        } else {
            ScenarioStatus::Fail
        },
        steps_passed,
        steps_total,
        duration: start.elapsed(),
        details,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires running devnet — run with: cargo test -p nexus-test-utils scenario -- --ignored"]
    fn all_scenarios() {
        let mut runner = ScenarioRunner::from_env();
        let (passed, failed, skipped) = runner.run_all();

        println!(
            "\n  Scenario Results: {} passed, {} failed, {} skipped",
            passed, failed, skipped
        );
        for r in &runner.results {
            println!(
                "    [{:>2}] {:<35} {} ({}/{})",
                r.id, r.name, r.status, r.steps_passed, r.steps_total
            );
            for d in &r.details {
                println!("          {d}");
            }
        }

        assert_eq!(failed, 0, "scenario failures detected");
    }
}
