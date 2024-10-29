//! Everything related to actual interaction with blockchain

use std::collections::HashMap;
use substrate_crypto_light::common::AccountId32;
use tokio::{
    sync::{mpsc, oneshot},
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;

use crate::{
    definitions::{api_v2::OrderInfo, Chain},
    error::{ChainError, Error},
    signer::Signer,
    state::State,
    utils::task_tracker::TaskTracker,
};

pub mod definitions;
pub mod payout;
pub mod rpc;
pub mod tracker;
pub mod utils;

use crate::definitions::api_v2::{Health, RpcInfo, ServerHealth};
use definitions::{ChainRequest, ChainTrackerRequest, WatchAccount};
use tracker::start_chain_watch;

/// Logging filter
pub const MODULE: &str = module_path!();

/// Wait this long before forgetting about stuck chain watcher
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(120000);

/// RPC server handle
#[derive(Clone, Debug)]
pub struct ChainManager {
    pub tx: mpsc::Sender<ChainRequest>,
}

impl ChainManager {
    /// Run once to start all chain connections; this should be very robust, if manager fails
    /// - all modules should be restarted, probably.
    pub fn ignite(
        chain: Vec<Chain>,
        state: State,
        signer: Signer,
        task_tracker: TaskTracker,
        cancellation_token: CancellationToken,
    ) -> Result<Self, Error> {
        let (tx, mut rx) = mpsc::channel(1024);

        let mut watch_chain = HashMap::new();

        let mut currency_map = HashMap::new();

        // Create a channel for receiving RPC status updates
        let (rpc_update_tx, mut rpc_update_rx) = mpsc::channel(1024);

        // start network monitors
        for c in chain {
            if c.endpoints.is_empty() {
                return Err(Error::EmptyEndpoints(c.name));
            }
            let (chain_tx, chain_rx) = mpsc::channel(1024);
            watch_chain.insert(c.name.clone(), chain_tx.clone());

            // this MUST assert that there are no duplicates in requested assets
            if let Some(ref a) = c.native_token {
                if let Some(_) = currency_map.insert(a.name.clone(), c.name.clone()) {
                    return Err(Error::DuplicateCurrency(a.name.clone()));
                }
            }
            for a in &c.asset {
                if let Some(_) = currency_map.insert(a.name.clone(), c.name.clone()) {
                    return Err(Error::DuplicateCurrency(a.name.clone()));
                }
            }

            start_chain_watch(
                c,
                chain_tx.clone(),
                chain_rx,
                state.interface(),
                signer.interface(),
                task_tracker.clone(),
                cancellation_token.clone(),
                rpc_update_tx.clone(),
            );
        }

        task_tracker
            .clone()
            .spawn("Blockchain connections manager", async move {
                let mut rpc_statuses: HashMap<(String, String), Health> = HashMap::new();


                // start requests engine
                loop {
                    tokio::select! {
                        Some(request) = rx.recv() => {
                            match request {
                                ChainRequest::WatchAccount(request) => {
                                    if let Some(chain) = currency_map.get(&request.currency) {
                                        if let Some(receiver) = watch_chain.get(chain) {
                                            let _unused = receiver
                                                .send(ChainTrackerRequest::WatchAccount(request))
                                                .await;
                                        } else {
                                            let _unused = request
                                                .res
                                                .send(Err(ChainError::InvalidChain(chain.to_string())));
                                        }
                                    } else {
                                        let _unused = request
                                            .res
                                            .send(Err(ChainError::InvalidCurrency(request.currency)));
                                    }
                                }
                                ChainRequest::Reap(request) => {
                                    if let Some(chain) = currency_map.get(&request.currency) {
                                        if let Some(receiver) = watch_chain.get(chain) {
                                            let _unused =
                                                receiver.send(ChainTrackerRequest::Reap(request)).await;
                                        } else {
                                            let _unused = request
                                                .res
                                                .send(Err(ChainError::InvalidChain(chain.to_string())));
                                        }
                                    } else {
                                        let _unused = request
                                            .res
                                            .send(Err(ChainError::InvalidCurrency(request.currency)));
                                    }
                                }
                                ChainRequest::Shutdown(res) => {
                                    for (name, chain) in watch_chain.drain() {
                                        let (tx, rx) = oneshot::channel();
                                        if chain.send(ChainTrackerRequest::Shutdown(tx)).await.is_ok() {
                                            if timeout(SHUTDOWN_TIMEOUT, rx).await.is_err() {
                                                tracing::error!("Chain monitor for {name} took too much time to wind down, probably it was frozen. Discarding it.");
                                            };
                                        }
                                    }
                                    let _ = res.send(());
                                    break;
                                }
                                ChainRequest::GetConnectedRpcs(res_tx) => {
                                    // Collect the RpcInfo from rpc_statuses
                                    let connected_rpcs: Vec<RpcInfo> = rpc_statuses.iter().map(|((chain_name, rpc_url), status)| {
                                        RpcInfo {
                                            chain_name: chain_name.clone(),
                                            rpc_url: rpc_url.clone(),
                                            status: *status,
                                        }
                                    }).collect();
                                    let _ = res_tx.send(connected_rpcs);
                                }
                            }
                        }
                        Some(rpc_update) = rpc_update_rx.recv() => {
                            rpc_statuses.insert(
                                (rpc_update.chain_name.clone(), rpc_update.rpc_url.clone()),
                                rpc_update.status,
                            );
                        }
                        else => break,
                    }
                }

                Ok("Chain manager is shutting down")
            });

        Ok(Self { tx })
    }

    pub async fn add_invoice(
        &self,
        id: String,
        order: OrderInfo,
        recipient: AccountId32,
    ) -> Result<(), ChainError> {
        let (res, rx) = oneshot::channel();
        self.tx
            .send(ChainRequest::WatchAccount(WatchAccount::new(
                id, order, recipient, res,
            )?))
            .await
            .map_err(|_| ChainError::MessageDropped)?;
        rx.await.map_err(|_| ChainError::MessageDropped)?
    }

    pub async fn get_connected_rpcs(&self) -> Result<Vec<RpcInfo>, Error> {
        let (res_tx, res_rx) = oneshot::channel();
        self.tx
            .send(ChainRequest::GetConnectedRpcs(res_tx))
            .await
            .map_err(|_| Error::Fatal)?;
        res_rx.await.map_err(|_| Error::Fatal)
    }

    pub async fn reap(
        &self,
        id: String,
        order: OrderInfo,
        recipient: AccountId32,
    ) -> Result<(), ChainError> {
        let (res, rx) = oneshot::channel();
        self.tx
            .send(ChainRequest::Reap(WatchAccount::new(
                id, order, recipient, res,
            )?))
            .await
            .map_err(|_| ChainError::MessageDropped)?;
        rx.await.map_err(|_| ChainError::MessageDropped)?
    }

    pub async fn shutdown(&self) -> () {
        let (tx, rx) = oneshot::channel();
        let _unused = self.tx.send(ChainRequest::Shutdown(tx)).await;
        let _ = rx.await;
        ()
    }
}
