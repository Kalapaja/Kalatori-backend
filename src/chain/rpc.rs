//! Blockchain operations that actually require calling the chain

use crate::{
    chain::{
        definitions::{BlockHash, EventFilter},
        utils::{
            asset_balance_query, block_number_query, events_entry_metadata, hashed_key_element,
            system_balance_query, system_properties_to_short_specs,
        },
    },
    definitions::api_v2::CurrencyProperties,
    definitions::{
        api_v2::{AssetId, TokenKind},
        Balance,
    },
    error::{ChainError, NotHexError},
    utils::unhex,
};
use codec::{DecodeAll, Encode};
use frame_metadata::{
    v15::{RuntimeMetadataV15, StorageEntryMetadata, StorageEntryType},
    RuntimeMetadata,
};
use hashing::twox_128;
use jsonrpsee::core::client::{ClientT, Subscription, SubscriptionClientT};
use jsonrpsee::rpc_params;
use jsonrpsee::ws_client::WsClient;
use scale_info::{form::PortableForm, PortableRegistry, TypeDef, TypeDefPrimitive};
use serde::Deserialize;
use serde_json::{Number, Value};
use std::{collections::HashMap, fmt::Debug};
use substrate_crypto_light::common::AccountId32;
use substrate_parser::{
    cards::{Event, ParsedData, Sequence},
    decode_all_as_type, decode_as_storage_entry,
    special_indicators::SpecialtyUnsignedInteger,
    storage_data::{KeyData, KeyPart},
    AsMetadata, ResolveType, ShortSpecs,
};

/// To prevent infinite loop while scanning for keys if the node decides to misbehave, limit number
/// of pages
///
/// TODO: add more timeouts
const MAX_KEY_PAGES: usize = 256;

// Pallets

const BALANCES: &str = "Balances";
const TRANSFER: &str = "Transfer";

// Runtime APIs

/// Fetch some runtime version identifier.
///
/// This does not have to be typesafe or anything; this could be used only to check if returned
/// value changes - and reboot the whole connection then, regardless of nature of change.
pub async fn runtime_version_identifier(
    client: &WsClient,
    block: &BlockHash,
) -> Result<Value, ChainError> {
    let value = client
        .request("state_getRuntimeVersion", rpc_params![block.to_string()])
        .await?;
    Ok(value)
}

pub async fn subscribe_blocks(client: &WsClient) -> Result<Subscription<BlockHead>, ChainError> {
    Ok(client
        .subscribe(
            "chain_subscribeFinalizedHeads",
            rpc_params![],
            "chain_unsubscribeFinalizedHeads",
        )
        .await?)
}

pub async fn get_value_from_storage(
    client: &WsClient,
    whole_key: &str,
    block: &BlockHash,
) -> Result<Value, ChainError> {
    let value: Value = client
        .request(
            "state_getStorage",
            rpc_params![whole_key, block.to_string()],
        )
        .await?;
    Ok(value)
}

pub async fn get_keys_from_storage(
    client: &WsClient,
    prefix: &str,
    storage_name: &str,
    block: &BlockHash,
) -> Result<Vec<Value>, ChainError> {
    let mut keys_vec = Vec::new();
    let storage_key_prefix = format!(
        "0x{}{}",
        const_hex::encode(twox_128(prefix.as_bytes())),
        const_hex::encode(twox_128(storage_name.as_bytes()))
    );

    let count = 10;
    // Because RPC API accepts parameters as a sequence and the last 2 parameters are
    // `start_key: Option<StorageKey>` and `hash: Option<Hash>`, API *always* takes `hash` as
    // `storage_key` if the latter is `None` and believes that `hash` is `None` because although
    // `StorageKey` and `Hash` are different types, any `Hash` perfectly deserializes as
    // `StorageKey`. Therefore, `start_key` must always be present to correctly use the
    // `state_getKeysPaged` call with the `hash` parameter.
    let mut start_key: String = "0x".into(); // Start from the beginning

    let params_template = vec![
        serde_json::to_value(storage_key_prefix.clone()).unwrap(),
        serde_json::to_value(count).unwrap(),
    ];

    for _ in 0..MAX_KEY_PAGES {
        let mut params = params_template.clone();
        params.push(serde_json::to_value(start_key.clone()).unwrap());

        params.push(serde_json::to_value(block.to_string()).unwrap());
        if let Ok(keys) = client.request("state_getKeysPaged", params).await {
            if let Value::Array(keys_inside) = &keys {
                if let Some(Value::String(key_string)) = keys_inside.last() {
                    start_key.clone_from(key_string);
                } else {
                    return Ok(keys_vec);
                }
            } else {
                return Ok(keys_vec);
            };

            keys_vec.push(keys);
        } else {
            return Ok(keys_vec);
        }
    }

    Ok(keys_vec)
}

/// fetch genesis hash, must be a hexadecimal string transformable into
/// H256 format
pub async fn genesis_hash(client: &WsClient) -> Result<BlockHash, ChainError> {
    let genesis_hash_request: Value = client
        .request(
            "chain_getBlockHash",
            rpc_params![Value::Number(Number::from(0u8))],
        )
        .await
        .map_err(ChainError::Client)?;
    match genesis_hash_request {
        Value::String(x) => BlockHash::from_str(&x),
        _ => return Err(ChainError::GenesisHashFormat),
    }
}

/// fetch block hash, to request later the metadata and specs for
/// the same block
pub async fn block_hash(
    client: &WsClient,
    number: Option<String>,
) -> Result<BlockHash, ChainError> {
    let rpc_params = if let Some(a) = number {
        rpc_params![a]
    } else {
        rpc_params![]
    };
    let block_hash_request: Value = client
        .request("chain_getBlockHash", rpc_params)
        .await
        .map_err(ChainError::Client)?;
    match block_hash_request {
        Value::String(x) => BlockHash::from_str(&x),
        _ => return Err(ChainError::BlockHashFormat),
    }
}

/// fetch metadata at known block
pub async fn metadata(
    client: &WsClient,
    block: &BlockHash,
) -> Result<RuntimeMetadataV15, ChainError> {
    let metadata_request: Value = client
        .request(
            "state_call",
            rpc_params![
                "Metadata_metadata_at_version",
                "0x0f000000",
                block.to_string()
            ],
        )
        .await
        .map_err(ChainError::Client)?;
    match metadata_request {
        Value::String(x) => {
            let metadata_request_raw = unhex(&x, NotHexError::Metadata)?;
            let maybe_metadata_raw = Option::<Vec<u8>>::decode_all(&mut &metadata_request_raw[..])
                .map_err(|_| ChainError::RawMetadataNotDecodeable)?;
            if let Some(meta_v15_bytes) = maybe_metadata_raw {
                if meta_v15_bytes.starts_with(b"meta") {
                    match RuntimeMetadata::decode_all(&mut &meta_v15_bytes[4..]) {
                        Ok(RuntimeMetadata::V15(runtime_metadata_v15)) => {
                            return Ok(runtime_metadata_v15)
                        }
                        Ok(_) => return Err(ChainError::NoMetadataV15),
                        Err(_) => return Err(ChainError::MetadataNotDecodeable),
                    }
                } else {
                    return Err(ChainError::NoMetaPrefix);
                }
            } else {
                return Err(ChainError::NoMetadataV15);
            }
        }
        _ => return Err(ChainError::MetadataFormat),
    };
}

// fetch specs at known block
pub async fn specs(
    client: &WsClient,
    metadata: &RuntimeMetadataV15,
    block: &BlockHash,
) -> Result<ShortSpecs, ChainError> {
    let specs_request: Value = client
        .request("system_properties", rpc_params![block.to_string()])
        .await?;
    match specs_request {
        Value::Object(properties) => system_properties_to_short_specs(&properties, &metadata),
        _ => return Err(ChainError::PropertiesFormat),
    }
}

pub async fn next_block_number(blocks: &mut Subscription<BlockHead>) -> Result<String, ChainError> {
    match blocks.next().await {
        Some(Ok(a)) => Ok(a.number),
        Some(Err(e)) => Err(e.into()),
        None => Err(ChainError::BlockSubscriptionTerminated),
    }
}

pub async fn next_block(
    client: &WsClient,
    blocks: &mut Subscription<BlockHead>,
) -> Result<BlockHash, ChainError> {
    block_hash(&client, Some(next_block_number(blocks).await?)).await
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct BlockHead {
    //digest: Value,
    //extrinsics_root: String,
    pub number: String,
    //parent_hash: String,
    //state_root: String,
}

/// Get all sufficient assets from a chain
#[expect(clippy::too_many_lines)]
pub async fn assets_set_at_block(
    client: &WsClient,
    block: &BlockHash,
    metadata_v15: &RuntimeMetadataV15,
    rpc_url: &str,
    specs: ShortSpecs,
) -> Result<HashMap<String, CurrencyProperties>, ChainError> {
    let mut assets_set = HashMap::new();
    let chain_name =
        <RuntimeMetadataV15 as AsMetadata<()>>::spec_name_version(metadata_v15)?.spec_name;
    let mut assets_asset_storage_metadata = None;
    let mut assets_metadata_storage_metadata = None;

    for pallet in metadata_v15.pallets.iter() {
        if let Some(storage) = &pallet.storage {
            if storage.prefix == "Assets" {
                for entry in storage.entries.iter() {
                    if entry.name == "Asset" {
                        assets_asset_storage_metadata = Some(entry);
                    }
                    if entry.name == "Metadata" {
                        assets_metadata_storage_metadata = Some(entry);
                    }
                    if assets_asset_storage_metadata.is_some()
                        && assets_metadata_storage_metadata.is_some()
                    {
                        break;
                    }
                }
                break;
            }
        }
    }

    if let (Some(assets_asset_storage_metadata), Some(assets_metadata_storage_metadata)) = (
        assets_asset_storage_metadata,
        assets_metadata_storage_metadata,
    ) {
        let available_keys_assets_asset_vec =
            get_keys_from_storage(client, "Assets", "Asset", block).await?;
        for available_keys_assets_asset in available_keys_assets_asset_vec {
            if let Value::Array(ref keys_array) = available_keys_assets_asset {
                for key in keys_array.iter() {
                    if let Value::String(string_key) = key {
                        let value_fetch = get_value_from_storage(client, string_key, block).await?;
                        if let Value::String(ref string_value) = value_fetch {
                            let key_data = unhex(string_key, NotHexError::StorageKey)?;
                            let value_data = unhex(string_value, NotHexError::StorageValue)?;
                            let storage_entry =
                                decode_as_storage_entry::<&[u8], (), RuntimeMetadataV15>(
                                    &key_data.as_ref(),
                                    &value_data.as_ref(),
                                    &mut (),
                                    assets_asset_storage_metadata,
                                    &metadata_v15.types,
                                )?;
                            let asset_id = {
                                if let KeyData::SingleHash { content } = storage_entry.key {
                                    if let KeyPart::Parsed(extended_data) = content {
                                        if let ParsedData::PrimitiveU32 {
                                            value,
                                            specialty: _,
                                        } = extended_data.data
                                        {
                                            Ok(value)
                                        } else {
                                            Err(ChainError::AssetIdFormat)
                                        }
                                    } else {
                                        Err(ChainError::AssetKeyEmpty)
                                    }
                                } else {
                                    Err(ChainError::AssetKeyNotSingleHash)
                                }
                            }?;
                            let mut verified_sufficient = false;
                            if let ParsedData::Composite(fields) = storage_entry.value.data {
                                for field_data in fields.iter() {
                                    if let Some(field_name) = &field_data.field_name {
                                        if field_name == "is_sufficient" {
                                            if let ParsedData::PrimitiveBool(is_it) =
                                                field_data.data.data
                                            {
                                                verified_sufficient = is_it;
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                            if verified_sufficient {
                                match &assets_metadata_storage_metadata.ty {
                                    StorageEntryType::Plain(_) => {
                                        return Err(ChainError::AssetMetadataPlain)
                                    }
                                    StorageEntryType::Map {
                                        hashers,
                                        key: key_ty,
                                        value: value_ty,
                                    } => {
                                        if hashers.len() == 1 {
                                            let hasher = &hashers[0];
                                            match metadata_v15
                                                .types
                                                .resolve_ty(key_ty.id, &mut ())?
                                                .type_def
                                            {
                                                TypeDef::Primitive(TypeDefPrimitive::U32) => {
                                                    let key_assets_metadata = format!(
                                                        "0x{}{}{}",
                                                        const_hex::encode(twox_128(
                                                            "Assets".as_bytes()
                                                        )),
                                                        const_hex::encode(twox_128(
                                                            "Metadata".as_bytes()
                                                        )),
                                                        const_hex::encode(hashed_key_element(
                                                            &asset_id.encode(),
                                                            hasher
                                                        ))
                                                    );
                                                    let value_fetch = get_value_from_storage(
                                                        client,
                                                        &key_assets_metadata,
                                                        block,
                                                    )
                                                    .await?;
                                                    if let Value::String(ref string_value) =
                                                        value_fetch
                                                    {
                                                        let value_data = unhex(
                                                            string_value,
                                                            NotHexError::StorageValue,
                                                        )?;
                                                        let value = decode_all_as_type::<
                                                            &[u8],
                                                            (),
                                                            RuntimeMetadataV15,
                                                        >(
                                                            value_ty,
                                                            &value_data.as_ref(),
                                                            &mut (),
                                                            &metadata_v15.types,
                                                        )?;

                                                        let mut name = None;
                                                        let mut symbol = None;
                                                        let mut decimals = None;

                                                        if let ParsedData::Composite(fields) =
                                                            value.data
                                                        {
                                                            for field_data in fields.iter() {
                                                                if let Some(field_name) =
                                                                    &field_data.field_name
                                                                {
                                                                    match field_name.as_str() {
                                                                "name" => match &field_data.data.data {
                                                                    ParsedData::Text{text, specialty: _} => {
                                                                        name = Some(text.to_owned());
                                                                    },
                                                                    ParsedData::Sequence(sequence) => {
                                                                        if let Sequence::U8(bytes) = &sequence.data {
                                                                            if let Ok(name_from_bytes) = String::from_utf8(bytes.to_owned()) {
                                                                                name = Some(name_from_bytes);
                                                                            }
                                                                        }
                                                                    }
                                                                    ParsedData::Composite(fields) => {
                                                                        if fields.len() == 1 {
                                                                            match &fields[0].data.data {
                                                                                ParsedData::Text{text, specialty: _} => {
                                                                                    name = Some(text.to_owned());
                                                                                },
                                                                                ParsedData::Sequence(sequence) => {
                                                                                    if let Sequence::U8(bytes) = &sequence.data {
                                                                                        if let Ok(name_from_bytes) = String::from_utf8(bytes.to_owned()) {
                                                                                            name = Some(name_from_bytes);
                                                                                        }
                                                                                    }
                                                                                },
                                                                                _ => {},
                                                                            }
                                                                        }
                                                                    },
                                                                    _ => {},
                                                                },
                                                                "symbol" => match &field_data.data.data {
                                                                    ParsedData::Text{text, specialty: _} => {
                                                                        symbol = Some(text.to_owned());
                                                                    },
                                                                    ParsedData::Sequence(sequence) => {
                                                                        if let Sequence::U8(bytes) = &sequence.data {
                                                                            if let Ok(symbol_from_bytes) = String::from_utf8(bytes.to_owned()) {
                                                                                symbol = Some(symbol_from_bytes);
                                                                            }
                                                                        }
                                                                    }
                                                                    ParsedData::Composite(fields) => {
                                                                        if fields.len() == 1 {
                                                                            match &fields[0].data.data {
                                                                                ParsedData::Text{text, specialty: _} => {
                                                                                    symbol = Some(text.to_owned());
                                                                                },
                                                                                ParsedData::Sequence(sequence) => {
                                                                                    if let Sequence::U8(bytes) = &sequence.data {
                                                                                        if let Ok(symbol_from_bytes) = String::from_utf8(bytes.to_owned()) {
                                                                                            symbol = Some(symbol_from_bytes);
                                                                                        }
                                                                                    }
                                                                                },
                                                                                _ => {},
                                                                            }
                                                                        }
                                                                    },
                                                                    _ => {},
                                                                },
                                                                "decimals" => {
                                                                    if let ParsedData::PrimitiveU8{value, specialty: _} = field_data.data.data {
                                                                        decimals = Some(value);
                                                                    }
                                                                },
                                                                _ => {},
                                                            }
                                                                }
                                                                if name.is_some()
                                                                    && symbol.is_some()
                                                                    && decimals.is_some()
                                                                {
                                                                    break;
                                                                }
                                                            }
                                                            if let (Some(symbol), Some(decimals)) =
                                                                (symbol, decimals)
                                                            {
                                                                assets_set.insert(
                                                                    symbol,
                                                                    CurrencyProperties {
                                                                        chain_name: chain_name
                                                                            .clone(),
                                                                        kind: TokenKind::Asset,
                                                                        decimals,
                                                                        rpc_url: rpc_url
                                                                            .to_string(),
                                                                        asset_id: Some(asset_id),
                                                                        ss58: specs.base58prefix,
                                                                    },
                                                                );
                                                            }
                                                        } else {
                                                            return Err(
                                                                ChainError::AssetMetadataUnexpected,
                                                            );
                                                        }
                                                    }
                                                }

                                                _ => return Err(ChainError::AssetMetadataType),
                                            }
                                        } else {
                                            return Err(ChainError::AssetMetadataMapSize);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(assets_set)
}

pub async fn asset_balance_at_account(
    client: &WsClient,
    block: &BlockHash,
    metadata_v15: &RuntimeMetadataV15,
    account_id: &AccountId32,
    asset_id: AssetId,
) -> Result<Balance, ChainError> {
    let query = asset_balance_query(metadata_v15, account_id, asset_id)?;

    let value_fetch = get_value_from_storage(client, &query.key, block).await?;
    if let Value::String(ref string_value) = value_fetch {
        let value_data = unhex(string_value, NotHexError::StorageValue)?;
        let value = decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            &query.value_ty,
            &value_data.as_ref(),
            &mut (),
            &metadata_v15.types,
        )?;
        if let ParsedData::Composite(fields) = value.data {
            for field in fields.iter() {
                if let ParsedData::PrimitiveU128 {
                    value,
                    specialty: SpecialtyUnsignedInteger::Balance,
                } = field.data.data
                {
                    return Ok(Balance(value));
                }
            }
            Err(ChainError::AssetBalanceNotFound)
        } else {
            Err(ChainError::AssetBalanceFormat)
        }
    } else {
        Err(ChainError::StorageValueFormat(value_fetch))
    }
}

pub async fn system_balance_at_account(
    client: &WsClient,
    block: &BlockHash,
    metadata_v15: &RuntimeMetadataV15,
    account_id: &AccountId32,
) -> Result<Balance, ChainError> {
    let query = system_balance_query(metadata_v15, account_id)?;

    let value_fetch = get_value_from_storage(client, &query.key, block).await?;
    if let Value::String(ref string_value) = value_fetch {
        let value_data = unhex(string_value, NotHexError::StorageValue)?;
        let value = decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            &query.value_ty,
            &value_data.as_ref(),
            &mut (),
            &metadata_v15.types,
        )?;
        if let ParsedData::Composite(fields) = value.data {
            for field in fields.iter() {
                if field.field_name == Some("data".to_string()) {
                    if let ParsedData::Composite(inner_fields) = &field.data.data {
                        for inner_field in inner_fields.iter() {
                            if inner_field.field_name == Some("free".to_string()) {
                                if let ParsedData::PrimitiveU128 {
                                    value,
                                    specialty: SpecialtyUnsignedInteger::Balance,
                                } = inner_field.data.data
                                {
                                    return Ok(Balance(value));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Err(ChainError::BalanceNotFound)
}

pub async fn transfer_events(
    client: &WsClient,
    block: &BlockHash,
    metadata_v15: &RuntimeMetadataV15,
) -> Result<Vec<Event>, ChainError> {
    let events_entry_metadata = events_entry_metadata(&metadata_v15)?;

    events_at_block(
        &client,
        block,
        Some(EventFilter {
            pallet: BALANCES,
            optional_event_variant: Some(TRANSFER),
        }),
        events_entry_metadata,
        &metadata_v15.types,
    )
    .await
}

async fn events_at_block(
    client: &WsClient,
    block: &BlockHash,
    optional_filter: Option<EventFilter<'_>>,
    events_entry_metadata: &StorageEntryMetadata<PortableForm>,
    types: &PortableRegistry,
) -> Result<Vec<Event>, ChainError> {
    let keys_from_storage_vec = get_keys_from_storage(client, "System", "Events", block).await?;
    let mut out = Vec::new();
    for keys_from_storage in keys_from_storage_vec {
        match keys_from_storage {
            Value::Array(ref keys_array) => {
                for key in keys_array {
                    if let Value::String(key) = key {
                        let data_from_storage = get_value_from_storage(client, &key, block).await?;
                        let key_bytes = unhex(&key, NotHexError::StorageValue)?;
                        let value_bytes =
                            if let Value::String(data_from_storage) = data_from_storage {
                                unhex(&data_from_storage, NotHexError::StorageValue)?
                            } else {
                                return Err(ChainError::StorageValueFormat(data_from_storage));
                            };
                        let storage_data =
                            decode_as_storage_entry::<&[u8], (), RuntimeMetadataV15>(
                                &key_bytes.as_ref(),
                                &value_bytes.as_ref(),
                                &mut (),
                                events_entry_metadata,
                                types,
                            )
                            .expect("RAM stored metadata access");
                        if let ParsedData::SequenceRaw(sequence_raw) = storage_data.value.data {
                            for sequence_element in sequence_raw.data {
                                if let ParsedData::Composite(event_record) = sequence_element {
                                    for event_record_element in event_record {
                                        if event_record_element.field_name
                                            == Some("event".to_string())
                                        {
                                            if let ParsedData::Event(Event(ref event)) =
                                                event_record_element.data.data
                                            {
                                                if let Some(ref filter) = optional_filter {
                                                    if let Some(event_variant) =
                                                        filter.optional_event_variant
                                                    {
                                                        if event.pallet_name == filter.pallet
                                                            && event.variant_name == event_variant
                                                        {
                                                            out.push(Event(event.to_owned()));
                                                        }
                                                    } else if event.pallet_name == filter.pallet {
                                                        out.push(Event(event.to_owned()));
                                                    }
                                                } else {
                                                    out.push(Event(event.to_owned()));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                tracing::warn!("{keys_from_storage}");
            }
        }
    }
    return Ok(out);
}

pub async fn current_block_number(
    client: &WsClient,
    metadata: &RuntimeMetadataV15,
    block: &BlockHash,
) -> Result<u32, ChainError> {
    let block_number_query = block_number_query(metadata)?;
    let fetched_value = get_value_from_storage(client, &block_number_query.key, block).await?;
    if let Value::String(hex_data) = fetched_value {
        let value_data = unhex(&hex_data, NotHexError::StorageValue)?;
        let value = decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            &block_number_query.value_ty,
            &value_data.as_ref(),
            &mut (),
            &metadata.types,
        )?;
        if let ParsedData::PrimitiveU32 {
            value,
            specialty: _,
        } = value.data
        {
            Ok(value)
        } else {
            Err(ChainError::BlockNumberFormat)
        }
    } else {
        Err(ChainError::StorageValueFormat(fetched_value))
    }
}

pub async fn get_nonce(
    client: &WsClient,
    account_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let rpc_params = rpc_params![account_id];
    println!("{rpc_params:?}");
    let nonce: Value = client.request("account_nextIndex", rpc_params).await?;
    println!("{nonce:?}");
    Ok(())
}

pub async fn send_stuff(client: &WsClient, data: &str) -> Result<Value, ChainError> {
    let rpc_params = rpc_params![data];
    Ok(client
        .request("author_submitAndWatchExtrinsic", rpc_params)
        .await?)
}
