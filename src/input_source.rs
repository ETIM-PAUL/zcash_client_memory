use std::num::NonZeroU32;

use shardtree::store::ShardStore;
use zcash_client_backend::{
    data_api::{
        AccountMeta, InputSource, NoteFilter, PoolMeta, ReceivedNotes, TargetValue, WalletRead,
        locking::LockFilter,
        wallet::{ConfirmationsPolicy, TargetHeight},
    },
    wallet::{NoteId, OutputRef},
};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::{
    PoolType,
    ShieldedPool::{self as ShieldedProtocol, Sapling},
    consensus::{self, BlockHeight, BranchId},
    value::Zatoshis,
};

#[cfg(feature = "orchard")]
use zcash_protocol::ShieldedPool::Orchard;

#[cfg(feature = "transparent-inputs")]
use {
    ::transparent::{address::TransparentAddress, bundle::OutPoint},
    zcash_client_backend::{
        data_api::{CoinbaseFilter, TransactionStatus},
        wallet::WalletTransparentOutput,
    },
};

use crate::{AccountId, MemoryWalletDb, error::Error, to_spendable_notes};

/// Returns the [`OutputRef`] under which lock state is tracked for a received shielded note.
pub(crate) fn note_output_ref(note: &crate::ReceivedNote) -> OutputRef {
    OutputRef::new(
        note.txid,
        PoolType::Shielded(note.note.pool()),
        note.output_index,
    )
}

/// Returns the [`OutputRef`] under which lock state is tracked for a transparent output.
#[cfg(feature = "transparent-inputs")]
pub(crate) fn utxo_output_ref(outpoint: &OutPoint) -> OutputRef {
    OutputRef::new(
        zcash_primitives::transaction::TxId::from_bytes(*outpoint.hash()),
        PoolType::TRANSPARENT,
        outpoint.n(),
    )
}

impl<P: consensus::Parameters> InputSource for MemoryWalletDb<P> {
    type Error = crate::error::Error;
    type AccountId = AccountId;
    type NoteRef = NoteId;

    fn get_spendable_note(
        &self,
        txid: &zcash_primitives::transaction::TxId,
        protocol: ShieldedProtocol,
        index: u32,
        target_height: TargetHeight,
        lock_filter: LockFilter<'_>,
    ) -> Result<
        Option<
            zcash_client_backend::wallet::ReceivedNote<
                Self::NoteRef,
                zcash_client_backend::wallet::Note,
            >,
        >,
        Self::Error,
    > {
        let note = self
            .received_notes
            .iter()
            .find(|rn| &rn.txid == txid && rn.note.pool() == protocol && rn.output_index == index);

        Ok(if let Some(note) = note {
            if self.note_is_spent(note, target_height)?
                || !self.output_lock_admits(&note_output_ref(note), target_height, lock_filter)
            {
                None
            } else {
                let (mined_height, max_shielding_input_height) = self.note_heights(note);
                Some(zcash_client_backend::wallet::ReceivedNote::from_parts(
                    note.note_id,
                    *txid,
                    index.try_into().unwrap(), // this overflow can never happen or else the chain is broken
                    note.note.clone(),
                    note.recipient_key_scope
                        .ok_or(Error::Missing("recipient key scope".into()))?,
                    note.commitment_tree_position
                        .ok_or(Error::Missing("commitment tree position".into()))?,
                    mined_height,
                    max_shielding_input_height,
                ))
            }
        } else {
            None
        })
    }

    fn select_spendable_notes(
        &self,
        account: Self::AccountId,
        target_value: TargetValue,
        sources: &[ShieldedProtocol],
        target_height: TargetHeight,
        confirmations_policy: ConfirmationsPolicy,
        exclude: &[Self::NoteRef],
        lock_filter: LockFilter<'_>,
    ) -> Result<zcash_client_backend::data_api::ReceivedNotes<Self::NoteRef>, Self::Error> {
        let sapling_eligible_notes = if sources.contains(&Sapling) {
            self.select_spendable_notes_from_pool(
                account,
                target_value,
                &Sapling,
                target_height,
                confirmations_policy,
                exclude,
                lock_filter,
            )?
        } else {
            Vec::new()
        };

        #[cfg(feature = "orchard")]
        let orchard_eligible_notes = if sources.contains(&Orchard) {
            self.select_spendable_notes_from_pool(
                account,
                target_value,
                &Orchard,
                target_height,
                confirmations_policy,
                exclude,
                lock_filter,
            )?
        } else {
            Vec::new()
        };

        #[cfg(feature = "orchard")]
        let ironwood_eligible_notes = if sources.contains(&ShieldedProtocol::Ironwood) {
            self.select_spendable_notes_from_pool(
                account,
                target_value,
                &ShieldedProtocol::Ironwood,
                target_height,
                confirmations_policy,
                exclude,
                lock_filter,
            )?
        } else {
            Vec::new()
        };

        to_spendable_notes(
            &sapling_eligible_notes,
            #[cfg(feature = "orchard")]
            &orchard_eligible_notes,
            #[cfg(feature = "orchard")]
            &ironwood_eligible_notes,
            &|note| self.note_heights(note),
        )
    }

    fn select_unspent_notes(
        &self,
        account: Self::AccountId,
        sources: &[ShieldedProtocol],
        target_height: TargetHeight,
        exclude: &[Self::NoteRef],
        lock_filter: LockFilter<'_>,
    ) -> Result<ReceivedNotes<Self::NoteRef>, Self::Error> {
        // Unlike `select_spendable_notes`, this does not apply confirmation, dust, or anchor
        // constraints: it returns every note the wallet knows to be unspent.
        let unspent = |pool: ShieldedProtocol| -> Result<Vec<&crate::ReceivedNote>, Error> {
            let mut notes = Vec::new();
            if !sources.contains(&pool) {
                return Ok(notes);
            }
            for note in self.received_notes.iter() {
                if note.account_id == account
                    && note.note.pool() == pool
                    && !exclude.contains(&note.note_id)
                    && !self.note_is_spent(note, target_height)?
                    && self.output_lock_admits(&note_output_ref(note), target_height, lock_filter)
                {
                    notes.push(note);
                }
            }
            notes.sort_by_key(|n| n.commitment_tree_position);
            Ok(notes)
        };

        let sapling_notes = unspent(Sapling)?;
        #[cfg(feature = "orchard")]
        let orchard_notes = unspent(Orchard)?;
        #[cfg(feature = "orchard")]
        let ironwood_notes = unspent(ShieldedProtocol::Ironwood)?;

        to_spendable_notes(
            &sapling_notes,
            #[cfg(feature = "orchard")]
            &orchard_notes,
            #[cfg(feature = "orchard")]
            &ironwood_notes,
            &|note| self.note_heights(note),
        )
    }

    fn select_single_spendable_note(
        &self,
        account: Self::AccountId,
        value: Zatoshis,
        sources: &[ShieldedProtocol],
        target_height: TargetHeight,
        confirmations_policy: ConfirmationsPolicy,
        exclude: &[Self::NoteRef],
        lock_filter: LockFilter<'_>,
    ) -> Result<ReceivedNotes<Self::NoteRef>, Self::Error> {
        // Returns the oldest eligible note, within the best lock tier, whose value alone covers
        // `value`, drawn from the first pool in `sources` (in the given preference order) that
        // holds one.
        for pool in sources {
            // Selecting with `AllFunds` yields every eligible note in the pool, in selection order.
            let candidates = self.select_spendable_notes_from_pool(
                account,
                TargetValue::AllFunds(zcash_client_backend::data_api::MaxSpendMode::MaxSpendable),
                pool,
                target_height,
                confirmations_policy,
                exclude,
                lock_filter,
            )?;
            if let Some(note) = candidates.into_iter().find(|n| n.note.value() >= value) {
                let single = [note];
                let heights = |note: &crate::ReceivedNote| self.note_heights(note);
                return match *pool {
                    Sapling => to_spendable_notes(
                        &single,
                        #[cfg(feature = "orchard")]
                        &[],
                        #[cfg(feature = "orchard")]
                        &[],
                        &heights,
                    ),
                    #[cfg(feature = "orchard")]
                    Orchard => to_spendable_notes(&[], &single, &[], &heights),
                    #[cfg(feature = "orchard")]
                    ShieldedProtocol::Ironwood => to_spendable_notes(&[], &[], &single, &heights),
                    #[cfg(not(feature = "orchard"))]
                    _ => Ok(ReceivedNotes::empty()),
                };
            }
        }
        Ok(ReceivedNotes::empty())
    }

    fn anchor_computable(
        &self,
        protocol: ShieldedProtocol,
        height: BlockHeight,
    ) -> Result<bool, Self::Error> {
        // An anchor is computable exactly at the heights whose note commitment tree checkpoints
        // the wallet retains.
        match protocol {
            Sapling => Ok(self
                .sapling_tree
                .store()
                .get_checkpoint(&height)
                .map_err(|e: std::convert::Infallible| -> Error { match e {} })?
                .is_some()),
            #[cfg(feature = "orchard")]
            Orchard => Ok(self
                .orchard_tree
                .store()
                .get_checkpoint(&height)
                .map_err(|e: std::convert::Infallible| -> Error { match e {} })?
                .is_some()),
            #[cfg(feature = "orchard")]
            ShieldedProtocol::Ironwood => Ok(self
                .ironwood_tree
                .store()
                .get_checkpoint(&height)
                .map_err(|e: std::convert::Infallible| -> Error { match e {} })?
                .is_some()),
            #[cfg(not(feature = "orchard"))]
            _ => Ok(false),
        }
    }

    #[cfg(feature = "transparent-inputs")]
    fn get_spendable_transparent_outputs(
        &self,
        address: &TransparentAddress,
        target_height: TargetHeight,
        confirmations_policy: ConfirmationsPolicy,
        output_filter: CoinbaseFilter,
        lock_filter: LockFilter<'_>,
    ) -> Result<Vec<WalletTransparentOutput<AccountId>>, Self::Error> {
        // TODO: take into consideration coinbase maturity
        // See <https://github.com/zcash/librustzcash/issues/821>
        let txos = self
            .transparent_received_outputs
            .iter()
            .filter(|(_, txo)| txo.address == *address)
            .map(|(outpoint, txo)| (outpoint, txo, self.tx_table.get(&txo.transaction_id)))
            .filter(|(outpoint, _, _)| {
                self.utxo_is_spendable(outpoint, target_height, confirmations_policy)
                    .unwrap_or(false)
                    && self.output_lock_admits(
                        &utxo_output_ref(outpoint),
                        target_height,
                        lock_filter,
                    )
            })
            .filter(|(_, _, tx)| match output_filter {
                CoinbaseFilter::AllTransparentOutputs => true,
                // Coinbase transactions have tx_index == 0. If tx_index is unknown
                // (None), conservatively treat it as **non-coinbase**, matching the SQL
                // implementation's IFNULL(tx_index, 1) == 0 behavior.
                CoinbaseFilter::CoinbaseOnly => {
                    tx.is_some_and(|tx| tx.tx_index().unwrap_or(1) == 0)
                }
                // Non-coinbase outputs only. Outputs whose tx_index is unknown are treated as
                // non-coinbase and included, matching the SQL implementation's IFNULL(tx_index, 1)
                // != 0 behavior.
                CoinbaseFilter::NonCoinbaseOnly => {
                    tx.map_or(true, |tx| tx.tx_index().unwrap_or(1) != 0)
                }
            })
            .filter_map(|(outpoint, txo, tx)| {
                txo.to_wallet_transparent_output(
                    outpoint,
                    tx.and_then(|tx| tx.mined_height()),
                    self.find_funding_account(&txo.transaction_id),
                )
            })
            .collect();
        Ok(txos)
    }

    #[cfg(feature = "transparent-inputs")]
    fn get_unspent_transparent_output(
        &self,
        outpoint: &OutPoint,
        _target_height: TargetHeight,
    ) -> Result<Option<WalletTransparentOutput<AccountId>>, Self::Error> {
        // FIXME: make use of `target_height` to check spendability.
        Ok(self
            .transparent_received_outputs
            .get(outpoint)
            .map(|txo| (txo, self.tx_table.get(&txo.transaction_id)))
            .and_then(|(txo, tx)| {
                txo.to_wallet_transparent_output(
                    outpoint,
                    tx.and_then(|tx| tx.mined_height()),
                    self.find_funding_account(&txo.transaction_id),
                )
            }))
    }

    fn get_account_metadata(
        &self,
        account_id: Self::AccountId,
        selector: &NoteFilter,
        target_height: TargetHeight,
        exclude: &[Self::NoteRef],
        lock_filter: LockFilter<'_>,
    ) -> Result<AccountMeta, Self::Error> {
        let confirmations_policy = ConfirmationsPolicy::new_symmetrical(
            NonZeroU32::MIN,
            #[cfg(feature = "transparent-inputs")]
            true,
        );

        let sapling_pool_meta = self.spendable_notes_meta(
            ShieldedProtocol::Sapling,
            account_id,
            selector,
            target_height,
            confirmations_policy,
            exclude,
            lock_filter,
        )?;

        #[cfg(feature = "orchard")]
        let orchard_pool_meta = self.spendable_notes_meta(
            ShieldedProtocol::Orchard,
            account_id,
            selector,
            target_height,
            confirmations_policy,
            exclude,
            lock_filter,
        )?;
        #[cfg(not(feature = "orchard"))]
        let orchard_pool_meta = None;

        // Ironwood is a distinct pool tracked separately from Orchard; mirror the Orchard
        // metadata query against the Ironwood value pool.
        #[cfg(feature = "orchard")]
        let ironwood_pool_meta = self.spendable_notes_meta(
            ShieldedProtocol::Ironwood,
            account_id,
            selector,
            target_height,
            confirmations_policy,
            exclude,
            lock_filter,
        )?;
        #[cfg(not(feature = "orchard"))]
        let ironwood_pool_meta = None;

        Ok(AccountMeta::new(
            sapling_pool_meta,
            orchard_pool_meta,
            ironwood_pool_meta,
        ))
    }
}

impl<P: consensus::Parameters> MemoryWalletDb<P> {
    /// Returns the height at which the transaction that produced `note` was mined, and the
    /// maximum height among the transparent inputs to that transaction. These feed the effective
    /// confirmations computation for externally-received value (see ZIP 315).
    pub(crate) fn note_heights(
        &self,
        note: &crate::ReceivedNote,
    ) -> crate::types::notes::received::NoteHeights {
        let tx = self.tx_table.get_transaction(&note.txid);
        let mined_height = tx.and_then(|tx| tx.mined_height());
        // Find the maximum height among transparent inputs to the transaction that produced this
        // note.
        let max_shielding_input_height = tx.and_then(|tx| {
            tx.raw()
                .and_then(|raw| {
                    // The branch id here is irrelevant; it does not affect any APIs that we end up
                    // using here.
                    Transaction::read(raw, BranchId::Sapling).ok()
                })
                .and_then(|tx| {
                    tx.transparent_bundle()
                        .iter()
                        .flat_map(|b| b.vin.iter())
                        .filter_map(|txin| {
                            self.tx_table
                                .get_transaction(txin.prevout().txid())
                                .and_then(|input_tx| input_tx.mined_height())
                        })
                        .max()
                })
        });
        (mined_height, max_shielding_input_height)
    }

    /// The selection tier of an output under `lock_filter`; outputs in lower tiers are selected
    /// first. Only outputs that `output_lock_admits` are ever tiered.
    ///
    /// Under `LockedInputPolicy::PreferUnlocked`, and by default, currently-unlocked outputs come
    /// first; under `PreferLocked`, outputs held by an admitted lock owner come first.
    fn lock_tier(
        &self,
        output: &OutputRef,
        target_height: TargetHeight,
        lock_filter: LockFilter<'_>,
    ) -> u8 {
        let locked = self.output_is_locked(output, target_height);
        match lock_filter {
            LockFilter::Policy(policy) if policy.prefers_locked() => u8::from(!locked),
            _ => u8::from(locked),
        }
    }

    // Select the spendable notes to cover the given target value considering only a single pool
    // Returns the notes sorted by lock tier, then oldest to newest
    #[allow(clippy::too_many_arguments)]
    fn select_spendable_notes_from_pool(
        &self,
        account: AccountId,
        target_value: TargetValue,
        pool: &ShieldedProtocol,
        target_height: TargetHeight,
        confirmations_policy: ConfirmationsPolicy,
        exclude: &[NoteId],
        lock_filter: LockFilter<'_>,
    ) -> Result<Vec<&crate::ReceivedNote>, Error> {
        let birthday_height = match self.get_wallet_birthday()? {
            Some(birthday) => birthday,
            None => {
                // the wallet birthday can only be unknown if there are no accounts in the wallet; in
                // such a case, the wallet has no notes to spend.
                return Ok(Vec::new());
            }
        };
        // First grab all eligible (unspent, spendable, fully scanned) notes into a vec.
        let mut eligible_notes = self
            .received_notes
            .iter()
            .filter(|note| note.account_id == account)
            .filter(|note| note.note.pool() == *pool)
            .filter(|note| {
                self.output_lock_admits(&note_output_ref(note), target_height, lock_filter)
            })
            .filter(|note| {
                self.note_is_spendable(
                    note,
                    birthday_height,
                    target_height,
                    confirmations_policy,
                    exclude,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        // sort by lock tier (see `lock_tier`), then oldest first (use location in commitment tree
        // since this gives a total order)
        eligible_notes.sort_by_key(|note| {
            (
                self.lock_tier(&note_output_ref(note), target_height, lock_filter),
                note.commitment_tree_position,
            )
        });

        // now take notes until we have enough to cover the target value
        let mut value_acc = Zatoshis::ZERO;
        let selection: Vec<_> = eligible_notes
            .into_iter()
            .take_while(|note| {
                let take = match target_value {
                    TargetValue::AtLeast(target) => value_acc <= target,
                    // Spending all funds selects every eligible note.
                    TargetValue::AllFunds(_) => true,
                };
                value_acc = (value_acc + note.note.value()).expect("value overflow");
                take
            })
            .collect();

        Ok(selection)
    }

    #[cfg(feature = "transparent-inputs")]
    pub(crate) fn utxo_is_spendable(
        &self,
        outpoint: &OutPoint,
        target_height: TargetHeight,
        confirmations_policy: ConfirmationsPolicy,
    ) -> Result<bool, Error> {
        let confirmed_height =
            BlockHeight::from(target_height) - u32::from(confirmations_policy.trusted());
        let utxo = self
            .transparent_received_outputs
            .get(outpoint)
            .ok_or(Error::NoteNotFound)?;
        if let Some(tx) = self.tx_table.get(&utxo.transaction_id) {
            Ok(
                tx.is_mined_or_unexpired_at(confirmed_height) // tx that created it is mined
                && !self.utxo_is_spent(outpoint, target_height)?, // not spent
            )
        } else {
            Ok(false)
        }
    }

    #[cfg(feature = "transparent-inputs")]
    fn utxo_is_spent(
        &self,
        outpoint: &OutPoint,
        target_height: TargetHeight,
    ) -> Result<bool, Error> {
        let spend = self.transparent_received_output_spends.get(outpoint);

        let spent = match spend {
            Some(txid) => {
                let spending_tx = self
                    .tx_table
                    .get(txid)
                    .ok_or(Error::TransactionNotFound(*txid))?;
                match spending_tx.status() {
                    TransactionStatus::Mined(_height) => true,
                    TransactionStatus::TxidNotRecognized => unreachable!(),
                    TransactionStatus::NotInMainChain => {
                        // transaction either never expires, or expires in the future.
                        spending_tx
                            .expiry_height()
                            .iter()
                            .all(|h| *h >= BlockHeight::from(target_height))
                    }
                }
            }
            None => false,
        };
        Ok(spent)
    }

    #[allow(clippy::too_many_arguments)]
    fn spendable_notes_meta(
        &self,
        protocol: ShieldedProtocol,
        account: AccountId,
        filter: &NoteFilter,
        target_height: TargetHeight,
        confirmations_policy: ConfirmationsPolicy,
        exclude: &[NoteId],
        lock_filter: LockFilter<'_>,
    ) -> Result<Option<PoolMeta>, Error> {
        let birthday_height = match self.get_wallet_birthday()? {
            Some(birthday) => birthday,
            None => {
                return Ok(None);
            }
        };
        let (count, total) = self
            .received_notes
            .iter()
            .filter(|note| note.account_id == account)
            .filter(|note| note.note.pool() == protocol)
            .filter(|note| {
                self.output_lock_admits(&note_output_ref(note), target_height, lock_filter)
            })
            .filter(|note| {
                self.note_is_spendable(
                    note,
                    birthday_height,
                    target_height,
                    confirmations_policy,
                    exclude,
                )
                .expect("note account & transaction are known to the wallet")
            })
            .filter(|note| {
                self.matches_note_filter(note, filter)
                    .unwrap()
                    .is_some_and(|b| b)
            })
            .fold((0, Zatoshis::ZERO), |(count, total), note| {
                (count + 1, (total + note.note.value()).unwrap())
            });

        Ok(Some(PoolMeta::new(count, total)))
    }

    #[allow(clippy::only_used_in_recursion)]
    fn matches_note_filter(
        &self,
        note: &crate::ReceivedNote,
        filter: &NoteFilter,
    ) -> Result<Option<bool>, Error> {
        match filter {
            NoteFilter::ExceedsMinValue(min_value) => Ok(Some(note.note.value() > *min_value)),
            NoteFilter::ExceedsPriorSendPercentile(_n) => todo!(),
            NoteFilter::ExceedsBalancePercentage(_p) => todo!(),
            // evaluate both conditions.
            // If one cannot be evaluated (e.g. it returns None) it is ignored
            NoteFilter::Combine(a, b) => {
                let matches_a = self.matches_note_filter(note, a)?.unwrap_or(true);
                let matches_b = self.matches_note_filter(note, b)?.unwrap_or(true);
                Ok(Some(matches_a && matches_b))
            }
            // Evaluate the first condition and return the result.
            // If the first condition cannot be evaluated then use the fallback instead
            NoteFilter::Attempt {
                condition,
                fallback,
            } => {
                if let Some(b) = self.matches_note_filter(note, condition)? {
                    Ok(Some(b))
                } else {
                    self.matches_note_filter(note, fallback)
                }
            }
        }
    }
}
