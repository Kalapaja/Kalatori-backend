use crate::{
    definitions::{api_v2::Timestamp, Chain},
    error::{Error, SeedEnvError},
    utils::logger,
};
use ahash::AHashMap;
use clap::{Arg, ArgAction, Parser};
use serde::Deserialize;
use std::{
    env, fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str,
};
use toml_edit::de;

shadow_rs::shadow!(shadow);

use shadow::{BUILD_TIME_3339, RUST_VERSION, SHORT_COMMIT};

macro_rules! env_var_prefix {
    ($var:literal) => {
        concat!("KALATORI_", $var)
    };
}

pub const SEED: &str = env_var_prefix!("SEED");
pub const OLD_SEED: &str = env_var_prefix!("OLD_SEED_");

const SOCKET_DEFAULT: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 16726);
pub const DATABASE_DEFAULT: &str = "kalatori.db";

#[derive(Parser)]
#[command(
    about,
    disable_help_flag(true),
    arg(
        Arg::new("help")
        .short('h')
        .long("help")
        .action(ArgAction::Help)
        .help("Print this text.")
    ),
    after_help(concat!(
    "`SEED` is a required environment variable.\n\nMore documentation can be found at ",
    env!("CARGO_PKG_REPOSITORY"),
    ".\n\nCopyright (C) 2024 ",
    clap::crate_authors!()
    )),
    disable_version_flag(true),
    version,
    arg(
        Arg::new("version")
        .short('V')
        .long("version")
        .action(ArgAction::Version)
        .help("Print the daemon version (and its build metadata).")
    ),
    long_version(shadow_rs::formatcp!(
    "{} ({SHORT_COMMIT})\n\nBuilt on {}\nwith {RUST_VERSION}.",
    clap::crate_version!(),
    shadow_rs::str_splice!(BUILD_TIME_3339, BUILD_TIME_3339.len().saturating_sub(6).., "Z").output,
    )),
)]
pub struct CliArgs {
    #[arg(
        short,
        long,
        env(env_var_prefix!("CONFIG")),
        value_name("PATH"),
        default_value("configs/polkadot.toml")
    )]
    pub config: String,

    #[arg(
        short,
        long,
        env(env_var_prefix!("LOG")),
        value_name("DIRECTIVES"),
        default_value(logger::default_filter()),
        default_missing_value(""),
        num_args(0..=1),
        require_equals(true),
    )]
    pub log: String,

    #[arg(long, env(env_var_prefix!("REMARK")), visible_alias("rmrk"), value_name("STRING"))]
    pub remark: Option<String>,

    #[arg(short, long, env(env_var_prefix!("RECIPIENT")), value_name("HEX/SS58 ADDRESS"))]
    pub recipient: String,
}

pub struct SeedEnvVars {
    pub seed: String,
    #[allow(dead_code)] // TODO: Do we actually need this?
    pub old_seeds: AHashMap<String, String>,
}

impl SeedEnvVars {
    pub fn parse() -> Result<Self, SeedEnvError> {
        const SEED_BYTES: &[u8] = SEED.as_bytes();

        let mut seed_option = None;
        let mut old_seeds = ahash::AHashMap::new();

        for (raw_key, raw_value) in env::vars_os() {
            match raw_key.as_encoded_bytes() {
                SEED_BYTES => {
                    // TODO: Audit that the environment access only happens in single-threaded code.
                    unsafe { env::remove_var(raw_key) };

                    seed_option = {
                        Some(
                            raw_value
                                .into_string()
                                .map_err(|_| SeedEnvError::InvalidUnicodeValue(SEED.into()))?,
                        )
                    };
                }
                raw_key_bytes => {
                    if let Some(stripped_raw_key) = raw_key_bytes.strip_prefix(OLD_SEED.as_bytes())
                    {
                        // TODO: Audit that the environment access only happens in single-threaded code.
                        unsafe { env::remove_var(&raw_key) };

                        let key = str::from_utf8(stripped_raw_key)
                            .map_err(|_| SeedEnvError::InvalidUnicodeOldSeedKey)?;
                        let value = raw_value.to_str().ok_or(SeedEnvError::InvalidUnicodeValue(
                            format!("{OLD_SEED}{key}").into(),
                        ))?;

                        old_seeds.insert(key.into(), value.into());
                    }
                }
            }
        }

        Ok(Self {
            seed: seed_option.ok_or(SeedEnvError::SeedNotPresent)?,
            old_seeds,
        })
    }
}

/// User-supplied settings through the config file.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub account_lifetime: Timestamp,
    #[serde(default = "default_host")]
    pub host: SocketAddr,
    pub database: Option<String>,
    pub debug: Option<bool>,
    #[serde(default)]
    pub in_memory_db: bool,
    pub chain: Vec<Chain>,
}

impl Config {
    pub fn parse(path: String) -> Result<Self, Error> {
        let unparsed_config =
            fs::read_to_string(&path).map_err(|e| Error::ConfigFileRead(path, e))?;

        de::from_str(&unparsed_config).map_err(Into::into)
    }
}

fn default_host() -> SocketAddr {
    SOCKET_DEFAULT
}
