//! Defines the types used by the [JSON RPC API]
//!
//! [JSON RPC API]: https://github.com/nimiq/core-js/wiki/JSON-RPC-API#common-data-types
use std::{
    fmt::{self, Display, Formatter},
    str::FromStr,
};

use clap::ValueEnum;
use nimiq_account::{BlockLog as BBlockLog, Log, TransactionLog};
use nimiq_block::{MicroJustification, MultiSignature};
use nimiq_blockchain_interface::{AbstractBlockchain, BlockchainError};
use nimiq_blockchain_proxy::BlockchainReadProxy;
use nimiq_bls::CompressedPublicKey;
use nimiq_collections::BitSet;
use nimiq_hash::{Blake2bHash, Blake2sHash, Hash};
use nimiq_keys::{Address, PublicKey};
use nimiq_primitives::{coin::Coin, policy::Policy, slots_allocation::Validators};
use nimiq_serde::Serialize as NimiqSerialize;
use nimiq_transaction::{
    account::htlc_contract::AnyHash,
    historic_transaction::{
        HistoricTransaction, HistoricTransactionData, JailEvent, PenalizeEvent, RewardEvent,
    },
    reward::RewardTransaction,
};
use nimiq_vrf::VrfSeed;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DeserializeFromStr, SerializeDisplay};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HashOrTx {
    Hash(Blake2bHash),
    Tx(Transaction),
}

impl From<Blake2bHash> for HashOrTx {
    fn from(hash: Blake2bHash) -> Self {
        HashOrTx::Hash(hash)
    }
}

impl From<nimiq_transaction::Transaction> for HashOrTx {
    fn from(transaction: nimiq_transaction::Transaction) -> Self {
        HashOrTx::Tx(Transaction::from_transaction(transaction))
    }
}

#[derive(Copy, Clone, Debug, SerializeDisplay, DeserializeFromStr)]
pub enum ValidityStartHeight {
    Absolute(u32),
    Relative(u32),
}

impl ValidityStartHeight {
    pub fn block_number(self, current_block_number: u32) -> u32 {
        match self {
            Self::Absolute(n) => n,
            Self::Relative(n) => n + current_block_number,
        }
    }
}

impl Default for ValidityStartHeight {
    fn default() -> Self {
        Self::Relative(0)
    }
}

impl Display for ValidityStartHeight {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Absolute(n) => write!(f, "{n}"),
            Self::Relative(n) => write!(f, "+{n}"),
        }
    }
}

impl FromStr for ValidityStartHeight {
    type Err = <u32 as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(stripped) = s.strip_prefix('+') {
            Ok(Self::Relative(stripped.parse()?))
        } else {
            Ok(Self::Absolute(s.parse()?))
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum HashAlgorithm {
    Blake2b = 1,
    Sha256 = 3,
    Sha512 = 4,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    pub hash: Blake2bHash,
    pub size: u32,
    pub batch: u32,
    pub epoch: u32,

    pub version: u16,
    pub number: u32,
    pub timestamp: u64,
    pub parent_hash: Blake2bHash,
    pub seed: VrfSeed,
    #[serde(with = "crate::serde_helpers::hex")]
    pub extra_data: Vec<u8>,
    pub state_hash: Blake2bHash,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<Blake2sHash>,
    pub history_hash: Blake2bHash,

    #[serde(skip_serializing_if = "Option::is_none")]
    transactions: Option<Vec<ExecutedTransaction>>,

    #[serde(flatten)]
    pub additional_fields: BlockAdditionalFields,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum BlockAdditionalFields {
    #[serde(rename_all = "camelCase")]
    Macro {
        is_election_block: bool,

        parent_election_hash: Blake2bHash,

        // None if not an election block.
        #[serde(skip_serializing_if = "Option::is_none")]
        interlink: Option<Vec<Blake2bHash>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        slots: Option<Vec<Slots>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        next_batch_initial_punished_set: Option<BitSet>,

        #[serde(skip_serializing_if = "Option::is_none")]
        justification: Option<TendermintProof>,
    },
    #[serde(rename_all = "camelCase")]
    Micro {
        producer: Slot,

        #[serde(skip_serializing_if = "Option::is_none")]
        equivocation_proofs: Option<Vec<EquivocationProof>>,

        #[serde(skip_serializing_if = "Option::is_none")]
        justification: Option<MicroJustification>,
    },
}

impl Block {
    pub fn from_macro_block(
        macro_block: nimiq_block::MacroBlock,
        include_body: bool,
    ) -> Result<Self, BlockchainError> {
        let block_number = macro_block.block_number();
        let timestamp = macro_block.timestamp();
        let size = macro_block.serialized_size() as u32;
        let batch = Policy::batch_at(block_number);
        let epoch = Policy::epoch_at(block_number);

        let slots = macro_block.get_validators().map(Slots::from_slots);

        let next_batch_initial_punished_set = match macro_block.body.clone() {
            None => None,
            Some(body) => Some(body.next_batch_initial_punished_set),
        };

        // Get the reward inherents and convert them to reward transactions.
        let mut transactions = None;
        if include_body {
            transactions = macro_block.body.as_ref().map(|body| {
                body.transactions
                    .iter()
                    .map(|tx| ExecutedTransaction {
                        transaction: Transaction::from_reward_tx(tx, block_number),
                        execution_result: true,
                    })
                    .collect()
            });
        }

        Ok(Block {
            hash: macro_block.hash(),
            size,
            batch,
            epoch,
            version: macro_block.header.version,
            number: block_number,
            timestamp,
            parent_hash: macro_block.header.parent_hash,
            seed: macro_block.header.seed,
            extra_data: macro_block.header.extra_data,
            state_hash: macro_block.header.state_root,
            body_hash: Some(macro_block.header.body_root),
            history_hash: macro_block.header.history_root,
            transactions,
            additional_fields: BlockAdditionalFields::Macro {
                is_election_block: Policy::is_election_block_at(block_number),
                parent_election_hash: macro_block.header.parent_election_hash,
                interlink: macro_block.header.interlink,
                slots,

                next_batch_initial_punished_set,
                justification: macro_block.justification.map(TendermintProof::from),
            },
        })
    }

    pub fn from_block(
        blockchain: &BlockchainReadProxy,
        block: nimiq_block::Block,
        include_body: bool,
    ) -> Result<Self, BlockchainError> {
        let block_number = block.block_number();
        let timestamp = block.timestamp();
        let size = block.serialized_size() as u32;
        let batch = Policy::batch_at(block_number);
        let epoch = Policy::epoch_at(block_number);

        match block {
            nimiq_block::Block::Macro(macro_block) => {
                Self::from_macro_block(macro_block, include_body)
            }

            nimiq_block::Block::Micro(micro_block) => {
                let (equivocation_proofs, transactions) = match micro_block.body {
                    None => (None, None),
                    Some(ref body) => (
                        Some(
                            body.equivocation_proofs
                                .clone()
                                .into_iter()
                                .map(Into::into)
                                .collect(),
                        ),
                        if include_body {
                            let head_height = blockchain.head().block_number();
                            Some(
                                body.transactions
                                    .clone()
                                    .into_iter()
                                    .map(|tx| {
                                        // We obtain an internal executed transaction
                                        // We need to grab the internal transaction and map it to the RPC transaction structure
                                        ExecutedTransaction::from_blockchain(
                                            tx,
                                            block_number,
                                            timestamp,
                                            head_height,
                                        )
                                    })
                                    .collect(),
                            )
                        } else {
                            None
                        },
                    ),
                };

                Ok(Block {
                    hash: micro_block.hash(),
                    size,
                    batch,
                    epoch,
                    version: micro_block.header.version,
                    number: block_number,
                    timestamp,
                    parent_hash: micro_block.header.parent_hash,
                    seed: micro_block.header.seed,
                    extra_data: micro_block.header.extra_data,
                    state_hash: micro_block.header.state_root,
                    body_hash: Some(micro_block.header.body_root),
                    history_hash: micro_block.header.history_root,
                    transactions,
                    additional_fields: BlockAdditionalFields::Micro {
                        producer: Slot::from(blockchain, block_number, block_number)?,
                        equivocation_proofs,
                        justification: micro_block.justification.map(Into::into),
                    },
                })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TendermintProof {
    round: u32,
    sig: MultiSignature,
}

impl From<nimiq_block::TendermintProof> for TendermintProof {
    fn from(tendermint_proof: nimiq_block::TendermintProof) -> Self {
        Self {
            round: tendermint_proof.round,
            sig: tendermint_proof.sig,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyConstants {
    pub staking_contract_address: String,
    pub coinbase_address: String,
    pub transaction_validity_window: u32,
    pub max_size_micro_body: usize,
    pub version: u16,
    pub slots: u16,
    pub blocks_per_batch: u32,
    pub batches_per_epoch: u16,
    pub blocks_per_epoch: u32,
    pub validator_deposit: u64,
    pub total_supply: u64,
    pub block_separation_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    pub slot_number: u16,
    pub validator: Address,
    pub public_key: CompressedPublicKey,
}

impl Slot {
    pub fn from(
        blockchain: &BlockchainReadProxy,
        block_number: u32,
        offset: u32,
    ) -> Result<Self, BlockchainError> {
        let (validator, slot_number) = blockchain.get_slot_owner_at(block_number, offset)?;

        Ok(Slot {
            slot_number,
            validator: validator.address,
            public_key: validator.voting_key.compressed().clone(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slots {
    pub first_slot_number: u16,
    pub num_slots: u16,
    pub validator: Address,
    pub public_key: CompressedPublicKey,
}

impl Slots {
    pub fn from_slots(validators: Validators) -> Vec<Slots> {
        let mut slots = vec![];

        for validator in validators.iter() {
            slots.push(Slots {
                first_slot_number: validator.slots.start,
                num_slots: validator.num_slots(),
                validator: validator.address.clone(),
                public_key: validator.voting_key.compressed().clone(),
            })
        }

        slots
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PenalizedSlots {
    pub block_number: u32,
    pub disabled: BitSet,
}

/// An equivocation proof proves that a validator misbehaved.
///
/// This can come in several forms, but e.g. producing two blocks in a single slot or voting twice
/// in the same round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum EquivocationProof {
    Fork(ForkProof),
    DoubleProposal(DoubleProposalProof),
    DoubleVote(DoubleVoteProof),
}

impl From<nimiq_block::EquivocationProof> for EquivocationProof {
    fn from(proof: nimiq_block::EquivocationProof) -> Self {
        match proof {
            nimiq_block::EquivocationProof::Fork(proof) => EquivocationProof::Fork(proof.into()),
            nimiq_block::EquivocationProof::DoubleProposal(proof) => {
                EquivocationProof::DoubleProposal(proof.into())
            }
            nimiq_block::EquivocationProof::DoubleVote(proof) => {
                EquivocationProof::DoubleVote(proof.into())
            }
        }
    }
}

/// Struct representing a fork proof. A fork proof proves that a given validator created or
/// continued a fork. For this it is enough to provide two different headers, with the same block
/// number, signed by the same validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkProof {
    pub block_number: u32,
    pub hashes: [Blake2bHash; 2],
}

impl From<nimiq_block::ForkProof> for ForkProof {
    fn from(fork_proof: nimiq_block::ForkProof) -> Self {
        Self {
            block_number: fork_proof.block_number(),
            hashes: [fork_proof.header1_hash(), fork_proof.header2_hash()],
        }
    }
}

/// Struct representing a double proposal proof. A double proposal proof proves that a given
/// validator created two macro block proposals at the same height, in the same round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoubleProposalProof {
    pub block_number: u32,
    pub hashes: [Blake2bHash; 2],
}

impl From<nimiq_block::DoubleProposalProof> for DoubleProposalProof {
    fn from(double_proposal_proof: nimiq_block::DoubleProposalProof) -> Self {
        Self {
            block_number: double_proposal_proof.block_number(),
            hashes: [
                double_proposal_proof.header1_hash(),
                double_proposal_proof.header2_hash(),
            ],
        }
    }
}

/// Struct representing a double vote proof. A double vote proof proves that a given
/// validator voted twice at same height, in the same round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoubleVoteProof {
    pub block_number: u32,
}

impl From<nimiq_block::DoubleVoteProof> for DoubleVoteProof {
    fn from(double_vote_proof: nimiq_block::DoubleVoteProof) -> Self {
        Self {
            block_number: double_vote_proof.block_number(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutedTransaction {
    #[serde(flatten)]
    transaction: Transaction,
    execution_result: bool,
}

impl ExecutedTransaction {
    pub fn from_blockchain(
        transaction: nimiq_transaction::ExecutedTransaction,
        block_number: u32,
        timestamp: u64,
        head_height: u32,
    ) -> Self {
        // We obtain an internal executed transaction
        // We need to grab the internal transaction and map it to the RPC transaction structure
        match transaction {
            nimiq_transaction::ExecutedTransaction::Ok(tx) => ExecutedTransaction {
                transaction: Transaction::from_blockchain(tx, block_number, timestamp, head_height),
                execution_result: true,
            },

            nimiq_transaction::ExecutedTransaction::Err(tx, ..) => ExecutedTransaction {
                transaction: Transaction::from_blockchain(tx, block_number, timestamp, head_height),
                execution_result: false,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub hash: Blake2bHash,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmations: Option<u32>,

    pub from: Address,
    pub to: Address,
    pub value: Coin,
    pub fee: Coin,
    #[serde(with = "crate::serde_helpers::hex")]
    pub recipient_data: Vec<u8>,
    pub flags: u8,
    pub validity_start_height: u32,
    #[serde(with = "crate::serde_helpers::hex")]
    pub proof: Vec<u8>,
}

impl Transaction {
    pub fn from_transaction(transaction: nimiq_transaction::Transaction) -> Self {
        Transaction::from(transaction, None, None, None)
    }

    pub fn from_blockchain(
        transaction: nimiq_transaction::Transaction,
        block_number: u32,
        timestamp: u64,
        head_height: u32,
    ) -> Self {
        Transaction::from(
            transaction,
            Some(block_number),
            Some(timestamp),
            Some(head_height),
        )
    }

    fn from(
        transaction: nimiq_transaction::Transaction,
        block_number: Option<u32>,
        timestamp: Option<u64>,
        head_height: Option<u32>,
    ) -> Self {
        Transaction {
            hash: transaction.hash(),
            block_number,
            timestamp,
            confirmations: match head_height {
                Some(height) => block_number.map(|block| height.saturating_sub(block) + 1),
                None => None,
            },
            from: transaction.sender,
            to: transaction.recipient,
            value: transaction.value,
            fee: transaction.fee,
            flags: transaction.flags.bits(),
            recipient_data: transaction.recipient_data,
            validity_start_height: transaction.validity_start_height,
            proof: transaction.proof,
        }
    }

    fn from_reward_tx(reward_tx: &RewardTransaction, block_number: u32) -> Self {
        Self {
            block_number: Some(block_number),
            from: Policy::COINBASE_ADDRESS,
            to: reward_tx.recipient.clone(),
            value: reward_tx.value,
            fee: Coin::ZERO,
            validity_start_height: block_number,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Inherent {
    #[serde(rename_all = "camelCase")]
    Reward {
        block_number: u32,
        block_time: u64,
        validator_address: Address,
        target: Address,
        value: Coin,
        hash: Blake2bHash,
    },
    #[serde(rename_all = "camelCase")]
    Penalize {
        block_number: u32,
        block_time: u64,
        validator_address: Address,
        offense_event_block: u32,
    },
    #[serde(rename_all = "camelCase")]
    Jail {
        block_number: u32,
        block_time: u64,
        validator_address: Address,
        offense_event_block: u32,
    },
}

impl Inherent {
    pub fn try_from(hist_tx: HistoricTransaction) -> Option<Self> {
        let hash = hist_tx.data.hash();
        Some(match hist_tx.data {
            HistoricTransactionData::Basic(_) => return None,
            HistoricTransactionData::Equivocation(_) => return None,
            HistoricTransactionData::Reward(RewardEvent {
                validator_address,
                reward_address,
                value,
            }) => Inherent::Reward {
                block_number: hist_tx.block_number,
                block_time: hist_tx.block_time,
                validator_address,
                target: reward_address,
                value,
                hash,
            },
            HistoricTransactionData::Penalize(PenalizeEvent {
                validator_address,
                offense_event_block,
            }) => Inherent::Penalize {
                block_number: hist_tx.block_number,
                block_time: hist_tx.block_time,
                validator_address,
                offense_event_block,
            },
            HistoricTransactionData::Jail(JailEvent {
                validator_address,
                offense_event_block,
            }) => Inherent::Jail {
                block_number: hist_tx.block_number,
                block_time: hist_tx.block_time,
                validator_address,
                offense_event_block,
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub address: Address,
    pub balance: Coin,

    #[serde(flatten)]
    pub account_additional_fields: AccountAdditionalFields,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AccountAdditionalFields {
    /// Additional account information for basic accounts.
    #[serde(rename_all = "camelCase")]
    Basic {},

    /// Additional account information for vesting contracts.
    #[serde(rename_all = "camelCase")]
    Vesting {
        /// User friendly address (NQ-address) of the owner of the vesting contract.
        owner: Address,
        /// The block that the vesting contracted commenced.
        vesting_start: u64,
        /// The number of blocks after which some part of the vested funds is released.
        vesting_step_blocks: u64,
        /// The amount (in Luna) released every vestingStepBlocks blocks.
        vesting_step_amount: Coin,
        /// The total amount (in smallest unit) that was provided at the contract creation.
        vesting_total_amount: Coin,
    },

    /// Additional account information for HTLC contracts.
    #[serde(rename_all = "camelCase")]
    HTLC {
        /// User friendly address (NQ-address) of the sender of the HTLC.
        sender: Address,
        /// User friendly address (NQ-address) of the recipient of the HTLC.
        recipient: Address,
        /// Hash algorithm and Hex-encoded hash root.
        hash_root: AnyHash,
        /// Number of hashes this HTLC is split into
        hash_count: u8,
        /// Block after which the contract can only be used by the original sender to recover funds.
        timeout: u64,
        /// The total amount (in smallest unit) that was provided at the contract creation.
        total_amount: Coin,
    },
    /// Additional account information for the staking contract.
    #[serde(rename_all = "camelCase")]
    Staking {},
}

impl Account {
    /// Maps an account to the RPC account type
    pub fn from_account(address: Address, account: nimiq_account::Account) -> Self {
        match account {
            nimiq_account::Account::Basic(basic) => Account {
                address,
                balance: basic.balance,
                account_additional_fields: AccountAdditionalFields::Basic {},
            },
            nimiq_account::Account::Vesting(vesting) => Account {
                address,
                balance: vesting.balance,
                account_additional_fields: AccountAdditionalFields::Vesting {
                    owner: vesting.owner,
                    vesting_start: vesting.start_time,
                    vesting_step_blocks: vesting.time_step,
                    vesting_step_amount: vesting.step_amount,
                    vesting_total_amount: vesting.total_amount,
                },
            },
            nimiq_account::Account::HTLC(htlc) => Account {
                address,
                balance: htlc.balance,
                account_additional_fields: AccountAdditionalFields::HTLC {
                    sender: htlc.sender,
                    recipient: htlc.recipient,
                    hash_root: htlc.hash_root,
                    hash_count: htlc.hash_count,
                    timeout: htlc.timeout,
                    total_amount: htlc.total_amount,
                },
            },
            nimiq_account::Account::Staking(staking) => Account {
                address,
                balance: staking.balance,
                account_additional_fields: AccountAdditionalFields::Staking {},
            },
        }
    }

    /// Maps an account to the RPC account type with blockchain state
    pub fn from_account_with_state(
        address: Address,
        account: nimiq_account::Account,
        blockchain_state: BlockchainState,
    ) -> RPCData<Self, BlockchainState> {
        RPCData {
            data: Self::from_account(address, account),
            metadata: blockchain_state,
        }
    }

    pub fn empty(address: Address) -> Self {
        Account {
            address,
            balance: Coin::ZERO,
            account_additional_fields: AccountAdditionalFields::Basic {},
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Staker {
    pub address: Address,
    pub balance: Coin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegation: Option<Address>,
    pub inactive_balance: Coin,
    pub inactive_from: Option<u32>,
}

impl Staker {
    pub fn from_staker(staker: &nimiq_account::Staker) -> Self {
        Staker {
            address: staker.address.clone(),
            balance: staker.balance,
            delegation: staker.delegation.clone(),
            inactive_balance: staker.inactive_balance,
            inactive_from: staker.inactive_from,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Validator {
    pub address: Address,
    pub signing_key: PublicKey,
    pub voting_key: CompressedPublicKey,
    pub reward_address: Address,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_data: Option<Blake2bHash>,
    pub balance: Coin,
    pub num_stakers: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inactivity_flag: Option<u32>,
    pub retired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jailed_from: Option<u32>,
}

impl Validator {
    pub fn from_validator(validator: &nimiq_account::Validator) -> Self {
        Validator {
            address: validator.address.clone(),
            signing_key: validator.signing_key,
            voting_key: validator.voting_key.clone(),
            reward_address: validator.reward_address.clone(),
            signal_data: validator.signal_data.clone(),
            balance: validator.total_stake,
            num_stakers: validator.num_stakers,
            inactivity_flag: validator.inactive_from,
            retired: validator.retired,
            jailed_from: validator.jailed_from,
        }
    }
}

pub type RPCResult<T, S, E> = Result<RPCData<T, S>, E>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RPCData<T, S> {
    pub data: T,
    pub metadata: S,
}

impl<T, S> RPCData<T, S> {
    pub fn new(data: T, metadata: S) -> Self {
        RPCData { data, metadata }
    }
}

impl<T> RPCData<T, BlockchainState> {
    pub fn with_blockchain(
        data: T,
        blockchain: &BlockchainReadProxy,
    ) -> RPCData<T, BlockchainState> {
        RPCData {
            data,
            metadata: BlockchainState::with_blockchain(blockchain),
        }
    }
}

impl<T> From<T> for RPCData<T, ()> {
    fn from(data: T) -> Self {
        RPCData { data, metadata: () }
    }
}

impl RPCData<BlockLog, BlockchainState> {
    pub fn with_block_log(block_log: BBlockLog) -> Self {
        match block_log {
            BBlockLog::AppliedBlock {
                inherent_logs,
                block_hash,
                block_number,
                timestamp,
                tx_logs,
                total_tx_size: _,
            } => Self::new(
                BlockLog::AppliedBlock {
                    inherent_logs,
                    timestamp,
                    tx_logs,
                },
                BlockchainState {
                    block_number,
                    block_hash,
                },
            ),
            BBlockLog::RevertedBlock {
                inherent_logs,
                block_hash,
                block_number,
                tx_logs,
                total_tx_size: _,
            } => Self::new(
                BlockLog::RevertedBlock {
                    inherent_logs,
                    tx_logs,
                },
                BlockchainState {
                    block_number,
                    block_hash,
                },
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockchainState {
    pub block_number: u32,
    pub block_hash: Blake2bHash,
}

impl BlockchainState {
    pub fn new(block_number: u32, block_hash: Blake2bHash) -> Self {
        BlockchainState {
            block_number,
            block_hash,
        }
    }

    pub fn with_blockchain(blockchain: &BlockchainReadProxy) -> Self {
        let block = blockchain.head();
        BlockchainState::new(block.block_number(), block.hash())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum LogType {
    PayFee,
    Transfer,
    HtlcCreate,
    HtlcTimeoutResolve,
    HtlcRegularTransfer,
    HtlcEarlyResolve,
    VestingCreate,
    CreateValidator,
    UpdateValidator,
    ValidatorFeeDeduction,
    DeactivateValidator,
    ReactivateValidator,
    JailValidator,
    CreateStaker,
    Stake,
    UpdateStaker,
    StakerFeeDeduction,
    RetireValidator,
    DeleteValidator,
    Unstake,
    SetInactiveStake,
    PayoutReward,
    Penalize,
    Jail,
    RevertContract,
    FailedTransaction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum BlockLog {
    #[serde(rename_all = "camelCase")]
    AppliedBlock {
        #[serde(rename = "inherents")]
        inherent_logs: Vec<Log>,
        timestamp: u64,
        #[serde(rename = "transactions")]
        tx_logs: Vec<TransactionLog>,
    },

    #[serde(rename_all = "camelCase")]
    RevertedBlock {
        #[serde(rename = "inherents")]
        inherent_logs: Vec<Log>,
        #[serde(rename = "transactions")]
        tx_logs: Vec<TransactionLog>,
    },
}

impl LogType {
    pub fn with_log(log: &Log) -> Self {
        match log {
            Log::PayFee { .. } => Self::PayFee,
            Log::Transfer { .. } => Self::Transfer,
            Log::HTLCCreate { .. } => Self::HtlcCreate,
            Log::HTLCTimeoutResolve { .. } => Self::HtlcTimeoutResolve,
            Log::HTLCRegularTransfer { .. } => Self::HtlcRegularTransfer,
            Log::HTLCEarlyResolve { .. } => Self::HtlcEarlyResolve,
            Log::VestingCreate { .. } => Self::VestingCreate,
            Log::CreateValidator { .. } => Self::CreateValidator,
            Log::UpdateValidator { .. } => Self::UpdateValidator,
            Log::DeactivateValidator { .. } => Self::DeactivateValidator,
            Log::ReactivateValidator { .. } => Self::ReactivateValidator,
            Log::JailValidator { .. } => Self::JailValidator,
            Log::CreateStaker { .. } => Self::CreateStaker,
            Log::Stake { .. } => Self::Stake,
            Log::UpdateStaker { .. } => Self::UpdateStaker,
            Log::RetireValidator { .. } => Self::RetireValidator,
            Log::DeleteValidator { .. } => Self::DeleteValidator,
            Log::Unstake { .. } => Self::Unstake,
            Log::SetInactiveStake { .. } => Self::SetInactiveStake,
            Log::PayoutReward { .. } => Self::PayoutReward,
            Log::Penalize { .. } => Self::Penalize,
            Log::Jail { .. } => Self::Jail,
            Log::RevertContract { .. } => Self::RevertContract,
            Log::FailedTransaction { .. } => Self::FailedTransaction,
            Log::ValidatorFeeDeduction { .. } => Self::ValidatorFeeDeduction,
            Log::StakerFeeDeduction { .. } => Self::StakerFeeDeduction,
        }
    }
}

/// Checks if a given log is related to any of the addresses provided and if it is of any of the log types provided.
/// If no addresses and log_types are provided it will return false.
/// If the vec of addresses is empty, compares only to the log_types (meaning it will not care about the addresses
/// the log is related to), and vice_versa.
pub fn is_of_log_type_and_related_to_addresses(
    log: &Log,
    addresses: &Vec<Address>,
    log_types: &Vec<LogType>,
) -> bool {
    // If addresses are empty, it tries to find a matching log_types and early return. Otherwise, it will finish and return false.
    if addresses.is_empty() {
        for log_type in log_types {
            if log_type.eq(&LogType::with_log(log)) {
                return true;
            }
        }
    } else {
        // Iterates over the addresses (and the log_types if those exist).
        for address in addresses {
            if log.is_related_to_address(address) {
                if log_types.is_empty() {
                    return true;
                }
                // If there are log_types to compare with, it tries to find a match and early return.
                for log_type in log_types {
                    if log_type.eq(&LogType::with_log(log)) {
                        return true;
                    }
                }
            }
        }
    }
    // It iterated over all the existing vecs and found no match.
    false
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ZKPState {
    latest_block: Block,
    latest_proof: Option<String>,
}

impl ZKPState {
    pub fn with_zkp_state(zkp_state: &nimiq_zkp_component::types::ZKPState) -> Self {
        let latest_block = Block::from_macro_block(zkp_state.latest_block.clone(), true).unwrap();
        let latest_proof = zkp_state
            .latest_proof
            .as_ref()
            .map(|latest_proof| format!("{latest_proof:?}"));

        Self {
            latest_block,
            latest_proof,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MempoolInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _0: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _1: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _2: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _5: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _10: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _20: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _50: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _100: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _200: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _500: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _1000: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _2000: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _5000: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _10000: Option<u32>,
    pub total: u32,
    pub buckets: Vec<u32>,
}

impl MempoolInfo {
    pub fn from_txs(transactions: Vec<nimiq_transaction::Transaction>) -> Self {
        let mut info = MempoolInfo {
            _0: None,
            _1: None,
            _2: None,
            _5: None,
            _10: None,
            _20: None,
            _50: None,
            _100: None,
            _200: None,
            _500: None,
            _1000: None,
            _2000: None,
            _5000: None,
            _10000: None,
            total: 0,
            buckets: vec![],
        };

        for tx in transactions {
            match tx.fee_per_byte() {
                x if x < 1.0 => {
                    if let Some(n) = info._0 {
                        info._0 = Some(n + 1);
                    } else {
                        info._0 = Some(1);
                        info.buckets.push(0)
                    }
                }
                x if x < 2.0 => {
                    if let Some(n) = info._1 {
                        info._1 = Some(n + 1);
                    } else {
                        info._1 = Some(1);
                        info.buckets.push(1)
                    }
                }
                x if x < 5.0 => {
                    if let Some(n) = info._2 {
                        info._2 = Some(n + 1);
                    } else {
                        info._2 = Some(1);
                        info.buckets.push(2)
                    }
                }
                x if x < 10.0 => {
                    if let Some(n) = info._5 {
                        info._5 = Some(n + 1);
                    } else {
                        info._5 = Some(1);
                        info.buckets.push(5)
                    }
                }
                x if x < 20.0 => {
                    if let Some(n) = info._10 {
                        info._10 = Some(n + 1);
                    } else {
                        info._10 = Some(1);
                        info.buckets.push(10)
                    }
                }
                x if x < 50.0 => {
                    if let Some(n) = info._20 {
                        info._20 = Some(n + 1);
                    } else {
                        info._20 = Some(1);
                        info.buckets.push(20)
                    }
                }
                x if x < 100.0 => {
                    if let Some(n) = info._50 {
                        info._50 = Some(n + 1);
                    } else {
                        info._50 = Some(1);
                        info.buckets.push(50)
                    }
                }
                x if x < 200.0 => {
                    if let Some(n) = info._100 {
                        info._100 = Some(n + 1);
                    } else {
                        info._100 = Some(1);
                        info.buckets.push(100)
                    }
                }
                x if x < 500.0 => {
                    if let Some(n) = info._200 {
                        info._200 = Some(n + 1);
                    } else {
                        info._200 = Some(1);
                        info.buckets.push(200)
                    }
                }
                x if x < 1000.0 => {
                    if let Some(n) = info._500 {
                        info._500 = Some(n + 1);
                    } else {
                        info._500 = Some(1);
                        info.buckets.push(500)
                    }
                }
                x if x < 2000.0 => {
                    if let Some(n) = info._1000 {
                        info._1000 = Some(n + 1);
                    } else {
                        info._1000 = Some(1);
                        info.buckets.push(1000)
                    }
                }
                x if x < 5000.0 => {
                    if let Some(n) = info._2000 {
                        info._2000 = Some(n + 1);
                    } else {
                        info._2000 = Some(1);
                        info.buckets.push(2000)
                    }
                }
                x if x < 10000.0 => {
                    if let Some(n) = info._5000 {
                        info._5000 = Some(n + 1);
                    } else {
                        info._5000 = Some(1);
                        info.buckets.push(5000)
                    }
                }
                _ => {
                    if let Some(n) = info._10000 {
                        info._10000 = Some(n + 1);
                    } else {
                        info._10000 = Some(1);
                        info.buckets.push(10000)
                    }
                }
            }

            info.total += 1;
        }

        info
    }
}
