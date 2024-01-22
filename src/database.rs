use crate::{
    rpc::{ChainProperties, EndpointProperties},
    Account, Balance, BlockNumber, RuntimeConfig, Version, DATABASE_VERSION, OVERRIDE_RPC, SEED,
};
use anyhow::{Context, Result};
use redb::{
    backends::InMemoryBackend, AccessGuard, ReadOnlyTable, ReadableTable, RedbValue, Table,
    TableDefinition, TableHandle, TypeName,
};
use std::sync::Arc;
use subxt::{
    ext::{
        codec::{Compact, Decode, Encode},
        sp_core::{
            crypto::Ss58Codec,
            sr25519::{Pair, Public},
            DeriveJunction, Pair as _,
        },
    },
    tx::PairSigner,
};
use tokio::{
    sync::{RwLock, RwLockReadGuard},
    task,
};

type Order = [u8; 32];

pub const MODULE: &str = module_path!();

// Tables

const ROOT: TableDefinition<'_, &str, Vec<u8>> = TableDefinition::new("root");
const INVOICES: TableDefinition<'_, &[u8; 32], Invoice> = TableDefinition::new("invoices");

// Keys

// The database version must be stored in a separate slot to be used by the not implemented yet
// database migration logic.
const DB_VERSION_KEY: &str = "db_version";
const DAEMON_INFO: &str = "daemon_info";
const LAST_BLOCK: &str = "last_block";

// Slots

#[derive(Debug, Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
pub struct Invoice {
    pub recipient: Account,
    pub order: Order,
    pub status: InvoiceStatus,
}

impl Invoice {
    pub fn signer(&self, pair: &Pair) -> Result<PairSigner<RuntimeConfig, Pair>> {
        let invoice_pair = pair
            .derive(
                [self.recipient.clone().into(), self.order]
                    .map(DeriveJunction::Hard)
                    .into_iter(),
                None,
            )
            .context("failed to derive an invoice key pair")?
            .0;

        Ok(PairSigner::new(invoice_pair))
    }
}

#[derive(Debug, Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
pub enum InvoiceStatus {
    Unpaid(Balance),
    Paid(Balance),
}

impl RedbValue for Invoice {
    type SelfType<'a> = Self;

    type AsBytes<'a> = Vec<u8>;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(mut data: &'a [u8]) -> Self::SelfType<'_>
    where
        Self: 'a,
    {
        Self::decode(&mut data).unwrap()
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'a>) -> Self::AsBytes<'_> {
        value.encode()
    }

    fn type_name() -> TypeName {
        TypeName::new(stringify!(Invoice))
    }
}

#[derive(Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
struct DaemonInfo {
    rpc: String,
    key: Public,
}

pub struct Database {
    database: redb::Database,
    properties: Arc<RwLock<ChainProperties>>,
    pair: Pair,
    rpc: String,
}

impl Database {
    pub fn initialise(
        path_option: Option<String>,
        override_rpc: bool,
        pair: Pair,
        EndpointProperties { url, chain }: EndpointProperties,
    ) -> Result<(Arc<Self>, Option<BlockNumber>)> {
        let public = pair.public();
        let public_formatted = public.to_ss58check_with_version(
            task::block_in_place(|| chain.blocking_read()).address_format,
        );
        let given_rpc = url.get();

        let mut database = if let Some(path) = path_option {
            log::info!("Creating/Opening the database at \"{path}\".");

            redb::Database::create(path)
        } else {
            log::warn!(
                "The in-memory backend for the database is selected. All saved data will be deleted after the shutdown!"
            );

            redb::Database::builder().create_with_backend(InMemoryBackend::new())
        }.context("failed to create/open the database")?;

        let tx = database
            .begin_write()
            .context("failed to begin a write transaction")?;
        let mut table = tx
            .open_table(ROOT)
            .with_context(|| format!("failed to open the `{}` table", ROOT.name()))?;
        drop(
            tx.open_table(INVOICES)
                .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))?,
        );

        let last_block = match (
            get_slot(&table, DB_VERSION_KEY)?,
            get_slot(&table, DAEMON_INFO)?,
            get_slot(&table, LAST_BLOCK)?,
        ) {
            (None, None, None) => {
                table
                    .insert(
                        DB_VERSION_KEY,
                        Compact(DATABASE_VERSION).encode(),
                    )
                    .context("failed to insert the database version")?;
                insert_daemon_info(&mut table, given_rpc.clone(), public)?;

                None
            }
            (Some(encoded_db_version), Some(daemon_info), last_block_option) => {
                let Compact::<Version>(db_version) =
                    decode_slot(encoded_db_version, DB_VERSION_KEY)?;
                let DaemonInfo { rpc: db_rpc, key } = decode_slot(daemon_info, DAEMON_INFO)?;

                if db_version != DATABASE_VERSION {
                    anyhow::bail!(
                        "database contains an unsupported database version (\"{db_version}\"), expected \"{DATABASE_VERSION}\""
                    );
                }

                if public != key {
                    anyhow::bail!(
                        "public key from `{SEED}` doesn't equal the one from the database (\"{public_formatted}\")"
                    );
                }

                if given_rpc != db_rpc {
                    if override_rpc {
                        log::warn!(
                            "The saved RPC endpoint ({db_rpc:?}) differs from the given one ({given_rpc:?}) and will be overwritten by it because `{OVERRIDE_RPC}` is set."
                        );

                        insert_daemon_info(&mut table, given_rpc.clone(), public)?;
                    } else {
                        anyhow::bail!(
                            "database contains a different RPC endpoint address ({db_rpc:?}), expected {given_rpc:?}"
                        );
                    }
                } else if override_rpc {
                    log::warn!(
                        "`{OVERRIDE_RPC}` is set but the saved RPC endpoint ({db_rpc:?}) equals to the given one."
                    );
                }

                if let Some(encoded_last_block) = last_block_option {
                    Some(decode_slot::<Compact<BlockNumber>>(encoded_last_block, LAST_BLOCK)?.0)
                } else {
                    None
                }
            }
            _ => anyhow::bail!(
                "database was found but it doesn't contain `{DB_VERSION_KEY:?}` and/or `{DAEMON_INFO:?}`, maybe it was created by another program"
            ),
        };

        drop(table);

        tx.commit().context("failed to commit a transaction")?;

        let compacted = database
            .compact()
            .context("failed to compact the database")?;

        if compacted {
            log::debug!("The database was successfully compacted.")
        } else {
            log::debug!("The database doesn't need the compaction.")
        }

        log::info!("Public key from the given seed: \"{public_formatted}\".");

        Ok((
            Arc::new(Self {
                database,
                properties: chain,
                pair,
                rpc: given_rpc,
            }),
            last_block,
        ))
    }

    pub fn rpc(&self) -> &str {
        &self.rpc
    }

    pub fn write(&self) -> Result<WriteTransaction<'_>> {
        self.database
            .begin_write()
            .map(WriteTransaction)
            .context("failed to begin a write transaction for the database")
    }

    pub fn read(&self) -> Result<ReadTransaction<'_>> {
        self.database
            .begin_read()
            .map(ReadTransaction)
            .context("failed to begin a read transaction for the database")
    }

    pub async fn properties(&self) -> RwLockReadGuard<'_, ChainProperties> {
        self.properties.read().await
    }

    pub fn pair(&self) -> &Pair {
        &self.pair
    }
}

pub struct ReadTransaction<'db>(redb::ReadTransaction<'db>);

impl ReadTransaction<'_> {
    pub fn invoices(&self) -> Result<ReadInvoices<'_>> {
        self.0
            .open_table(INVOICES)
            .map(ReadInvoices)
            .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))
    }
}

pub struct ReadInvoices<'tx>(ReadOnlyTable<'tx, &'static [u8; 32], Invoice>);

impl ReadInvoices<'_> {
    pub fn get(&self, account: &Account) -> Result<Option<AccessGuard<'_, Invoice>>> {
        self.0
            .get(AsRef::<[u8; 32]>::as_ref(account))
            .context("failed to get an invoice from the database")
    }
}

pub struct WriteTransaction<'db>(redb::WriteTransaction<'db>);

impl<'db> WriteTransaction<'db> {
    pub fn root(&self) -> Result<Root<'db, '_>> {
        self.0
            .open_table(ROOT)
            .map(Root)
            .with_context(|| format!("failed to open the `{}` table", ROOT.name()))
    }

    pub fn invoices(&self) -> Result<WriteInvoices<'db, '_>> {
        self.0
            .open_table(INVOICES)
            .map(WriteInvoices)
            .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))
    }

    pub fn commit(self) -> Result<()> {
        self.0
            .commit()
            .context("failed to commit a write transaction in the database")
    }
}

pub struct WriteInvoices<'db, 'tx>(Table<'db, 'tx, &'static [u8; 32], Invoice>);

impl WriteInvoices<'_, '_> {
    pub fn save(
        &mut self,
        account: &Account,
        invoice: &Invoice,
    ) -> Result<Option<AccessGuard<'_, Invoice>>> {
        self.0
            .insert(AsRef::<[u8; 32]>::as_ref(account), invoice)
            .context("failed to save an invoice in the database")
    }
}

pub struct Root<'db, 'tx>(Table<'db, 'tx, &'static str, Vec<u8>>);

impl Root<'_, '_> {
    pub fn save_last_block(&mut self, number: BlockNumber) -> Result<()> {
        self.0
            .insert(LAST_BLOCK, Compact(number).encode())
            .context("context")?;

        Ok(())
    }
}

fn get_slot(table: &Table<'_, '_, &str, Vec<u8>>, key: &str) -> Result<Option<Vec<u8>>> {
    table
        .get(key)
        .map(|slot_option| slot_option.map(|slot| slot.value().to_vec()))
        .with_context(|| format!("failed to get the {key:?} slot"))
}

fn decode_slot<T: Decode>(slot: Vec<u8>, key: &str) -> Result<T> {
    T::decode(&mut slot.as_ref()).with_context(|| format!("failed to decode the {key:?} slot"))
}

fn insert_daemon_info(
    table: &mut Table<'_, '_, &str, Vec<u8>>,
    rpc: String,
    key: Public,
) -> Result<()> {
    table
        .insert(DAEMON_INFO, DaemonInfo { rpc, key }.encode())
        .map(|_| ())
        .context("failed to insert the daemon info")
}
