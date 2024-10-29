//! Database server module
//!
//! We do not need concurrency here, as this is our actual source of truth for legally binging
//! commercial offers and contracts, hence causality is a must. Care must be taken that no threads
//! are spawned here other than main database server thread that does everything in series.

use crate::definitions::api_v2::ServerInfo;
use crate::{
    definitions::{
        api_v2::{
            CurrencyInfo, OrderCreateResponse, OrderInfo, OrderQuery, PaymentStatus, Timestamp,
            WithdrawalStatus,
        },
        Version,
    },
    error::{DbError, Error},
    utils::task_tracker::TaskTracker,
};
use codec::{Decode, Encode};
use names::{Generator, Name};
use std::time::SystemTime;
use substrate_crypto_light::common::AccountId32;
use tokio::sync::{mpsc, oneshot};

pub const MODULE: &str = module_path!();

const DB_VERSION: Version = 0;

// Tables
/*
const ROOT: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("root");
const KEYS: TableDefinition<'_, PublicSlot, U256Slot> = TableDefinition::new("keys");
const CHAINS: TableDefinition<'_, ChainHash, BlockNumber> = TableDefinition::new("chains");
const INVOICES: TableDefinition<'_, InvoiceKey, Invoice> = TableDefinition::new("invoices");
*/
const ACCOUNTS: &str = "accounts";

//type ACCOUNTS_KEY = (Option<AssetId>, Account);
//type ACCOUNTS_VALUE = InvoiceKey;

const TRANSACTIONS: &str = "transactions";

//type TRANSACTIONS_KEY = BlockNumber;
//type TRANSACTIONS_VALUE = (Account, Nonce, Transfer);

const HIT_LIST: &str = "hit_list";

//type HIT_LIST_KEY = BlockNumber;
//type HIT_LIST_VALUE = (Option<AssetId>, Account);

// `ROOT` keys

// The database version must be stored in a separate slot to be used by the not implemented yet
// database migration logic.
const DB_VERSION_KEY: &str = "db_version";
const SERVER_INFO_ID: &str = "instance_id";

const ORDERS_TABLE: &[u8] = b"orders";
const SERVER_INFO_TABLE: &[u8] = b"server_info";

// Slots

type InvoiceKey = &'static [u8];
type U256Slot = [u64; 4];
type BlockHash = [u8; 32];
type ChainHash = [u8; 32];
type PublicSlot = [u8; 32];
type BalanceSlot = u128;
type Derivation = [u8; 32];
pub type Account = [u8; 32];
/*
#[derive(Encode, Decode)]
enum ChainKind {
    Id(Vec<Compact<AssetId>>),
    MultiLocation(Vec<Compact<AssetId>>),
}

#[derive(Encode, Decode)]
struct DaemonInfo {
    chains: Vec<(String, ChainProperties)>,
    current_key: PublicSlot,
    old_keys_death_timestamps: Vec<(PublicSlot, Timestamp)>,
}

#[derive(Encode, Decode)]
struct ChainProperties {
    genesis: BlockHash,
    hash: ChainHash,
    kind: ChainKind,
}

#[derive(Encode, Decode)]
struct Transfer(Option<Compact<AssetId>>, #[codec(compact)] BalanceSlot);

#[derive(Encode, Decode, Debug)]
struct Invoice {
    derivation: (PublicSlot, Derivation),
    paid: bool,
    #[codec(compact)]
    timestamp: Timestamp,
    #[codec(compact)]
    price: BalanceSlot,
    callback: String,
    message: String,
    transactions: TransferTxs,
}

#[derive(Encode, Decode, Debug)]
enum TransferTxs {
    Asset {
        #[codec(compact)]
        id: AssetId,
        // transactions: TransferTxsAsset,
    },
    Native {
        recipient: Account,
        encoded: Vec<u8>,
        exact_amount: Option<Compact<BalanceSlot>>,
    },
}

// #[derive(Encode, Decode, Debug)]
// struct TransferTxsAsset<T> {
//     recipient: Account,
//     encoded: Vec<u8>,
//     #[codec(compact)]
//     amount: BalanceSlot,
// }

#[derive(Encode, Decode, Debug)]
struct TransferTx {
    recipient: Account,
    exact_amount: Option<Compact<BalanceSlot>>,
}
*/
/*
impl Value for Invoice {
    type SelfType<'a> = Self;

    type AsBytes<'a> = Vec<u8>;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(mut data: &[u8]) -> Self::SelfType<'_>
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
}*/

pub struct ConfigWoChains {
    pub recipient: AccountId32,
    pub debug: Option<bool>,
    pub remark: Option<String>,
    //pub depth: Option<Duration>,
}

/// Database server handle
#[derive(Clone, Debug)]
pub struct Database {
    tx: mpsc::Sender<DbRequest>,
}

impl Database {
    pub fn init(
        path_option: Option<String>,
        task_tracker: TaskTracker,
        account_lifetime: Timestamp,
    ) -> Result<Self, DbError> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let database = if let Some(path) = path_option {
            tracing::info!("Creating/Opening the database at {path:?}.");

            sled::open(path).map_err(DbError::DbStartError)?
        } else {
            // TODO
            /*
            tracing::warn!(
                "The in-memory backend for the database is selected. All saved data will be deleted after the shutdown!"
            );*/
            sled::open("temp.db").map_err(DbError::DbStartError)?
        };
        let orders = database
            .open_tree(ORDERS_TABLE)
            .map_err(DbError::DbStartError)?;

        task_tracker.spawn("Database server", async move {
            // No process forking beyond this point!
            while let Some(request) = rx.recv().await {
                match request {
                    DbRequest::ActiveOrderList(res) => {
                        let _unused = res.send(Ok(orders
                            .iter()
                            .filter_map(|a| a.ok())
                            .filter_map(|(a, b)| {
                                match (String::decode(&mut &a[..]), OrderInfo::decode(&mut &b[..]))
                                {
                                    (Ok(a), Ok(b)) => Some((a, b)),
                                    _ => None,
                                }
                            })
                            .filter(|(a, b)| b.payment_status == PaymentStatus::Pending)
                            .collect()));
                    }
                    DbRequest::CreateOrder(request) => {
                        let _unused = request.res.send(create_order(
                            request.order,
                            request.query,
                            request.currency,
                            request.payment_account,
                            &orders,
                            account_lifetime,
                        ));
                    }
                    DbRequest::ReadOrder(request) => {
                        let _unused = request.res.send(read_order(request.order, &orders));
                    }
                    DbRequest::MarkPaid(request) => {
                        let _unused = request.res.send(mark_paid(request.order, &orders));
                    }
                    DbRequest::MarkWithdrawn(request) => {
                        let _unused = request.res.send(mark_withdrawn(request.order, &orders));
                    }
                    DbRequest::MarkStuck(request) => {
                        let _unused = request.res.send(mark_stuck(request.order, &orders));
                    }
                    DbRequest::InitializeServerInfo(res) => {
                        let server_info_tree = database
                            .open_tree(SERVER_INFO_TABLE)
                            .map_err(DbError::DbStartError);
                        let result = server_info_tree.and_then(|tree| {
                            if let Some(server_info_data) =
                                tree.get(SERVER_INFO_ID).map_err(DbError::DbInternalError)?
                            {
                                let server_info: ServerInfo =
                                    serde_json::from_slice(&server_info_data).map_err(|e| {
                                        DbError::DeserializationError(e.to_string())
                                    })?;
                                Ok(server_info.instance_id)
                            } else {
                                let mut generator = Generator::default();
                                let new_instance_id = generator
                                    .next()
                                    .unwrap_or_else(|| "unknown-instance".to_string());
                                let server_info_data = ServerInfo {
                                    version: env!("CARGO_PKG_VERSION").to_string(),
                                    instance_id: new_instance_id.clone(),
                                    debug: false,
                                    kalatori_remark: None,
                                };
                                tree.insert(
                                    SERVER_INFO_ID,
                                    serde_json::to_vec(&server_info_data)
                                        .map_err(|e| DbError::SerializationError(e.to_string()))?,
                                )?;
                                Ok(new_instance_id)
                            }
                        });
                        let _unused = res.send(result);
                    }
                    DbRequest::Shutdown(res) => {
                        let _ = res.send(());
                        break;
                    }
                };
            }

            drop(database.flush());

            Ok("Database server is shutting down")
        });

        Ok(Self { tx })
    }

    pub async fn initialize_server_info(&self) -> Result<String, DbError> {
        let (res, rx) = oneshot::channel();
        let _unused = self.tx.send(DbRequest::InitializeServerInfo(res)).await;
        rx.await.map_err(|_| DbError::DbEngineDown)?
    }

    pub async fn order_list(&self) -> Result<Vec<(String, OrderInfo)>, DbError> {
        let (res, rx) = oneshot::channel();
        let _unused = self.tx.send(DbRequest::ActiveOrderList(res)).await;
        rx.await.map_err(|_| DbError::DbEngineDown)?
    }

    pub async fn create_order(
        &self,
        order: String,
        query: OrderQuery,
        currency: CurrencyInfo,
        payment_account: String,
    ) -> Result<OrderCreateResponse, DbError> {
        let (res, rx) = oneshot::channel();
        let _unused = self
            .tx
            .send(DbRequest::CreateOrder(CreateOrder {
                order,
                query,
                currency,
                payment_account,
                res,
            }))
            .await;
        rx.await.map_err(|_| DbError::DbEngineDown)?
    }

    pub async fn read_order(&self, order: String) -> Result<Option<OrderInfo>, DbError> {
        let (res, rx) = oneshot::channel();
        let _unused = self
            .tx
            .send(DbRequest::ReadOrder(ReadOrder { order, res }))
            .await;
        rx.await.map_err(|_| DbError::DbEngineDown)?
    }

    pub async fn mark_paid(&self, order: String) -> Result<OrderInfo, DbError> {
        let (res, rx) = oneshot::channel();
        let _unused = self
            .tx
            .send(DbRequest::MarkPaid(MarkPaid { order, res }))
            .await;
        rx.await.map_err(|_| DbError::DbEngineDown)?
    }

    pub async fn mark_withdrawn(&self, order: String) -> Result<(), DbError> {
        let (res, rx) = oneshot::channel();
        let _unused = self
            .tx
            .send(DbRequest::MarkWithdrawn(ModifyOrder { order, res }))
            .await;
        rx.await.map_err(|_| DbError::DbEngineDown)?
    }

    pub async fn mark_stuck(&self, order: String) -> Result<(), DbError> {
        let (res, rx) = oneshot::channel();
        let _unused = self
            .tx
            .send(DbRequest::MarkStuck(ModifyOrder { order, res }))
            .await;
        rx.await.map_err(|_| DbError::DbEngineDown)?
    }

    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        let _unused = self.tx.send(DbRequest::Shutdown(tx)).await;
        let _ = rx.await;
    }
}

enum DbRequest {
    CreateOrder(CreateOrder),
    ActiveOrderList(oneshot::Sender<Result<Vec<(String, OrderInfo)>, DbError>>),
    ReadOrder(ReadOrder),
    MarkPaid(MarkPaid),
    MarkWithdrawn(ModifyOrder),
    MarkStuck(ModifyOrder),
    InitializeServerInfo(oneshot::Sender<Result<String, DbError>>),
    Shutdown(oneshot::Sender<()>),
}

pub struct CreateOrder {
    pub order: String,
    pub query: OrderQuery,
    pub currency: CurrencyInfo,
    pub payment_account: String,
    pub res: oneshot::Sender<Result<OrderCreateResponse, DbError>>,
}

pub struct ReadOrder {
    pub order: String,
    pub res: oneshot::Sender<Result<Option<OrderInfo>, DbError>>,
}

pub struct ModifyOrder {
    pub order: String,
    pub res: oneshot::Sender<Result<(), DbError>>,
}

pub struct MarkPaid {
    pub order: String,
    pub res: oneshot::Sender<Result<OrderInfo, DbError>>,
}

fn calculate_death_ts(account_lifetime: Timestamp) -> Timestamp {
    let start = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    Timestamp(start + account_lifetime.0)
}

fn create_order(
    order: String,
    query: OrderQuery,
    currency: CurrencyInfo,
    payment_account: String,
    orders: &sled::Tree,
    account_lifetime: Timestamp,
) -> Result<OrderCreateResponse, DbError> {
    let order_key = order.encode();
    Ok(if let Some(record) = orders.get(&order_key)? {
        let mut old_order_info = OrderInfo::decode(&mut &record[..])?;
        match old_order_info.payment_status {
            PaymentStatus::Pending => {
                let death = calculate_death_ts(account_lifetime);

                old_order_info.death = death;
                old_order_info.currency = currency;
                old_order_info.amount = query.amount;

                orders.insert(&order_key, old_order_info.encode())?;
                OrderCreateResponse::Modified(old_order_info)
            }
            PaymentStatus::Paid => OrderCreateResponse::Collision(old_order_info),
        }
    } else {
        let death = calculate_death_ts(account_lifetime);
        let order_info_new = OrderInfo::new(query, currency, payment_account, death);

        orders.insert(&order_key, order_info_new.encode())?;
        OrderCreateResponse::New(order_info_new)
    })
}

fn read_order(order: String, orders: &sled::Tree) -> Result<Option<OrderInfo>, DbError> {
    let order_key = order.encode();
    if let Some(order) = orders.get(order_key)? {
        Ok(Some(OrderInfo::decode(&mut &order[..])?))
    } else {
        Ok(None)
    }
}

fn mark_paid(order: String, orders: &sled::Tree) -> Result<OrderInfo, DbError> {
    let order_key = order.encode();
    if let Some(order_info) = orders.get(order_key)? {
        let mut order_info = OrderInfo::decode(&mut &order_info[..])?;
        if order_info.payment_status == PaymentStatus::Pending {
            order_info.payment_status = PaymentStatus::Paid;
            orders.insert(order.encode(), order_info.encode())?;
            Ok(order_info)
        } else {
            Err(DbError::AlreadyPaid(order))
        }
    } else {
        Err(DbError::OrderNotFound(order))
    }
}
fn mark_withdrawn(order: String, orders: &sled::Tree) -> Result<(), DbError> {
    if let Some(order_info) = orders.get(order.clone())? {
        let mut order_info = OrderInfo::decode(&mut &order_info[..])?;
        if order_info.payment_status == PaymentStatus::Paid {
            if order_info.withdrawal_status == WithdrawalStatus::Waiting {
                order_info.withdrawal_status = WithdrawalStatus::Completed;
                orders.insert(order.encode(), order_info.encode())?;
                Ok(())
            } else {
                Err(DbError::WithdrawalWasAttempted(order))
            }
        } else {
            Err(DbError::NotPaid(order))
        }
    } else {
        Err(DbError::OrderNotFound(order))
    }
}
fn mark_stuck(order: String, orders: &sled::Tree) -> Result<(), DbError> {
    if let Some(order_info) = orders.get(order.clone())? {
        let mut order_info = OrderInfo::decode(&mut &order_info[..])?;
        if order_info.payment_status == PaymentStatus::Paid {
            if order_info.withdrawal_status == WithdrawalStatus::Waiting {
                order_info.withdrawal_status = WithdrawalStatus::Failed;
                orders.insert(order.encode(), order_info.encode())?;
                Ok(())
            } else {
                Err(DbError::WithdrawalWasAttempted(order))
            }
        } else {
            Err(DbError::NotPaid(order))
        }
    } else {
        Err(DbError::OrderNotFound(order))
    }
}
//impl StateInterface {
/*
    Ok((
        OrderStatus {
            order,
            payment_status: if invoice.paid {
                PaymentStatus::Paid
            } else {
                PaymentStatus::Pending
            },
            message: String::new(),
            recipient: state.0.recipient.to_ss58check(),
            server_info: state.server_info(),
            order_info: OrderInfo {
                withdrawal_status: WithdrawalStatus::Waiting,
                amount: invoice.amount.format(6),
                currency: CurrencyInfo {
                    currency: "USDC".into(),
                    chain_name: "assethub-polkadot".into(),
                    kind: TokenKind::Asset,
                    decimals: 6,
                    rpc_url: state.rpc.clone(),
                    asset_id: Some(1337),
                },
                callback: invoice.callback.clone(),
                transactions: vec![],
                payment_account: invoice.paym_acc.to_ss58check(),
            },
        },
        OrderSuccess::Found,
    ))
} else {
    Ok((
        OrderStatus {
            order,
            payment_status: PaymentStatus::Unknown,
message: String::new(),
            recipient: state.0.recipient.to_ss58check(),
            server_info: state.server_info(),
            order_info: OrderInfo {
                withdrawal_status: WithdrawalStatus::Waiting,
                amount: 0f64,
                currency: CurrencyInfo {
                    currency: "USDC".into(),
                    chain_name: "assethub-polkadot".into(),
                    kind: TokenKind::Asset,
                    decimals: 6,
                    rpc_url: state.rpc.clone(),
                    asset_id: Some(1337),
                },
                callback: String::new(),
                transactions: vec![],
                payment_account: String::new(),
            },
        },
        OrderSuccess::Found,
    ))
}*/

/*
 *
let pay_acc: AccountId = state
        .0
        .pair
        .derive(vec![DeriveJunction::hard(order.clone())].into_iter(), None)
        .unwrap()
        .0
        .public()
        .into();

 * */

/*(
    OrderStatus {
        order,
        payment_status: PaymentStatus::Pending,
        message: String::new(),
        recipient: state.0.recipient.to_ss58check(),
        server_info: state.server_info(),
        order_info: OrderInfo {
            withdrawal_status: WithdrawalStatus::Waiting,
            amount,
            currency: CurrencyInfo {
                currency: "USDC".into(),
                chain_name: "assethub-polkadot".into(),
                kind: TokenKind::Asset,
                decimals: 6,
                rpc_url: state.rpc.clone(),
                asset_id: Some(1337),
            },
            callback,
            transactions: vec![],
            payment_account: pay_acc.to_ss58check(),
        },
    },
    OrderSuccess::Created,
))*/

/*
        ServerStatus {
            description: state.server_info(),
            supported_currencies: state.currencies.clone(),
        }
*/
/*
#[derive(Deserialize, Debug)]
pub struct Invoicee {
    pub callback: String,
    pub amount: Balance,
    pub paid: bool,
    pub paym_acc: Account,
}
*/
/*

*/
/*
        pub fn server_info(&self) -> ServerInfo {
            ServerInfo {
                version: env!("CARGO_PKG_VERSION"),
                instance_id: String::new(),
                debug: self.debug,
                kalatori_remark: self.remark.clone(),
            }
        }
*/
/*
    pub fn currency_properties(&self, currency_name: &str) -> Result<&CurrencyProperties, ErrorDb> {
        self.currencies
            .get(currency_name)
            .ok_or(ErrorDb::CurrencyKeyNotFound)
    }

    pub fn currency_info(&self, currency_name: &str) -> Result<CurrencyInfo, ErrorDb> {
        let currency = self.currency_properties(currency_name)?;
        Ok(CurrencyInfo {
            currency: currency_name.to_string(),
            chain_name: currency.chain_name.clone(),
            kind: currency.kind,
            decimals: currency.decimals,
            rpc_url: currency.rpc_url.clone(),
            asset_id: currency.asset_id,
        })
    }
*/
//     pub fn rpc(&self) -> &str {
//         &self.rpc
//     }

//     pub fn destination(&self) -> &Option<Account> {
//         &self.destination
//     }

//     pub fn write(&self) -> Result<WriteTransaction<'_>> {
//         self.db
//             .begin_write()
//             .map(WriteTransaction)
//             .context("failed to begin a write transaction for the database")
//     }

//     pub fn read(&self) -> Result<ReadTransaction<'_>> {
//         self.db
//             .begin_read()
//             .map(ReadTransaction)
//             .context("failed to begin a read transaction for the database")
//     }

//     pub async fn properties(&self) -> RwLockReadGuard<'_, ChainProperties> {
//         self.properties.read().await
//     }

//     pub fn pair(&self) -> &Pair {
//         &self.pair
//     }

/*
pub struct ReadTransaction(redb::ReadTransaction);

impl ReadTransaction {
    pub fn invoices(&self) -> Result<ReadInvoices> {
        self.0
            .open_table(INVOICES)
            .map(ReadInvoices)
            .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))
    }
}

pub struct ReadInvoices<'a>(ReadOnlyTable<&'a [u8], Invoice>);

impl <'a> ReadInvoices<'a> {
    pub fn get(&self, account: &Account) -> Result<Option<AccessGuard<'_, Invoice>>> {
        self.0
            .get(&*account)
            .context("failed to get an invoice from the database")
    }
*/
//     pub fn try_iter(
//         &self,
//     ) -> Result<impl Iterator<Item = Result<(AccessGuard<'_, &[u8; 32]>, AccessGuard<'_, Invoice>)>>>
//     {
//         self.0
//             .iter()
//             .context("failed to get the invoices iterator")
//             .map(|iter| iter.map(|item| item.context("failed to get an invoice from the iterator")))
//     }
// }

// pub struct WriteTransaction<'db>(redb::WriteTransaction<'db>);

// impl<'db> WriteTransaction<'db> {
//     pub fn root(&self) -> Result<Root<'db, '_>> {
//         self.0
//             .open_table(ROOT)
//             .map(Root)
//             .with_context(|| format!("failed to open the `{}` table", ROOT.name()))
//     }

//     pub fn invoices(&self) -> Result<WriteInvoices<'db, '_>> {
//         self.0
//             .open_table(INVOICES)
//             .map(WriteInvoices)
//             .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))
//     }

//     pub fn commit(self) -> Result<()> {
//         self.0
//             .commit()
//             .context("failed to commit a write transaction in the database")
//     }
// }

// pub struct WriteInvoices<'db, 'tx>(Table<'db, 'tx, &'static [u8; 32], Invoice>);

// impl WriteInvoices<'_, '_> {
//     pub fn save(
//         &mut self,
//         account: &Account,
//         invoice: &Invoice,
//     ) -> Result<Option<AccessGuard<'_, Invoice>>> {
//         self.0
//             .insert(AsRef::<[u8; 32]>::as_ref(account), invoice)
//             .context("failed to save an invoice in the database")
//     }
// }

// pub struct Root<'db, 'tx>(Table<'db, 'tx, &'static str, Vec<u8>>);

// impl Root<'_, '_> {
//     pub fn save_last_block(&mut self, number: BlockNumber) -> Result<()> {
//         self.0
//             .insert(LAST_BLOCK, Compact(number).encode())
//             .context("context")?;

//         Ok(())
//     }
// }

// fn get_slot(table: &Table<'_, &str, Vec<u8>>, key: &str) -> Result<Option<Vec<u8>>> {
//     table
//         .get(key)
//         .map(|slot_option| slot_option.map(|slot| slot.value().clone()))
//         .with_context(|| format!("failed to get the {key:?} slot"))
// }

// fn decode_slot<T: Decode>(mut slot: &[u8], key: &str) -> Result<T> {
//     T::decode(&mut slot).with_context(|| format!("failed to decode the {key:?} slot"))
// }

// fn insert_daemon_info(
//     table: &mut Table<'_, '_, &str, Vec<u8>>,
//     rpc: String,
//     key: Public,
// ) -> Result<()> {
//     table
//         .insert(DAEMON_INFO, DaemonInfo { rpc, key }.encode())
//         .map(|_| ())
//         .context("failed to insert the daemon info")
// }
