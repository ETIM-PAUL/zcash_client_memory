//! Advisory output locking for the in-memory wallet.
//!
//! Lock state is held in [`MemoryWalletDb::output_locks`] and follows the semantics documented in
//! [`zcash_client_backend::data_api::locking`]. It is deliberately transient: it is not part of
//! the serialized wallet, so a wallet restored from bytes holds no locks.

use zcash_client_backend::{
    data_api::{
        WalletRead,
        locking::{LockError, LockOwner, OutputLockStore},
        wallet::TargetHeight,
    },
    wallet::OutputRef,
};
use zcash_protocol::{
    PoolType, TxId,
    consensus::{self, BlockHeight},
};

#[cfg(feature = "transparent-inputs")]
use crate::input_source::utxo_output_ref;
use crate::{AccountId, MemoryWalletDb, error::Error, input_source::note_output_ref};

impl<P: consensus::Parameters> MemoryWalletDb<P> {
    /// Returns the account that received the output named by `output`, or `None` if the wallet
    /// has no such output.
    fn output_account(&self, output: &OutputRef) -> Option<AccountId> {
        match output.pool() {
            PoolType::Shielded(pool) => self
                .received_notes
                .iter()
                .find(|n| {
                    &n.txid == output.txid()
                        && n.output_index == output.output_index()
                        && n.note.pool() == pool
                })
                .map(|n| n.account_id),
            PoolType::Transparent => {
                #[cfg(feature = "transparent-inputs")]
                {
                    let outpoint = ::transparent::bundle::OutPoint::new(
                        *output.txid().as_ref(),
                        output.output_index(),
                    );
                    self.transparent_received_outputs
                        .get(&outpoint)
                        .map(|txo| txo.account_id)
                }
                #[cfg(not(feature = "transparent-inputs"))]
                {
                    None
                }
            }
        }
    }

    /// Releases the lock on every output that the transaction `txid` is recorded as having
    /// spent.
    ///
    /// Once a spend is recorded, the spend record itself protects the output from being selected
    /// again, so the advisory lock has served its purpose and is dropped ("unlock-on-store").
    pub(crate) fn unlock_outputs_spent_by(&mut self, txid: &TxId) {
        if self.output_locks.is_empty() {
            return;
        }

        let mut spent: Vec<OutputRef> = self
            .received_note_spends
            .iter()
            .filter(|(_, spender)| *spender == txid)
            .filter_map(|(note_id, _)| {
                self.received_notes
                    .iter()
                    .find(|n| &n.note_id == note_id)
                    .map(note_output_ref)
            })
            .collect();

        #[cfg(feature = "transparent-inputs")]
        spent.extend(
            self.transparent_received_output_spends
                .iter()
                .filter(|(_, spender)| *spender == txid)
                .map(|(outpoint, _)| utxo_output_ref(outpoint)),
        );

        for output in spent {
            self.output_locks.remove(&output);
        }
    }

    /// The height of the next block to be mined, if the chain tip is known.
    fn lock_target_height(&self) -> Result<Option<TargetHeight>, Error> {
        Ok(self.chain_height()?.map(|tip| TargetHeight::from(tip + 1)))
    }
}

impl<P: consensus::Parameters> OutputLockStore for MemoryWalletDb<P> {
    type Error = Error;
    type AccountId = AccountId;

    fn lock_outputs(
        &mut self,
        outputs: &[OutputRef],
        owner: LockOwner,
        lock_expiry_height: BlockHeight,
    ) -> Result<usize, LockError<Self::Error>> {
        let chain_tip = self.chain_height().map_err(LockError::Storage)?;

        // Check every output before modifying any lock state, so that acquisition either
        // succeeds completely or leaves the wallet unchanged.
        for output in outputs {
            if self.output_account(output).is_none() {
                return Err(LockError::LockFailure(*output));
            }
            if let Some((existing_owner, existing_expiry)) = self.output_locks.get(output) {
                // A lock is replaceable if it has expired as of the chain tip, or is held by the
                // same owner. With an unknown chain tip nothing can be known to have expired.
                let expired = chain_tip.is_some_and(|tip| *existing_expiry <= tip);
                if !expired && *existing_owner != owner {
                    return Err(LockError::LockFailure(*output));
                }
            }
        }

        for output in outputs {
            self.output_locks
                .insert(*output, (owner, lock_expiry_height));
        }
        Ok(outputs.len())
    }

    fn unlock_output(&mut self, output: &OutputRef, owner: LockOwner) -> Result<bool, Self::Error> {
        match self.output_locks.get(output) {
            Some((existing_owner, _)) if *existing_owner == owner => {
                self.output_locks.remove(output);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn clear_locked_outputs(&mut self, account: Self::AccountId) -> Result<usize, Self::Error> {
        let to_clear: Vec<OutputRef> = self
            .output_locks
            .keys()
            .filter(|output| self.output_account(output) == Some(account))
            .copied()
            .collect();
        for output in &to_clear {
            self.output_locks.remove(output);
        }
        Ok(to_clear.len())
    }

    fn get_locked_outputs(&self, account: Self::AccountId) -> Result<Vec<OutputRef>, Self::Error> {
        let Some(target_height) = self.lock_target_height()? else {
            // Without a known chain tip no lock can be judged unexpired.
            return Ok(vec![]);
        };
        Ok(self
            .output_locks
            .keys()
            .filter(|output| {
                self.output_is_locked(output, target_height)
                    && self.output_account(output) == Some(account)
            })
            .copied()
            .collect())
    }
}
