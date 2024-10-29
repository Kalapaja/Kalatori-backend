//! Utils to process chain data without accessing the chain

use crate::{chain::definitions::BlockHash, definitions::api_v2::AssetId, error::ChainError};
use codec::Encode;
use frame_metadata::{
    v14::StorageHasher,
    v15::{RuntimeMetadataV15, StorageEntryMetadata, StorageEntryType},
};
use hashing::{blake2_128, blake2_256, twox_128, twox_256, twox_64};
use scale_info::{form::PortableForm, TypeDef, TypeDefPrimitive};
use serde_json::{Map, Value};
use substrate_constructor::{
    fill_prepare::{
        prepare_type, EraToFill, PrimitiveToFill, RegularPrimitiveToFill, SpecialTypeToFill,
        SpecialtyUnsignedToFill, TransactionToFill, TypeContentToFill, TypeToFill, UnsignedToFill,
        VariantSelector, DEFAULT_PERIOD,
    },
    finalize::Finalize,
    storage_query::{
        EntrySelector, EntrySelectorFunctional, FinalizedStorageQuery, StorageEntryTypeToFill,
        StorageSelector, StorageSelectorFunctional,
    },
};
use substrate_crypto_light::common::AccountId32;
use substrate_parser::{
    cards::{ExtendedData, FieldData, ParsedData},
    decode_all_as_type,
    decoding_sci::Ty,
    propagated::Propagated,
    special_indicators::SpecialtyUnsignedInteger,
    ResolveType, ShortSpecs,
};

pub struct AssetTransferConstructor<'a> {
    pub asset_id: u32,
    pub amount: u128,
    pub to_account: &'a AccountId32,
}

pub fn construct_single_asset_transfer_call(
    metadata: &RuntimeMetadataV15,
    asset_transfer_constructor: &AssetTransferConstructor,
) -> Result<CallToFill, ChainError> {
    let mut call = prepare_type::<(), RuntimeMetadataV15>(
        &Ty::Symbol(&metadata.extrinsic.call_ty),
        &mut (),
        &metadata.types,
        Propagated::new(),
    )?;

    if let TypeContentToFill::Variant(ref mut pallet_selector) = call.content {
        let mut index_assets_in_pallets = None;

        for (index_pallet, variant_pallet) in pallet_selector.available_variants.iter().enumerate()
        {
            if variant_pallet.name == "Assets" {
                index_assets_in_pallets = Some(index_pallet);
                break;
            }
        }

        if let Some(index_assets_in_pallets) = index_assets_in_pallets {
            *pallet_selector = VariantSelector::new_at::<(), RuntimeMetadataV15>(
                &pallet_selector.available_variants,
                &mut (),
                &metadata.types,
                index_assets_in_pallets,
            )?;

            if pallet_selector.selected.fields_to_fill.len() == 1 {
                if let TypeContentToFill::Variant(ref mut method_selector) =
                    pallet_selector.selected.fields_to_fill[0]
                        .type_to_fill
                        .content
                {
                    let mut index_transfer_in_methods = None;

                    for (index_method, variant_method) in
                        method_selector.available_variants.iter().enumerate()
                    {
                        if variant_method.name.as_str() == "transfer" {
                            index_transfer_in_methods = Some(index_method);
                            break;
                        }
                    }

                    if let Some(index_transfer_in_methods) = index_transfer_in_methods {
                        *method_selector = VariantSelector::new_at::<(), RuntimeMetadataV15>(
                            &method_selector.available_variants,
                            &mut (),
                            &metadata.types,
                            index_transfer_in_methods,
                        )?;

                        for field in method_selector.selected.fields_to_fill.iter_mut() {
                            if let Some(ref mut field_name) = field.field_name {
                                match field_name.as_str() {
                                    "target" => {
                                        if let TypeContentToFill::Variant(ref mut dest_selector) =
                                            field.type_to_fill.content
                                        {
                                            let mut index_account_id_in_dest_selector = None;

                                            for (index, dest_variant) in
                                                dest_selector.available_variants.iter().enumerate()
                                            {
                                                if dest_variant.name == "Id" {
                                                    index_account_id_in_dest_selector = Some(index);
                                                    break;
                                                }
                                            }

                                            if let Some(index_account_id_in_dest_selector) =
                                                index_account_id_in_dest_selector
                                            {
                                                *dest_selector = VariantSelector::new_at::<
                                                    (),
                                                    RuntimeMetadataV15,
                                                >(
                                                    &dest_selector.available_variants,
                                                    &mut (),
                                                    &metadata.types,
                                                    index_account_id_in_dest_selector,
                                                )?;

                                                if dest_selector.selected.fields_to_fill.len() == 1
                                                {
                                                    if let TypeContentToFill::SpecialType(
                                                        SpecialTypeToFill::AccountId32(
                                                            ref mut account_to_fill,
                                                        ),
                                                    ) = dest_selector.selected.fields_to_fill[0]
                                                        .type_to_fill
                                                        .content
                                                    {
                                                        *account_to_fill = Some(
                                                            asset_transfer_constructor
                                                                .to_account
                                                                .to_owned(),
                                                        )
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    "id" => match field.type_to_fill.content {
                                        TypeContentToFill::Primitive(
                                            PrimitiveToFill::CompactUnsigned(
                                                SpecialtyUnsignedToFill {
                                                    content: UnsignedToFill::U32(ref mut value),
                                                    specialty: SpecialtyUnsignedInteger::None,
                                                },
                                            ),
                                        ) => {
                                            *value = Some(asset_transfer_constructor.asset_id);
                                        }
                                        TypeContentToFill::Primitive(
                                            PrimitiveToFill::Unsigned(SpecialtyUnsignedToFill {
                                                content: UnsignedToFill::U32(ref mut value),
                                                specialty: SpecialtyUnsignedInteger::None,
                                            }),
                                        ) => {
                                            *value = Some(asset_transfer_constructor.asset_id);
                                        }
                                        _ => {}
                                    },
                                    "amount" => match field.type_to_fill.content {
                                        TypeContentToFill::Primitive(
                                            PrimitiveToFill::CompactUnsigned(
                                                SpecialtyUnsignedToFill {
                                                    content: UnsignedToFill::U128(ref mut value),
                                                    specialty: SpecialtyUnsignedInteger::Balance,
                                                },
                                            ),
                                        ) => {
                                            *value = Some(asset_transfer_constructor.amount);
                                        }
                                        TypeContentToFill::Primitive(
                                            PrimitiveToFill::Unsigned(SpecialtyUnsignedToFill {
                                                content: UnsignedToFill::U128(ref mut value),
                                                specialty: SpecialtyUnsignedInteger::Balance,
                                            }),
                                        ) => {
                                            *value = Some(asset_transfer_constructor.amount);
                                        }
                                        _ => {}
                                    },
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(CallToFill(call))
}

pub struct BalanceTransferConstructor<'a> {
    pub amount: u128,
    pub to_account: &'a AccountId32,
    pub is_clearing: bool,
}

#[derive(Clone, Debug)]
pub struct CallToFill(pub TypeToFill);

pub fn construct_batch_transaction(
    metadata: &RuntimeMetadataV15,
    genesis_hash: BlockHash,
    author: AccountId32,
    call_set: &[CallToFill],
    block: BlockHash,
    block_number: u32,
    nonce: u32,
) -> Result<TransactionToFill, ChainError> {
    let mut transaction_to_fill = TransactionToFill::init(&mut (), metadata, genesis_hash.0)?;

    // deal with author
    match transaction_to_fill.author.content {
        TypeContentToFill::Composite(ref mut fields_to_fill) => {
            if fields_to_fill.len() == 1 {
                if let TypeContentToFill::SpecialType(SpecialTypeToFill::AccountId32(ref mut a)) =
                    fields_to_fill[0].type_to_fill.content
                {
                    *a = Some(author);
                }
            }
        }
        TypeContentToFill::SpecialType(SpecialTypeToFill::AccountId32(ref mut a)) => {
            *a = Some(author);
        }
        TypeContentToFill::Variant(ref mut variant_selector) => {
            let mut index_account_id = None;

            for (index, variant_id) in variant_selector.available_variants.iter().enumerate() {
                if variant_id.name == "Id" {
                    index_account_id = Some(index);
                    break;
                }
            }

            if let Some(index_account_id) = index_account_id {
                *variant_selector = VariantSelector::new_at::<(), RuntimeMetadataV15>(
                    &variant_selector.available_variants,
                    &mut (),
                    &metadata.types,
                    index_account_id,
                )?;

                if variant_selector.selected.fields_to_fill.len() == 1 {
                    if let TypeContentToFill::SpecialType(SpecialTypeToFill::AccountId32(
                        ref mut a,
                    )) = variant_selector.selected.fields_to_fill[0]
                        .type_to_fill
                        .content
                    {
                        *a = Some(author);
                    }
                }
            }
        }
        _ => {}
    }

    // deal with call
    transaction_to_fill.call = construct_batch_call(metadata, call_set)?.0;

    // set era to mortal
    for ext in transaction_to_fill.extensions.iter_mut() {
        match ext.content {
            TypeContentToFill::Composite(ref mut fields) => {
                if fields.len() == 1 {
                    if let TypeContentToFill::SpecialType(SpecialTypeToFill::Era(ref mut era)) =
                        fields[0].type_to_fill.content
                    {
                        *era = EraToFill::Mortal {
                            period_entry: DEFAULT_PERIOD,
                            block_number_entry: None,
                        };
                        break;
                    }
                }
            }
            TypeContentToFill::SpecialType(SpecialTypeToFill::Era(ref mut era)) => {
                *era = EraToFill::Mortal {
                    period_entry: DEFAULT_PERIOD,
                    block_number_entry: None,
                };
                break;
            }
            _ => {}
        }
    }

    transaction_to_fill.populate_block_info(Some(block.0), Some(block_number.into()));
    transaction_to_fill.populate_nonce(nonce);

    for ext in transaction_to_fill.extensions.iter_mut() {
        if ext.finalize().is_none() {
            println!("{ext:?}");
        }
    }

    Ok(transaction_to_fill)
}

pub fn construct_batch_call(
    metadata: &RuntimeMetadataV15,
    call_set: &[CallToFill],
) -> Result<CallToFill, ChainError> {
    let mut call = prepare_type::<(), RuntimeMetadataV15>(
        &Ty::Symbol(&metadata.extrinsic.call_ty),
        &mut (),
        &metadata.types,
        Propagated::new(),
    )?;

    if let TypeContentToFill::Variant(ref mut pallet_selector) = call.content {
        let mut index_utility_in_pallets = None;

        for (index_pallet, variant_pallet) in pallet_selector.available_variants.iter().enumerate()
        {
            if variant_pallet.name == "Utility" {
                index_utility_in_pallets = Some(index_pallet);
                break;
            }
        }

        if let Some(index_utility_in_pallets) = index_utility_in_pallets {
            *pallet_selector = VariantSelector::new_at::<(), RuntimeMetadataV15>(
                &pallet_selector.available_variants,
                &mut (),
                &metadata.types,
                index_utility_in_pallets,
            )?;

            if pallet_selector.selected.fields_to_fill.len() == 1 {
                if let TypeContentToFill::Variant(ref mut method_selector) =
                    pallet_selector.selected.fields_to_fill[0]
                        .type_to_fill
                        .content
                {
                    let mut index_batch_all_in_methods = None;

                    for (index_method, variant_method) in
                        method_selector.available_variants.iter().enumerate()
                    {
                        if variant_method.name == "batch_all" {
                            index_batch_all_in_methods = Some(index_method);
                            break;
                        }
                    }

                    if let Some(index_batch_all_in_methods) = index_batch_all_in_methods {
                        *method_selector = VariantSelector::new_at::<(), RuntimeMetadataV15>(
                            &method_selector.available_variants,
                            &mut (),
                            &metadata.types,
                            index_batch_all_in_methods,
                        )?;

                        if method_selector.selected.fields_to_fill.len() == 1
                            && method_selector.selected.fields_to_fill[0].field_name
                                == Some("calls".to_string())
                        {
                            if let TypeContentToFill::SequenceRegular(ref mut calls_sequence) =
                                method_selector.selected.fields_to_fill[0]
                                    .type_to_fill
                                    .content
                            {
                                calls_sequence.content = call_set
                                    .iter()
                                    .map(|call| call.0.content.to_owned())
                                    .collect();
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(CallToFill(call))
}

pub fn construct_single_balance_transfer_call(
    metadata: &RuntimeMetadataV15,
    balance_transfer_constructor: &BalanceTransferConstructor,
) -> Result<CallToFill, ChainError> {
    let mut call = prepare_type::<(), RuntimeMetadataV15>(
        &Ty::Symbol(&metadata.extrinsic.call_ty),
        &mut (),
        &metadata.types,
        Propagated::new(),
    )?;

    if let TypeContentToFill::Variant(ref mut pallet_selector) = call.content {
        let mut index_balances_in_pallets = None;

        for (index_pallet, variant_pallet) in pallet_selector.available_variants.iter().enumerate()
        {
            if variant_pallet.name == "Balances" {
                index_balances_in_pallets = Some(index_pallet);
                break;
            }
        }

        if let Some(index_balances_in_pallets) = index_balances_in_pallets {
            *pallet_selector = VariantSelector::new_at::<(), RuntimeMetadataV15>(
                &pallet_selector.available_variants,
                &mut (),
                &metadata.types,
                index_balances_in_pallets,
            )?;

            if pallet_selector.selected.fields_to_fill.len() == 1 {
                if let TypeContentToFill::Variant(ref mut method_selector) =
                    pallet_selector.selected.fields_to_fill[0]
                        .type_to_fill
                        .content
                {
                    let mut index_transfer_in_methods = None;

                    for (index_method, variant_method) in
                        method_selector.available_variants.iter().enumerate()
                    {
                        match variant_method.name.as_str() {
                            "transfer_keep_alive" => {
                                if !balance_transfer_constructor.is_clearing {
                                    index_transfer_in_methods = Some(index_method)
                                }
                            }
                            "transfer_all" => {
                                if balance_transfer_constructor.is_clearing {
                                    index_transfer_in_methods = Some(index_method)
                                }
                            }
                            _ => {}
                        }
                        if index_transfer_in_methods.is_some() {
                            break;
                        }
                    }

                    if let Some(index_transfer_in_methods) = index_transfer_in_methods {
                        *method_selector = VariantSelector::new_at::<(), RuntimeMetadataV15>(
                            &method_selector.available_variants,
                            &mut (),
                            &metadata.types,
                            index_transfer_in_methods,
                        )?;

                        for field in method_selector.selected.fields_to_fill.iter_mut() {
                            if let Some(ref mut field_name) = field.field_name {
                                match field_name.as_str() {
                                    "dest" => {
                                        if let TypeContentToFill::Variant(ref mut dest_selector) =
                                            field.type_to_fill.content
                                        {
                                            let mut index_account_id_in_dest_selector = None;

                                            for (index, dest_variant) in
                                                dest_selector.available_variants.iter().enumerate()
                                            {
                                                if dest_variant.name == "Id" {
                                                    index_account_id_in_dest_selector = Some(index);
                                                    break;
                                                }
                                            }

                                            if let Some(index_account_id_in_dest_selector) =
                                                index_account_id_in_dest_selector
                                            {
                                                *dest_selector = VariantSelector::new_at::<
                                                    (),
                                                    RuntimeMetadataV15,
                                                >(
                                                    &dest_selector.available_variants,
                                                    &mut (),
                                                    &metadata.types,
                                                    index_account_id_in_dest_selector,
                                                )?;

                                                if dest_selector.selected.fields_to_fill.len() == 1
                                                {
                                                    if let TypeContentToFill::SpecialType(
                                                        SpecialTypeToFill::AccountId32(
                                                            ref mut account_to_fill,
                                                        ),
                                                    ) = dest_selector.selected.fields_to_fill[0]
                                                        .type_to_fill
                                                        .content
                                                    {
                                                        *account_to_fill = Some(
                                                            balance_transfer_constructor
                                                                .to_account
                                                                .to_owned(),
                                                        )
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    "keep_alive" => {
                                        if let TypeContentToFill::Primitive(
                                            PrimitiveToFill::Regular(RegularPrimitiveToFill::Bool(
                                                ref mut keep_alive_bool,
                                            )),
                                        ) = field.type_to_fill.content
                                        {
                                            *keep_alive_bool = Some(false);
                                        }
                                    }
                                    "value" => match field.type_to_fill.content {
                                        TypeContentToFill::Primitive(
                                            PrimitiveToFill::CompactUnsigned(
                                                SpecialtyUnsignedToFill {
                                                    content: UnsignedToFill::U128(ref mut value),
                                                    specialty: SpecialtyUnsignedInteger::Balance,
                                                },
                                            ),
                                        ) => {
                                            *value = Some(balance_transfer_constructor.amount);
                                        }
                                        TypeContentToFill::Primitive(
                                            PrimitiveToFill::Unsigned(SpecialtyUnsignedToFill {
                                                content: UnsignedToFill::U128(ref mut value),
                                                specialty: SpecialtyUnsignedInteger::Balance,
                                            }),
                                        ) => {
                                            *value = Some(balance_transfer_constructor.amount);
                                        }
                                        _ => {}
                                    },
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(CallToFill(call))
}

pub fn block_number_query(
    metadata_v15: &RuntimeMetadataV15,
) -> Result<FinalizedStorageQuery, ChainError> {
    let storage_selector = StorageSelector::init(&mut (), metadata_v15)?;

    if let StorageSelector::Functional(mut storage_selector_functional) = storage_selector {
        let mut index_system_in_pallet_selector = None;

        for (index, pallet) in storage_selector_functional
            .available_pallets
            .iter()
            .enumerate()
        {
            if pallet.prefix == "System" {
                index_system_in_pallet_selector = Some(index);
                break;
            }
        }

        if let Some(index_system_in_pallet_selector) = index_system_in_pallet_selector {
            // System - Number (current block number)
            storage_selector_functional =
                StorageSelectorFunctional::new_at::<(), RuntimeMetadataV15>(
                    &storage_selector_functional.available_pallets,
                    &mut (),
                    &metadata_v15.types,
                    index_system_in_pallet_selector,
                )?;

            if let EntrySelector::Functional(ref mut entry_selector_functional) =
                storage_selector_functional.query.entry_selector
            {
                let mut entry_index = None;
                for (index, entry) in entry_selector_functional
                    .available_entries
                    .iter()
                    .enumerate()
                {
                    if entry.name == "Number" {
                        entry_index = Some(index);
                        break;
                    }
                }
                if let Some(entry_index) = entry_index {
                    *entry_selector_functional =
                        EntrySelectorFunctional::new_at::<(), RuntimeMetadataV15>(
                            &entry_selector_functional.available_entries,
                            &mut (),
                            &metadata_v15.types,
                            entry_index,
                        )?;

                    Ok(storage_selector_functional
                        .query
                        .finalize()
                        .transpose()
                        .ok_or(ChainError::StorageQuery)??)
                } else {
                    Err(ChainError::NoBlockNumberDefinition)
                }
            } else {
                Err(ChainError::NoStorageInSystem)
            }
        } else {
            Err(ChainError::NoSystem)
        }
    } else {
        Err(ChainError::NoStorage)
    }
}

pub fn events_entry_metadata(
    metadata: &RuntimeMetadataV15,
) -> Result<&StorageEntryMetadata<PortableForm>, ChainError> {
    let mut found_events_entry_metadata = None;
    for pallet in &metadata.pallets {
        if let Some(storage) = &pallet.storage {
            if storage.prefix == "System" {
                for entry in &storage.entries {
                    if entry.name == "Events" {
                        found_events_entry_metadata = Some(entry);
                        break;
                    }
                }
            }
        }
    }
    match found_events_entry_metadata {
        Some(a) => Ok(a),
        None => Err(ChainError::EventsNonexistant),
    }
}

pub fn was_balance_received_at_account(
    known_account: &AccountId32,
    balance_transfer_event_fields: &[FieldData],
) -> bool {
    let mut found_receiver = None;
    for field in balance_transfer_event_fields.iter() {
        if let Some(ref field_name) = field.field_name {
            if field_name == "to" {
                if let ParsedData::Id(ref account_id32) = field.data.data {
                    if found_receiver.is_none() {
                        found_receiver = Some(account_id32);
                    } else {
                        found_receiver = None;
                        break;
                    }
                }
            }
        }
    }
    if let Some(receiver) = found_receiver {
        receiver.0 == known_account.0
    } else {
        false
    }
}

pub fn asset_balance_query(
    metadata_v15: &RuntimeMetadataV15,
    account_id: &AccountId32,
    asset_id: AssetId,
) -> Result<FinalizedStorageQuery, ChainError> {
    let storage_selector = StorageSelector::init(&mut (), metadata_v15)?;

    if let StorageSelector::Functional(mut storage_selector_functional) = storage_selector {
        let mut index_assets_in_pallet_selector: Option<usize> = None;

        for (index, pallet) in storage_selector_functional
            .available_pallets
            .iter()
            .enumerate()
        {
            match pallet.prefix.as_str() {
                "Assets" => index_assets_in_pallet_selector = Some(index),
                _ => {}
            }
            if index_assets_in_pallet_selector.is_some() {
                break;
            }
        }
        if let Some(index_assets_in_pallet_selector) = index_assets_in_pallet_selector {
            storage_selector_functional =
                StorageSelectorFunctional::new_at::<(), RuntimeMetadataV15>(
                    &storage_selector_functional.available_pallets,
                    &mut (),
                    &metadata_v15.types,
                    index_assets_in_pallet_selector,
                )?;
            if let EntrySelector::Functional(ref mut entry_selector_functional) =
                storage_selector_functional.query.entry_selector
            {
                let mut entry_index = None;
                for (index, entry) in entry_selector_functional
                    .available_entries
                    .iter()
                    .enumerate()
                {
                    if entry.name == "Account" {
                        entry_index = Some(index);
                        break;
                    }
                }
                if let Some(entry_index) = entry_index {
                    *entry_selector_functional =
                        EntrySelectorFunctional::new_at::<(), RuntimeMetadataV15>(
                            &entry_selector_functional.available_entries,
                            &mut (),
                            &metadata_v15.types,
                            entry_index,
                        )?;
                    if let StorageEntryTypeToFill::Map {
                        hashers: _,
                        ref mut key_to_fill,
                        value: _,
                    } = entry_selector_functional.selected_entry.type_to_fill
                    {
                        if let TypeContentToFill::Tuple(ref mut set) = key_to_fill.content {
                            for ty in set.iter_mut() {
                                match ty.content {
                                    TypeContentToFill::SpecialType(
                                        SpecialTypeToFill::AccountId32(ref mut account_to_fill),
                                    ) => *account_to_fill = Some(*account_id),
                                    TypeContentToFill::Primitive(
                                        PrimitiveToFill::CompactUnsigned(
                                            ref mut specialty_unsigned_to_fill,
                                        ),
                                    ) => {
                                        if let UnsignedToFill::U32(ref mut u) =
                                            specialty_unsigned_to_fill.content
                                        {
                                            *u = Some(asset_id);
                                        }
                                    }
                                    TypeContentToFill::Primitive(PrimitiveToFill::Unsigned(
                                        ref mut specialty_unsigned_to_fill,
                                    )) => {
                                        if let UnsignedToFill::U32(ref mut u) =
                                            specialty_unsigned_to_fill.content
                                        {
                                            *u = Some(asset_id);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }

        storage_selector_functional
            .query
            .finalize()?
            .ok_or(ChainError::StorageQuery)
    } else {
        Err(ChainError::StorageQuery)
    }
}

pub fn system_balance_query(
    metadata_v15: &RuntimeMetadataV15,
    account_id: &AccountId32,
) -> Result<FinalizedStorageQuery, ChainError> {
    let storage_selector = StorageSelector::init(&mut (), metadata_v15)?;
    let mut index_system_in_pallet_selector = None;

    if let StorageSelector::Functional(mut storage_selector_functional) = storage_selector {
        for (index, pallet) in storage_selector_functional
            .available_pallets
            .iter()
            .enumerate()
        {
            match pallet.prefix.as_str() {
                "System" => index_system_in_pallet_selector = Some(index),
                _ => {}
            }
            if index_system_in_pallet_selector.is_some() {
                break;
            }
        }
        if let Some(index_system_in_pallet_selector) = index_system_in_pallet_selector {
            storage_selector_functional =
                StorageSelectorFunctional::new_at::<(), RuntimeMetadataV15>(
                    &storage_selector_functional.available_pallets,
                    &mut (),
                    &metadata_v15.types,
                    index_system_in_pallet_selector,
                )?;

            if let EntrySelector::Functional(ref mut entry_selector_functional) =
                storage_selector_functional.query.entry_selector
            {
                let mut entry_index = None;
                for (index, entry) in entry_selector_functional
                    .available_entries
                    .iter()
                    .enumerate()
                {
                    if entry.name == "Account" {
                        entry_index = Some(index);
                        break;
                    }
                }
                if let Some(entry_index) = entry_index {
                    *entry_selector_functional =
                        EntrySelectorFunctional::new_at::<(), RuntimeMetadataV15>(
                            &entry_selector_functional.available_entries,
                            &mut (),
                            &metadata_v15.types,
                            entry_index,
                        )?;
                    if let StorageEntryTypeToFill::Map {
                        hashers: _,
                        ref mut key_to_fill,
                        value: _,
                    } = entry_selector_functional.selected_entry.type_to_fill
                    {
                        if let TypeContentToFill::SpecialType(SpecialTypeToFill::AccountId32(
                            ref mut account_to_fill,
                        )) = key_to_fill.content
                        {
                            *account_to_fill = Some(*account_id)
                        }
                    }
                }
            }
        }
        storage_selector_functional
            .query
            .finalize()?
            .ok_or(ChainError::StorageQuery)
    } else {
        Err(ChainError::StorageQuery)
    }
}

pub fn hashed_key_element(data: &[u8], hasher: &StorageHasher) -> Vec<u8> {
    match hasher {
        StorageHasher::Blake2_128 => blake2_128(data).to_vec(),
        StorageHasher::Blake2_256 => blake2_256(data).to_vec(),
        StorageHasher::Blake2_128Concat => [blake2_128(data).to_vec(), data.to_vec()].concat(),
        StorageHasher::Twox128 => twox_128(data).to_vec(),
        StorageHasher::Twox256 => twox_256(data).to_vec(),
        StorageHasher::Twox64Concat => [twox_64(data).to_vec(), data.to_vec()].concat(),
        StorageHasher::Identity => data.to_vec(),
    }
}

pub fn whole_key_u32_value(
    prefix: &str,
    storage_name: &str,
    metadata_v15: &RuntimeMetadataV15,
    entered_data: u32,
) -> Result<String, ChainError> {
    for pallet in metadata_v15.pallets.iter() {
        if let Some(storage) = &pallet.storage {
            if storage.prefix == prefix {
                for entry in storage.entries.iter() {
                    if entry.name == storage_name {
                        match &entry.ty {
                            StorageEntryType::Plain(_) => {
                                return Err(ChainError::StorageEntryNotMap)
                            }
                            StorageEntryType::Map {
                                hashers,
                                key: key_ty,
                                value: _,
                            } => {
                                if hashers.len() == 1 {
                                    let hasher = &hashers[0];
                                    match metadata_v15
                                        .types
                                        .resolve_ty(key_ty.id, &mut ())?
                                        .type_def
                                    {
                                        TypeDef::Primitive(TypeDefPrimitive::U32) => {
                                            return Ok(format!(
                                                "0x{}{}{}",
                                                const_hex::encode(twox_128(prefix.as_bytes())),
                                                const_hex::encode(twox_128(
                                                    storage_name.as_bytes()
                                                )),
                                                const_hex::encode(hashed_key_element(
                                                    &entered_data.encode(),
                                                    hasher
                                                ))
                                            ))
                                        }
                                        _ => return Err(ChainError::StorageKeyNotU32),
                                    }
                                } else {
                                    return Err(ChainError::StorageEntryMapMultiple);
                                }
                            }
                        }
                    }
                }
                return Err(ChainError::StorageKeyNotFound(storage_name.to_string()));
            }
        }
    }
    Err(ChainError::NoPallet)
}

pub fn decimals(x: &Map<String, Value>) -> Result<u8, ChainError> {
    match x.get("tokenDecimals") {
        // decimals info is fetched in `system_properties` rpc call
        Some(a) => match a {
            // fetched decimals value is a number
            Value::Number(b) => match b.as_u64() {
                // number is integer and could be represented as `u64` (the only
                // suitable interpretation available for `Number`)
                Some(c) => match c.try_into() {
                    // this `u64` fits into `u8` that decimals is supposed to be
                    Ok(d) => Ok(d),

                    // this `u64` does not fit into `u8`, this is an error
                    Err(_) => Err(ChainError::DecimalsFormatNotSupported(a.to_string())),
                },

                // number could not be represented as `u64`, this is an error
                None => Err(ChainError::DecimalsFormatNotSupported(a.to_string())),
            },

            // fetched decimals is an array
            Value::Array(b) => {
                // array with only one element
                if b.len() == 1 {
                    // this element is a number, process same as
                    // `Value::Number(_)`
                    if let Value::Number(c) = &b[0] {
                        match c.as_u64() {
                            // number is integer and could be represented as
                            // `u64` (the only suitable interpretation available
                            // for `Number`)
                            Some(d) => match d.try_into() {
                                // this `u64` fits into `u8` that decimals is
                                // supposed to be
                                Ok(f) => Ok(f),

                                // this `u64` does not fit into `u8`, this is an
                                // error
                                Err(_) => {
                                    Err(ChainError::DecimalsFormatNotSupported(a.to_string()))
                                }
                            },

                            // number could not be represented as `u64`, this is
                            // an error
                            None => Err(ChainError::DecimalsFormatNotSupported(a.to_string())),
                        }
                    } else {
                        // element is not a number, this is an error
                        Err(ChainError::DecimalsFormatNotSupported(a.to_string()))
                    }
                } else {
                    // decimals are an array with more than one element
                    Err(ChainError::DecimalsFormatNotSupported(a.to_string()))
                }
            }

            // unexpected decimals format
            _ => Err(ChainError::DecimalsFormatNotSupported(a.to_string())),
        },

        // decimals are missing
        None => Err(ChainError::NoDecimals),
    }
}

pub fn optional_prefix_from_meta(metadata: &RuntimeMetadataV15) -> Option<u16> {
    let mut base58_prefix_data = None;
    for pallet in &metadata.pallets {
        if pallet.name == "System" {
            for system_constant in &pallet.constants {
                if system_constant.name == "SS58Prefix" {
                    base58_prefix_data = Some((&system_constant.value, &system_constant.ty));
                    break;
                }
            }
            break;
        }
    }
    if let Some((value, ty_symbol)) = base58_prefix_data {
        match decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            ty_symbol,
            &value.as_ref(),
            &mut (),
            &metadata.types,
        ) {
            Ok(extended_data) => match extended_data.data {
                ParsedData::PrimitiveU8 {
                    value,
                    specialty: _,
                } => Some(value.into()),
                ParsedData::PrimitiveU16 {
                    value,
                    specialty: _,
                } => Some(value),
                ParsedData::PrimitiveU32 {
                    value,
                    specialty: _,
                } => value.try_into().ok(),
                ParsedData::PrimitiveU64 {
                    value,
                    specialty: _,
                } => value.try_into().ok(),
                ParsedData::PrimitiveU128 {
                    value,
                    specialty: _,
                } => value.try_into().ok(),
                _ => None,
            },
            Err(_) => None,
        }
    } else {
        None
    }
}

pub fn fetch_constant(
    metadata: &RuntimeMetadataV15,
    pallet_name: &str,
    constant_name: &str,
) -> Option<ExtendedData> {
    let mut found = None;
    for pallet in &metadata.pallets {
        if pallet.name == pallet_name {
            for constant in &pallet.constants {
                if constant.name == constant_name {
                    found = Some((&constant.value, &constant.ty));
                    break;
                }
            }
            break;
        }
    }
    if let Some((value, ty_symbol)) = found {
        decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            ty_symbol,
            &value.as_ref(),
            &mut (),
            &metadata.types,
        )
        .ok()
    } else {
        None
    }
}

pub fn system_properties_to_short_specs(
    system_properties: &Map<String, Value>,
    metadata: &RuntimeMetadataV15,
) -> Result<ShortSpecs, ChainError> {
    let optional_prefix_from_meta = optional_prefix_from_meta(metadata);
    let base58prefix = base58prefix(system_properties, optional_prefix_from_meta)?;
    let decimals = decimals(system_properties)?;
    let unit = unit(system_properties)?;
    Ok(ShortSpecs {
        base58prefix,
        decimals,
        unit,
    })
}

pub fn pallet_index(metadata: &RuntimeMetadataV15, name: &str) -> Option<u8> {
    for pallet in &metadata.pallets {
        if pallet.name == name {
            return Some(pallet.index);
        }
    }
    return None;
}

pub fn storage_key(prefix: &str, storage_name: &str) -> String {
    format!(
        "0x{}{}",
        const_hex::encode(twox_128(prefix.as_bytes())),
        const_hex::encode(twox_128(storage_name.as_bytes()))
    )
}

pub fn base58prefix(
    x: &Map<String, Value>,
    optional_prefix_from_meta: Option<u16>,
) -> Result<u16, ChainError> {
    let base58prefix: u16 = match x.get("ss58Format") {
        // base58 prefix is fetched in `system_properties` rpc call
        Some(a) => match a {
            // base58 prefix value is a number
            Value::Number(b) => match b.as_u64() {
                // number is integer and could be represented as `u64` (the only
                // suitable interpretation available for `Number`)
                Some(c) => match c.try_into() {
                    // this `u64` fits into `u16` that base58 prefix is supposed
                    // to be
                    Ok(d) => match optional_prefix_from_meta {
                        // base58 prefix was found in `SS58Prefix` constant of
                        // the network metadata
                        //
                        // check that the prefixes match
                        Some(prefix_from_meta) => {
                            if prefix_from_meta == d {
                                d
                            } else {
                                return Err(ChainError::Base58PrefixMismatch {
                                    specs: d,
                                    meta: prefix_from_meta,
                                });
                            }
                        }

                        // no base58 prefix was found in the network metadata
                        None => d,
                    },

                    // `u64` value does not fit into `u16` base58 prefix format,
                    // this is an error
                    Err(_) => {
                        return Err(ChainError::Base58PrefixFormatNotSupported(a.to_string()))
                    }
                },

                // base58 prefix value could not be presented as `u64` number,
                // this is an error
                None => return Err(ChainError::Base58PrefixFormatNotSupported(a.to_string())),
            },

            // base58 prefix value is not a number, this is an error
            _ => return Err(ChainError::Base58PrefixFormatNotSupported(a.to_string())),
        },

        // no base58 prefix fetched in `system_properties` rpc call
        None => match optional_prefix_from_meta {
            // base58 prefix was found in `SS58Prefix` constant of the network
            // metadata
            Some(prefix_from_meta) => prefix_from_meta,

            // no base58 prefix at all, this is an error
            None => return Err(ChainError::NoBase58Prefix),
        },
    };
    Ok(base58prefix)
}

pub fn unit(x: &Map<String, Value>) -> Result<String, ChainError> {
    match x.get("tokenSymbol") {
        // unit info is fetched in `system_properties` rpc call
        Some(a) => match a {
            // fetched unit value is a `String`
            Value::String(b) => {
                // definitive unit found
                Ok(b.to_string())
            }

            // fetched an array of units
            Value::Array(b) => {
                // array with a single element
                if b.len() == 1 {
                    // single `String` element array, process same as `String`
                    if let Value::String(c) = &b[0] {
                        // definitive unit found
                        Ok(c.to_string())
                    } else {
                        // element is not a `String`, this is an error
                        Err(ChainError::UnitFormatNotSupported(a.to_string()))
                    }
                } else {
                    // units are an array with more than one element
                    Err(ChainError::UnitFormatNotSupported(a.to_string()))
                }
            }

            // unexpected unit format
            _ => Err(ChainError::UnitFormatNotSupported(a.to_string())),
        },

        // unit missing
        None => Err(ChainError::NoUnit),
    }
}
