#![cfg(feature = "test-bpf")]
#![allow(dead_code)]

mod fixtures;

use crate::fixtures::marginfi_account::MarginfiAccountFixture;
use anchor_lang::{prelude::ErrorCode, InstructionData, ToAccountMetas};
use fixed::types::I80F48;
use fixed_macro::types::I80F48;
use fixtures::prelude::*;
use marginfi::{
    prelude::MarginfiError,
    state::{
        marginfi_account::MarginfiAccount,
        marginfi_group::{Bank, BankConfig, BankVaultType},
    },
};
use pretty_assertions::assert_eq;
use solana_program::{
    instruction::Instruction, program_pack::Pack, system_instruction, system_program,
};
use solana_program_test::*;
use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

#[tokio::test]
async fn success_create_marginfi_account() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let test_f = TestFixture::new(None).await;

    // Create & initialize marginfi account
    let marginfi_account_key = Keypair::new();

    let accounts = marginfi::accounts::InitializeMarginfiAccount {
        marginfi_group: test_f.marginfi_group.key,
        marginfi_account: marginfi_account_key.pubkey(),
        signer: test_f.payer(),
        system_program: system_program::id(),
    };
    let init_marginfi_account_ix = Instruction {
        program_id: marginfi::id(),
        accounts: accounts.to_account_metas(Some(true)),
        data: marginfi::instruction::InitializeMarginfiAccount {}.data(),
    };

    let size = MarginfiAccountFixture::get_size();
    let create_marginfi_account_ix = system_instruction::create_account(
        &*&test_f.payer(),
        &marginfi_account_key.pubkey(),
        test_f.get_minimum_rent_for_size(size).await,
        size as u64,
        &marginfi::id(),
    );

    let tx = Transaction::new_signed_with_payer(
        &[create_marginfi_account_ix, init_marginfi_account_ix],
        Some(&test_f.payer()),
        &[&test_f.payer_keypair(), &marginfi_account_key],
        test_f.get_latest_blockhash().await,
    );

    let res = test_f
        .context
        .borrow_mut()
        .banks_client
        .process_transaction(tx)
        .await;
    assert!(res.is_ok());

    // Fetch & deserialize marginfi account
    let marginfi_account: MarginfiAccount = test_f
        .load_and_deserialize(&marginfi_account_key.pubkey())
        .await;

    // Check basic properties
    assert_eq!(marginfi_account.group, test_f.marginfi_group.key);
    assert_eq!(marginfi_account.owner, test_f.payer());
    assert!(marginfi_account
        .lending_account
        .balances
        .iter()
        .all(|bank| bank.is_none()));

    Ok(())
}

#[tokio::test]
async fn success_deposit() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let mut test_f = TestFixture::new(None).await;

    // Setup sample bank
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(test_f.usdc_mint.key, *DEFAULT_USDC_TEST_BANK_CONFIG)
        .await?;

    let marginfi_account_f = test_f.create_marginfi_account().await;

    let owner = test_f.context.borrow().payer.pubkey();
    let token_account_f =
        TokenAccountFixture::new(test_f.context.clone(), &test_f.usdc_mint.key, &owner).await;

    test_f
        .usdc_mint
        .mint_to(&token_account_f.key, native!(1_000, "USDC"))
        .await;

    let res = marginfi_account_f
        .try_bank_deposit(
            test_f.usdc_mint.key,
            token_account_f.key,
            native!(1_000, "USDC"),
        )
        .await;
    assert!(res.is_ok());

    let marginfi_account = marginfi_account_f.load().await;

    // Check balance is active
    let usdc_bank: Bank = test_f
        .load_and_deserialize(&find_bank_pda(&test_f.marginfi_group.key, &test_f.usdc_mint.key).0)
        .await;
    assert!(marginfi_account
        .lending_account
        .get_balance(&test_f.usdc_mint.key, &usdc_bank)
        .is_some());
    assert_eq!(
        marginfi_account
            .lending_account
            .get_active_balances_iter()
            .collect::<Vec<_>>()
            .len(),
        1
    );

    Ok(())
}

#[tokio::test]
async fn failure_deposit_capacity_exceeded() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let mut test_f = TestFixture::new(None).await;

    // Setup sample bank
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.usdc_mint.key,
            BankConfig {
                pyth_oracle: PYTH_USDC_FEED,
                max_capacity: native!(100, "USDC"),
                ..Default::default()
            },
        )
        .await?;

    // Fund user account
    let marginfi_account_f = test_f.create_marginfi_account().await;

    let owner = test_f.context.borrow().payer.pubkey();
    let token_account_f =
        TokenAccountFixture::new(test_f.context.clone(), &test_f.usdc_mint.key, &owner).await;

    test_f
        .usdc_mint
        .mint_to(&token_account_f.key, native!(1_000, "USDC"))
        .await;

    // Make lawful deposit
    let res = marginfi_account_f
        .try_bank_deposit(
            test_f.usdc_mint.key,
            token_account_f.key,
            native!(99, "USDC"),
        )
        .await;
    assert!(res.is_ok());

    // Make unlawful deposit
    let res = marginfi_account_f
        .try_bank_deposit(
            test_f.usdc_mint.key,
            token_account_f.key,
            native!(101, "USDC"),
        )
        .await;
    assert_custom_error!(res.unwrap_err(), MarginfiError::BankDepositCapacityExceeded);

    Ok(())
}

#[tokio::test]
async fn failure_deposit_bank_not_found() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let mut test_f = TestFixture::new(None).await;

    // Setup sample bank
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.usdc_mint.key,
            BankConfig {
                pyth_oracle: PYTH_USDC_FEED,
                max_capacity: native!(100, "USDC"),
                ..Default::default()
            },
        )
        .await?;

    // Fund user account
    let marginfi_account_f = test_f.create_marginfi_account().await;

    let owner = test_f.context.borrow().payer.pubkey();
    let sol_token_account_f =
        TokenAccountFixture::new(test_f.context.clone(), &test_f.sol_mint.key, &owner).await;

    test_f
        .sol_mint
        .mint_to(&sol_token_account_f.key, native!(1_000, "SOL"))
        .await;

    let res = marginfi_account_f
        .try_bank_deposit(
            test_f.sol_mint.key,
            sol_token_account_f.key,
            native!(1, "SOL"),
        )
        .await;
    assert_anchor_error!(res.unwrap_err(), ErrorCode::AccountNotInitialized);

    Ok(())
}

#[tokio::test]
async fn success_borrow() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let mut test_f = TestFixture::new(None).await;

    test_f
        .marginfi_group
        .try_lending_pool_add_bank(test_f.usdc_mint.key, *DEFAULT_USDC_TEST_BANK_CONFIG)
        .await?;
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(test_f.sol_mint.key, *DEFAULT_SOL_TEST_BANK_CONFIG)
        .await?;

    let user_pubkey = test_f.context.borrow().payer.pubkey();
    let user_usdc_token_account_f =
        TokenAccountFixture::new(test_f.context.clone(), &test_f.usdc_mint.key, &user_pubkey).await;
    let user_sol_token_account_f =
        TokenAccountFixture::new(test_f.context.clone(), &test_f.sol_mint.key, &user_pubkey).await;

    let marginfi_account_f = test_f.create_marginfi_account().await;

    test_f
        .usdc_mint
        .mint_to(&user_usdc_token_account_f.key, native!(1_000, "USDC"))
        .await;
    let liquidity_vault = find_bank_vault_pda(
        &test_f.marginfi_group.key,
        &test_f.sol_mint.key,
        BankVaultType::Liquidity,
    );

    test_f
        .sol_mint
        .mint_to(&liquidity_vault.0, native!(1_000_000, "SOL"))
        .await;

    marginfi_account_f
        .try_bank_deposit(
            test_f.usdc_mint.key,
            user_usdc_token_account_f.key,
            native!(1_000, "USDC"),
        )
        .await?;

    let res = marginfi_account_f
        .try_bank_withdraw(
            test_f.sol_mint.key,
            user_sol_token_account_f.key,
            native!(2, "SOL"),
        )
        .await;
    assert!(res.is_ok());

    // Check token balances are correct
    assert_eq!(
        user_usdc_token_account_f.balance().await,
        native!(0, "USDC")
    );
    assert_eq!(user_sol_token_account_f.balance().await, native!(2, "SOL"));

    // TODO: check health is sane

    Ok(())
}

#[tokio::test]
async fn failure_borrow_not_enough_collateral() -> anyhow::Result<()> {
    // Setup test executor with non-admin payer
    let mut test_f = TestFixture::new(None).await;

    // Setup sample bank
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(test_f.usdc_mint.key, *DEFAULT_USDC_TEST_BANK_CONFIG)
        .await?;
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(test_f.sol_mint.key, *DEFAULT_SOL_TEST_BANK_CONFIG)
        .await?;

    let marginfi_account_f = test_f.create_marginfi_account().await;

    let user_pubkey = test_f.context.borrow().payer.pubkey();
    let user_usdc_token_account_f =
        TokenAccountFixture::new(test_f.context.clone(), &test_f.usdc_mint.key, &user_pubkey).await;
    let user_sol_token_account_f =
        TokenAccountFixture::new(test_f.context.clone(), &test_f.sol_mint.key, &user_pubkey).await;

    test_f
        .usdc_mint
        .mint_to(&user_usdc_token_account_f.key, native!(1_000, "USDC"))
        .await;

    let liquidity_vault = find_bank_vault_pda(
        &test_f.marginfi_group.key,
        &test_f.sol_mint.key,
        BankVaultType::Liquidity,
    )
    .0;
    test_f
        .sol_mint
        .mint_to(&liquidity_vault, native!(1_000_000, "SOL"))
        .await;

    marginfi_account_f
        .try_bank_deposit(
            test_f.usdc_mint.key,
            user_usdc_token_account_f.key,
            native!(1, "USDC"),
        )
        .await?;

    let res = marginfi_account_f
        .try_bank_withdraw(
            test_f.sol_mint.key,
            user_sol_token_account_f.key,
            native!(1, "SOL"),
        )
        .await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::BadAccountHealth);

    Ok(())
}

#[tokio::test]
async fn liquidation_successful() -> anyhow::Result<()> {
    let test_f = TestFixture::new(None).await;

    // Setup sample bank
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.usdc_mint.key,
            BankConfig {
                ..*DEFAULT_USDC_TEST_BANK_CONFIG
            },
        )
        .await?;
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.sol_mint.key,
            BankConfig {
                deposit_weight_init: I80F48!(1).into(),
                ..*DEFAULT_SOL_TEST_BANK_CONFIG
            },
        )
        .await?;

    let depositor = test_f.create_marginfi_account().await;
    let deposit_account = test_f
        .usdc_mint
        .create_and_mint_to(native!(200, "USDC"))
        .await;
    depositor
        .try_bank_deposit(test_f.usdc_mint.key, deposit_account, native!(200, "USDC"))
        .await?;

    let borrower = test_f.create_marginfi_account().await;
    let borrower_sol_account = test_f
        .sol_mint
        .create_and_mint_to(native!(100, "SOL"))
        .await;
    let borrower_usdc_account = test_f.usdc_mint.create_and_mint_to(0).await;
    borrower
        .try_bank_deposit(
            test_f.sol_mint.key,
            borrower_sol_account,
            native!(100, "SOL"),
        )
        .await?;
    borrower
        .try_bank_withdraw(
            test_f.usdc_mint.key,
            borrower_usdc_account,
            native!(100, "USDC"),
        )
        .await?;

    depositor
        .try_liquidate(
            borrower.key,
            test_f.sol_mint.key,
            native!(1, "SOL"),
            test_f.usdc_mint.key,
        )
        .await?;

    // Checks
    let sol_bank: Bank = test_f
        .load_and_deserialize(&find_bank_pda(&test_f.marginfi_group.key, &test_f.sol_mint.key).0)
        .await;
    let usdc_bank: Bank = test_f
        .load_and_deserialize(&find_bank_pda(&test_f.marginfi_group.key, &test_f.usdc_mint.key).0)
        .await;

    let depositor_ma = depositor.load().await;
    let borrower_ma = borrower.load().await;

    // Depositors should have 1 SOL
    assert_eq!(
        sol_bank
            .get_deposit_amount(
                depositor_ma.lending_account.balances[1]
                    .unwrap()
                    .deposit_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(1, "SOL"))
    );

    // Depositors should have 190.25 USDC
    assert_eq_noise!(
        usdc_bank
            .get_deposit_amount(
                depositor_ma.lending_account.balances[0]
                    .unwrap()
                    .deposit_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(190.25, "USDC", f64)),
        native!(0.00001, "USDC", f64)
    );

    // Borrower should have 99 SOL
    assert_eq!(
        sol_bank
            .get_deposit_amount(
                borrower_ma.lending_account.balances[0]
                    .unwrap()
                    .deposit_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(99, "SOL"))
    );

    // Borrower should have 90.50 USDC
    assert_eq_noise!(
        usdc_bank
            .get_liability_amount(
                borrower_ma.lending_account.balances[1]
                    .unwrap()
                    .liability_shares
                    .into()
            )
            .unwrap(),
        I80F48::from(native!(90.50, "USDC", f64)),
        native!(0.00001, "USDC", f64)
    );

    // Check insurance fund fee
    let mut ctx = test_f.context.borrow_mut();
    let insurance_fund = ctx
        .banks_client
        .get_account(usdc_bank.insurance_vault)
        .await?
        .unwrap();
    let token_account =
        anchor_spl::token::spl_token::state::Account::unpack_from_slice(&insurance_fund.data)?;

    assert_eq_noise!(
        token_account.amount as i64,
        native!(0.25, "USDC", f64) as i64,
        1
    );

    Ok(())
}
#[tokio::test]
async fn liquidation_failed_liquidatee_not_unhealthy() -> anyhow::Result<()> {
    let test_f = TestFixture::new(None).await;

    // Setup sample bank
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.usdc_mint.key,
            BankConfig {
                ..*DEFAULT_USDC_TEST_BANK_CONFIG
            },
        )
        .await?;
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.sol_mint.key,
            BankConfig {
                deposit_weight_init: I80F48!(1).into(),
                deposit_weight_maint: I80F48!(1).into(),
                ..*DEFAULT_SOL_TEST_BANK_CONFIG
            },
        )
        .await?;

    let depositor = test_f.create_marginfi_account().await;
    let deposit_account = test_f
        .usdc_mint
        .create_and_mint_to(native!(200, "USDC"))
        .await;
    depositor
        .try_bank_deposit(test_f.usdc_mint.key, deposit_account, native!(200, "USDC"))
        .await?;

    let borrower = test_f.create_marginfi_account().await;
    let borrower_sol_account = test_f
        .sol_mint
        .create_and_mint_to(native!(100, "SOL"))
        .await;
    let borrower_usdc_account = test_f.usdc_mint.create_and_mint_to(0).await;
    borrower
        .try_bank_deposit(
            test_f.sol_mint.key,
            borrower_sol_account,
            native!(100, "SOL"),
        )
        .await?;
    borrower
        .try_bank_withdraw(
            test_f.usdc_mint.key,
            borrower_usdc_account,
            native!(100, "USDC"),
        )
        .await?;

    let res = depositor
        .try_liquidate(
            borrower.key,
            test_f.sol_mint.key,
            native!(1, "SOL"),
            test_f.usdc_mint.key,
        )
        .await;

    assert_custom_error!(
        res.unwrap_err(),
        MarginfiError::AccountIllegalPostLiquidationState
    );

    Ok(())
}
#[tokio::test]
async fn liquidation_failed_liquidation_too_severe() -> anyhow::Result<()> {
    let test_f = TestFixture::new(None).await;

    // Setup sample bank
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.usdc_mint.key,
            BankConfig {
                deposit_weight_init: I80F48!(1).into(),
                deposit_weight_maint: I80F48!(1).into(),
                ..*DEFAULT_USDC_TEST_BANK_CONFIG
            },
        )
        .await?;
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.sol_mint.key,
            BankConfig {
                deposit_weight_init: I80F48!(1).into(),
                deposit_weight_maint: I80F48!(0.5).into(),
                ..*DEFAULT_SOL_TEST_BANK_CONFIG
            },
        )
        .await?;

    let depositor = test_f.create_marginfi_account().await;
    let deposit_account = test_f
        .usdc_mint
        .create_and_mint_to(native!(200, "USDC"))
        .await;
    depositor
        .try_bank_deposit(test_f.usdc_mint.key, deposit_account, native!(200, "USDC"))
        .await?;

    let borrower = test_f.create_marginfi_account().await;
    let borrower_sol_account = test_f
        .sol_mint
        .create_and_mint_to(native!(100, "SOL"))
        .await;
    let borrower_usdc_account = test_f.usdc_mint.create_and_mint_to(0).await;
    borrower
        .try_bank_deposit(
            test_f.sol_mint.key,
            borrower_sol_account,
            native!(10, "SOL"),
        )
        .await?;
    borrower
        .try_bank_withdraw(
            test_f.usdc_mint.key,
            borrower_usdc_account,
            native!(60, "USDC"),
        )
        .await?;

    let res = depositor
        .try_liquidate(
            borrower.key,
            test_f.sol_mint.key,
            native!(10, "SOL"),
            test_f.usdc_mint.key,
        )
        .await;

    assert_custom_error!(
        res.unwrap_err(),
        MarginfiError::AccountIllegalPostLiquidationState
    );

    let res = depositor
        .try_liquidate(
            borrower.key,
            test_f.sol_mint.key,
            native!(1, "SOL"),
            test_f.usdc_mint.key,
        )
        .await;

    assert!(res.is_ok());

    Ok(())
}
#[tokio::test]
async fn liquidation_failed_liquidator_no_collateral() -> anyhow::Result<()> {
    let test_f = TestFixture::new(None).await;

    // Setup sample bank
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.usdc_mint.key,
            BankConfig {
                deposit_weight_init: I80F48!(1).into(),
                ..*DEFAULT_USDC_TEST_BANK_CONFIG
            },
        )
        .await?;
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.sol_mint.key,
            BankConfig {
                deposit_weight_init: I80F48!(1).into(),
                ..*DEFAULT_SOL_TEST_BANK_CONFIG
            },
        )
        .await?;
    test_f
        .marginfi_group
        .try_lending_pool_add_bank(
            test_f.sol_equivalent_mint.key,
            BankConfig {
                deposit_weight_init: I80F48!(1).into(),
                ..*DEFAULT_SOL_EQUIVALENT_TEST_BANK_CONFIG
            },
        )
        .await?;

    let depositor = test_f.create_marginfi_account().await;
    let deposit_account = test_f
        .usdc_mint
        .create_and_mint_to(native!(200, "USDC"))
        .await;
    depositor
        .try_bank_deposit(test_f.usdc_mint.key, deposit_account, native!(200, "USDC"))
        .await?;

    let borrower = test_f.create_marginfi_account().await;
    let borrower_sol_account = test_f
        .sol_mint
        .create_and_mint_to(native!(100, "SOL"))
        .await;
    let borrower_sol_2_account = test_f
        .sol_equivalent_mint
        .create_and_mint_to(native!(100, "SOL"))
        .await;
    let borrower_usdc_account = test_f.usdc_mint.create_and_mint_to(0).await;
    borrower
        .try_bank_deposit(
            test_f.sol_mint.key,
            borrower_sol_account,
            native!(10, "SOL"),
        )
        .await?;
    borrower
        .try_bank_deposit(
            test_f.sol_equivalent_mint.key,
            borrower_sol_2_account,
            native!(1, "SOL"),
        )
        .await?;

    borrower
        .try_bank_withdraw(
            test_f.usdc_mint.key,
            borrower_usdc_account,
            native!(60, "USDC"),
        )
        .await?;

    let res = depositor
        .try_liquidate(
            borrower.key,
            test_f.sol_mint.key,
            native!(2, "SOL"),
            test_f.usdc_mint.key,
        )
        .await;

    assert_custom_error!(res.unwrap_err(), MarginfiError::BorrowingNotAllowed);

    let res = depositor
        .try_liquidate(
            borrower.key,
            test_f.sol_mint.key,
            native!(1, "SOL"),
            test_f.usdc_mint.key,
        )
        .await;

    assert!(res.is_ok());
    Ok(())
}