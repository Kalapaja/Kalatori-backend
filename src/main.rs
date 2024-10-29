use clap::Parser;
use std::process::ExitCode;
use substrate_crypto_light::common::{AccountId32, AsBase58};
use tokio::{runtime::Runtime, sync::oneshot};
use tokio_util::sync::CancellationToken;
use tracing::Level;
use utils::{
    logger,
    shutdown::{self, ShutdownNotification, ShutdownOutcome},
    task_tracker::TaskTracker,
};

mod arguments;
mod callback;
mod chain;
mod database;
mod definitions;
mod error;
mod handlers;
mod server;
mod signer;
mod state;
mod utils;

use arguments::{CliArgs, Config, SeedEnvVars, DATABASE_DEFAULT};
use chain::ChainManager;
use database::ConfigWoChains;
use error::{Error, PrettyCause};
use signer::Signer;
use state::State;

fn main() -> ExitCode {
    let shutdown_notification = ShutdownNotification::new();

    // Sets the panic hook to print directly to the standard error because the logger isn't
    // initialized yet.
    shutdown::set_panic_hook(|panic| eprintln!("{panic}"), shutdown_notification.clone());

    if let Err(error) = try_main(shutdown_notification.clone()) {
        // TODO: https://github.com/rust-lang/rust/issues/92698
        // An equilibristic to conditionally print an error message without storing it as `String`
        // on the heap.
        let print = |message| {
            if tracing::event_enabled!(Level::ERROR) {
                tracing::error!("{message}");
            } else {
                eprintln!("{message}");
            }
        };

        print(format_args!(
            "Badbye! The daemon's got an error during the initialization:{}",
            error.pretty_cause()
        ));

        ExitCode::FAILURE
    } else {
        match *shutdown_notification.outcome.read_blocking() {
            ShutdownOutcome::UserRequested => {
                tracing::info!("Goodbye!");

                ExitCode::SUCCESS
            }
            ShutdownOutcome::UnrecoverableError { panic } => {
                tracing::error!(
                    "Badbye! The daemon's shut down with errors{}.",
                    if panic { " due to internal bugs" } else { "" }
                );

                ExitCode::FAILURE
            }
        }
    }
}

fn try_main(shutdown_notification: ShutdownNotification) -> Result<(), Error> {
    let cli_args = CliArgs::parse();

    logger::initialize(cli_args.log)?;
    shutdown::set_panic_hook(
        |panic| tracing::error!("{panic}"),
        shutdown_notification.clone(),
    );

    tracing::info!("Kalatori {} is starting...", env!("CARGO_PKG_VERSION"));

    let seed_env_vars = SeedEnvVars::parse()?;
    let config = Config::parse(cli_args.config)?;

    Runtime::new()
        .map_err(Error::Runtime)?
        .block_on(async_try_main(
            shutdown_notification,
            cli_args.recipient,
            cli_args.remark,
            config,
            seed_env_vars,
        ))
}

async fn async_try_main(
    shutdown_notification: ShutdownNotification,
    recipient_string: String,
    remark: Option<String>,
    config: Config,
    seed_env_vars: SeedEnvVars,
) -> Result<(), Error> {
    let database_path = if config.in_memory_db {
        if config.database.is_some() {
            tracing::warn!(
                "`database` is set in the config but ignored because `in_memory_db` is \"true\""
            );
        }

        None
    } else {
        Some(config.database.unwrap_or_else(|| DATABASE_DEFAULT.into()))
    };

    // Start services

    let (task_tracker, error_rx) = TaskTracker::new();

    let recipient = AccountId32::from_base58_string(&recipient_string)
        .map_err(Error::RecipientAccount)?
        .0;

    let signer = Signer::init(recipient, task_tracker.clone(), seed_env_vars.seed)?;

    let db =
        database::Database::init(database_path, task_tracker.clone(), config.account_lifetime)?;

    let instance_id = db.initialize_server_info().await?;

    let (cm_tx, cm_rx) = oneshot::channel();

    let state = State::initialise(
        signer.interface(),
        ConfigWoChains {
            recipient,
            debug: config.debug,
            remark,
            //depth: config.depth,
        },
        db,
        cm_rx,
        instance_id,
        task_tracker.clone(),
        shutdown_notification.token.clone(),
    )?;

    cm_tx
        .send(ChainManager::ignite(
            config.chain,
            state.interface(),
            signer.interface(),
            task_tracker.clone(),
            shutdown_notification.token.clone(),
        )?)
        .map_err(|_| Error::Fatal)?;

    let server = server::new(
        shutdown_notification.token.clone(),
        config.host,
        state.interface(),
    )
    .await?;

    task_tracker.spawn("the server module", server);

    let shutdown_completed = CancellationToken::new();
    let mut shutdown_listener = tokio::spawn(shutdown::listener(
        shutdown_notification.token.clone(),
        shutdown_completed.clone(),
    ));

    tracing::info!("The initialization has been completed.");

    // Start the main loop and wait for it to gracefully end or the early termination signal.
    tokio::select! {
        biased;
        () = task_tracker.wait_and_shutdown(error_rx, shutdown_notification) => {
            shutdown_completed.cancel();

            shutdown_listener.await
        }
        shutdown_listener_result = &mut shutdown_listener => shutdown_listener_result
    }
    .expect("shutdown listener shouldn't panic")
}
