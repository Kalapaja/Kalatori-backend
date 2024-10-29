//! Common objects for chain interaction system

use crate::{
    chain::{
        rpc::{asset_balance_at_account, system_balance_at_account},
        tracker::ChainWatcher,
    },
    definitions::{
        api_v2::{OrderInfo, RpcInfo, Timestamp},
        Balance,
    },
    error::{ChainError, NotHexError},
    utils::unhex,
};
use jsonrpsee::ws_client::WsClient;
use primitive_types::H256;
use substrate_crypto_light::common::{AccountId32, AsBase58};
use tokio::sync::oneshot;

/// Abstraction to distinguish block hash from many other H256 things
#[derive(Debug, Clone)]
pub struct BlockHash(pub primitive_types::H256);

impl BlockHash {
    /// Convert block hash to RPC-friendly format
    pub fn to_string(&self) -> String {
        format!("0x{}", const_hex::encode(&self.0))
    }

    /// Convert string returned by RPC to typesafe block
    ///
    /// TODO: integrate nicely with serde
    pub fn from_str(s: &str) -> Result<Self, crate::error::ChainError> {
        let block_hash_raw = unhex(&s, NotHexError::BlockHash)?;
        Ok(BlockHash(H256(
            block_hash_raw
                .try_into()
                .map_err(|_| ChainError::BlockHashLength)?,
        )))
    }
}

#[derive(Debug)]
pub struct EventFilter<'a> {
    pub pallet: &'a str,
    pub optional_event_variant: Option<&'a str>,
}
/*
#[derive(Debug)]
struct ChainProperties {
    specs: ShortSpecs,
    metadata: RuntimeMetadataV15,
    existential_deposit: Option<Balance>,
    assets_pallet: Option<AssetsPallet>,
    block_hash_count: BlockNumber,
    account_lifetime: BlockNumber,
    depth: Option<NonZeroU64>,
}

#[derive(Debug)]
struct AssetsPallet {
    multi_location: Option<PalletIndex>,
    assets: HashMap<AssetId, AssetProperties>,
}

#[derive(Debug)]
struct AssetProperties {
    min_balance: Balance,
    decimals: Decimals,
}

#[derive(Debug)]
pub struct Currency {
    chain: String,
    asset: Option<AssetId>,
}

#[derive(Debug)]
pub struct ConnectedChain {
    rpc: String,
    client: WsClient,
    genesis: BlockHash,
    properties: ChainProperties,
}

*/
pub enum ChainRequest {
    WatchAccount(WatchAccount),
    Reap(WatchAccount),
    Shutdown(oneshot::Sender<()>),
    GetConnectedRpcs(oneshot::Sender<Vec<RpcInfo>>),
}

#[derive(Debug)]
pub struct WatchAccount {
    pub id: String,
    pub address: AccountId32,
    pub currency: String,
    pub amount: Balance,
    pub recipient: AccountId32,
    pub res: oneshot::Sender<Result<(), ChainError>>,
    pub death: Timestamp,
}

impl WatchAccount {
    pub fn new(
        id: String,
        order: OrderInfo,
        recipient: AccountId32,
        res: oneshot::Sender<Result<(), ChainError>>,
    ) -> Result<WatchAccount, ChainError> {
        Ok(WatchAccount {
            id,
            address: AccountId32::from_base58_string(&order.payment_account)
                .map_err(ChainError::InvoiceAccount)?
                .0,
            currency: order.currency.currency,
            amount: Balance::parse(order.amount, order.currency.decimals),
            recipient,
            res,
            death: order.death,
        })
    }
}

pub enum ChainTrackerRequest {
    WatchAccount(WatchAccount),
    NewBlock(String),
    Reap(WatchAccount),
    Shutdown(oneshot::Sender<()>),
}

#[derive(Clone, Debug)]
pub struct Invoice {
    pub id: String,
    pub address: AccountId32,
    pub currency: String,
    pub amount: Balance,
    pub recipient: AccountId32,
    pub death: Timestamp,
}

impl Invoice {
    pub fn from_request(watch_account: WatchAccount) -> Self {
        drop(watch_account.res.send(Ok(())));
        Invoice {
            id: watch_account.id,
            address: watch_account.address,
            currency: watch_account.currency,
            amount: watch_account.amount,
            recipient: watch_account.recipient,
            death: watch_account.death,
        }
    }

    pub async fn balance(
        &self,
        client: &WsClient,
        chain_watcher: &ChainWatcher,
        block: &BlockHash,
    ) -> Result<Balance, ChainError> {
        let currency = chain_watcher
            .assets
            .get(&self.currency)
            .ok_or(ChainError::InvalidCurrency(self.currency.clone()))?;
        if let Some(asset_id) = currency.asset_id {
            let balance = asset_balance_at_account(
                client,
                &block,
                &chain_watcher.metadata,
                &self.address,
                asset_id,
            )
            .await?;
            Ok(balance)
        } else {
            let balance =
                system_balance_at_account(client, &block, &chain_watcher.metadata, &self.address)
                    .await?;
            Ok(balance)
        }
    }

    pub async fn check(
        &self,
        client: &WsClient,
        chain_watcher: &ChainWatcher,
        block: &BlockHash,
    ) -> Result<bool, ChainError> {
        Ok(self.balance(client, chain_watcher, block).await? >= self.amount)
    }
}
