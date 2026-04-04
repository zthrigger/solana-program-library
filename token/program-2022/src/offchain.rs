//! Offchain helper for fetching required accounts to build instructions

pub use spl_transfer_hook_interface::offchain::{AccountDataResult, AccountFetchError};
use {
    crate::{
        error::TokenError,
        extension::{transfer_hook, StateWithExtensions},
        state::Mint,
    },
    solana_program::{instruction::Instruction, program_error::ProgramError, pubkey::Pubkey},
    spl_transfer_hook_interface::offchain::add_extra_account_metas_for_execute,
    std::future::Future,
};

/// Offchain helper to create a `TransferChecked` instruction with all
/// additional required account metas for a transfer, including the ones
/// required by the transfer hook.
///
/// To be client-agnostic and to avoid pulling in the full solana-sdk, this
/// simply takes a function that will return its data as `Future<Vec<u8>>` for
/// the given address. Can be called in the following way:
///
/// ```rust,ignore
/// let instruction = create_transfer_checked_instruction_with_extra_metas(
///     &spl_token_2022::id(),
///     &source,
///     &mint,
///     &destination,
///     &authority,
///     &[],
///     amount,
///     decimals,
///     |address| self.client.get_account(&address).map_ok(|opt| opt.map(|acc| acc.data)),
/// )
/// .await?
/// ```
#[allow(clippy::too_many_arguments)]
pub async fn create_transfer_checked_instruction_with_extra_metas<F, Fut>(
    token_program_id: &Pubkey,
    source_pubkey: &Pubkey,
    mint_pubkey: &Pubkey,
    destination_pubkey: &Pubkey,
    authority_pubkey: &Pubkey,
    signer_pubkeys: &[&Pubkey],
    amount: u64,
    decimals: u8,
    fetch_account_data_fn: F,
) -> Result<Instruction, AccountFetchError>
where
    F: Fn(Pubkey) -> Fut,
    Fut: Future<Output = AccountDataResult>,
{
    let mut transfer_instruction = crate::instruction::transfer_checked(
        token_program_id,
        source_pubkey,
        mint_pubkey,
        destination_pubkey,
        authority_pubkey,
        signer_pubkeys,
        amount,
        decimals,
    )?;

    add_extra_account_metas(
        &mut transfer_instruction,
        source_pubkey,
        mint_pubkey,
        destination_pubkey,
        authority_pubkey,
        amount,
        fetch_account_data_fn,
    )
    .await?;

    Ok(transfer_instruction)
}

/// Offchain helper to add required account metas to an instruction, including
/// the ones required by the transfer hook.
///
/// To be client-agnostic and to avoid pulling in the full solana-sdk, this
/// simply takes a function that will return its data as `Future<Vec<u8>>` for
/// the given address. Can be called in the following way:
///
/// ```rust,ignore
/// let mut transfer_instruction = spl_token_2022::instruction::transfer_checked(
///     &spl_token_2022::id(),
///     source_pubkey,
///     mint_pubkey,
///     destination_pubkey,
///     authority_pubkey,
///     signer_pubkeys,
///     amount,
///     decimals,
/// )?;
/// add_extra_account_metas(
///     &mut transfer_instruction,
///     source_pubkey,
///     mint_pubkey,
///     destination_pubkey,
///     authority_pubkey,
///     amount,
///     fetch_account_data_fn,
/// ).await?;
/// ```
pub async fn add_extra_account_metas<F, Fut>(
    instruction: &mut Instruction,
    source_pubkey: &Pubkey,
    mint_pubkey: &Pubkey,
    destination_pubkey: &Pubkey,
    authority_pubkey: &Pubkey,
    amount: u64,
    fetch_account_data_fn: F,
) -> Result<(), AccountFetchError>
where
    F: Fn(Pubkey) -> Fut,
    Fut: Future<Output = AccountDataResult>,
{
    let mint_data = fetch_account_data_fn(*mint_pubkey)
        .await?
        .ok_or(ProgramError::InvalidAccountData)?;
    let mint = StateWithExtensions::<Mint>::unpack(&mint_data)?;

    if let Some(program_id) = transfer_hook::get_program_id(&mint) {
        add_extra_account_metas_for_execute(
            instruction,
            &program_id,
            source_pubkey,
            mint_pubkey,
            destination_pubkey,
            authority_pubkey,
            amount,
            fetch_account_data_fn,
        )
        .await?;

        instruction
            .accounts
            .extend_from_slice(&execute_ix.accounts[5..]);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::extension::{
            transfer_hook::TransferHook, BaseStateWithExtensionsMut, ExtensionType,
            StateWithExtensionsMut,
        },
        solana_program::{instruction::AccountMeta, program_option::COption},
        solana_program_test::tokio,
        spl_pod::optional_keys::OptionalNonZeroPubkey,
        spl_tlv_account_resolution::{
            account::ExtraAccountMeta, seeds::Seed, state::ExtraAccountMetaList,
        },
        spl_transfer_hook_interface::{
            get_extra_account_metas_address, instruction::ExecuteInstruction,
        },
    };

    const DECIMALS: u8 = 0;
    const MINT_PUBKEY: Pubkey = Pubkey::new_from_array([1u8; 32]);
    const TRANSFER_HOOK_PROGRAM_ID: Pubkey = Pubkey::new_from_array([2u8; 32]);
    const EXTRA_META_1: Pubkey = Pubkey::new_from_array([3u8; 32]);
    const EXTRA_META_2: Pubkey = Pubkey::new_from_array([4u8; 32]);

    // Mock to return the mint data or the validation state account data
    async fn mock_fetch_account_data_fn(address: Pubkey) -> AccountDataResult {
        if address == MINT_PUBKEY {
            let mint_len =
                ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::TransferHook])
                    .unwrap();
            let mut data = vec![0u8; mint_len];
            let mut mint = StateWithExtensionsMut::<Mint>::unpack_uninitialized(&mut data).unwrap();

            let extension = mint.init_extension::<TransferHook>(true).unwrap();
            extension.program_id =
                OptionalNonZeroPubkey::try_from(Some(TRANSFER_HOOK_PROGRAM_ID)).unwrap();

            mint.base.mint_authority = COption::Some(Pubkey::new_unique());
            mint.base.decimals = DECIMALS;
            mint.base.is_initialized = true;
            mint.base.freeze_authority = COption::None;
            mint.pack_base();
            mint.init_account_type().unwrap();

            Ok(Some(data))
        } else if address
            == get_extra_account_metas_address(&MINT_PUBKEY, &TRANSFER_HOOK_PROGRAM_ID)
        {
            let extra_metas = vec![
                ExtraAccountMeta::new_with_pubkey(&EXTRA_META_1, true, false).unwrap(),
                ExtraAccountMeta::new_with_pubkey(&EXTRA_META_2, true, false).unwrap(),
                ExtraAccountMeta::new_with_seeds(
                    &[
                        Seed::AccountKey { index: 0 }, // source
                        Seed::AccountKey { index: 2 }, // destination
                        Seed::AccountKey { index: 4 }, // validation state
                    ],
                    false,
                    true,
                )
                .unwrap(),
                ExtraAccountMeta::new_with_seeds(
                    &[
                        Seed::InstructionData {
                            index: 8,
                            length: 8,
                        }, // amount
                        Seed::AccountKey { index: 2 }, // destination
                        Seed::AccountKey { index: 5 }, // extra meta 1
                        Seed::AccountKey { index: 7 }, // extra meta 3 (PDA)
                    ],
                    false,
                    true,
                )
                .unwrap(),
            ];
            let account_size = ExtraAccountMetaList::size_of(extra_metas.len()).unwrap();
            let mut data = vec![0u8; account_size];
            ExtraAccountMetaList::init::<ExecuteInstruction>(&mut data, &extra_metas)?;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn test_create_transfer_checked_instruction_with_extra_metas() {
        let source = Pubkey::new_unique();
        let destination = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let amount = 100u64;

        let validate_state_pubkey =
            get_extra_account_metas_address(&MINT_PUBKEY, &TRANSFER_HOOK_PROGRAM_ID);
        let extra_meta_3_pubkey = Pubkey::find_program_address(
            &[
                source.as_ref(),
                destination.as_ref(),
                validate_state_pubkey.as_ref(),
            ],
            &TRANSFER_HOOK_PROGRAM_ID,
        )
        .0;
        let extra_meta_4_pubkey = Pubkey::find_program_address(
            &[
                amount.to_le_bytes().as_ref(),
                destination.as_ref(),
                EXTRA_META_1.as_ref(),
                extra_meta_3_pubkey.as_ref(),
            ],
            &TRANSFER_HOOK_PROGRAM_ID,
        )
        .0;

        let instruction = create_transfer_checked_instruction_with_extra_metas(
            &crate::id(),
            &source,
            &MINT_PUBKEY,
            &destination,
            &authority,
            &[],
            amount,
            DECIMALS,
            mock_fetch_account_data_fn,
        )
        .await
        .unwrap();

        let check_metas = [
            AccountMeta::new(source, false),
            AccountMeta::new_readonly(MINT_PUBKEY, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new_readonly(EXTRA_META_1, true),
            AccountMeta::new_readonly(EXTRA_META_2, true),
            AccountMeta::new(extra_meta_3_pubkey, false),
            AccountMeta::new(extra_meta_4_pubkey, false),
            AccountMeta::new_readonly(TRANSFER_HOOK_PROGRAM_ID, false),
            AccountMeta::new_readonly(validate_state_pubkey, false),
        ];

        assert_eq!(instruction.accounts, check_metas);

        // With additional signers
        let signer_1 = Pubkey::new_unique();
        let signer_2 = Pubkey::new_unique();
        let signer_3 = Pubkey::new_unique();

        let instruction = create_transfer_checked_instruction_with_extra_metas(
            &crate::id(),
            &source,
            &MINT_PUBKEY,
            &destination,
            &authority,
            &[&signer_1, &signer_2, &signer_3],
            amount,
            DECIMALS,
            mock_fetch_account_data_fn,
        // Finally, use the onchain function to add the extra account metas to
        // the _execute_ CPI instruction from onchain
        let mut onchain_execute_cpi_instruction = spl_transfer_hook_interface::instruction::execute(
            &transfer_hook_program_id,
            &source_pubkey,
            &mint_pubkey,
            &destination_pubkey,
            &authority_pubkey,
            &validate_state_pubkey,
            amount,
        );
        let mut onchain_execute_cpi_account_infos = vec![
            source_account_info.clone(),
            mint_account_info.clone(),
            destination_account_info.clone(),
            authority_account_info.clone(),
            validate_state_account_info.clone(),
        ];
        let all_account_infos = &[
            source_account_info.clone(),
            mint_account_info.clone(),
            destination_account_info.clone(),
            authority_account_info.clone(),
            validate_state_account_info.clone(),
            extra_meta_1_account_info.clone(),
            extra_meta_2_account_info.clone(),
            extra_meta_3_account_info.clone(),
            extra_meta_4_account_info.clone(),
            extra_meta_5_account_info.clone(),
            extra_meta_6_account_info.clone(),
        ];

        ExtraAccountMetaList::add_to_cpi_instruction::<ExecuteInstruction>(
            &mut onchain_execute_cpi_instruction,
            &mut onchain_execute_cpi_account_infos,
            &MOCK_EXTRA_METAS_STATE,
            all_account_infos,
        )
        .unwrap();

        // The two `Execute` instructions should have the same accounts
        assert_eq!(
            offchain_execute_instruction.accounts,
            onchain_execute_cpi_instruction.accounts,
        );

        // Still, the transfer instruction is going to be missing the
        // the validation account at index 4
        assert_ne!(
            offchain_transfer_instruction.accounts,
            offchain_execute_instruction.accounts,
        );
        assert_ne!(
            offchain_transfer_instruction.accounts[4].pubkey,
            validate_state_pubkey,
        );

        // Even though both execute instructions have the validation account
        // at index 4
        assert_eq!(
            offchain_execute_instruction.accounts[4].pubkey,
            validate_state_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[4].pubkey,
            validate_state_pubkey,
        );

        // The most important thing is verifying all PDAs are correct across
        // all lists
        // PDA 1
        assert_eq!(
            offchain_transfer_instruction.accounts[4].pubkey,
            extra_meta_1_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[5].pubkey,
            extra_meta_1_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[5].pubkey,
            extra_meta_1_pubkey,
        );
        // PDA 2
        assert_eq!(
            offchain_transfer_instruction.accounts[5].pubkey,
            extra_meta_2_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[6].pubkey,
            extra_meta_2_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[6].pubkey,
            extra_meta_2_pubkey,
        );
        // PDA 3
        assert_eq!(
            offchain_transfer_instruction.accounts[6].pubkey,
            extra_meta_3_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[7].pubkey,
            extra_meta_3_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[7].pubkey,
            extra_meta_3_pubkey,
        );
        // PDA 4
        assert_eq!(
            offchain_transfer_instruction.accounts[7].pubkey,
            extra_meta_4_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[8].pubkey,
            extra_meta_4_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[8].pubkey,
            extra_meta_4_pubkey,
        );
        // PDA 5
        assert_eq!(
            offchain_transfer_instruction.accounts[8].pubkey,
            extra_meta_5_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[9].pubkey,
            extra_meta_5_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[9].pubkey,
            extra_meta_5_pubkey,
        );
        // PDA 6
        assert_eq!(
            offchain_transfer_instruction.accounts[9].pubkey,
            extra_meta_6_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[10].pubkey,
            extra_meta_6_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[10].pubkey,
            extra_meta_6_pubkey,
        );
    }

    #[tokio::test]
    async fn test_create_transfer_instruction_with_extra_metas() {
        let spl_token_2022_program_id = crate::id();
        let transfer_hook_program_id = TRANSFER_HOOK_PROGRAM_ID;
        let amount = 2u64;

        let source_pubkey = Pubkey::new_unique();
        let mut source_data = vec![0; 165]; // Mock
        let mut source_lamports = 0; // Mock
        let source_account_info = AccountInfo::new(
            &source_pubkey,
            false,
            true,
            &mut source_lamports,
            &mut source_data,
            &spl_token_2022_program_id,
            false,
            0,
        );

        let mint_pubkey = MINT_PUBKEY;
        let mut mint_data = MOCK_MINT_STATE.to_vec();
        let mut mint_lamports = 0; // Mock
        let mint_account_info = AccountInfo::new(
            &mint_pubkey,
            false,
            true,
            &mut mint_lamports,
            &mut mint_data,
            &spl_token_2022_program_id,
            false,
            0,
        );

        let destination_pubkey = Pubkey::new_unique();
        let mut destination_data = vec![0; 165]; // Mock
        let mut destination_lamports = 0; // Mock
        let destination_account_info = AccountInfo::new(
            &destination_pubkey,
            false,
            true,
            &mut destination_lamports,
            &mut destination_data,
            &spl_token_2022_program_id,
            false,
            0,
        );

        let authority_pubkey = Pubkey::new_unique();
        let mut authority_data = vec![]; // Mock
        let mut authority_lamports = 0; // Mock
        let authority_account_info = AccountInfo::new(
            &authority_pubkey,
            false,
            true,
            &mut authority_lamports,
            &mut authority_data,
            &system_program::ID,
            false,
            0,
        );

        let validate_state_pubkey =
            get_extra_account_metas_address(&mint_pubkey, &transfer_hook_program_id);

        let extra_meta_1_pubkey = Pubkey::find_program_address(
            &[
                &source_pubkey.to_bytes(), // Account key at index 0
                &mint_pubkey.to_bytes(),   // Account key at index 1
            ],
            &transfer_hook_program_id,
        )
        .0;
        let mut extra_meta_1_data = vec![]; // Mock
        let mut extra_meta_1_lamports = 0; // Mock
        let extra_meta_1_account_info = AccountInfo::new(
            &extra_meta_1_pubkey,
            false,
            true,
            &mut extra_meta_1_lamports,
            &mut extra_meta_1_data,
            &transfer_hook_program_id,
            false,
            0,
        );

        let extra_meta_2_pubkey = Pubkey::find_program_address(
            &[
                &validate_state_pubkey.to_bytes(), // Account key at index 4
            ],
            &transfer_hook_program_id,
        )
        .0;
        let mut extra_meta_2_data = vec![]; // Mock
        let mut extra_meta_2_lamports = 0; // Mock
        let extra_meta_2_account_info = AccountInfo::new(
            &extra_meta_2_pubkey,
            false,
            true,
            &mut extra_meta_2_lamports,
            &mut extra_meta_2_data,
            &transfer_hook_program_id,
            false,
            0,
        );

        let extra_meta_3_pubkey = Pubkey::find_program_address(
            &[
                b"prefix",
                amount.to_le_bytes().as_ref(), // Instruction data 8..16
            ],
            &transfer_hook_program_id,
        )
        .0;
        let mut extra_meta_3_data = vec![]; // Mock
        let mut extra_meta_3_lamports = 0; // Mock
        let extra_meta_3_account_info = AccountInfo::new(
            &extra_meta_3_pubkey,
            false,
            true,
            &mut extra_meta_3_lamports,
            &mut extra_meta_3_data,
            &transfer_hook_program_id,
            false,
            0,
        );

        let extra_meta_4_pubkey = Pubkey::new_from_array([7; 32]); // Some arbitrary program ID
        let mut extra_meta_4_data = vec![]; // Mock
        let mut extra_meta_4_lamports = 0; // Mock
        let extra_meta_4_account_info = AccountInfo::new(
            &extra_meta_4_pubkey,
            false,
            true,
            &mut extra_meta_4_lamports,
            &mut extra_meta_4_data,
            &transfer_hook_program_id,
            true, // Executable program
            0,
        );

        let extra_meta_5_pubkey = Pubkey::find_program_address(
            &[
                b"prefix",
                amount.to_le_bytes().as_ref(), // Instruction data 8..16
                extra_meta_2_pubkey.as_ref(),
            ],
            &extra_meta_4_pubkey, // PDA off of the arbitrary program ID
        )
        .0;
        let mut extra_meta_5_data = vec![]; // Mock
        let mut extra_meta_5_lamports = 0; // Mock
        let extra_meta_5_account_info = AccountInfo::new(
            &extra_meta_5_pubkey,
            false,
            true,
            &mut extra_meta_5_lamports,
            &mut extra_meta_5_data,
            &extra_meta_4_pubkey,
            false,
            0,
        );

        let extra_meta_6_pubkey = Pubkey::find_program_address(
            &[
                b"another_prefix",
                amount.to_le_bytes().as_ref(), // Instruction data 8..16
                extra_meta_2_pubkey.as_ref(),
                extra_meta_5_pubkey.as_ref(),
            ],
            &extra_meta_4_pubkey, // PDA off of the arbitrary program ID
        )
        .0;
        let mut extra_meta_6_data = vec![]; // Mock
        let mut extra_meta_6_lamports = 0; // Mock
        let extra_meta_6_account_info = AccountInfo::new(
            &extra_meta_6_pubkey,
            false,
            true,
            &mut extra_meta_6_lamports,
            &mut extra_meta_6_data,
            &extra_meta_4_pubkey,
            false,
            0,
        );

        let mut validate_state_data = MOCK_EXTRA_METAS_STATE.to_vec();
        let mut validate_state_lamports = 0; // Mock
        let validate_state_account_info = AccountInfo::new(
            &validate_state_pubkey,
            false,
            true,
            &mut validate_state_lamports,
            &mut validate_state_data,
            &transfer_hook_program_id,
            false,
            0,
        );

        // First use the transfer instruction builder function to add the extra
        // account metas to the transfer instruction from offchain
        let offchain_transfer_instruction = create_transfer_instruction_with_extra_metas(
            &spl_token_2022_program_id,
            &source_pubkey,
            &mint_pubkey,
            &destination_pubkey,
            &authority_pubkey,
            &[],
            amount,
            9,
            mock_fetch_account_data_fn,
        )
        .await
        .unwrap();

        // Then use the offchain function to add the extra account metas to the
        // _execute_ instruction from offchain
        let mut offchain_execute_instruction = spl_transfer_hook_interface::instruction::execute(
            &transfer_hook_program_id,
            &source_pubkey,
            &mint_pubkey,
            &destination_pubkey,
            &authority_pubkey,
            &validate_state_pubkey,
            amount,
        );

        ExtraAccountMetaList::add_to_instruction::<ExecuteInstruction, _, _>(
            &mut offchain_execute_instruction,
            mock_fetch_account_data_fn,
            &MOCK_EXTRA_METAS_STATE,
        )
        .await
        .unwrap();

        let check_metas = [
            AccountMeta::new(source, false),
            AccountMeta::new_readonly(MINT_PUBKEY, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(authority, false), // False because of additional signers
            AccountMeta::new_readonly(signer_1, true),
            AccountMeta::new_readonly(signer_2, true),
            AccountMeta::new_readonly(signer_3, true),
            AccountMeta::new_readonly(EXTRA_META_1, true),
            AccountMeta::new_readonly(EXTRA_META_2, true),
            AccountMeta::new(extra_meta_3_pubkey, false),
            AccountMeta::new(extra_meta_4_pubkey, false),
            AccountMeta::new_readonly(TRANSFER_HOOK_PROGRAM_ID, false),
            AccountMeta::new_readonly(validate_state_pubkey, false),
        ];

        assert_eq!(instruction.accounts, check_metas);
        // Finally, use the onchain function to add the extra account metas to
        // the _execute_ CPI instruction from onchain
        let mut onchain_execute_cpi_instruction = spl_transfer_hook_interface::instruction::execute(
            &transfer_hook_program_id,
            &source_pubkey,
            &mint_pubkey,
            &destination_pubkey,
            &authority_pubkey,
            &validate_state_pubkey,
            amount,
        );
        let mut onchain_execute_cpi_account_infos = vec![
            source_account_info.clone(),
            mint_account_info.clone(),
            destination_account_info.clone(),
            authority_account_info.clone(),
            validate_state_account_info.clone(),
        ];
        let all_account_infos = &[
            source_account_info.clone(),
            mint_account_info.clone(),
            destination_account_info.clone(),
            authority_account_info.clone(),
            validate_state_account_info.clone(),
            extra_meta_1_account_info.clone(),
            extra_meta_2_account_info.clone(),
            extra_meta_3_account_info.clone(),
            extra_meta_4_account_info.clone(),
            extra_meta_5_account_info.clone(),
            extra_meta_6_account_info.clone(),
        ];

        ExtraAccountMetaList::add_to_cpi_instruction::<ExecuteInstruction>(
            &mut onchain_execute_cpi_instruction,
            &mut onchain_execute_cpi_account_infos,
            &MOCK_EXTRA_METAS_STATE,
            all_account_infos,
        )
        .unwrap();

        // The two `Execute` instructions should have the same accounts
        assert_eq!(
            offchain_execute_instruction.accounts,
            onchain_execute_cpi_instruction.accounts,
        );

        // Still, the transfer instruction is going to be missing the
        // the validation account at index 4
        assert_ne!(
            offchain_transfer_instruction.accounts,
            offchain_execute_instruction.accounts,
        );
        assert_ne!(
            offchain_transfer_instruction.accounts[4].pubkey,
            validate_state_pubkey,
        );

        // Even though both execute instructions have the validation account
        // at index 4
        assert_eq!(
            offchain_execute_instruction.accounts[4].pubkey,
            validate_state_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[4].pubkey,
            validate_state_pubkey,
        );

        // The most important thing is verifying all PDAs are correct across
        // all lists
        // PDA 1
        assert_eq!(
            offchain_transfer_instruction.accounts[4].pubkey,
            extra_meta_1_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[5].pubkey,
            extra_meta_1_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[5].pubkey,
            extra_meta_1_pubkey,
        );
        // PDA 2
        assert_eq!(
            offchain_transfer_instruction.accounts[5].pubkey,
            extra_meta_2_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[6].pubkey,
            extra_meta_2_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[6].pubkey,
            extra_meta_2_pubkey,
        );
        // PDA 3
        assert_eq!(
            offchain_transfer_instruction.accounts[6].pubkey,
            extra_meta_3_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[7].pubkey,
            extra_meta_3_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[7].pubkey,
            extra_meta_3_pubkey,
        );
        // PDA 4
        assert_eq!(
            offchain_transfer_instruction.accounts[7].pubkey,
            extra_meta_4_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[8].pubkey,
            extra_meta_4_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[8].pubkey,
            extra_meta_4_pubkey,
        );
        // PDA 5
        assert_eq!(
            offchain_transfer_instruction.accounts[8].pubkey,
            extra_meta_5_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[9].pubkey,
            extra_meta_5_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[9].pubkey,
            extra_meta_5_pubkey,
        );
        // PDA 6
        assert_eq!(
            offchain_transfer_instruction.accounts[9].pubkey,
            extra_meta_6_pubkey,
        );
        assert_eq!(
            offchain_execute_instruction.accounts[10].pubkey,
            extra_meta_6_pubkey,
        );
        assert_eq!(
            onchain_execute_cpi_instruction.accounts[10].pubkey,
            extra_meta_6_pubkey,
        );
    }
}
