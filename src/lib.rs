use anyhow::{Context, Error, Result};
use database::Database;
use env_logger::{Builder, Env};
use environment_variables::{DATABASE, DECIMALS, DESTINATION, HOST, IN_MEMORY_DB, LOG, LOG_STYLE, OVERRIDE_RPC, RPC, SEED};
use log::LevelFilter;
use rpc::Processor;
use std::{
    env::{self, VarError},
    future::Future,
};
use subxt::{
    config::{
        signed_extensions::{
            AnyOf, ChargeTransactionPayment, CheckGenesis, CheckMortality, CheckNonce,
            CheckSpecVersion, CheckTxVersion,
        },
        Header,
    },
    ext::sp_core::{crypto::AccountId32, Pair},
    Config, PolkadotConfig,
};
use tokio::{
    signal,
    sync::mpsc::{self, UnboundedSender},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

mod database;
mod rpc;

pub mod server;

pub mod environment_variables {
    pub const HOST: &str = "KALATORI_HOST";
    pub const SEED: &str = "KALATORI_SEED";
    pub const LOG: &str = "KALATORI_LOG";
    pub const LOG_STYLE: &str = "KALATORI_LOG_STYLE";
    pub const DATABASE: &str = "KALATORI_DATABASE";
    pub const RPC: &str = "KALATORI_RPC";
    pub const OVERRIDE_RPC: &str = "KALATORI_OVERRIDE_RPC";
    pub const IN_MEMORY_DB: &str = "KALATORI_IN_MEMORY_DB";
    pub const DECIMALS: &str = "KALATORI_DECIMALS";
    pub const DESTINATION: &str = "KALATORI_DESTINATION";
}

pub const DEFAULT_RPC: &str = "wss://westend-rpc.polkadot.io";
pub const DEFAULT_DATABASE: &str = "database.redb";
pub const DATABASE_VERSION: Version = 0;

// https://github.com/paritytech/polkadot-sdk/blob/7c9fd83805cc446983a7698c7a3281677cf655c8/substrate/client/cli/src/config.rs#L50
const SCANNER_TO_LISTENER_SWITCH_POINT: BlockNumber = 512;

type OnlineClient = subxt::OnlineClient<RuntimeConfig>;
type Account = <RuntimeConfig as Config>::AccountId;
type BlockNumber = <<RuntimeConfig as Config>::Header as Header>::Number;
type Hash = <RuntimeConfig as Config>::Hash;
// https://github.com/paritytech/polkadot-sdk/blob/a3dc2f15f23b3fd25ada62917bfab169a01f2b0d/substrate/bin/node/primitives/src/lib.rs#L43
type Balance = u128;
// https://github.com/paritytech/subxt/blob/f06a95d687605bf826db9d83b2932a73a57b169f/subxt/src/config/signed_extensions.rs#L71
type Nonce = u64;
// https://github.com/dtolnay/semver/blob/f9cc2df9415c880bd3610c2cdb6785ac7cad31ea/src/lib.rs#L163-L165
type Version = u64;
// https://github.com/serde-rs/json/blob/0131ac68212e8094bd14ee618587d731b4f9a68b/src/number.rs#L29
type Decimals = u64;

struct RuntimeConfig;

impl Config for RuntimeConfig {
    type Hash = <PolkadotConfig as Config>::Hash;
    type AccountId = AccountId32;
    type Address = <PolkadotConfig as Config>::Address;
    type Signature = <PolkadotConfig as Config>::Signature;
    type Hasher = <PolkadotConfig as Config>::Hasher;
    type Header = <PolkadotConfig as Config>::Header;
    type ExtrinsicParams = AnyOf<
        Self,
        (
            CheckTxVersion,
            CheckSpecVersion,
            CheckNonce,
            CheckGenesis<Self>,
            CheckMortality<Self>,
            ChargeTransactionPayment,
        ),
    >;
    type AssetId = <PolkadotConfig as Config>::AssetId;
}

#[doc(hidden)]
#[tokio::main]
#[allow(clippy::too_many_lines)] // FIXME: Better structured main() is needed
pub async fn main() -> Result<()> {
    let mut builder = Builder::new();

    if cfg!(debug_assertions) {
        builder.filter_level(LevelFilter::Debug)
    } else {
        builder
            .filter_level(LevelFilter::Off)
            .filter_module(server::MODULE, LevelFilter::Info)
            .filter_module(rpc::MODULE, LevelFilter::Info)
            .filter_module(database::MODULE, LevelFilter::Info)
            .filter_module(env!("CARGO_PKG_NAME"), LevelFilter::Info)
    }
    .parse_env(Env::new().filter(LOG).write_style(LOG_STYLE))
    .init();

    let host = env::var(HOST)
        .with_context(|| format!("`{HOST}` isn't set"))?
        .parse()
        .with_context(|| format!("failed to convert `{HOST}` to a socket address"))?;

    let pair = Pair::from_string(
        &env::var(SEED).with_context(|| format!("`{SEED}` isn't set"))?,
        None,
    )
    .with_context(|| format!("failed to generate a key pair from `{SEED}`"))?;

    let endpoint = env::var(RPC).or_else(|error| {
        if error == VarError::NotPresent {
            log::debug!(
                "`{RPC}` isn't present, using the default value instead: \"{DEFAULT_RPC}\"."
            );

            Ok(DEFAULT_RPC.into())
        } else {
            Err(error).context(format!("failed to read `{RPC}`"))
        }
    })?;

    let override_rpc = env::var_os(OVERRIDE_RPC).is_some();

    let database_path = if env::var_os(IN_MEMORY_DB).is_none() {
        Some(env::var(DATABASE).or_else(|error| {
            if error == VarError::NotPresent {
                log::debug!(
                    "`{DATABASE}` isn't present, using the default value instead: \"{DEFAULT_DATABASE}\"."
                );

                Ok(DEFAULT_DATABASE.into())
            } else {
                Err(error).context(format!("failed to read `{DATABASE}`"))
            }
        })?)
    } else {
        if env::var_os(DATABASE).is_some() {
            log::warn!(
                "`{IN_MEMORY_DB}` is set along with `{DATABASE}`. The latter will be ignored."
            );
        }

        None
    };

    let decimals = match env::var(DECIMALS) {
        Ok(decimals) => decimals
            .parse()
            .map(Some)
            .with_context(|| format!("failed to convert `{DECIMALS}` to a socket address")),
        Err(VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).context(format!("failed to read `{DECIMALS}`")),
    }?;

    let destination = match env::var(DESTINATION) {
        Ok(destination) => Ok(Some(
            AccountId32::try_from(hex::decode(&destination[2..])?.as_ref())
                .map_err(|()| anyhow::anyhow!("unknown destination address length"))?,
        )),
        Err(VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).context(format!("failed to read `{DESTINATION}`")),
    }?;

    log::info!(
        "Kalatori {} by {} is starting...",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS")
    );

    let shutdown_notification = CancellationToken::new();
    let (error_tx, mut error_rx) = mpsc::unbounded_channel();

    let (api_config, endpoint_properties, updater) =
        rpc::prepare(endpoint, decimals, shutdown_notification.clone())
            .await
            .context("failed to prepare the node module")?;

    let (database, last_saved_block) = Database::initialise(
        database_path,
        override_rpc,
        pair,
        endpoint_properties,
        destination,
    )
    .context("failed to initialise the database module")?;

    let processor = Processor::new(api_config, database.clone(), shutdown_notification.clone())
        .context("failed to initialise the RPC module")?;

    let server = server::new(shutdown_notification.clone(), host, database)
        .await
        .context("failed to initialise the server module")?;

    let task_tracker = TaskTracker::new();

    task_tracker.close();

    task_tracker.spawn(shutdown(
        shutdown_listener(shutdown_notification.clone()),
        error_tx.clone(),
    ));
    task_tracker.spawn(shutdown(updater.ignite(), error_tx.clone()));
    task_tracker.spawn(shutdown(
        processor.ignite(last_saved_block, task_tracker.clone(), error_tx.clone()),
        error_tx,
    ));
    task_tracker.spawn(server);

    while let Some(error) = error_rx.recv().await {
        log::error!("Received a fatal error!\n{error:?}");

        if !shutdown_notification.is_cancelled() {
            log::info!("Initialising the shutdown...");

            shutdown_notification.cancel();
        }
    }

    task_tracker.wait().await;

    log::info!("Goodbye!");

    Ok(())
}

async fn shutdown_listener(shutdown_notification: CancellationToken) -> Result<&'static str> {
    tokio::select! {
        biased;
        signal = signal::ctrl_c() => {
            signal.context("failed to listen for the shutdown signal")?;

            // Print shutdown log messages on the next line after the Control-C command.
            println!();

            log::info!("Received the shutdown signal. Initialising the shutdown...");

            shutdown_notification.cancel();

            Ok("The shutdown signal listener is shut down.")
        }
        () = shutdown_notification.cancelled() => {
            Ok("The shutdown signal listener is shut down.")
        }
    }
}

async fn shutdown(
    task: impl Future<Output = Result<&'static str>>,
    error_tx: UnboundedSender<Error>,
) {
    match task.await {
        Ok(shutdown_message) => log::info!("{shutdown_message}"),
        Err(error) => error_tx.send(error).unwrap(),
    }
}
