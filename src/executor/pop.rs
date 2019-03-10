// Sonic
//
// Fast, lightweight and schema-less search backend
// Copyright: 2019, Valerian Saliou <valerian@valeriansaliou.name>
// License: Mozilla Public License v2.0 (MPL v2.0)

use linked_hash_set::LinkedHashSet;
use std::iter::FromIterator;

use crate::lexer::token::TokenLexer;
use crate::store::identifiers::StoreTermHashed;
use crate::store::item::StoreItem;
use crate::store::kv::{StoreKVActionBuilder, StoreKVPool, STORE_ACCESS_LOCK};

pub struct ExecutorPop;

impl ExecutorPop {
    pub fn execute<'a>(store: StoreItem<'a>, lexer: TokenLexer<'a>) -> Result<u64, ()> {
        if let StoreItem(collection, Some(bucket), Some(object)) = store {
            // Important: acquire database access read lock, and reference it in context. This \
            //   prevents the database from being erased while using it in this block.
            let _access = STORE_ACCESS_LOCK.read().unwrap();

            if let Ok(kv_store) = StoreKVPool::acquire(collection) {
                let action = StoreKVActionBuilder::write(bucket, kv_store);

                // Try to resolve existing OID to IID (if it does not exist, there is nothing to \
                //   be flushed)
                let oid = object.as_str().to_owned();

                if let Ok(iid_value) = action.get_oid_to_iid(&oid) {
                    let mut count_popped = 0;

                    if let Some(iid) = iid_value {
                        // Try to resolve existing search terms from IID, and perform an algebraic \
                        //   AND on all popped terms to generate a list of terms to be cleaned up.
                        if let Ok(Some(iid_terms_hashed_vec)) = action.get_iid_to_terms(iid) {
                            info!(
                                "got pop executor stored iid-to-terms: {:?}",
                                iid_terms_hashed_vec
                            );

                            let iid_terms_hashed: LinkedHashSet<StoreTermHashed> =
                                LinkedHashSet::from_iter(
                                    iid_terms_hashed_vec.iter().map(|value| *value),
                                );
                            let pop_terms_hashed: LinkedHashSet<StoreTermHashed> =
                                LinkedHashSet::from_iter(lexer.map(|item| item.1));

                            let remaining_terms: LinkedHashSet<StoreTermHashed> = iid_terms_hashed
                                .difference(&pop_terms_hashed)
                                .map(|value| *value)
                                .collect();

                            debug!(
                                "got pop executor terms remaining terms: {:?} for iid: {}",
                                remaining_terms, iid
                            );

                            count_popped = (iid_terms_hashed.len() - remaining_terms.len()) as u64;

                            if count_popped > 0 {
                                if remaining_terms.len() == 0 {
                                    info!("nuke whole bucket for pop executor");

                                    // Flush bucket (batch operation, as it is shared w/ other \
                                    //   executors)
                                    action
                                        .batch_flush_bucket(iid, &oid, &iid_terms_hashed_vec)
                                        .ok();
                                } else {
                                    info!("nuke only certain terms for pop executor");

                                    // Nuke IID in Term-to-IIDs list
                                    for pop_term_hashed in &pop_terms_hashed {
                                        if iid_terms_hashed.contains(pop_term_hashed) == true {
                                            if let Ok(Some(mut pop_term_iids)) =
                                                action.get_term_to_iids(*pop_term_hashed)
                                            {
                                                // Remove IID from list of IIDs to be popped
                                                pop_term_iids.retain(|cur_iid| cur_iid != &iid);

                                                if pop_term_iids.is_empty() == true {
                                                    // IIDs list was empty, delete whole key
                                                    action
                                                        .delete_term_to_iids(*pop_term_hashed)
                                                        .ok();
                                                } else {
                                                    // Re-build IIDs list w/o current IID
                                                    action
                                                        .set_term_to_iids(
                                                            *pop_term_hashed,
                                                            &pop_term_iids,
                                                        )
                                                        .ok();
                                                }
                                            }
                                        }
                                    }

                                    // Bump IID-to-Terms list
                                    let remaining_terms_vec: Vec<StoreTermHashed> =
                                        Vec::from_iter(remaining_terms.into_iter());

                                    action.set_iid_to_terms(iid, &remaining_terms_vec).ok();
                                }
                            }
                        }
                    }

                    return Ok(count_popped);
                }
            }
        }

        Err(())
    }
}