use crate::{
    database::{Database, Invoice, InvoiceStatus, ReadInvoices},
    shutdown, Account, Balance, BlockNumber, Decimals, Hash, Nonce, OnlineClient, RuntimeConfig,
    DECIMALS, SCANNER_TO_LISTENER_SWITCH_POINT,
};
use anyhow::{Context, Result};
use reconnecting_jsonrpsee_ws_client::ClientBuilder;
use serde::{Deserialize, Deserializer};
use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fmt::{self, Arguments, Display, Formatter, Write},
    sync::Arc,
};
use subxt::{
    backend::{
        legacy::{LegacyBackend, LegacyRpcMethods},
        rpc::{RpcClient, RpcSubscription},
        Backend, BackendExt, RuntimeVersion,
    },
    blocks::{Block, BlocksClient},
    config::{
        signed_extensions::{ChargeTransactionPaymentParams, CheckMortalityParams},
        Header,
    },
    constants::ConstantsClient,
    dynamic::{self, Value},
    error::RpcError,
    ext::{
        futures::TryFutureExt,
        scale_decode::DecodeAsType,
        scale_value::{self, At},
        sp_core::{
            crypto::{AccountId32, Ss58AddressFormat},
            sr25519::Pair,
        },
    },
    storage::StorageClient,
    tx::{PairSigner, SubmittableExtrinsic, TxClient},
    Config, Metadata,
};
use tokio::sync::{mpsc::UnboundedSender, RwLock};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub const MODULE: &str = module_path!();

const MAX_BLOCK_NUMBER_ERROR: &str = "block number type overflow is occurred";
const BLOCK_NONCE_ERROR: &str = "failed to fetch an account nonce by the scanner client";

// Pallets

const SYSTEM: &str = "System";
const BALANCES: &str = "Balances";
const UTILITY: &str = "Utility";

async fn fetch_best_block(methods: &LegacyRpcMethods<RuntimeConfig>) -> Result<Hash> {
    methods
        .chain_get_block_hash(None)
        .await
        .context("failed to get the best block hash")?
        .context("received nothing after requesting the best block hash")
}

async fn fetch_runtime(
    methods: &LegacyRpcMethods<RuntimeConfig>,
    backend: &impl Backend<RuntimeConfig>,
) -> Result<(Metadata, RuntimeVersion)> {
    let best_block = fetch_best_block(methods).await?;

    Ok((
        fetch_metadata(backend, best_block)
            .await
            .context("failed to fetch metadata")?,
        methods
            .state_get_runtime_version(Some(best_block))
            .await
            .map(|runtime_version| RuntimeVersion {
                spec_version: runtime_version.spec_version,
                transaction_version: runtime_version.transaction_version,
            })
            .context("failed to fetch the runtime version")?,
    ))
}

async fn fetch_metadata(backend: &impl Backend<RuntimeConfig>, at: Hash) -> Result<Metadata> {
    const LATEST_SUPPORTED_METADATA_VERSION: u32 = 15;

    backend
        .metadata_at_version(LATEST_SUPPORTED_METADATA_VERSION, at)
        .or_else(|error| async {
            if let subxt::Error::Rpc(RpcError::ClientError(_)) | subxt::Error::Other(_) = error {
                backend.legacy_metadata(at).await
            } else {
                Err(error)
            }
        })
        .await
        .map_err(Into::into)
}

fn fetch_constant<T: DecodeAsType>(
    constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
    constant: (&str, &str),
) -> Result<T> {
    constants
        .at(&dynamic::constant(constant.0, constant.1))
        .with_context(|| format!("failed to get the constant {constant:?}"))?
        .as_type()
        .with_context(|| format!("failed to decode the constant {constant:?}"))
}

pub struct ChainProperties {
    pub address_format: Ss58AddressFormat,
    pub existential_deposit: Balance,
    pub decimals: Decimals,
    pub block_hash_count: BlockNumber,
}

impl ChainProperties {
    fn fetch_only_constants(
        constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
        decimals: Decimals,
    ) -> Result<Self> {
        const ADDRESS_PREFIX: (&str, &str) = (SYSTEM, "SS58Prefix");
        const EXISTENTIAL_DEPOSIT: (&str, &str) = (BALANCES, "ExistentialDeposit");
        const BLOCK_HASH_COUNT: (&str, &str) = (SYSTEM, "BlockHashCount");

        Ok(Self {
            address_format: Ss58AddressFormat::custom(fetch_constant(constants, ADDRESS_PREFIX)?),
            existential_deposit: fetch_constant(constants, EXISTENTIAL_DEPOSIT)?,
            block_hash_count: fetch_constant(constants, BLOCK_HASH_COUNT)?,
            decimals,
        })
    }

    async fn fetch(
        constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
        methods: &LegacyRpcMethods<RuntimeConfig>,
    ) -> Result<Self> {
        const DECIMALS_KEY: &str = "tokenDecimals";

        let system_properties = methods
            .system_properties()
            .await
            .context("failed to get the chain system properties")?;
        let encoded_decimals = system_properties
            .get(DECIMALS_KEY)
            .with_context(|| format!(
                "{DECIMALS_KEY:?} wasn't found in a response of the `system_properties` RPC call, set `{DECIMALS}` to set the decimal places number manually"
            ))?;
        let decimals = encoded_decimals
            .as_u64()
            .with_context(|| format!(
                "failed to decode the decimal places number, expected a positive integer, got \"{encoded_decimals}\""
            ))?;

        Self::fetch_only_constants(constants, decimals)
    }
}

pub struct ApiConfig {
    api: Arc<OnlineClient>,
    methods: Arc<LegacyRpcMethods<RuntimeConfig>>,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
}

pub struct EndpointProperties {
    pub url: CheckedUrl,
    pub chain: Arc<RwLock<ChainProperties>>,
}

pub struct CheckedUrl(String);

impl CheckedUrl {
    pub fn get(self) -> String {
        self.0
    }
}

pub async fn prepare(
    url: String,
    decimals_option: Option<Decimals>,
    shutdown_notification: CancellationToken,
) -> Result<(ApiConfig, EndpointProperties, Updater)> {
    // TODO:
    // The current reconnecting client implementation automatically restores all subscriptions,
    // including unrecoverable ones, losing all notifications! For now, it shouldn't affect the
    // daemon, but may in the future, so we should consider creating our own implementation.
    let rpc = RpcClient::new(
        ClientBuilder::new()
            .build(url.clone())
            .await
            .context("failed to construct the RPC client")?,
    );

    log::info!("Connected to an RPC server at \"{url}\".");

    let methods = Arc::new(LegacyRpcMethods::new(rpc.clone()));
    let backend = Arc::new(LegacyBackend::new(rpc));

    let (metadata, runtime_version) = fetch_runtime(&methods, &*backend)
        .await
        .context("failed to fetch the runtime of the API client")?;
    let genesis_hash = methods
        .genesis_hash()
        .await
        .context("failed to get the genesis hash")?;
    let api = Arc::new(
        OnlineClient::from_backend_with(genesis_hash, runtime_version, metadata, backend.clone())
            .context("failed to construct the API client")?,
    );
    let constants = api.constants();

    let (properties_result, decimals_set) = if let Some(decimals) = decimals_option {
        (
            ChainProperties::fetch_only_constants(&constants, decimals),
            true,
        )
    } else {
        (ChainProperties::fetch(&constants, &methods).await, false)
    };
    let properties = properties_result?;

    log::info!(
        "Chain properties:\n\
         Decimal places number: {}.\n\
         Address format: \"{}\" ({}).\n\
         Existential deposit: {}.\n\
         Block hash count: {}.",
        properties.decimals,
        properties.address_format,
        properties.address_format.prefix(),
        properties.existential_deposit,
        properties.block_hash_count
    );

    let arc_properties = Arc::new(RwLock::const_new(properties));

    Ok((
        ApiConfig {
            api: api.clone(),
            methods: methods.clone(),
            backend: backend.clone(),
        },
        EndpointProperties {
            url: CheckedUrl(url),
            chain: arc_properties.clone(),
        },
        Updater {
            methods,
            backend,
            api,
            constants,
            shutdown_notification,
            properties: arc_properties,
            decimals_set,
        },
    ))
}

pub struct Updater {
    methods: Arc<LegacyRpcMethods<RuntimeConfig>>,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
    api: Arc<OnlineClient>,
    constants: ConstantsClient<RuntimeConfig, OnlineClient>,
    shutdown_notification: CancellationToken,
    properties: Arc<RwLock<ChainProperties>>,
    decimals_set: bool,
}

impl Updater {
    pub async fn ignite(self) -> Result<&'static str> {
        loop {
            let mut updates = self
                .backend
                .stream_runtime_version()
                .await
                .context("failed to get the runtime updates stream")?;

            if let Some(current_runtime_version_result) = updates.next().await {
                let current_runtime_version = current_runtime_version_result
                    .context("failed to decode the current runtime version")?;

                // The updates stream is always returns the current runtime version in the first
                // item. We don't skip it though because during a connection loss the runtime can be
                // updated, hence this condition will catch this.
                if self.api.runtime_version() != current_runtime_version {
                    self.process_update()
                        .await
                        .context("failed to process the first API client update")?;
                }

                loop {
                    tokio::select! {
                        biased;
                        () = self.shutdown_notification.cancelled() => {
                            return Ok("The API client updater is shut down.");
                        }
                        runtime_version = updates.next() => {
                            if runtime_version.is_some() {
                                self.process_update()
                                    .await
                                    .context(
                                        "failed to process an update for the API client"
                                    )?;
                            } else {
                                break;
                            }
                        }
                    }
                }
            }

            log::warn!(
                "Lost the connection while listening the endpoint for API client runtime updates. Retrying..."
            );
        }
    }

    async fn process_update(&self) -> Result<()> {
        // We don't use the runtime version from the updates stream because it doesn't provide the
        // best block hash, so we fetch it ourselves (in `fetch_runtime`) and use it to make sure
        // that metadata & the runtime version are from the same block.
        let (metadata, runtime_version) = fetch_runtime(&self.methods, &*self.backend)
            .await
            .context("failed to fetch a new runtime for the API client")?;

        self.api.set_metadata(metadata);
        self.api.set_runtime_version(runtime_version);

        let (mut current_properties, new_properties_result) = if self.decimals_set {
            let current_properties = self.properties.write().await;
            let new_properties_result =
                ChainProperties::fetch_only_constants(&self.constants, current_properties.decimals);

            (current_properties, new_properties_result)
        } else {
            (
                self.properties.write().await,
                ChainProperties::fetch(&self.constants, &self.methods).await,
            )
        };
        let new_properties = new_properties_result?;

        let mut changed = String::new();
        let mut add_change = |message: Arguments<'_>| {
            changed.write_fmt(message).unwrap();
        };

        if new_properties.address_format != current_properties.address_format {
            add_change(format_args!(
                "\nOld {value}: \"{}\" ({}). New {value}: \"{}\" ({}).",
                current_properties.address_format,
                current_properties.address_format.prefix(),
                new_properties.address_format,
                new_properties.address_format.prefix(),
                value = "address format",
            ));
        }

        if new_properties.existential_deposit != current_properties.existential_deposit {
            add_change(format_args!(
                "\nOld {value}: {}. New {value}: {}.",
                current_properties.existential_deposit,
                new_properties.existential_deposit,
                value = "existential deposit"
            ));
        }

        if new_properties.decimals != current_properties.decimals {
            add_change(format_args!(
                "\nOld {value}: {}. New {value}: {}.",
                current_properties.decimals,
                new_properties.decimals,
                value = "decimal places number"
            ));
        }

        if new_properties.block_hash_count != current_properties.block_hash_count {
            add_change(format_args!(
                "\nOld {value}: {}. New {value}: {}.",
                current_properties.block_hash_count,
                new_properties.block_hash_count,
                value = "block hash count"
            ));
        }

        if !changed.is_empty() {
            *current_properties = new_properties;

            log::warn!("The chain properties has been changed:{changed}");
        }

        log::info!("A runtime update has been found and applied for the API client.");

        Ok(())
    }
}

#[derive(Debug)]
struct Shutdown;

impl Error for Shutdown {}

// Not used, but required for the `anyhow::Context` trait.
impl Display for Shutdown {
    fn fmt(&self, _: &mut Formatter<'_>) -> fmt::Result {
        unimplemented!()
    }
}

struct Api {
    tx: TxClient<RuntimeConfig, OnlineClient>,
    blocks: BlocksClient<RuntimeConfig, OnlineClient>,
}

struct Scanner {
    client: OnlineClient,
    blocks: BlocksClient<RuntimeConfig, OnlineClient>,
    storage: StorageClient<RuntimeConfig, OnlineClient>,
}

struct ProcessorFinalized {
    database: Arc<Database>,
    client: OnlineClient,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
    methods: Arc<LegacyRpcMethods<RuntimeConfig>>,
    shutdown_notification: CancellationToken,
}

impl ProcessorFinalized {
    async fn finalized_head_number_and_hash(&self) -> Result<(BlockNumber, Hash)> {
        let head_hash = self
            .methods
            .chain_get_finalized_head()
            .await
            .context("failed to get the finalized head hash")?;
        let head = self
            .methods
            .chain_get_block(Some(head_hash))
            .await
            .context("failed to get the finalized head")?
            .context("received nothing after requesting the finalized head")?;

        Ok((head.block.header.number, head_hash))
    }

    pub async fn ignite(self) -> Result<&'static str> {
        self.execute().await.or_else(|error| {
            error
                .downcast()
                .map(|Shutdown| "The RPC module is shut down.")
        })
    }

    async fn execute(mut self) -> Result<&'static str> {
        let write_tx = self.database.write()?;
        let mut write_invoices = write_tx.invoices()?;
        let (mut finalized_number, finalized_hash) = self.finalized_head_number_and_hash().await?;

        self.set_client_metadata(finalized_hash).await?;

        // TODO:
        // Design a new DB format to store unpaid accounts in a separate table.

        for invoice_result in self.database.read()?.invoices()?.try_getting_iterator()? {
            let invoice = invoice_result?;

            match invoice.1.value().status {
                InvoiceStatus::Unpaid(price) => {
                    if self
                        .balance(finalized_hash, &Account::from(*invoice.0.value()))
                        .await?
                        >= price
                    {
                        let mut changed_invoice = invoice.1.value();

                        changed_invoice.status = InvoiceStatus::Paid(price);

                        log::debug!("background scan {changed_invoice:?}");

                        write_invoices
                            .save(&Account::from(*invoice.0.value()), &changed_invoice)?;
                    }
                }
                InvoiceStatus::Paid(_) => continue,
            }
        }

        drop(write_invoices);

        write_tx.commit()?;

        let mut subscription = self.finalized_heads().await?;

        loop {
            self.process_finalized_heads(subscription, &mut finalized_number)
                .await?;

            log::warn!("Lost the connection while processing finalized heads. Retrying...");

            subscription = self
                .finalized_heads()
                .await
                .context("failed to update the subscription while processing finalized heads")?;
        }
    }

    async fn process_skipped(
        &self,
        next_unscanned: &mut BlockNumber,
        head: BlockNumber,
    ) -> Result<()> {
        for skipped_number in *next_unscanned..head {
            if self.shutdown_notification.is_cancelled() {
                return Err(Shutdown.into());
            }

            let skipped_hash = self
                .methods
                .chain_get_block_hash(Some(skipped_number.into()))
                .await
                .context("failed to get the hash of a skipped block")?
                .context("received nothing after requesting the hash of a skipped block")?;

            self.process_block(skipped_number, skipped_hash).await?;
        }

        *next_unscanned = head;

        Ok(())
    }

    async fn process_finalized_heads(
        &mut self,
        mut subscription: RpcSubscription<<RuntimeConfig as Config>::Header>,
        next_unscanned: &mut BlockNumber,
    ) -> Result<()> {
        loop {
            tokio::select! {
                biased;
                () = self.shutdown_notification.cancelled() => {
                    return Err(Shutdown.into());
                }
                head_result_option = subscription.next() => {
                    if let Some(head_result) = head_result_option {
                        let head = head_result.context(
                            "received an error from the RPC client while processing finalized heads"
                        )?;

                        self
                            .process_skipped(next_unscanned, head.number)
                            .await
                            .context("failed to process a skipped gap in the listening mode")?;
                        self.process_block(head.number, head.hash()).await?;

                        *next_unscanned = head.number
                            .checked_add(1)
                            .context(MAX_BLOCK_NUMBER_ERROR)?;
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn finalized_heads(&self) -> Result<RpcSubscription<<RuntimeConfig as Config>::Header>> {
        self.methods
            .chain_subscribe_finalized_heads()
            .await
            .context("failed to subscribe to finalized heads")
    }

    async fn process_block(&self, number: BlockNumber, hash: Hash) -> Result<()> {
        log::debug!("background block {number}");

        let block = self
            .client
            .blocks()
            .at(hash)
            .await
            .context("failed to obtain a block for processing")?;
        let events = block
            .events()
            .await
            .context("failed to obtain block events")?;

        let read_tx = self.database.read()?;
        let read_invoices = read_tx.invoices()?;

        let mut update = false;
        let mut invoices_changes = HashMap::new();

        for event_result in events.iter() {
            let event = event_result.context("failed to decode an event")?;
            let metadata = event.event_metadata();

            match (metadata.pallet.name(), &*metadata.variant.name) {
                (SYSTEM, UPDATE) => update = true,
                (BALANCES, TRANSFER) => Transfer::deserialize(
                    event
                        .field_values()
                        .context("failed to decode event's fields")?,
                )
                .context("failed to deserialize a transfer event")?
                .process(&mut invoices_changes, &read_invoices)?,
                _ => {}
            }
        }

        let write_tx = self.database.write()?;
        let mut write_invoices = write_tx.invoices()?;

        for (invoice, mut changes) in invoices_changes {
            if let InvoiceStatus::Unpaid(price) = changes.invoice.status {
                let balance = self.balance(hash, &invoice).await?;

                if balance >= price {
                    changes.invoice.status = InvoiceStatus::Paid(price);

                    write_invoices.save(&invoice, &changes.invoice)?;
                }
            }
        }

        drop(write_invoices);

        write_tx.commit()?;

        if update {
            self.set_client_metadata(hash)
                .await
                .context("failed to update metadata in the finalized client")?;

            log::info!("A metadata update has been found and applied for the finalized client.");
        }

        Ok(())
    }

    async fn set_client_metadata(&self, at: Hash) -> Result<()> {
        let metadata = fetch_metadata(&*self.backend, at)
            .await
            .context("failed to fetch metadata for the scanner client")?;

        self.client.set_metadata(metadata);

        Ok(())
    }

    async fn balance(&self, hash: Hash, account: &Account) -> Result<Balance> {
        const ACCOUNT: &str = "Account";
        const ACCOUNT_BALANCES: &str = "data";
        const FREE_BALANCE: &str = "free";

        let account_info = self
            .client
            .storage()
            .at(hash)
            .fetch_or_default(&dynamic::storage(
                SYSTEM,
                ACCOUNT,
                vec![AsRef::<[u8; 32]>::as_ref(account)],
            ))
            .await
            .context("failed to fetch account info from the chain")?
            .to_value()
            .context("failed to decode account info")?;
        let encoded_balance = account_info
            .at(ACCOUNT_BALANCES)
            .with_context(|| format!("{ACCOUNT_BALANCES} field wasn't found in account info"))?
            .at(FREE_BALANCE)
            .with_context(|| format!("{FREE_BALANCE} wasn't found in account balance info"))?;

        encoded_balance.as_u128().with_context(|| {
            format!("expected `u128` as the type of a free balance, got {encoded_balance}")
        })
    }
}

pub struct Processor {
    api: Api,
    scanner: Scanner,
    methods: Arc<LegacyRpcMethods<RuntimeConfig>>,
    database: Arc<Database>,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
    shutdown_notification: CancellationToken,
}

const UPDATE: &str = "CodeUpdated";
const TRANSFER: &str = "Transfer";

impl Processor {
    pub fn new(
        ApiConfig {
            api,
            methods,
            backend,
        }: ApiConfig,
        database: Arc<Database>,
        shutdown_notification: CancellationToken,
    ) -> Result<Self> {
        let scanner = OnlineClient::from_backend_with(
            api.genesis_hash(),
            api.runtime_version(),
            api.metadata(),
            backend.clone(),
        )
        .context("failed to initialize the scanner client")?;

        Ok(Processor {
            api: Api {
                tx: api.tx(),
                blocks: api.blocks(),
            },
            scanner: Scanner {
                blocks: scanner.blocks(),
                storage: scanner.storage(),
                client: scanner,
            },
            methods,
            database,
            shutdown_notification,
            backend,
        })
    }

    pub async fn ignite(
        self,
        latest_saved_block: Option<BlockNumber>,
        task_tracker: TaskTracker,
        error_tx: UnboundedSender<anyhow::Error>,
    ) -> Result<&'static str> {
        self.execute(latest_saved_block, task_tracker, error_tx)
            .await
            .or_else(|error| {
                error
                    .downcast()
                    .map(|Shutdown| "The RPC module is shut down.")
            })
    }

    async fn execute(
        mut self,
        latest_saved_block: Option<BlockNumber>,
        task_tracker: TaskTracker,
        error_tx: UnboundedSender<anyhow::Error>,
    ) -> Result<&'static str> {
        task_tracker.spawn(shutdown(
            ProcessorFinalized {
                database: self.database.clone(),
                client: self.scanner.client.clone(),
                backend: self.backend.clone(),
                methods: self.methods.clone(),
                shutdown_notification: self.shutdown_notification.clone(),
            }
            .ignite(),
            error_tx,
        ));

        let (mut head_number, head_hash) = self
            .finalized_head_number_and_hash()
            .await
            .context("failed to get the chain head")?;

        let mut next_unscanned_number;
        let mut subscription;

        if let Some(latest_saved) = latest_saved_block {
            let latest_saved_hash = self
                .methods
                .chain_get_block_hash(Some(latest_saved.into()))
                .await
                .context("failed to get the hash of the last saved block")?
                .context("received nothing after requesting the hash of the last saved block")?;

            self.set_scanner_metadata(latest_saved_hash).await?;

            next_unscanned_number = latest_saved
                .checked_add(1)
                .context(MAX_BLOCK_NUMBER_ERROR)?;

            let mut unscanned_amount = head_number.saturating_sub(next_unscanned_number);

            if unscanned_amount >= SCANNER_TO_LISTENER_SWITCH_POINT {
                log::info!(
                    "Detected {unscanned_amount} unscanned blocks! Catching up may take a while."
                );

                while unscanned_amount >= SCANNER_TO_LISTENER_SWITCH_POINT {
                    self.process_skipped(&mut next_unscanned_number, head_number)
                        .await
                        .context("failed to process a skipped gap in the scanning mode")?;

                    (head_number, _) = self
                        .finalized_head_number_and_hash()
                        .await
                        .context("failed to get a new chain head")?;
                    unscanned_amount = head_number.saturating_sub(next_unscanned_number);
                }

                log::info!(
                    "Scanning of skipped blocks has been completed! Switching to the listening mode..."
                );
            }

            subscription = self.finalized_heads().await?;
        } else {
            self.set_scanner_metadata(head_hash).await?;

            next_unscanned_number = head_number.checked_add(1).context(MAX_BLOCK_NUMBER_ERROR)?;
            subscription = self.finalized_heads().await?;
        }

        // Skip all already scanned blocks in cases like the first startup (we always skip the first
        // block to fetch right metadata), an instant daemon restart, or a connection to a lagging
        // endpoint.
        'skipping: loop {
            loop {
                tokio::select! {
                    biased;
                    () = self.shutdown_notification.cancelled() => {
                        return Err(Shutdown.into());
                    }
                    header_result_option = subscription.next() => {
                        if let Some(header_result) = header_result_option {
                            let header = header_result.context(
                                "received an error from the RPC client while skipping saved finalized heads"
                            )?;

                            if header.number >= next_unscanned_number {
                                break 'skipping;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }

            log::warn!("Lost the connection while skipping already scanned blocks. Retrying...");

            subscription = self
                .finalized_heads()
                .await
                .context("failed to update the subscription while skipping scanned blocks")?;
        }

        loop {
            self.process_finalized_heads(subscription, &mut next_unscanned_number)
                .await?;

            log::warn!("Lost the connection while processing finalized heads. Retrying...");

            subscription = self
                .finalized_heads()
                .await
                .context("failed to update the subscription while processing finalized heads")?;
        }
    }

    async fn finalized_head_number_and_hash(&self) -> Result<(BlockNumber, Hash)> {
        let head_hash = self
            .methods
            .chain_get_finalized_head()
            .await
            .context("failed to get the finalized head hash")?;
        let head = self
            .methods
            .chain_get_block(Some(head_hash))
            .await
            .context("failed to get the finalized head")?
            .context("received nothing after requesting the finalized head")?;

        Ok((head.block.header.number, head_hash))
    }

    async fn set_scanner_metadata(&self, at: Hash) -> Result<()> {
        let metadata = fetch_metadata(&*self.backend, at)
            .await
            .context("failed to fetch metadata for the scanner client")?;

        self.scanner.client.set_metadata(metadata);

        Ok(())
    }

    async fn finalized_heads(&self) -> Result<RpcSubscription<<RuntimeConfig as Config>::Header>> {
        self.methods
            .chain_subscribe_finalized_heads()
            .await
            .context("failed to subscribe to finalized heads")
    }

    async fn process_skipped(
        &self,
        next_unscanned: &mut BlockNumber,
        head: BlockNumber,
    ) -> Result<()> {
        for skipped_number in *next_unscanned..head {
            if self.shutdown_notification.is_cancelled() {
                return Err(Shutdown.into());
            }

            let skipped_hash = self
                .methods
                .chain_get_block_hash(Some(skipped_number.into()))
                .await
                .context("failed to get the hash of a skipped block")?
                .context("received nothing after requesting the hash of a skipped block")?;

            self.process_block(skipped_number, skipped_hash).await?;
        }

        *next_unscanned = head;

        Ok(())
    }

    async fn process_finalized_heads(
        &mut self,
        mut subscription: RpcSubscription<<RuntimeConfig as Config>::Header>,
        next_unscanned: &mut BlockNumber,
    ) -> Result<()> {
        loop {
            tokio::select! {
                biased;
                () = self.shutdown_notification.cancelled() => {
                    return Err(Shutdown.into());
                }
                head_result_option = subscription.next() => {
                    if let Some(head_result) = head_result_option {
                        let head = head_result.context(
                            "received an error from the RPC client while processing finalized heads"
                        )?;

                        self
                            .process_skipped(next_unscanned, head.number)
                            .await
                            .context("failed to process a skipped gap in the listening mode")?;
                        self.process_block(head.number, head.hash()).await?;

                        *next_unscanned = head.number
                            .checked_add(1)
                            .context(MAX_BLOCK_NUMBER_ERROR)?;
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn process_block(&self, number: BlockNumber, hash: Hash) -> Result<()> {
        log::info!("Processing the block: {number}.");

        let block = self
            .scanner
            .blocks
            .at(hash)
            .await
            .context("failed to obtain a block for processing")?;
        let events = block
            .events()
            .await
            .context("failed to obtain block events")?;

        let read_tx = self.database.read()?;
        let read_invoices = read_tx.invoices()?;

        let mut update = false;
        let mut invoices_changes = HashMap::new();

        for event_result in events.iter() {
            let event = event_result.context("failed to decode an event")?;
            let metadata = event.event_metadata();

            match (metadata.pallet.name(), &*metadata.variant.name) {
                (SYSTEM, UPDATE) => update = true,
                (BALANCES, TRANSFER) => Transfer::deserialize(
                    event
                        .field_values()
                        .context("failed to decode event's fields")?,
                )
                .context("failed to deserialize a transfer event")?
                .process(&mut invoices_changes, &read_invoices)?,
                _ => {}
            }
        }

        for (invoice, changes) in invoices_changes {
            let price = changes.invoice.price_as_u128();

            self.process_unpaid(&block, changes, hash, invoice, price)
                .await
                .context("failed to process an unpaid invoice")?;
        }

        if update {
            self.set_scanner_metadata(hash)
                .await
                .context("failed to update metadata in the scanner client")?;

            log::info!("A metadata update has been found and applied for the scanner client.");
        }

        let write_tx = self.database.write()?;

        write_tx.root()?.save_last_block(number)?;
        write_tx.commit()?;

        Ok(())
    }

    async fn balance(&self, hash: Hash, account: &Account) -> Result<Balance> {
        const ACCOUNT: &str = "Account";
        const ACCOUNT_BALANCES: &str = "data";
        const FREE_BALANCE: &str = "free";

        let account_info = self
            .scanner
            .storage
            .at(hash)
            .fetch_or_default(&dynamic::storage(
                SYSTEM,
                ACCOUNT,
                vec![AsRef::<[u8; 32]>::as_ref(account)],
            ))
            .await
            .context("failed to fetch account info from the chain")?
            .to_value()
            .context("failed to decode account info")?;
        let encoded_balance = account_info
            .at(ACCOUNT_BALANCES)
            .with_context(|| format!("{ACCOUNT_BALANCES} field wasn't found in account info"))?
            .at(FREE_BALANCE)
            .with_context(|| format!("{FREE_BALANCE} wasn't found in account balance info"))?;

        encoded_balance.as_u128().with_context(|| {
            format!("expected `u128` as the type of a free balance, got {encoded_balance}")
        })
    }

    async fn batch_transfer(
        &self,
        nonce: Nonce,
        block_hash_count: BlockNumber,
        signer: &PairSigner<RuntimeConfig, Pair>,
        transfers: Vec<Value>,
    ) -> Result<SubmittableExtrinsic<RuntimeConfig, OnlineClient>> {
        const FORCE_BATCH: &str = "force_batch";

        let call = dynamic::tx(UTILITY, FORCE_BATCH, vec![Value::from(transfers)]);
        let (number, hash) = self
            .finalized_head_number_and_hash()
            .await
            .context("failed to get the chain head while constructing a transaction")?;
        let extensions = (
            (),
            (),
            (),
            (),
            CheckMortalityParams::mortal(block_hash_count.into(), number.into(), hash),
            ChargeTransactionPaymentParams::no_tip(),
        );

        self.api
            .tx
            .create_signed_with_nonce(&call, signer, nonce, extensions)
            .context("failed to create a transfer transaction")
    }

    async fn current_nonce(&self, account: &Account) -> Result<Nonce> {
        self.api
            .blocks
            .at(fetch_best_block(&self.methods).await?)
            .await
            .context("failed to obtain the best block for fetching an account nonce")?
            .account_nonce(account)
            .await
            .context("failed to fetch an account nonce by the API client")
    }

    async fn process_unpaid(
        &self,
        block: &Block<RuntimeConfig, OnlineClient>,
        mut changes: InvoiceChanges,
        hash: Hash,
        invoice: Account,
        price: Balance,
    ) -> Result<()> {
        let balance = self.balance(hash, &invoice).await?;

        if let Some(_remaining) = balance.checked_sub(price) {
            changes.invoice.status = InvoiceStatus::Paid(price);

            let block_nonce = block
                .account_nonce(&invoice)
                .await
                .context(BLOCK_NONCE_ERROR)?;
            let current_nonce = self.current_nonce(&invoice).await?;

            if current_nonce <= block_nonce {
                let properties = self.database.properties().await;
                let block_hash_count = properties.block_hash_count;
                let signer = changes.invoice.signer(self.database.pair())?;

                let transfers = vec![construct_transfer(&changes.invoice.recipient, price)];
                let tx = self
                    .batch_transfer(current_nonce, block_hash_count, &signer, transfers.clone())
                    .await?;
                //let mut fee = calculate_estimate_fee(&tx).await?;
                /*
                    if false //let Some(a) = (fee + properties.existential_deposit + price).checked_sub(balance)
                    {
                        let price_mod = price - a;

                        transfers = vec![construct_transfer(&changes.invoice.recipient, price_mod)];
                        tx = self
                            .batch_transfer(current_nonce, block_hash_count, &signer, transfers.clone())
                            .await?;

                        self.methods
                            .author_submit_extrinsic(tx.encoded())
                            .await
                            .context("failed to submit an extrinsic")?;
                    } else if let Some((account, amount)) = changes.incoming.into_iter().next() {
                        let mut temp_transfers = transfers.clone();

                        temp_transfers.push(construct_transfer(&account, amount));
                        tx = self
                            .batch_transfer(
                                current_nonce,
                                block_hash_count,
                                &signer,
                                temp_transfers.clone(),
                            )
                            .await?;
                        fee = calculate_estimate_fee(&tx).await?;

                        if let Some(a) =
                            (fee + properties.existential_deposit + amount).checked_sub(remaining)
                        {
                            let amount_mod = amount - a;

                            transfers.push(construct_transfer(&account, amount_mod));
                            tx = self
                                .batch_transfer(
                                    current_nonce,
                                    block_hash_count,
                                    &signer,
                                    transfers.clone(),
                                )
                                .await?;

                            self.methods
                                .author_submit_extrinsic(tx.encoded())
                                .await
                                .context("failed to submit an extrinsic")?;
                        } else {
                            self.methods
                                .author_submit_extrinsic(tx.encoded())
                                .await
                                .context("failed to submit an extrinsic")?;
                        }
                    } else {
                */
                self.methods
                    .author_submit_extrinsic(tx.encoded())
                    .await
                    .context("failed to submit an extrinsic")?;

                //}
            }
        }

        Ok(())
    }
}

fn construct_transfer(to: &Account, _amount: Balance) -> Value {
    const TRANSFER_ALL: &str = "transfer_all";

    dynamic::tx(
        BALANCES,
        TRANSFER_ALL,
        vec![scale_value::value!(Id(Value::from_bytes(to))), false.into()],
    )
    .into_value()
}

/*
async fn calculate_estimate_fee(
    extrinsic: &SubmittableExtrinsic<RuntimeConfig, OnlineClient>,
) -> Result<Balance> {
    extrinsic
        .partial_fee_estimate()
        .await
        .map(|f| f * 2)
        .context("failed to obtain a transfer's estimate fee")
}
*/

struct InvoiceChanges {
    invoice: Invoice,
    incoming: HashMap<Account, Balance>,
}

#[derive(Deserialize)]
struct Transfer {
    // The implementation of `Deserialize` for `AccountId32` works only with strings.
    #[serde(deserialize_with = "account_deserializer")]
    from: AccountId32,
    #[serde(deserialize_with = "account_deserializer")]
    to: AccountId32,
    amount: Balance,
}

fn account_deserializer<'de, D>(deserializer: D) -> Result<AccountId32, D::Error>
where
    D: Deserializer<'de>,
{
    <([u8; 32],)>::deserialize(deserializer).map(|address| AccountId32::new(address.0))
}

impl Transfer {
    fn process(
        self,
        invoices_changes: &mut HashMap<Account, InvoiceChanges>,
        invoices: &ReadInvoices<'_>,
    ) -> Result<()> {
        if self.from == self.to || self.amount == 0 {
            return Ok(());
        }

        match invoices_changes.entry(self.to) {
            Entry::Occupied(entry) => {
                entry
                    .into_mut()
                    .incoming
                    .entry(self.from)
                    .and_modify(|amount| *amount = amount.saturating_add(self.amount))
                    .or_insert(self.amount);
            }
            Entry::Vacant(entry) => {
                if let (None, Some(encoded_invoice)) =
                    (invoices.get(&self.from)?, invoices.get(entry.key())?)
                {
                    entry.insert(InvoiceChanges {
                        invoice: encoded_invoice.value(),
                        incoming: [(self.from, self.amount)].into(),
                    });
                }
            }
        }

        Ok(())
    }
}
