#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    coin, to_binary, Addr, BankMsg, Binary, Coin, Decimal, Deps, DepsMut, Empty, Env, MessageInfo,
    Order, Reply, ReplyOn, StdError, StdResult, Timestamp, Uint128, WasmMsg,
};
use cw2::set_contract_version;
use cw721::Cw721ReceiveMsg;
use cw_utils::{may_pay, maybe_addr, nonpayable, parse_reply_instantiate_data};
use semver::Version;
use sg_std::math::U64Ext;
use sg_std::StargazeMsgWrapper;
use url::Url;

use open_edition_factory::msg::{OpenEditionMinterCreateMsg, ParamsResponse};
use open_edition_factory::types::NftMetadataType;
use sg1::checked_fair_burn;
use sg2::query::Sg2QueryMsg;
use sg2::{MinterParams, Token};
use sg4::{Status, StatusResponse, SudoMsg};
use sg721::{ExecuteMsg as Sg721ExecuteMsg, InstantiateMsg as Sg721InstantiateMsg};

use crate::error::ContractError;
use crate::helpers::mint_nft_msg;
use crate::msg::{
    ConfigResponse, EndTimeResponse, ExecuteMsg, MintCountResponse, MintPriceResponse, QueryMsg,
    StartTimeResponse, TotalMintCountResponse,
};
use crate::state::{
    increment_token_index, Config, ConfigExtension, CONFIG, MINTER_ADDRS, SG721_ADDRESS, STATUS,
    TOTAL_MINT_COUNT,
};

pub type Response = cosmwasm_std::Response<StargazeMsgWrapper>;
pub type SubMsg = cosmwasm_std::SubMsg<StargazeMsgWrapper>;

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:sg-open-edition-minter";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

const INSTANTIATE_SG721_REPLY_ID: u64 = 1;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    mut msg: OpenEditionMinterCreateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let factory = info.sender.clone();

    // Make sure the sender is the factory contract
    // This will fail if the sender cannot parse a response from the factory contract
    let factory_response: ParamsResponse = deps
        .querier
        .query_wasm_smart(factory.clone(), &Sg2QueryMsg::Params {})?;
    let factory_params = factory_response.params;

    // set default status so it can be queried without failing
    STATUS.save(deps.storage, &Status::default())?;

    match msg.init_msg.nft_data.nft_data_type {
        // If off-chain metadata -> Sanitize base token uri
        NftMetadataType::OffChainMetadata => {
            let base_token_uri = msg
                .init_msg
                .nft_data
                .token_uri
                .as_ref()
                .map(|uri| uri.trim().to_string())
                .map_or_else(|| Err(ContractError::InvalidBaseTokenURI {}), Ok)?;

            if Url::parse(&base_token_uri)?.scheme() != "ipfs" {
                return Err(ContractError::InvalidBaseTokenURI {});
            }

            msg.init_msg.nft_data.token_uri = Some(base_token_uri);
        }
        // If on-chain metadata -> make sure that the image data is a valid URL
        NftMetadataType::OnChainMetadata => {
            let base_img_url = msg
                .init_msg
                .nft_data
                .extension
                .as_ref()
                .and_then(|ext| ext.image.as_ref().map(|img| img.trim()))
                .map(Url::parse)
                .transpose()?
                .map(|url| url.to_string());
            if let Some(ext) = msg.init_msg.nft_data.extension.as_mut() {
                ext.image = base_img_url;
            }
        }
    }

    // Validations/Check at the factory level:
    // - Mint price, # of tokens / address, Start & End time

    // Use default start trading time if not provided
    let mut collection_info = msg.collection_params.info.clone();
    let offset = factory_params.max_trading_offset_secs;
    let default_start_time_with_offset = msg.init_msg.start_time.plus_seconds(offset);
    if let Some(start_trading_time) = msg.collection_params.info.start_trading_time {
        // If trading start time > start_time + offset, return error
        if start_trading_time > default_start_time_with_offset {
            return Err(ContractError::InvalidStartTradingTime(
                start_trading_time,
                default_start_time_with_offset,
            ));
        }
    }
    let start_trading_time = msg
        .collection_params
        .info
        .start_trading_time
        .or(Some(default_start_time_with_offset));
    collection_info.start_trading_time = start_trading_time;
    let config = Config {
        factory: factory.clone(),
        collection_code_id: msg.collection_params.code_id,
        extension: ConfigExtension {
            admin: deps
                .api
                .addr_validate(&msg.collection_params.info.creator)?,
            payment_address: maybe_addr(deps.api, msg.init_msg.payment_address)?,
            per_address_limit: msg.init_msg.per_address_limit,
            start_time: msg.init_msg.start_time,
            end_time: msg.init_msg.end_time,
            nft_data: msg.init_msg.nft_data,
        },
        mint_price: sg2::Fungible(msg.init_msg.mint_price),
        allowed_burn_collections: msg.allowed_burn_collections,
    };

    CONFIG.save(deps.storage, &config)?;

    // Init the minted tokens count
    TOTAL_MINT_COUNT.save(deps.storage, &0)?;

    // Submessage to instantiate sg721 contract
    let submsg = SubMsg {
        msg: WasmMsg::Instantiate {
            code_id: msg.collection_params.code_id,
            msg: to_binary(&Sg721InstantiateMsg {
                name: msg.collection_params.name.clone(),
                symbol: msg.collection_params.symbol,
                minter: env.contract.address.to_string(),
                collection_info,
            })?,
            funds: info.funds,
            admin: Some(config.extension.admin.to_string()),
            label: format!("SG721-{}", msg.collection_params.name.trim()),
        }
        .into(),
        id: INSTANTIATE_SG721_REPLY_ID,
        gas_limit: None,
        reply_on: ReplyOn::Success,
    };

    Ok(Response::new()
        .add_attribute("action", "instantiate")
        .add_attribute("contract_name", CONTRACT_NAME)
        .add_attribute("contract_version", CONTRACT_VERSION)
        .add_attribute("sender", factory)
        .add_submessage(submsg))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Mint {} => execute_mint_sender(deps, env, info),
        ExecuteMsg::Purge {} => execute_purge(deps, env, info),
        ExecuteMsg::UpdateMintPrice { price } => execute_update_mint_price(deps, env, info, price),
        ExecuteMsg::UpdateStartTime(time) => execute_update_start_time(deps, env, info, time),
        ExecuteMsg::UpdateEndTime(time) => execute_update_end_time(deps, env, info, time),
        ExecuteMsg::UpdateStartTradingTime(time) => {
            execute_update_start_trading_time(deps, env, info, time)
        }
        ExecuteMsg::UpdatePerAddressLimit { per_address_limit } => {
            execute_update_per_address_limit(deps, env, info, per_address_limit)
        }
        ExecuteMsg::MintTo { recipient } => execute_mint_to(deps, env, info, recipient),
        ExecuteMsg::ReceiveNft(msg) => burn_and_mint(deps, env, info, msg),
    }
}

// Purge frees data after a mint has ended
// Anyone can purge
pub fn execute_purge(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    // Check if mint has ended
    let end_time = CONFIG.load(deps.storage)?.extension.end_time;
    if env.block.time <= end_time {
        return Err(ContractError::MintingHasNotYetEnded {});
    }

    let keys = MINTER_ADDRS
        .keys(deps.storage, None, None, Order::Ascending)
        .collect::<Vec<_>>();
    for key in keys {
        MINTER_ADDRS.remove(deps.storage, &key?);
    }

    Ok(Response::new()
        .add_attribute("action", "purge")
        .add_attribute("contract", env.contract.address.to_string())
        .add_attribute("sender", info.sender))
}

pub fn execute_mint_sender(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let action = "mint_sender";

    // Check if after start_time and before end time
    if env.block.time < config.extension.start_time {
        return Err(ContractError::BeforeMintStartTime {});
    }
    if env.block.time >= config.extension.end_time {
        return Err(ContractError::AfterMintEndTime {});
    }

    // Check if already minted max per address limit
    if matches!(mint_count_per_addr(deps.as_ref(), &info)?, count if count >= config.extension.per_address_limit)
    {
        return Err(ContractError::MaxPerAddressLimitExceeded {});
    }
    _execute_mint(deps, info, action, false, None)
}

pub fn execute_mint_to(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    recipient: String,
) -> Result<Response, ContractError> {
    let recipient = deps.api.addr_validate(&recipient)?;
    let config = CONFIG.load(deps.storage)?;
    let action = "mint_to";

    // Check only admin
    if info.sender != config.extension.admin {
        return Err(ContractError::Unauthorized(
            "Sender is not an admin".to_owned(),
        ));
    }

    if env.block.time >= config.extension.end_time {
        return Err(ContractError::AfterMintEndTime {});
    }

    _execute_mint(deps, info, action, true, Some(recipient))
}

fn pay_mint_if_not_burn_collection(
    info: MessageInfo,
    mint_price_with_discounts: Coin,
    config_denom: String,
    allowed_burn_collections: Option<Vec<Addr>>,
) -> Result<Uint128, ContractError> {
    match burn_to_mint::sender_is_allowed_burn_collection(info.clone(), allowed_burn_collections) {
        true => Ok(Uint128::new(0)),
        false => {
            let payment = may_pay(&info, &config_denom)?;
            if payment != mint_price_with_discounts.amount {
                return Err(ContractError::IncorrectPaymentAmount(
                    coin(payment.u128(), &config_denom),
                    mint_price_with_discounts,
                ));
            }
            Ok(payment)
        }
    }
}

fn fairburn_if_not_burn_collection(
    deps: &DepsMut,
    info: MessageInfo,
    mint_price_with_discounts: Coin,
    mint_fee: Decimal,
    factory_params: MinterParams<open_edition_factory::state::ParamsExtension>,
    allowed_burn_collections: Option<Vec<Addr>>,
) -> Result<(Response, Uint128), ContractError> {
    let mut res = Response::new();
    match burn_to_mint::sender_is_allowed_burn_collection(info.clone(), allowed_burn_collections) {
        true => Ok((res, Uint128::new(0))),
        false => {
            let network_fee = mint_price_with_discounts.amount * mint_fee;
            // This is for the network fee msg
            checked_fair_burn(
                &info,
                network_fee.u128(),
                Some(
                    deps.api
                        .addr_validate(&factory_params.extension.dev_fee_address)?,
                ),
                &mut res,
            )?;
            Ok((res, network_fee))
        }
    }
}

fn _compute_seller_amount_if_not_contract_sender(
    info: MessageInfo,
    res: Response,
    mint_price_with_discounts: Coin,
    network_fee: Uint128,
    config_extension: crate::state::ConfigExtension,
    allowed_burn_collections: Option<Vec<Addr>>,
) -> Result<(Response, Uint128), ContractError> {
    let mut res = res;
    match burn_to_mint::sender_is_allowed_burn_collection(info, allowed_burn_collections) {
        true => Ok((res, Uint128::new(0))),
        false => {
            let seller_amount = {
                // the net amount is mint price - network fee (mint free + dev fee)
                let amount = mint_price_with_discounts.amount.checked_sub(network_fee)?;
                let payment_address = config_extension.payment_address;
                let seller = config_extension.admin;
                // Sending 0 coins fails, so only send if amount is non-zero
                if !amount.is_zero() {
                    let msg = BankMsg::Send {
                        to_address: payment_address.unwrap_or(seller).to_string(),
                        amount: vec![coin(amount.u128(), mint_price_with_discounts.denom)],
                    };
                    res = res.add_message(msg);
                }
                amount
            };
            Ok((res, seller_amount))
        }
    }
}
// Generalize checks and mint message creation
// mint -> _execute_mint(recipient: None, token_id: None)
// mint_to(recipient: "friend") -> _execute_mint(Some(recipient), token_id: None)
fn _execute_mint(
    deps: DepsMut,
    info: MessageInfo,
    action: &str,
    is_admin: bool,
    recipient: Option<Addr>,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;

    let sg721_address = SG721_ADDRESS.load(deps.storage)?;

    let recipient_addr = match recipient {
        Some(some_recipient) => some_recipient,
        None => info.sender.clone(),
    };

    let mint_price_with_discounts: Coin = mint_price(deps.as_ref(), is_admin)?;
    let config_denom = config
        .mint_price
        .clone()
        .denom()
        .map_err(|_| ContractError::IncorrectFungibility {})?;
    // Exact payment only accepted

    pay_mint_if_not_burn_collection(
        info.clone(),
        mint_price_with_discounts.clone(),
        config_denom,
        config.allowed_burn_collections.clone(),
    )?;

    let factory: ParamsResponse = deps
        .querier
        .query_wasm_smart(config.factory, &Sg2QueryMsg::Params {})?;
    let factory_params = factory.params;

    // Create fee msgs
    // Metadata Storage fees -> minting fee will be enabled for on-chain metadata mints
    // dev fees are intrinsic in the mint fee (assuming a 50% share)
    let mint_fee = if is_admin {
        factory_params
            .extension
            .airdrop_mint_fee_bps
            .bps_to_decimal()
    } else {
        factory_params.mint_fee_bps.bps_to_decimal()
    };
    let (mut res, network_fee) = fairburn_if_not_burn_collection(
        &deps,
        info.clone(),
        mint_price_with_discounts.clone(),
        mint_fee,
        factory_params,
        config.allowed_burn_collections.clone(),
    )?;
    // Token ID to mint + update the config counter
    let token_id = increment_token_index(deps.storage)?.to_string();

    // Create mint msg -> dependents on the NFT data type
    let msg = mint_nft_msg(
        sg721_address,
        token_id.clone(),
        recipient_addr.clone(),
        match config.extension.nft_data.nft_data_type {
            NftMetadataType::OnChainMetadata => config.extension.clone().nft_data.extension,
            NftMetadataType::OffChainMetadata => None,
        },
        match config.extension.nft_data.nft_data_type {
            NftMetadataType::OnChainMetadata => None,
            NftMetadataType::OffChainMetadata => config.extension.clone().nft_data.token_uri,
        },
    )?;
    res = res.add_message(msg);
    // Save the new mint count for the sender's address
    let new_mint_count = mint_count_per_addr(deps.as_ref(), &info)? + 1;
    MINTER_ADDRS.save(deps.storage, &info.sender, &new_mint_count)?;

    // Update the mint count
    TOTAL_MINT_COUNT.update(
        deps.storage,
        |mut updated_mint_count| -> Result<_, ContractError> {
            updated_mint_count += 1u32;
            Ok(updated_mint_count)
        },
    )?;

    let (res, seller_amount) = _compute_seller_amount_if_not_contract_sender(
        info.clone(),
        res,
        mint_price_with_discounts.clone(),
        network_fee,
        config.extension,
        config.allowed_burn_collections,
    )?;

    Ok(res
        .add_attribute("action", action)
        .add_attribute("sender", info.sender)
        .add_attribute("recipient", recipient_addr)
        .add_attribute("token_id", token_id)
        .add_attribute("network_fee", network_fee.to_string())
        .add_attribute("mint_price", mint_price_with_discounts.amount)
        .add_attribute("seller_amount", seller_amount))
}

pub fn execute_update_mint_price(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    price: u128,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut config = CONFIG.load(deps.storage)?;
    if info.sender != config.extension.admin {
        return Err(ContractError::Unauthorized(
            "Sender is not an admin".to_owned(),
        ));
    }

    // If we are after the end_time return error
    if env.block.time >= config.extension.end_time {
        return Err(ContractError::AfterMintEndTime {});
    }

    let config_mint_price = config.mint_price.clone().amount()?.u128();

    // If current time is after the stored start_time, only allow lowering price
    if env.block.time >= config.extension.start_time && price >= config_mint_price {
        return Err(ContractError::UpdatedMintPriceTooHigh {
            allowed: config_mint_price,
            updated: price,
        });
    }

    let factory: ParamsResponse = deps
        .querier
        .query_wasm_smart(config.clone().factory, &Sg2QueryMsg::Params {})?;
    let factory_params = factory.params;

    let min_mint_price = factory_params.min_mint_price.amount()?;

    if min_mint_price.u128() > price {
        return Err(ContractError::InsufficientMintPrice {
            expected: min_mint_price.u128(),
            got: price,
        });
    }

    config.mint_price = Token::new_fungible_token(price, config.mint_price.clone().denom()?);
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new()
        .add_attribute("action", "update_mint_price")
        .add_attribute("sender", info.sender)
        .add_attribute("mint_price", price.to_string()))
}

pub fn execute_update_start_time(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    start_time: Timestamp,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut config = CONFIG.load(deps.storage)?;
    if info.sender != config.extension.admin {
        return Err(ContractError::Unauthorized(
            "Sender is not an admin".to_owned(),
        ));
    }
    // If current time is after the stored start time return error
    if env.block.time >= config.extension.start_time {
        return Err(ContractError::AlreadyStarted {});
    }

    // If current time already passed the new start_time return error
    if env.block.time > start_time {
        return Err(ContractError::InvalidStartTime(start_time, env.block.time));
    }

    // If the new start_time is after end_time return error
    if start_time > config.extension.end_time {
        return Err(ContractError::InvalidStartTime(
            config.extension.end_time,
            start_time,
        ));
    }

    config.extension.start_time = start_time;
    CONFIG.save(deps.storage, &config)?;
    Ok(Response::new()
        .add_attribute("action", "update_start_time")
        .add_attribute("sender", info.sender)
        .add_attribute("start_time", start_time.to_string()))
}

pub fn execute_update_end_time(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    end_time: Timestamp,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut config = CONFIG.load(deps.storage)?;
    if info.sender != config.extension.admin {
        return Err(ContractError::Unauthorized(
            "Sender is not an admin".to_owned(),
        ));
    }
    // If current time is after the stored end time return error
    if env.block.time >= config.extension.end_time {
        return Err(ContractError::AlreadyStarted {});
    }

    // If current time already passed the new end_time return error
    if env.block.time > end_time {
        return Err(ContractError::InvalidEndTime(end_time, env.block.time));
    }

    // If the new end_time if before the start_time return error
    if end_time < config.extension.start_time {
        return Err(ContractError::InvalidEndTime(
            end_time,
            config.extension.start_time,
        ));
    }

    config.extension.end_time = end_time;
    CONFIG.save(deps.storage, &config)?;
    Ok(Response::new()
        .add_attribute("action", "update_end_time")
        .add_attribute("sender", info.sender)
        .add_attribute("end_time", end_time.to_string()))
}

pub fn execute_update_start_trading_time(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    start_time: Option<Timestamp>,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let config = CONFIG.load(deps.storage)?;
    let sg721_contract_addr = SG721_ADDRESS.load(deps.storage)?;

    if info.sender != config.extension.admin {
        return Err(ContractError::Unauthorized(
            "Sender is not an admin".to_owned(),
        ));
    }

    // add custom rules here
    let factory_params: ParamsResponse = deps
        .querier
        .query_wasm_smart(config.factory.clone(), &Sg2QueryMsg::Params {})?;
    let default_start_time_with_offset = config
        .extension
        .start_time
        .plus_seconds(factory_params.params.max_trading_offset_secs);

    if let Some(start_trading_time) = start_time {
        if env.block.time > start_trading_time {
            return Err(ContractError::InvalidStartTradingTime(
                env.block.time,
                start_trading_time,
            ));
        }
        // If new start_trading_time > old start time + offset , return error
        if start_trading_time > default_start_time_with_offset {
            return Err(ContractError::InvalidStartTradingTime(
                start_trading_time,
                default_start_time_with_offset,
            ));
        }
    }

    // execute sg721 contract
    let msg = WasmMsg::Execute {
        contract_addr: sg721_contract_addr.to_string(),
        msg: to_binary(&Sg721ExecuteMsg::<Empty, Empty>::UpdateStartTradingTime(
            start_time,
        ))?,
        funds: vec![],
    };

    Ok(Response::new()
        .add_attribute("action", "update_start_trading_time")
        .add_attribute("sender", info.sender)
        .add_message(msg))
}

pub fn execute_update_per_address_limit(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    per_address_limit: u32,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut config = CONFIG.load(deps.storage)?;
    if info.sender != config.extension.admin {
        return Err(ContractError::Unauthorized(
            "Sender is not an admin".to_owned(),
        ));
    }

    let factory: ParamsResponse = deps
        .querier
        .query_wasm_smart(config.factory.clone(), &Sg2QueryMsg::Params {})?;
    let factory_params = factory.params;

    if per_address_limit == 0 || per_address_limit > factory_params.extension.max_per_address_limit
    {
        return Err(ContractError::InvalidPerAddressLimit {
            max: factory_params.extension.max_per_address_limit,
            min: 1,
            got: per_address_limit,
        });
    }

    config.extension.per_address_limit = per_address_limit;
    CONFIG.save(deps.storage, &config)?;
    Ok(Response::new()
        .add_attribute("action", "update_per_address_limit")
        .add_attribute("sender", info.sender)
        .add_attribute("limit", per_address_limit.to_string()))
}

pub fn burn_and_mint(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: Cw721ReceiveMsg,
) -> Result<Response, ContractError> {
    let res = burn_to_mint::generate_burn_msg(info.clone(), msg)?;
    let mint_res = execute_mint_sender(deps, env, info)?;
    Ok(mint_res.add_submessages(res.messages))
}

// if admin_no_fee => no fee,
// else if in whitelist => whitelist price
// else => config unit price
pub fn mint_price(deps: Deps, is_admin: bool) -> Result<Coin, StdError> {
    let config = CONFIG.load(deps.storage)?;

    let config_mint_price = config.mint_price.clone().amount()?;
    let config_denom = config.mint_price.denom()?;
    if is_admin {
        let factory: ParamsResponse = deps
            .querier
            .query_wasm_smart(config.factory, &Sg2QueryMsg::Params {})?;
        let factory_params = factory.params;
        Ok(coin(
            factory_params.extension.airdrop_mint_price.amount.u128(),
            config_denom,
        ))
    } else {
        Ok(coin(config_mint_price.u128(), config_denom))
    }
}

fn mint_count_per_addr(deps: Deps, info: &MessageInfo) -> Result<u32, StdError> {
    let mint_count = (MINTER_ADDRS.key(&info.sender).may_load(deps.storage)?).unwrap_or(0);
    Ok(mint_count)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn sudo(deps: DepsMut, _env: Env, msg: SudoMsg) -> Result<Response, ContractError> {
    match msg {
        SudoMsg::UpdateStatus {
            is_verified,
            is_blocked,
            is_explicit,
        } => update_status(deps, is_verified, is_blocked, is_explicit)
            .map_err(|_| ContractError::UpdateStatus {}),
    }
}

/// Only governance can update contract params
pub fn update_status(
    deps: DepsMut,
    is_verified: bool,
    is_blocked: bool,
    is_explicit: bool,
) -> StdResult<Response> {
    let mut status = STATUS.load(deps.storage)?;
    status.is_verified = is_verified;
    status.is_blocked = is_blocked;
    status.is_explicit = is_explicit;

    Ok(Response::new().add_attribute("action", "sudo_update_status"))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_binary(&query_config(deps)?),
        QueryMsg::Status {} => to_binary(&query_status(deps)?),
        QueryMsg::StartTime {} => to_binary(&query_start_time(deps)?),
        QueryMsg::EndTime {} => to_binary(&query_end_time(deps)?),
        QueryMsg::MintPrice {} => to_binary(&query_mint_price(deps)?),
        QueryMsg::MintCount { address } => to_binary(&query_mint_count_per_address(deps, address)?),
        QueryMsg::TotalMintCount {} => to_binary(&query_mint_count(deps)?),
    }
}

fn query_config(deps: Deps) -> StdResult<ConfigResponse> {
    let config = CONFIG.load(deps.storage)?;
    let sg721_address = SG721_ADDRESS.load(deps.storage)?;

    let config_mint_price = config.mint_price.clone().amount()?;
    let config_denom = config.mint_price.denom()?;

    Ok(ConfigResponse {
        admin: config.extension.admin.to_string(),
        nft_data: config.extension.nft_data,
        payment_address: config.extension.payment_address,
        per_address_limit: config.extension.per_address_limit,
        end_time: config.extension.end_time,
        sg721_address: sg721_address.to_string(),
        sg721_code_id: config.collection_code_id,
        start_time: config.extension.start_time,
        mint_price: coin(config_mint_price.u128(), config_denom),
        factory: config.factory.to_string(),
    })
}

pub fn query_status(deps: Deps) -> StdResult<StatusResponse> {
    let status = STATUS.load(deps.storage)?;

    Ok(StatusResponse { status })
}

fn query_mint_count_per_address(deps: Deps, address: String) -> StdResult<MintCountResponse> {
    let addr = deps.api.addr_validate(&address)?;
    let mint_count = (MINTER_ADDRS.key(&addr).may_load(deps.storage)?).unwrap_or(0);
    Ok(MintCountResponse {
        address: addr.to_string(),
        count: mint_count,
    })
}

fn query_mint_count(deps: Deps) -> StdResult<TotalMintCountResponse> {
    let mint_count = TOTAL_MINT_COUNT.load(deps.storage)?;
    Ok(TotalMintCountResponse { count: mint_count })
}

fn query_start_time(deps: Deps) -> StdResult<StartTimeResponse> {
    let config = CONFIG.load(deps.storage)?;
    Ok(StartTimeResponse {
        start_time: config.extension.start_time.to_string(),
    })
}

fn query_end_time(deps: Deps) -> StdResult<EndTimeResponse> {
    let config = CONFIG.load(deps.storage)?;
    Ok(EndTimeResponse {
        end_time: config.extension.end_time.to_string(),
    })
}

fn query_mint_price(deps: Deps) -> StdResult<MintPriceResponse> {
    let config = CONFIG.load(deps.storage)?;

    let factory: ParamsResponse = deps
        .querier
        .query_wasm_smart(config.factory, &Sg2QueryMsg::Params {})?;

    let config_mint_price = config.mint_price.clone().amount()?;
    let config_denom = config.mint_price.denom()?;

    let factory_params = factory.params;

    let current_price = mint_price(deps, false)?;
    let airdrop_price = coin(
        factory_params.extension.airdrop_mint_price.amount.u128(),
        config_denom.clone(),
    );
    Ok(MintPriceResponse {
        public_price: coin(config_mint_price.u128(), config_denom),
        airdrop_price,
        current_price,
    })
}

// Reply callback triggered from cw721 contract instantiation
#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, _env: Env, msg: Reply) -> Result<Response, ContractError> {
    if msg.id != INSTANTIATE_SG721_REPLY_ID {
        return Err(ContractError::InvalidReplyID {});
    }

    let reply = parse_reply_instantiate_data(msg);
    match reply {
        Ok(res) => {
            let sg721_address = res.contract_address;
            SG721_ADDRESS.save(deps.storage, &Addr::unchecked(sg721_address.clone()))?;
            Ok(Response::default()
                .add_attribute("action", "instantiate_sg721_reply")
                .add_attribute("sg721_address", sg721_address))
        }
        Err(_) => Err(ContractError::InstantiateSg721Error {}),
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, _env: Env, _msg: Empty) -> Result<Response, ContractError> {
    let current_version = cw2::get_contract_version(deps.storage)?;
    if current_version.contract != CONTRACT_NAME {
        return Err(StdError::generic_err("Cannot upgrade to a different contract").into());
    }
    let version: Version = current_version
        .version
        .parse()
        .map_err(|_| StdError::generic_err("Invalid contract version"))?;
    let new_version: Version = CONTRACT_VERSION
        .parse()
        .map_err(|_| StdError::generic_err("Invalid contract version"))?;

    if version > new_version {
        return Err(StdError::generic_err("Cannot upgrade to a previous contract version").into());
    }
    // if same version return
    if version == new_version {
        return Ok(Response::new());
    }

    // set new contract version
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    Ok(Response::new())
}
