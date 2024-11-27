//! A tracker that follows individual chain

use crate::{
    chain::{
        definitions::{BlockHash, ChainTrackerRequest, Invoice},
        payout::payout,
        rpc::{
            assets_set_at_block, block_hash, genesis_hash, metadata, next_block, next_block_number,
            runtime_version_identifier, specs, subscribe_blocks, transfer_events,
        },
        utils::parse_transfer_event,
    },
    database::{FinalizedTxDb, TransactionInfoDb, TransactionInfoDbInner, TxKind},
    definitions::{
        api_v2::{Amount, CurrencyProperties, Health, RpcInfo, TokenKind, TxStatus},
        Chain,
    },
    error::{ChainError, Error},
    signer::Signer,
    state::State,
    utils::task_tracker::TaskTracker,
};
use frame_metadata::v15::RuntimeMetadataV15;
use jsonrpsee::ws_client::{WsClient, WsClientBuilder};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    time::SystemTime,
};
use substrate_crypto_light::common::AsBase58;
use substrate_parser::{AsMetadata, ShortSpecs};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::{
    sync::mpsc,
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;

#[allow(clippy::too_many_lines)]
pub fn start_chain_watch(
    chain: Chain,
    chain_tx: mpsc::Sender<ChainTrackerRequest>,
    mut chain_rx: mpsc::Receiver<ChainTrackerRequest>,
    state: State,
    signer: Signer,
    task_tracker: TaskTracker,
    cancellation_token: CancellationToken,
    rpc_update_tx: mpsc::Sender<RpcInfo>,
) {
    task_tracker
        .clone()
        .spawn(format!("Chain {} watcher", chain.name.clone()), async move {
            let watchdog = 120000;
            let mut watched_accounts = HashMap::new();
            let mut shutdown = false;
            // TODO: random pick instead
            for endpoint in chain.endpoints.clone().iter().cycle() {
                // not restarting chain if shutdown is in progress
                if shutdown || cancellation_token.is_cancelled() {
                    break;
                }

                let _ = rpc_update_tx.send(RpcInfo {
                    chain_name: chain.name.clone(),
                    rpc_url: endpoint.clone(),
                    status: Health::Degraded,
                }).await;

                if let Ok(client) = WsClientBuilder::default().build(endpoint).await {
                    let _ = rpc_update_tx.send(RpcInfo {
                        chain_name: chain.name.clone(),
                        rpc_url: endpoint.clone(),
                        status: Health::Ok,
                    }).await;

                    // prepare chain
                    let watcher = match ChainWatcher::prepare_chain(
                        &client,
                        chain.clone(),
                        &mut watched_accounts,
                        endpoint,
                        chain_tx.clone(),
                        state.interface(),
                        task_tracker.clone(),
                    )
                        .await
                    {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to connect to chain {}, due to {} switching RPC server...",
                                chain.name,
                                e
                            );
                            continue;
                        }
                    };


                    // fulfill requests
                    while let Ok(Some(request)) =
                        timeout(Duration::from_millis(watchdog), chain_rx.recv()).await
                    {
                        match request {
                            ChainTrackerRequest::NewBlock(block_number) => {
                                // TODO: hide this under rpc module
                                let block = match block_hash(&client, Some(block_number)).await {
                                    Ok(a) => a,
                                    Err(e) => {
                                        tracing::info!(
                                            "Failed to receive block in chain {}, due to {}; Switching RPC server...",
                                            chain.name,
                                            e
                                        );
                                        break;
                                    },
                                };

                                tracing::debug!("Block hash {} from {}", block.to_string(), chain.name);

                                if watcher.version != runtime_version_identifier(&client, &block).await? {
                                    tracing::info!("Different runtime version reported! Restarting connection...");
                                    break;
                                }

                                tracing::debug!("Watched accounts: {watched_accounts:?}");

                                let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis() as u64;

                                let mut id_remove_list = Vec::new();

                                match transfer_events(
                                    &client,
                                    &block,
                                    &watcher.metadata,
                                )
                                    .await {
                                        Ok((timestamp, events)) => {
                                        tracing::debug!("Got a block with timestamp {timestamp:?} & events: {events:?}");

                                        for (id, invoice) in &watched_accounts {
                                            for (extrinsic_option, event) in &events {
                                                if let Some((tx_kind, another_account, transfer_amount)) = parse_transfer_event(&invoice.address, &event.0.fields) {
                                                    tracing::debug!("Found {tx_kind:?} from/to {another_account:?} with {transfer_amount:?} token(s).");

                                                    let Some((position_in_block, extrinsic)) = extrinsic_option else {
                                                        return Err(Error::from(ChainError::TransferEventNoExtrinsic));
                                                    };

                                                    let finalized_tx_timestamp = (OffsetDateTime::UNIX_EPOCH + Duration::from_millis(timestamp.0))
                                                        .format(&Rfc3339).unwrap().into();
                                                    let finalized_tx = FinalizedTxDb {
                                                            block_number,
                                                            position_in_block: *position_in_block
                                                        }.into();
                                                    let amount = Amount::Exact(transfer_amount.format(invoice.currency.decimals));
                                                    let status = TxStatus::Finalized;
                                                    let currency = invoice.currency.clone();
                                                    let transaction_bytes = const_hex::encode_prefixed(extrinsic);

                                                    match tx_kind {
                                                        kind @ TxKind::Payment => {
                                                            state.record_transaction(
                                                                TransactionInfoDb {
                                                                    transaction_bytes,
                                                                    inner: TransactionInfoDbInner {
                                                                        finalized_tx,
                                                                        finalized_tx_timestamp,
                                                                        sender: another_account.to_base58_string(42),
                                                                        recipient: invoice.address.to_base58_string(42),
                                                                        amount,
                                                                        currency,
                                                                        status,
                                                                        kind,
                                                                    } },
                                                                    id.clone()).await?;
                                                        }
                                                        kind @ TxKind::Withdrawal => {
                                                            state.record_transaction(
                                                                TransactionInfoDb {
                                                                    transaction_bytes,
                                                                    inner: TransactionInfoDbInner {
                                                                        finalized_tx,
                                                                        finalized_tx_timestamp,
                                                                        sender: invoice.address.to_base58_string(42),
                                                                        recipient: another_account.to_base58_string(42),
                                                                        amount,
                                                                        currency,
                                                                        status,
                                                                        kind,
                                                                    } },
                                                                    id.clone()).await?;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("events fetch error: {0:?}", e);
                                    }
                                }

                                // Important! There used to be a significant oprimisation that
                                // watched events and checked only accounts that have tranfers into
                                // them in given block; this was found to be unreliable: there are
                                // ways to transfer funds without emitting a transfer event (one
                                // notable example is through asset exchange procedure directed
                                // straight into invoice account), and probably even without any
                                // reliably expected event (through XCM). Thus we just scan all
                                // accounts, every time. Please submit a PR or an issue if you
                                // figure out a reliable optimization for this.
                                for (id, invoice) in &watched_accounts {
                                    match invoice.check(&client, &watcher, &block).await {
                                        Ok(true) => {
                                            state.order_paid(id.clone()).await;
                                        },
                                        Err(e) => {
                                            tracing::warn!("account fetch error: {0:?}", e);
                                        }
                                        _ => {}
                                    }

                                    if invoice.death.0 <= now {
                                        match state.is_order_paid(id.clone()).await {
                                            Ok(paid_db) => {
                                                if !paid_db {
                                                    match invoice.check(&client, &watcher, &block).await {
                                                        Ok(paid) => {
                                                            if paid {
                                                                state.order_paid(id.clone()).await;
                                                            }
                                                        }
                                                        Err(e) => {
                                                            tracing::warn!("account fetch error: {0:?}", e);
                                                        }
                                                    }
                                                }

                                                tracing::debug!("Removing an account {id:?} due to passing its death timestamp");
                                                id_remove_list.push(id.to_owned());
                                            }
                                            Err(e) => {
                                                tracing::warn!("account read error: {e:?}");
                                            }
                                        }
                                    }
                                }

                                for id in id_remove_list {
                                    watched_accounts.remove(&id);
                                };

                                tracing::debug!("Block {} from {} processed successfully", block.to_string(), chain.name);
                            }
                            ChainTrackerRequest::WatchAccount(request) => {
                                watched_accounts.insert(request.id.clone(), Invoice::from_request(request));
                            }
                            ChainTrackerRequest::Reap(request) => {
                                let id = request.id.clone();
                                let rpc = endpoint.clone();
                                let reap_state_handle = state.interface();
                                let watcher_for_reaper = watcher.clone();
                                let signer_for_reaper = signer.interface();

                                task_tracker.clone().spawn(format!("Initiate payout for order {}", id.clone()), async move {
                                    drop(payout(rpc, Invoice::from_request(request), reap_state_handle, watcher_for_reaper, signer_for_reaper).await);
                                    Ok(format!("Payout attempt for order {id} terminated"))
                                });
                            }
                            ChainTrackerRequest::ForceReap(request) => {
                                let id = request.id.clone();
                                let rpc = endpoint.clone();
                                let reap_state_handle = state.interface();
                                let watcher_for_reaper = watcher.clone();
                                let signer_for_reaper = signer.interface();
                                task_tracker.clone().spawn(format!("Initiate forced payout for order {}", id.clone()), async move {
                                    drop(payout(rpc, Invoice::from_request(request), reap_state_handle, watcher_for_reaper, signer_for_reaper).await);
                                    Ok(format!("Forced payout attempt for order {id} terminated"))
                                });
                            }
                            ChainTrackerRequest::Shutdown(res) => {
                                shutdown = true;
                                let _ = res.send(());
                                break;
                            }
                        }
                    }
                } else {
                    drop(rpc_update_tx.send(RpcInfo {
                        chain_name: chain.name.clone(),
                        rpc_url: endpoint.clone(),
                        status: Health::Critical,
                    }).await);
                }
            }
            Ok(format!("Chain {} monitor shut down", chain.name))
        });
}

#[derive(Debug, Clone)]
pub struct ChainWatcher {
    pub genesis_hash: BlockHash,
    pub metadata: RuntimeMetadataV15,
    pub specs: ShortSpecs,
    pub assets: HashMap<String, CurrencyProperties>,
    version: Value,
}

impl ChainWatcher {
    #[expect(clippy::too_many_lines)]
    pub async fn prepare_chain(
        client: &WsClient,
        chain: Chain,
        watched_accounts: &mut HashMap<String, Invoice>,
        rpc_url: &str,
        chain_tx: mpsc::Sender<ChainTrackerRequest>,
        state: State,
        task_tracker: TaskTracker,
    ) -> Result<Self, ChainError> {
        let genesis_hash = genesis_hash(client).await?;
        let mut blocks = subscribe_blocks(client).await?;
        let block = next_block(client, &mut blocks).await?;
        let version = runtime_version_identifier(client, &block).await?;
        let metadata = metadata(client, &block).await?;
        let name = <RuntimeMetadataV15 as AsMetadata<()>>::spec_name_version(&metadata)?.spec_name;
        if name != chain.name {
            return Err(ChainError::WrongNetwork {
                expected: chain.name,
                actual: name,
                rpc: rpc_url.to_string(),
            });
        }
        let specs = specs(client, &metadata, &block).await?;
        let mut assets =
            assets_set_at_block(client, &block, &metadata, rpc_url, specs.clone()).await?;

        // TODO: make this verbosity less annoying
        tracing::info!(
            "chain {} requires native token {:?} and {:?}",
            &chain.name,
            &chain.native_token,
            &chain.asset
        );
        // Remove unwanted assets
        assets = assets
            .into_iter()
            .filter_map(|(asset_name, properties)| {
                tracing::info!(
                    "chain {} has token {} with properties {:?}",
                    &chain.name,
                    &asset_name,
                    &properties
                );

                chain
                    .asset
                    .iter()
                    .find(|a| Some(a.id) == properties.asset_id)
                    .map(|a| (a.name.clone(), properties))
            })
            .collect();

        if let Some(ref native_token) = chain.native_token {
            if native_token.decimals == specs.decimals {
                assets.insert(
                    native_token.name.clone(),
                    CurrencyProperties {
                        chain_name: name.clone(),
                        kind: TokenKind::Native,
                        decimals: specs.decimals,
                        rpc_url: rpc_url.to_owned(),
                        asset_id: None,
                        ss58: 0,
                    },
                );
            }
        }

        // Deduplication is done on chain manager level;
        // Check that we have same number of assets as requested (we've checked that we have only
        // wanted ones and performed deduplication before)
        //
        // This is probably an optimisation, but I don't have time to analyse perfirmance right
        // now, it's just simpler to implement
        //
        // TODO: maybe check if at least one endpoint responds with proper assets and if not, shut
        // down
        if assets.len() != chain.asset.len() + usize::from(chain.native_token.is_some()) {
            return Err(ChainError::AssetsInvalid(chain.name));
        }
        // this MUST assert that assets match exactly before reporting it

        state.connect_chain(assets.clone()).await;

        let chain = ChainWatcher {
            genesis_hash,
            metadata,
            specs,
            assets,
            version,
        };

        // check monitored accounts
        let mut id_remove_list = Vec::new();
        for (id, account) in watched_accounts.iter() {
            match account.check(client, &chain, &block).await {
                Ok(true) => {
                    state.order_paid(id.clone()).await;
                    id_remove_list.push(id.to_owned());
                }
                Ok(false) => (),
                Err(e) => {
                    tracing::warn!("account fetch error: {0}", e);
                }
            }
        }
        for id in id_remove_list {
            watched_accounts.remove(&id);
        }

        let rpc = rpc_url.to_string();
        task_tracker.spawn(format!("watching blocks at {rpc}"), async move {
            loop {
                match next_block_number(&mut blocks).await {
                    Ok(block) => {
                        tracing::debug!("received block {block} from {rpc}");
                        if let Err(e) = chain_tx.send(ChainTrackerRequest::NewBlock(block)).await {
                            tracing::warn!(
                                "Block watch internal communication error: {e} at {rpc}"
                            );
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn! {"Block watch error: {e} at {rpc}"};
                        break;
                    }
                }
            }
            // this should reset chain monitor on timeout;
            // but if this breaks, it means that the latter is already down either way
            Ok(format!("Block watch at {rpc} stopped"))
        });

        Ok(chain)
    }
}
