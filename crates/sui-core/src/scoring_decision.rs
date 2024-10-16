// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
use crate::authority::AuthorityMetrics;
use arc_swap::ArcSwap;
use narwhal_config::{Authority, Committee, Stake};
use narwhal_types::ReputationScores;
use std::collections::HashMap;
use std::sync::Arc;
use sui_types::base_types::AuthorityName;
use tracing::debug;

/// Updates list of authorities that are deemed to have low reputation scores by consensus
/// these may be lagging behind the network, byzantine, or not reliably participating for any reason.
/// The algorithm is flagging as low scoring authorities all the validators that have the lowest scores
/// up to the defined protocol_config.consensus_bad_nodes_stake_threshold. This is done to align the
/// submission side with the Narwhal leader election schedule. Practically we don't want to submit
/// transactions for sequencing to validators that have low scores and are not part of the leader
/// schedule since the chances of getting them sequenced are lower.
pub fn update_low_scoring_authorities(
    low_scoring_authorities: Arc<ArcSwap<HashMap<AuthorityName, u64>>>,
    committee: &Committee,
    reputation_scores: ReputationScores,
    metrics: &Arc<AuthorityMetrics>,
    consensus_bad_nodes_stake_threshold: u64,
) {
    assert!((0..=33).contains(&consensus_bad_nodes_stake_threshold), "The bad_nodes_stake_threshold should be in range [0 - 33], out of bounds parameter detected {}", consensus_bad_nodes_stake_threshold);

    if !reputation_scores.final_of_schedule {
        return;
    }

    // We order the authorities by score ascending order in the exact same way as the reputation
    // scores do - so we keep complete alignment between implementations
    let scores_per_authority_order_asc: Vec<(AuthorityName, u64, &Authority)> = reputation_scores
        .authorities_by_score_desc()
        .iter()
        .rev() // we reverse so we get them in asc order
        .map(|(authority_id, score)| {
            let authority = committee.authority(authority_id).unwrap();
            let name: AuthorityName = authority.protocol_key().into();

            (name, *score, authority)
        })
        .collect();

    let mut final_low_scoring_map = HashMap::new();
    let mut total_stake = 0;
    for (authority_name, score, authority) in scores_per_authority_order_asc {
        total_stake += authority.stake();

        let included = if total_stake
            <= (consensus_bad_nodes_stake_threshold * committee.total_stake()) / 100 as Stake
        {
            final_low_scoring_map.insert(authority_name, score);
            true
        } else {
            false
        };

        if !authority.hostname().is_empty() {
            debug!(
                "authority {} has score {}, is low scoring: {}",
                authority.hostname(),
                score,
                included
            );

            metrics
                .consensus_handler_scores
                .with_label_values(&[authority.hostname()])
                .set(score as i64);
        }
    }
    // Report the actual flagged final low scoring authorities
    metrics
        .consensus_handler_num_low_scoring_authorities
        .set(final_low_scoring_map.len() as i64);
    low_scoring_authorities.swap(Arc::new(final_low_scoring_map));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::mutable_key_type)]
    use crate::authority::AuthorityMetrics;
    use crate::scoring_decision::update_low_scoring_authorities;
    use arc_swap::ArcSwap;
    use fastcrypto::traits::{InsecureDefault, KeyPair as _};
    use mysten_network::Multiaddr;
    use narwhal_config::Committee;
    use narwhal_config::CommitteeBuilder;
    use narwhal_crypto::KeyPair;
    use narwhal_crypto::NetworkPublicKey;
    use narwhal_types::ReputationScores;
    use prometheus::Registry;
    use rand::rngs::{OsRng, StdRng};
    use rand::SeedableRng;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    pub fn test_update_low_scoring_authorities() {
        // GIVEN
        // Total stake is 8 for this committee and every authority has equal stake = 1
        let committee = generate_committee(8);

        let mut authorities = committee.authorities();
        let a1 = authorities.next().unwrap();
        let a2 = authorities.next().unwrap();
        let a3 = authorities.next().unwrap();
        let a4 = authorities.next().unwrap();
        let a5 = authorities.next().unwrap();
        let a6 = authorities.next().unwrap();
        let a7 = authorities.next().unwrap();
        let a8 = authorities.next().unwrap();

        let low_scoring = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        let metrics = Arc::new(AuthorityMetrics::new(&Registry::new()));

        // there is a low outlier in the non zero scores, exclude it as well as down nodes
        let mut scores = HashMap::new();
        scores.insert(a1.id(), 350_u64);
        scores.insert(a2.id(), 390_u64);
        scores.insert(a3.id(), 50_u64);
        scores.insert(a4.id(), 50_u64);
        scores.insert(a5.id(), 0_u64); // down node
        scores.insert(a6.id(), 300_u64);
        scores.insert(a7.id(), 340_u64);
        scores.insert(a8.id(), 310_u64);
        let reputation_scores = ReputationScores {
            scores_per_authority: scores,
            final_of_schedule: true,
        };

        // WHEN
        let consensus_bad_nodes_stake_threshold = 33; // 33 * 8 / 100 = 2 maximum stake that will considered low scoring

        update_low_scoring_authorities(
            low_scoring.clone(),
            &committee,
            reputation_scores.clone(),
            &metrics,
            consensus_bad_nodes_stake_threshold,
        );

        // THEN
        assert_eq!(low_scoring.load().len(), 2);
        println!("low scoring {:?}", low_scoring.load());
        assert_eq!(
            *low_scoring.load().get(&a3.protocol_key().into()).unwrap(), // Since a3 & a4 have equal scores, we resolve the decision with a3.id < a4.id
            50
        );
        assert_eq!(
            *low_scoring.load().get(&a5.protocol_key().into()).unwrap(),
            0
        );

        // WHEN setting the threshold to lower
        let consensus_bad_nodes_stake_threshold = 20; // 20 * 8 / 100 = 1 maximum
        update_low_scoring_authorities(
            low_scoring.clone(),
            &committee,
            reputation_scores,
            &metrics,
            consensus_bad_nodes_stake_threshold,
        );

        // THEN
        assert_eq!(low_scoring.load().len(), 1);
        assert_eq!(
            *low_scoring.load().get(&a5.protocol_key().into()).unwrap(),
            0
        );
    }

    /// Generate a random committee for the given size. It's important to create the Authorities
    /// via the committee to ensure than an AuthorityIdentifier will be assigned, as this is dynamically
    /// calculated during committee creation.
    fn generate_committee(committee_size: usize) -> Arc<Committee> {
        let mut committee_builder = CommitteeBuilder::new(0);
        let mut rng = StdRng::from_rng(&mut OsRng).unwrap();

        for i in 0..committee_size {
            let pair = KeyPair::generate(&mut rng);
            let public_key = pair.public().clone();

            committee_builder = committee_builder.add_authority(
                public_key.clone(),
                1,
                Multiaddr::empty(),
                NetworkPublicKey::insecure_default(),
                i.to_string(),
            );
        }

        Arc::new(committee_builder.build())
    }
}
