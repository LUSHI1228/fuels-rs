#![allow(async_fn_in_trait)]

use std::{collections::HashMap, fmt::Display};

use fuel_core_client::client::pagination::{PaginatedResult, PaginationRequest};
use fuel_tx::{Output, Receipt, TxId, TxPointer, UtxoId};
use fuel_types::{AssetId, Bytes32, ContractId, Nonce};
use fuels_core::{
    constants::BASE_ASSET_ID,
    types::{
        bech32::{Bech32Address, Bech32ContractId},
        coin::Coin,
        coin_type::CoinType,
        errors::{Error, Result},
        input::Input,
        message::Message,
        transaction::{Transaction, TxPolicies},
        transaction_builders::{
            BuildableTransaction, ScriptTransactionBuilder, TransactionBuilder,
        },
        transaction_response::TransactionResponse,
    },
};

use crate::{
    accounts_utils::{adjust_inputs_outputs, calculate_missing_base_amount, extract_message_nonce},
    provider::{Provider, ResourceFilter},
};

#[derive(Debug)]
pub struct AccountError(String);

impl AccountError {
    pub fn no_provider() -> Self {
        Self("No provider was setup: make sure to set_provider in your account!".to_string())
    }
}

impl Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for AccountError {}

impl From<AccountError> for Error {
    fn from(e: AccountError) -> Self {
        Error::AccountError(e.0)
    }
}

pub type AccountResult<T> = std::result::Result<T, AccountError>;

pub trait ViewOnlyAccount: std::fmt::Debug + Send + Sync + Clone {
    fn address(&self) -> &Bech32Address;

    fn try_provider(&self) -> AccountResult<&Provider>;

    async fn get_transactions(
        &self,
        request: PaginationRequest<String>,
    ) -> Result<PaginatedResult<TransactionResponse, String>> {
        Ok(self
            .try_provider()?
            .get_transactions_by_owner(self.address(), request)
            .await?)
    }

    /// Gets all unspent coins of asset `asset_id` owned by the account.
    async fn get_coins(&self, asset_id: AssetId) -> Result<Vec<Coin>> {
        Ok(self
            .try_provider()?
            .get_coins(self.address(), asset_id)
            .await?)
    }

    /// Get the balance of all spendable coins `asset_id` for address `address`. This is different
    /// from getting coins because we are just returning a number (the sum of UTXOs amount) instead
    /// of the UTXOs.
    async fn get_asset_balance(&self, asset_id: &AssetId) -> Result<u64> {
        self.try_provider()?
            .get_asset_balance(self.address(), *asset_id)
            .await
            .map_err(Into::into)
    }

    /// Gets all unspent messages owned by the account.
    async fn get_messages(&self) -> Result<Vec<Message>> {
        Ok(self.try_provider()?.get_messages(self.address()).await?)
    }

    /// Get all the spendable balances of all assets for the account. This is different from getting
    /// the coins because we are only returning the sum of UTXOs coins amount and not the UTXOs
    /// coins themselves.
    async fn get_balances(&self) -> Result<HashMap<String, u64>> {
        self.try_provider()?
            .get_balances(self.address())
            .await
            .map_err(Into::into)
    }

    /// Get some spendable resources (coins and messages) of asset `asset_id` owned by the account
    /// that add up at least to amount `amount`. The returned coins (UTXOs) are actual coins that
    /// can be spent. The number of UXTOs is optimized to prevent dust accumulation.
    async fn get_spendable_resources(
        &self,
        asset_id: AssetId,
        amount: u64,
    ) -> Result<Vec<CoinType>> {
        let filter = ResourceFilter {
            from: self.address().clone(),
            asset_id,
            amount,
            ..Default::default()
        };

        self.try_provider()?
            .get_spendable_resources(filter)
            .await
            .map_err(Into::into)
    }
}

pub trait Account: ViewOnlyAccount {
    /// Returns a vector consisting of `Input::Coin`s and `Input::Message`s for the given
    /// asset ID and amount. The `witness_index` is the position of the witness (signature)
    /// in the transaction's list of witnesses. In the validation process, the node will
    /// use the witness at this index to validate the coins returned by this method.
    async fn get_asset_inputs_for_amount(
        &self,
        asset_id: AssetId,
        amount: u64,
    ) -> Result<Vec<Input>>;

    /// Returns a vector containing the output coin and change output given an asset and amount
    fn get_asset_outputs_for_amount(
        &self,
        to: &Bech32Address,
        asset_id: AssetId,
        amount: u64,
    ) -> Vec<Output> {
        vec![
            Output::coin(to.into(), amount, asset_id),
            // Note that the change will be computed by the node.
            // Here we only have to tell the node who will own the change and its asset ID.
            Output::change(self.address().into(), 0, asset_id),
        ]
    }

    /// Add base asset inputs to the transaction to cover the estimated fee.
    /// Requires contract inputs to be at the start of the transactions inputs vec
    /// so that their indexes are retained
    async fn adjust_for_fee<Tb: TransactionBuilder + Sync>(
        &self,
        tb: &mut Tb,
        used_base_amount: u64,
    ) -> Result<()> {
        let missing_base_amount =
            calculate_missing_base_amount(tb, used_base_amount, self.try_provider()?).await?;

        if missing_base_amount > 0 {
            let new_base_inputs = self
                .get_asset_inputs_for_amount(BASE_ASSET_ID, missing_base_amount)
                .await?;

            adjust_inputs_outputs(tb, new_base_inputs, self.address());
        };

        Ok(())
    }

    // Add signatures to the builder if the underlying account is a wallet
    fn add_witnesses<Tb: TransactionBuilder>(&self, _tb: &mut Tb) -> Result<()> {
        Ok(())
    }

    /// Transfer funds from this account to another `Address`.
    /// Fails if amount for asset ID is larger than address's spendable coins.
    /// Returns the transaction ID that was sent and the list of receipts.
    async fn transfer(
        &self,
        to: &Bech32Address,
        amount: u64,
        asset_id: AssetId,
        tx_policies: TxPolicies,
    ) -> Result<(TxId, Vec<Receipt>)> {
        let provider = self.try_provider()?;

        let inputs = self.get_asset_inputs_for_amount(asset_id, amount).await?;
        let outputs = self.get_asset_outputs_for_amount(to, asset_id, amount);

        let mut tx_builder =
            ScriptTransactionBuilder::prepare_transfer(inputs, outputs, tx_policies);

        self.add_witnesses(&mut tx_builder)?;

        let used_base_amount = if asset_id == AssetId::BASE { amount } else { 0 };
        self.adjust_for_fee(&mut tx_builder, used_base_amount)
            .await?;

        let tx = tx_builder.build(provider).await?;
        let tx_id = tx.id(provider.chain_id());

        let tx_status = provider.send_transaction_and_await_commit(tx).await?;

        let receipts = tx_status.take_receipts_checked(None)?;

        Ok((tx_id, receipts))
    }

    /// Unconditionally transfers `balance` of type `asset_id` to
    /// the contract at `to`.
    /// Fails if balance for `asset_id` is larger than this account's spendable balance.
    /// Returns the corresponding transaction ID and the list of receipts.
    ///
    /// CAUTION !!!
    ///
    /// This will transfer coins to a contract, possibly leading
    /// to the PERMANENT LOSS OF COINS if not used with care.
    async fn force_transfer_to_contract(
        &self,
        to: &Bech32ContractId,
        balance: u64,
        asset_id: AssetId,
        tx_policies: TxPolicies,
    ) -> std::result::Result<(String, Vec<Receipt>), Error> {
        let provider = self.try_provider()?;

        let zeroes = Bytes32::zeroed();
        let plain_contract_id: ContractId = to.into();

        let mut inputs = vec![Input::contract(
            UtxoId::new(zeroes, 0),
            zeroes,
            zeroes,
            TxPointer::default(),
            plain_contract_id,
        )];

        inputs.extend(self.get_asset_inputs_for_amount(asset_id, balance).await?);

        let outputs = vec![
            Output::contract(0, zeroes, zeroes),
            Output::change(self.address().into(), 0, asset_id),
        ];

        // Build transaction and sign it
        let mut tb = ScriptTransactionBuilder::prepare_contract_transfer(
            plain_contract_id,
            balance,
            asset_id,
            inputs,
            outputs,
            tx_policies,
        );

        self.add_witnesses(&mut tb)?;
        self.adjust_for_fee(&mut tb, balance).await?;

        let tx = tb.build(provider).await?;

        let tx_id = tx.id(provider.chain_id());
        let tx_status = provider.send_transaction_and_await_commit(tx).await?;

        let receipts = tx_status.take_receipts_checked(None)?;

        Ok((tx_id.to_string(), receipts))
    }

    /// Withdraws an amount of the base asset to
    /// an address on the base chain.
    /// Returns the transaction ID, message ID and the list of receipts.
    async fn withdraw_to_base_layer(
        &self,
        to: &Bech32Address,
        amount: u64,
        tx_policies: TxPolicies,
    ) -> std::result::Result<(TxId, Nonce, Vec<Receipt>), Error> {
        let provider = self.try_provider()?;

        let inputs = self
            .get_asset_inputs_for_amount(BASE_ASSET_ID, amount)
            .await?;

        let mut tb = ScriptTransactionBuilder::prepare_message_to_output(
            to.into(),
            amount,
            inputs,
            tx_policies,
        );

        self.add_witnesses(&mut tb)?;
        self.adjust_for_fee(&mut tb, amount).await?;

        let tx = tb.build(provider).await?;

        let tx_id = tx.id(provider.chain_id());
        let tx_status = provider.send_transaction_and_await_commit(tx).await?;

        let receipts = tx_status.take_receipts_checked(None)?;

        let nonce = extract_message_nonce(&receipts)
            .expect("MessageId could not be retrieved from tx receipts.");

        Ok((tx_id, nonce, receipts))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use fuel_crypto::{Message, SecretKey, Signature};
    use fuel_tx::{Address, ConsensusParameters, Output, Transaction as FuelTransaction};
    use fuels_core::{
        traits::Signer,
        types::{transaction::Transaction, transaction_builders::DryRunner},
    };
    use rand::{rngs::StdRng, RngCore, SeedableRng};

    use super::*;
    use crate::wallet::WalletUnlocked;

    #[tokio::test]
    async fn sign_and_verify() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // ANCHOR: sign_message
        let mut rng = StdRng::seed_from_u64(2322u64);
        let mut secret_seed = [0u8; 32];
        rng.fill_bytes(&mut secret_seed);

        let secret = secret_seed
            .as_slice()
            .try_into()
            .expect("The seed size is valid");

        // Create a wallet using the private key created above.
        let wallet = WalletUnlocked::new_from_private_key(secret, None);

        let message = Message::new("my message".as_bytes());
        let signature = wallet.sign(message).await?;

        // Check if signature is what we expect it to be
        assert_eq!(signature, Signature::from_str("0x8eeb238db1adea4152644f1cd827b552dfa9ab3f4939718bb45ca476d167c6512a656f4d4c7356bfb9561b14448c230c6e7e4bd781df5ee9e5999faa6495163d")?);

        // Recover address that signed the message
        let recovered_address = signature.recover(&message)?;

        assert_eq!(wallet.address().hash(), recovered_address.hash());

        // Verify signature
        signature.verify(&recovered_address, &message)?;
        // ANCHOR_END: sign_message

        Ok(())
    }

    #[derive(Default)]
    struct MockDryRunner {
        c_param: ConsensusParameters,
    }

    impl DryRunner for MockDryRunner {
        async fn dry_run_and_get_used_gas(&self, _: FuelTransaction, _: f32) -> Result<u64> {
            Ok(0)
        }
        fn consensus_parameters(&self) -> &ConsensusParameters {
            &self.c_param
        }
        async fn min_gas_price(&self) -> Result<u64> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn sign_tx_and_verify() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // ANCHOR: sign_tb
        let secret = SecretKey::from_str(
            "5f70feeff1f229e4a95e1056e8b4d80d0b24b565674860cc213bdb07127ce1b1",
        )?;
        let wallet = WalletUnlocked::new_from_private_key(secret, None);

        // Set up a transaction
        let mut tb = {
            let input_coin = Input::ResourceSigned {
                resource: CoinType::Coin(Coin {
                    amount: 10000000,
                    owner: wallet.address().clone(),
                    ..Default::default()
                }),
            };

            let output_coin = Output::coin(
                Address::from_str(
                    "0xc7862855b418ba8f58878db434b21053a61a2025209889cc115989e8040ff077",
                )?,
                1,
                Default::default(),
            );

            ScriptTransactionBuilder::prepare_transfer(
                vec![input_coin],
                vec![output_coin],
                Default::default(),
            )
        };

        // Add `Signer` to the transaction builder
        tb.add_signer(wallet.clone())?;
        // ANCHOR_END: sign_tb

        let tx = tb.build(&MockDryRunner::default()).await?; // Resolve signatures and add corresponding witness indexes

        // Extract the signature from the tx witnesses
        let bytes = <[u8; Signature::LEN]>::try_from(tx.witnesses().first().unwrap().as_ref())?;
        let tx_signature = Signature::from_bytes(bytes);

        // Sign the transaction manually
        let message = Message::from_bytes(*tx.id(0.into()));
        let signature = wallet.sign(message).await?;

        // Check if the signatures are the same
        assert_eq!(signature, tx_signature);

        // Check if the signature is what we expect it to be
        assert_eq!(signature, Signature::from_str("a7446cb9703d3bc9e68677715fc7ef6ed72ff4eeac0c67bdb0d9b9c8ba38048e078e38fdd85bf988cefd3737005f1be97ed8b9662f002b0480d4404ebb397fed")?);

        // Recover the address that signed the transaction
        let recovered_address = signature.recover(&message)?;

        assert_eq!(wallet.address().hash(), recovered_address.hash());

        // Verify signature
        signature.verify(&recovered_address, &message)?;

        Ok(())
    }
}
