use nimiq_blockchain_interface::{AbstractBlockchain, BlockchainError};
use nimiq_database::mdbx::MdbxReadTransaction;
use nimiq_primitives::{
    policy::Policy,
    slots_allocation::{Slot, Validators},
};
use nimiq_vrf::{VrfEntropy, VrfSeed};

use crate::Blockchain;

/// Implements methods to handle slots and validators.
impl Blockchain {
    /// Gets the active validators for a given epoch.
    pub fn get_validators_for_epoch(
        &self,
        epoch: u32,
        txn: Option<&MdbxReadTransaction>,
    ) -> Result<Validators, BlockchainError> {
        let current_epoch = Policy::epoch_at(self.state.main_chain.head.block_number());

        if epoch == current_epoch {
            self.state
                .current_slots
                .clone()
                .ok_or(BlockchainError::NoValidatorsFound)
        } else if epoch + 1 == current_epoch {
            self.state
                .previous_slots
                .clone()
                .ok_or(BlockchainError::NoValidatorsFound)
        } else if epoch == 0 {
            Err(BlockchainError::InvalidEpoch)
        } else {
            self.chain_store
                .get_block_at(
                    Policy::election_block_of(epoch - 1).ok_or(BlockchainError::InvalidEpoch)?,
                    true,
                    txn,
                )?
                .unwrap_macro()
                .get_validators()
                .ok_or(BlockchainError::NoValidatorsFound)
        }
    }

    /// Calculates the next validators from a given seed.
    pub fn next_validators(&self, seed: &VrfSeed) -> Validators {
        let staking_contract = self.get_staking_contract();
        let data_store = self.get_staking_contract_store();
        let txn = self.read_transaction();
        staking_contract.select_validators(&data_store.read(&txn), seed)
    }

    pub fn get_proposer(
        &self,
        block_number: u32,
        offset: u32,
        vrf_entropy: VrfEntropy,
        txn: Option<&MdbxReadTransaction>,
    ) -> Result<Slot, BlockchainError> {
        // Fetch the latest macro block that precedes the block at the given block_number.
        // We use the disabled_slots set from that macro block for the slot selection.
        // Slots are not immediately disabled once they are penalized.
        //  An offline validator will thus continue to delay the chain as his slot(s) will still
        //  be selected until the end of the batch.
        let macro_block = self.get_block_at(Policy::macro_block_before(block_number), true, txn)?;
        let disabled_slots = macro_block
            .unwrap_macro()
            .body
            .unwrap()
            .next_batch_initial_punished_set;

        // Compute the slot number of the next proposer.
        let slot_number = <Blockchain as AbstractBlockchain>::compute_slot_number(
            offset,
            vrf_entropy,
            disabled_slots,
        );

        // Fetch the validators that are active in given block's epoch.
        let epoch_number = Policy::epoch_at(block_number);
        let validators = self.get_validators_for_epoch(epoch_number, txn)?;

        // Get the validator that owns the proposer slot.
        let validator = validators.get_validator_by_slot_number(slot_number);

        // Also get the slot band for convenient access.
        let slot_band = validators.get_band_from_slot(slot_number);

        Ok(Slot {
            number: slot_number,
            band: slot_band,
            validator: validator.clone(),
        })
    }
}
