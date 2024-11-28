!!!OUTDATED!!!

The daemon consists of 5 modules:

* `callback.rs`
* `database.rs`
* `main.rs`
* `rpc.rs`
* `server.rs`

## `main.rs`

Everything starts here. Previously, the start logic was in `lib.rs` because this allowed to add documentation directly to the source code (e.g., for the server API), but since Markdown isn't sufficient for complex descriptions & generating the documentation with `rustdoc` just to read it is inconvenient for those who won't build the daemon from the source, it was decided to use only `main.rs` while keeping the documentation elsewhere.

At the start, the daemon reads the following environment variables:

* `KALATORI_CONFIG`

The path to a config file.

* `KALATORI_LOG`

[Filter directives for the logger.](https://docs.rs/tracing-subscriber/0.3/tracing_subscriber/filter/struct.EnvFilter.html)

* `KALATORI_SEED`

The seed phrase that new accounts will be derived from.

* `KALATORI_OLD_SEED_*`

Old seed phrases for existing accounts. Used for the unimplemented seed rotation logic.

* `KALATORI_RECIPIENT`

The recipient account address in the SS58 format.

* `KALATORI_REMARK`

The arbitrary string to be passed with the server info from the server API.

Then the daemon parses a config file. The config format can be found in at the end of `main.rs`. Examples are in the [configs](../configs) directory.

## `server.rs`

Contains the server & server API implementation using the `axum` framework. Currently, everything is hardcoded to USDC from the Polkadot Asset Hub parachain.

The amount parameter in API is treated as `f64` during a de- and serialization due to restrictions on the frontend side. This approach is error-prone because `f64` is insufficient to hold `u128` (that's the actual type of the amount parameter) leading to the loss of last digits during a parsing/formatting. Moreover, the converting between `f64` & `u128` suffers from rounding errors that could unexpectedly mutate a lenghty number during a roundtrip. An example of such a number can be found in the test at the end of `main.rs`. A proposed solution to this problem was to create a custom (de)serializer that'd parse/format floats in the amount parameter directly to/from `u128` skipping the erroneous parts with `f64`, but as it's a quite long task, it was decided to stick with the default (de)serializer with `f64` and hope to get no lenghty amounts.

## `callback.rs`

Contains a simple function to make callbacks with the order info. To do this, it was decided to use the `ureq` crate, a blocking HTTP-client. To prevent blocking the async executor, every callback is spawned in the Tokio's blocking threadpool. Since we don't need any complex HTTP 1 features as well as HTTP 2/3, `ureq` is a good choice for a rather dependencies-heavy crate that the daemon is.

## `rps.rs`

The most complex module of the daemon where all the crucial blockchain-related logic happens. In the first version of daemon, only one chain & currency (a native token or an asset) were supported in 2 separate binaries. With the requirement to support both a native token & multiple assets within a single daemon, a new architecture was needed. Since from Substrate RPC's POV there's no difference between relaychains & parachains, it was decided to make the support of any number of chains with the parallel processing, different configurations & currencies, not just a pair of a relaychain with a native token & a parachain with assets in 2 threads. The config file was introduced to process chain configurations from a much more convenient format than a cluster of environment variables. For now, the daemon can parse this config and prepare chains & their currencies for the block parsing loop. The block parsing loop isn't implemented.

### Assumptions

The current Substrate RPC API & pallets used by the daemon, unfortunately, don't have any strict standard, which all nodes & chains follow, and the common implementations of RPC API & pallets lack of some essential features. This creates restrictions & assumptions for which nodes & chains the daemon can be used with. Of course, our first priority is and will be the support of the Polkadot relaychain & the Polkadot Asset Hub parachain with the support of USDT & USDC assets on that parachain, but in the future, we'll try to lift the following limitations and support a broader range of nodes & chains along with the evolution of the Substrate ecosystem & its standards.

#### [`ChargeAssetTxPayment`]

One of difficult parts in the chain preparation is to determine & dynamically process the asset type in [`ChargeAssetTxPayment`], the signed extension that's used to pay transaction fees in an asset instead of a native token. Kusama, Westend & Rococo Asset Hub parachains have the [`MultiLocation`] struct as the asset type, and only Polkadot Asset Hub has the `u32` type as its asset type. There's also a possibility of other types, but the daemon supports only these 2 because they're the most common. The daemon chooses between them by looking up the properties of [`ChargeAssetTxPayment`] in the metadata returned from an RPC server. To avoid depending on the `staging-xcm` crate, the daemon in `asset.rs` has the copy of [`MultiLocation`] from this crate with custom trait implementations to process it along with `u32`.

#### Transfers

All extrinsics sent by the daemon are transfer transactions. Despite they themselves are quite unsophisticated, the whole process of *the approximate fee calculation to subtract it from the transfer amount and then, in case of a fail because a real fee was higher than the daemon estimated, the resending of the same transaction after another never accurate calculation* does sound like a really hard path.

We tried to use the [`transfer_allow_death`] call in the Balances pallet for all transfers because we thought that a transfer fee would never be greater than the existential deposit but that isn't true for all chains (e.g., it's true for Polkadot but not for Kusama), and unlikely will be a standard, so we've stuck with the [`transfer_all`] call for transfers to both the beneficiary & overpayers accounts.

The Assets pallet doesn't have a call similar to Balances's [`transfer_all`], so the daemon uses [`transfer`] assuming that a fee for that call wouldn't be higher than asset's [`min_balance`]. For now, that's true for USDT & USDC on Polkadot Asset Hub, but **if one these or another assets won't meet this criteria, the daemon won't be able to work with them**. We plan to propose and hopefully include some kind of the `transfer_all` call in Assets pallet to subsequently eliminate this restriction.

#### Transactions

The daemon uses the `author_submit_extrinsic` method to submit transactions. Another option to do this is the `author_submit_and_watch_extrinsic` method, it was considered more reliable, albeit more heavy & restricted, because it provides a subscription to a status of every submitted transaction, but its main problem is there's no way to resume a subscription (e.g., in case of an abnormal daemon shutdown or a loss of a connection with an RPC server). To track a transactions status, the daemon will use the [CheckMortality](https://docs.rs/frame-system/32/frame_system/struct.CheckMortality.html) signed extension. It's used to add mortality to a transaction, mortality measured with blocks, so the daemon can calculate a block number on which a transaction would be dead (invalid) and wait for it. If the transaction hasn't appeared before its death block, the transaction considered lost. It might happen for a variety of reasons that the daemon doesn't have the control of, e.g., a connection loss, faulty RPC server, unusual chain's runtime implementation.

Unresolved questions:

* What should the daemon do in the case of a lost transaction?

One of expected behaviours would be to send it again, but it's unclear how many attempts should be made before giving up.

* How should the transaction mortality affect account lifetimes in the database? (See the description of the `"hitlist"` database table.)

## `database.rs`

To store data, the daemon uses the `redb` crate, a key-value store with tables. The daemon database consists of the following tables:

* `"root"`
* `"keys"`
* `"chains"`
* `"invoices"`
* `"accounts*"`
* `"transactions*"`
* `"hit_list*"`

### `"root"`

Contains the database format version & the daemon info. The daemon info tracks the information about chains & keys the daemon is using.

#### Chains

Chains are stored as `Map<String, ChainProperties>`. `String` is a arbitrary name of a chain from the daemon config. Chain properties consists of a chain's hash, genesis hash, kind, and assets. Chain hash is used as an internal key to a chain in other tables instead of a string from the config to enable the renaming of a chain without changing its internal key. A genesis hash is used to check whether given RPC endpoints represent a right chain. A kind means whether a chain operates with [`MultiLocation`] or `u32` as the asset type (if a chain doesn't have assets, `u32` is the default).

#### Keys

Accounts for new invoices are derived from the current key. If the database has no invoices, the current key can be replaced with another one from the `KALATORI_SEED` environment variable. If the database has invoices, the current key can be replaced in the same way, but the previous current key's seed must be moved to a new `KALATORI_OLD_SEED_*` variable. Then the daemon will match old seeds with public keys stored in the database. This is used for the key rotation logic.

### `"keys"`

The contuation of the above paragraph. Keys are also stored here to track a number of account created with a certain key. Once the number reaches 0, its key is deleted from the database, and the daemon won't require its seed in `KALATORI_OLD_SEED_*` variables.

This data is stored in separated database because the `"root"` table stores data in encoded form requiring a full decode & encode roundtrip for each slot change.

### `"chains"`

Used to store the last saved block for each chain.

### `"invoices"`

Invoices are stored as `Map<String, Invoice>`. The key has an arbitrary format, and set by the server module that in turn receives it from the frontend.

An invoice contains:

* The public key & the hard derivation joint.

The payment account address can be generated from them.

In the first version of the daemon, the database didn't store the invoice's derivation joint and calculated it from the invoice's string key received from the server module. There was a very small chance of the hash collision that could lead to, e.g., returning information about already paid invoice instead of creating a new one, so it was decided to use a calculated derivation joint as the initial hash, check if the database contains an account from the joint, and if not, create an account from the initial joint, or calculate an unoccupied joint and create an account with it.

* The payment status.

It's unclear how to properly separate unpaid accounts for the unpaid account checking on the daemon startup. There are a few possible options: iterate over all invoices filtering unpaid ones out; storing paid & unpaid invoices in separate tables; storing only unpaid invoices' keys in a separate table.

* The creation timestamp.

* The price.

Must be checked to be greater than its currency's existential deposit.

* The callback.

Used to send invoice statuses back to the frontend. Ignored if empty.

* The message.

A message with an arbitrary human-readable format. Usually contains error messages.

* Transactions.

Finalized & pending transfer transactions. Each transaction contains the recipient, the amount, the string with the hex-encoded transaction, and the currency info (since each invoice has only 1 currency, it's unclear why the server API needs the currency info for every invoice's transaction).

> The next tables have `*` at the end of their name. It's the placeholder for a chain hash. The daemon creates a set of these table for each chain.

### `"accounts*"`

Stores account addresses with currency info (`Map<(Option<AssetId>, AccountId), InvoiceKey>`) for the block processing loop. What's notably 1 account address can be used for different currencies in case of an aforementioned hash collision because while invoices are created for 1 currency, their account's transactions don't depend on currencies, so the daemon should be able to process multi-currency accounts without conflicts.

### `"transactions*"`

Stores info about pending transfer transactions with their death block as the key.

### `"hit_list*"`

The content of this table is in progress. It should be used to track lifetimes of invoices to remove them from the database, thereby maintaining the stable element search time and avoiding the unused accounts tracking for overpayments. Currently, this table looks like `Map<BlockNumber, (Option<AssetId>, Account)>`, but this layout is insufficient because of the following

Unresolved questions:

* On a startup, the daemon simply removes all dead invoices. But consider the following scenario: the daemon shuts down (due to a crash or manually) after creating an invoice, invoice's account receives money, and after a time longer than the invoice's lifetime, the daemon starts up and immediately deletes the dead but paid invoice. How to avoid this situation? Should the daemon check on startup all unpaid invoices, and renew lifetimes of just paid ones? What about overpayments on paid invoices? Should the daemon renew lifetimes of all invoices then?

* How transaction lifetimes should affect ones of invoices? Currently, if some transaction appears in a block after its invoice's lifetime, the transaction is ignored because its invoice have been deleted. This applies to both incoming & outgoing refund/withdrawal transactions. Lifetimes of transactions aren't fixed values, their upper bound depends on the chains' `BlockHashCount` runtime parameter that can be different on each chain and modified on a runtime upgrade, and the lower bound depends on a block producing/finalization algorithm and properties of a connection with an RPC server.

[`MultiLocation`]: https://docs.rs/staging-xcm/11/staging_xcm/v3/struct.MultiLocation.html
[`transfer_all`]: https://docs.rs/pallet-balances/33/pallet_balances/pallet/struct.Pallet.html#method.transfer_all
[`transfer_allow_death`]: https://docs.rs/pallet-balances/33/pallet_balances/pallet/struct.Pallet.html#method.transfer_allow_death
[`transfer`]: https://docs.rs/pallet-assets/33/pallet_assets/pallet/struct.Pallet.html#method.transfer
[`min_balance`]: https://docs.rs/pallet-assets/33/src/pallet_assets/types.rs.html#66
[`ChargeAssetTxPayment`]: https://docs.rs/pallet-asset-tx-payment/32/pallet_asset_tx_payment/struct.ChargeAssetTxPayment.html
