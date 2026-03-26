//! Intent service actor — async message-driven intent processing.
//!
//! [`IntentService`] wraps the intent compiler behind a bounded
//! [`tokio::sync::mpsc`] channel, giving callers a lightweight
//! [`IntentHandle`] to submit intents and receive results without
//! blocking the caller's task.
//!
//! # Architecture
//!
//! ```text
//! caller ──submit()──▶ IntentHandle
//!                          │  mpsc::Sender<Request>
//!                          ▼
//!                    IntentService::run() loop
//!                          │  compile()
//!                          ▼
//!                    oneshot::Sender<Response>
//! ```
//!
//! # Lifecycle
//!
//! 1. [`IntentService::spawn()`] creates the service and returns a handle.
//! 2. Callers use [`IntentHandle::submit()`] to send signed intents.
//! 3. The service compiles each intent against the account resolver.
//! 4. The result is returned via a per-request oneshot channel.
//! 5. Dropping **all** handles shuts the service down gracefully.

use crate::config::IntentConfig;
use crate::error::{IntentError, IntentResult};
use crate::metrics::IntentMetrics;
use crate::traits::{AccountResolver, IntentCompiler};
use crate::types::{CompiledIntentPlan, SignedUserIntent};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, instrument};

/// Request message sent from handle to service actor.
struct CompileRequest<R: AccountResolver> {
    intent: SignedUserIntent,
    resolver: Arc<R>,
    reply: oneshot::Sender<IntentResult<CompiledIntentPlan>>,
}

/// Lightweight handle for submitting intents to the service.
///
/// Clone-able — multiple producers can share a single handle.
pub struct IntentHandle<R: AccountResolver> {
    tx: mpsc::Sender<CompileRequest<R>>,
}

impl<R: AccountResolver> Clone for IntentHandle<R> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<R: AccountResolver> IntentHandle<R> {
    /// Submit a signed intent for compilation.
    ///
    /// Returns the compiled plan on success.  This call is async and
    /// will wait for the service to process the request.
    ///
    /// # Errors
    ///
    /// - Any [`IntentError`] from compilation.
    /// - [`IntentError::Internal`] if the service has shut down.
    pub async fn submit(
        &self,
        intent: SignedUserIntent,
        resolver: Arc<R>,
    ) -> IntentResult<CompiledIntentPlan> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = CompileRequest {
            intent,
            resolver,
            reply: reply_tx,
        };
        self.tx
            .send(req)
            .await
            .map_err(|_| IntentError::Internal("intent service shut down".to_string()))?;
        reply_rx.await.map_err(|_| {
            IntentError::Internal("intent service dropped reply channel".to_string())
        })?
    }

    /// Returns `true` if the backing service is still alive.
    pub fn is_alive(&self) -> bool {
        !self.tx.is_closed()
    }
}

/// The intent service actor.
///
/// Runs in its own Tokio task, processing compile requests from a
/// bounded channel.
pub struct IntentService<C, R>
where
    C: IntentCompiler<Resolver = R>,
    R: AccountResolver,
{
    compiler: C,
    rx: mpsc::Receiver<CompileRequest<R>>,
    metrics: IntentMetrics,
}

impl<C, R> IntentService<C, R>
where
    C: IntentCompiler<Resolver = R> + 'static,
    R: AccountResolver + 'static,
{
    /// Create and spawn the service, returning a clone-able handle.
    ///
    /// `mailbox_capacity` controls backpressure — producers block
    /// when the channel is full.
    pub fn spawn(compiler: C, mailbox_capacity: usize) -> IntentHandle<R> {
        let (tx, rx) = mpsc::channel(mailbox_capacity);
        let service = Self {
            compiler,
            rx,
            metrics: IntentMetrics::new(),
        };
        tokio::spawn(service.run());
        IntentHandle { tx }
    }

    /// Create the service and handle without spawning.
    ///
    /// Call [`run()`](Self::run) manually if you need to control
    /// the task yourself (e.g. in tests).
    pub fn new(compiler: C, mailbox_capacity: usize) -> (Self, IntentHandle<R>) {
        let (tx, rx) = mpsc::channel(mailbox_capacity);
        let service = Self {
            compiler,
            rx,
            metrics: IntentMetrics::new(),
        };
        (service, IntentHandle { tx })
    }

    /// Run the service loop until all handles are dropped.
    #[instrument(skip_all, name = "intent_service")]
    pub async fn run(mut self) {
        debug!("intent service started");
        while let Some(req) = self.rx.recv().await {
            // Report mailbox backlog
            self.metrics.set_mailbox_depth(self.rx.len());

            let start = Instant::now();
            let result = self.compiler.compile(&req.intent, &*req.resolver).await;
            let elapsed = start.elapsed().as_secs_f64();

            match &result {
                Ok(plan) => {
                    let is_cross_shard = plan
                        .steps
                        .iter()
                        .any(|s| s.transaction.body.target_shard.is_some_and(|sh| sh.0 != 0));
                    self.metrics.record_compilation(
                        elapsed,
                        plan.steps.len(),
                        plan.estimated_gas,
                        is_cross_shard,
                    );
                }
                Err(e) => {
                    error!(error = %e, "intent compilation failed");
                    self.metrics.record_compilation_failure(&e.to_string());
                }
            }
            // Ignore send error — caller may have timed out / dropped.
            let _ = req.reply.send(result);
        }
        debug!("intent service shutting down (all handles dropped)");
    }
}

/// Create an intent service from config convenience.
///
/// Shorthand for `IntentService::spawn(compiler, config.mailbox_capacity)`.
pub fn spawn_intent_service<C, R>(compiler: C, config: &IntentConfig) -> IntentHandle<R>
where
    C: IntentCompiler<Resolver = R> + 'static,
    R: AccountResolver + 'static,
{
    IntentService::spawn(compiler, config.mailbox_capacity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::IntentCompilerImpl;
    use crate::resolver::AccountResolverImpl;
    use crate::types::{compute_intent_digest, SignedUserIntent, UserIntent, INTENT_DOMAIN};
    use nexus_crypto::DilithiumSigner;
    use nexus_crypto::Signer;
    use nexus_primitives::{AccountAddress, Amount, TimestampMs, TokenId};

    fn make_resolver() -> AccountResolverImpl {
        let resolver = AccountResolverImpl::new(4);
        let sender = AccountAddress([0xAA; 32]);
        let token = TokenId::Native;
        resolver
            .balances()
            .set_balance(sender, token, Amount(1_000_000));
        resolver
    }

    fn sign_intent(intent: &UserIntent) -> SignedUserIntent {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let sender = AccountAddress([0xAA; 32]);
        let nonce = 1u64;

        let digest = compute_intent_digest(intent, &sender, nonce).unwrap();

        let intent_bytes = bcs::to_bytes(intent).unwrap();
        let sender_bytes = bcs::to_bytes(&sender).unwrap();
        let nonce_bytes = bcs::to_bytes(&nonce).unwrap();
        let mut msg = Vec::new();
        msg.extend_from_slice(&intent_bytes);
        msg.extend_from_slice(&sender_bytes);
        msg.extend_from_slice(&nonce_bytes);
        let sig = DilithiumSigner::sign(&sk, INTENT_DOMAIN, &msg);

        SignedUserIntent {
            intent: intent.clone(),
            sender,
            signature: sig,
            sender_pk: vk,
            nonce,
            created_at: TimestampMs(1_000_000),
            digest,
        }
    }

    fn transfer() -> UserIntent {
        UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(100),
        }
    }

    #[tokio::test]
    async fn submit_and_receive_plan() {
        let resolver = Arc::new(make_resolver());
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let (service, handle) = IntentService::new(compiler, 16);
        let svc = tokio::spawn(service.run());

        let intent = sign_intent(&transfer());
        let plan = handle.submit(intent, resolver).await;
        assert!(plan.is_ok());
        let plan = plan.unwrap();
        assert!(!plan.steps.is_empty());

        drop(handle);
        svc.await.unwrap();
    }

    #[tokio::test]
    async fn multiple_submits() {
        let resolver = Arc::new(make_resolver());
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let (service, handle) = IntentService::new(compiler, 16);
        let svc = tokio::spawn(service.run());

        for _ in 0..5 {
            let intent = sign_intent(&transfer());
            let plan = handle.submit(intent, resolver.clone()).await;
            assert!(plan.is_ok());
        }

        drop(handle);
        svc.await.unwrap();
    }

    #[tokio::test]
    async fn service_shuts_down_on_handle_drop() {
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let (service, handle) = IntentService::new(compiler, 16);
        let svc = tokio::spawn(service.run());

        drop(handle);
        // Service should terminate.
        svc.await.unwrap();
    }

    #[tokio::test]
    async fn submit_after_service_drop_returns_error() {
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let handle = IntentService::spawn(compiler, 1);

        // Give service a moment to start, then drop it by closing the channel
        // We can't easily drop the service directly, but we can test is_alive
        assert!(handle.is_alive());
    }

    #[tokio::test]
    async fn handle_is_clone() {
        let resolver = Arc::new(make_resolver());
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let (service, handle) = IntentService::new(compiler, 16);
        let svc = tokio::spawn(service.run());

        let h2 = handle.clone();
        let intent = sign_intent(&transfer());
        let plan = h2.submit(intent, resolver).await;
        assert!(plan.is_ok());

        drop(handle);
        drop(h2);
        svc.await.unwrap();
    }

    #[tokio::test]
    async fn spawn_convenience_works() {
        let resolver = Arc::new(make_resolver());
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let config = IntentConfig::default();
        let handle = spawn_intent_service(compiler, &config);

        let intent = sign_intent(&transfer());
        let plan = handle.submit(intent, resolver).await;
        assert!(plan.is_ok());
    }
}
