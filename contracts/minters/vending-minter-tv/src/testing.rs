use cosmwasm_std::{coin, coins, Addr, Empty, Timestamp};
use cw_multi_test::{no_init, AppBuilder, BankSudo, Contract, ContractWrapper};
use cw_multi_test::{Executor, SudoMsg};
use sg2::tests::mock_collection_params_1;
use sg_std::{GENESIS_MINT_START_TIME, NATIVE_DENOM};
use test_suite::common_setup::contract_boxes::App;
use test_suite::common_setup::keeper::StargazeKeeper;
use test_suite::common_setup::setup_accounts_and_block::INITIAL_BALANCE;
use test_suite::common_setup::setup_minter::{
    common::constants::{
        AIRDROP_MINT_FEE_FAIR_BURN, AIRDROP_MINT_PRICE, CREATION_FEE, MAX_PER_ADDRESS_LIMIT,
        MAX_TOKEN_LIMIT, MINT_FEE_FAIR_BURN, MIN_MINT_PRICE, SHUFFLE_FEE,
    },
    vending_minter::mock_params::mock_init_extension,
};
use vending_factory::msg::{
    TokenVaultVendingMinterCreateMsg, TokenVaultVendingMinterInitMsgExtension, VaultInfo,
};
use vending_factory::state::{ParamsExtension, VendingMinterParams};

use crate::msg::ExecuteMsg;

const FACTORY_ADMIN: &str = "factory_admin";
const CREATOR: &str = "creator";

fn cw_vesting_contract() -> Box<dyn Contract<Empty>> {
    let contract = ContractWrapper::new(
        cw_vesting::contract::execute,
        cw_vesting::contract::instantiate,
        cw_vesting::contract::query,
    );
    Box::new(contract)
}

fn contract_vending_factory() -> Box<dyn Contract<Empty>> {
    let contract = ContractWrapper::new(
        vending_factory::contract::execute,
        vending_factory::contract::instantiate,
        vending_factory::contract::query,
    )
    .with_sudo(vending_factory::contract::sudo);
    Box::new(contract)
}

fn contract_vending_minter() -> Box<dyn Contract<Empty>> {
    let contract = ContractWrapper::new(
        crate::contract::execute,
        crate::contract::instantiate,
        crate::contract::query,
    )
    .with_reply(crate::contract::reply);
    Box::new(contract)
}

fn contract_tv_collection() -> Box<dyn Contract<Empty>> {
    let contract = ContractWrapper::new(
        sg721_tv::entry::execute,
        sg721_tv::entry::instantiate,
        sg721_tv::entry::query,
    );
    Box::new(contract)
}

fn setup_app() -> App {
    let mut app = AppBuilder::new()
        .with_stargate(StargazeKeeper)
        .build(no_init);

    app.sudo(SudoMsg::Bank({
        BankSudo::Mint {
            to_address: FACTORY_ADMIN.to_string(),
            amount: coins(INITIAL_BALANCE, NATIVE_DENOM),
        }
    }))
    .unwrap();

    app.sudo(SudoMsg::Bank({
        BankSudo::Mint {
            to_address: CREATOR.to_string(),
            amount: coins(CREATION_FEE, NATIVE_DENOM),
        }
    }))
    .unwrap();

    app
}

fn setup_contracts(app: &mut App) -> (Addr, u64, u64, u64) {
    let factory_code_id = app.store_code(contract_vending_factory());
    let vesting_code_id = app.store_code(cw_vesting_contract());
    let vending_code_id = app.store_code(contract_vending_minter());
    let collection_code_id = app.store_code(contract_tv_collection());

    let mut params = mock_params(None);
    params.code_id = vending_code_id;
    params.allowed_sg721_code_ids = vec![collection_code_id];

    let factory_addr = app
        .instantiate_contract(
            factory_code_id,
            Addr::unchecked(FACTORY_ADMIN),
            &vending_factory::msg::InstantiateMsg { params },
            &[],
            "factory",
            None,
        )
        .unwrap();

    (
        factory_addr,
        vesting_code_id,
        vending_code_id,
        collection_code_id,
    )
}

pub fn mock_params(mint_denom: Option<String>) -> VendingMinterParams {
    VendingMinterParams {
        code_id: 1,
        allowed_sg721_code_ids: vec![1, 3, 5, 6],
        frozen: false,
        creation_fee: coin(CREATION_FEE, NATIVE_DENOM),
        min_mint_price: coin(
            MIN_MINT_PRICE,
            mint_denom.unwrap_or_else(|| NATIVE_DENOM.to_string()),
        ),
        mint_fee_bps: MINT_FEE_FAIR_BURN,
        max_trading_offset_secs: 60 * 60 * 24 * 7,
        extension: ParamsExtension {
            max_token_limit: MAX_TOKEN_LIMIT,
            max_per_address_limit: MAX_PER_ADDRESS_LIMIT,
            airdrop_mint_price: coin(AIRDROP_MINT_PRICE, NATIVE_DENOM),
            airdrop_mint_fee_bps: AIRDROP_MINT_FEE_FAIR_BURN,
            shuffle_fee: coin(SHUFFLE_FEE, NATIVE_DENOM),
        },
    }
}

#[test]
fn proper_initialization() {
    let mut app = setup_app();

    let (factory_addr, vesting_code_id, _, collection_code_id) = setup_contracts(&mut app);

    let base = mock_init_extension(None, None);

    let vault_info = VaultInfo {
        token_balance: coin(100u128, NATIVE_DENOM),
        vesting_schedule: cw_vesting::vesting::Schedule::SaturatingLinear,
        vesting_duration_seconds: 1000,
        unbonding_duration_seconds: 0,
        vesting_code_id,
    };

    let init_msg = TokenVaultVendingMinterInitMsgExtension { base, vault_info };

    let start_time = Timestamp::from_nanos(GENESIS_MINT_START_TIME);
    let mut collection_params = mock_collection_params_1(Some(start_time));
    collection_params.code_id = collection_code_id;

    let create_minter_msg = TokenVaultVendingMinterCreateMsg {
        init_msg,
        collection_params,
    };

    let msg = vending_factory::msg::ExecuteMsg::CreateTokenVaultMinter(create_minter_msg);

    let creation_fee = coins(CREATION_FEE, NATIVE_DENOM);

    let res = app.execute_contract(
        Addr::unchecked(CREATOR),
        factory_addr.clone(),
        &msg,
        &creation_fee,
    );

    assert!(res.is_ok())
}

// #[test]
// fn mint() {
//     proper_initialization();

//     let mint_msg = ExecuteMsg::Mint {};
//     let res = router.execute_contract(
//         buyer.clone(),
//         minter_addr.clone(),
//         &mint_msg,
//         &coins(MINT_PRICE, NATIVE_DENOM),
//     );
//     assert!(res.is_ok());
// }
