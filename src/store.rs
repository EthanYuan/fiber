use crate::{
    fiber::{
        channel::{ChannelActorState, ChannelActorStateStore, ChannelState},
        graph::{ChannelInfo, NetworkGraphStateStore, NodeInfo, PaymentSession},
        types::{Hash256, Pubkey},
    },
    invoice::{CkbInvoice, InvoiceError, InvoiceStore},
};
use ckb_types::packed::OutPoint;
use ckb_types::prelude::Entity;
use rocksdb::{prelude::*, DBIterator, IteratorMode, WriteBatch, DB};
use serde_json;
use std::{path::Path, sync::Arc};
use tentacle::{multiaddr::Multiaddr, secio::PeerId};

#[derive(Clone)]
pub struct Store {
    pub(crate) db: Arc<DB>,
}

impl Store {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        let db = Arc::new(DB::open_default(path).expect("Failed to open rocksdb"));
        Self { db }
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> Option<Vec<u8>> {
        self.db
            .get(key.as_ref())
            .map(|v| v.map(|vi| vi.to_vec()))
            .expect("get should be OK")
    }

    #[allow(dead_code)]
    fn get_range<K: AsRef<[u8]>>(
        &self,
        lower_bound: Option<K>,
        upper_bound: Option<K>,
    ) -> DBIterator {
        assert!(lower_bound.is_some() || upper_bound.is_some());
        let mut read_options = ReadOptions::default();
        if let Some(lower_bound) = lower_bound {
            read_options.set_iterate_lower_bound(lower_bound.as_ref());
        }
        if let Some(upper_bound) = upper_bound {
            read_options.set_iterate_upper_bound(upper_bound.as_ref());
        }
        let mode = IteratorMode::Start;
        self.db.get_iter(&read_options, mode)
    }

    fn batch(&self) -> Batch {
        Batch {
            db: Arc::clone(&self.db),
            wb: WriteBatch::default(),
        }
    }
}

pub struct Batch {
    db: Arc<DB>,
    wb: WriteBatch,
}

impl Batch {
    fn store(&self) -> Store {
        Store {
            db: Arc::clone(&self.db),
        }
    }

    fn put_kv(&mut self, key_value: KeyValue) {
        match key_value {
            KeyValue::ChannelActorState(id, state) => {
                let key = [&[CHANNEL_ACTOR_STATE_PREFIX], id.as_ref()].concat();
                self.put(
                    key,
                    serde_json::to_vec(&state).expect("serialize ChannelActorState should be OK"),
                );
            }
            KeyValue::CkbInvoice(id, invoice) => {
                let key = [&[CKB_INVOICE_PREFIX], id.as_ref()].concat();
                self.put(
                    key,
                    serde_json::to_vec(&invoice).expect("serialize CkbInvoice should be OK"),
                );
            }
            KeyValue::CkbInvoicePreimage(id, preimage) => {
                let key = [&[CKB_INVOICE_PREIMAGE_PREFIX], id.as_ref()].concat();
                self.put(
                    key,
                    serde_json::to_vec(&preimage).expect("serialize Hash256 should be OK"),
                );
            }
            KeyValue::PeerIdChannelId((peer_id, channel_id), state) => {
                let key = [
                    &[PEER_ID_CHANNEL_ID_PREFIX],
                    peer_id.as_bytes(),
                    channel_id.as_ref(),
                ]
                .concat();
                self.put(
                    key,
                    serde_json::to_vec(&state).expect("serialize ChannelState should be OK"),
                );
            }
            KeyValue::ChannelInfo(channel_id, channel) => {
                // Save channel update timestamp to index, so that we can query channels by timestamp
                self.put(
                    [
                        CHANNEL_UPDATE_INDEX_PREFIX.to_be_bytes().as_slice(),
                        channel.timestamp.to_be_bytes().as_slice(),
                    ]
                    .concat(),
                    channel_id.as_slice(),
                );

                // Save channel announcement block numbers to index, so that we can query channels by block number
                self.put(
                    [
                        CHANNEL_ANNOUNCEMENT_INDEX_PREFIX.to_be_bytes().as_slice(),
                        channel.funding_tx_block_number.to_be_bytes().as_slice(),
                        channel.funding_tx_index.to_be_bytes().as_slice(),
                    ]
                    .concat(),
                    channel_id.as_slice(),
                );

                let mut key = Vec::with_capacity(37);
                key.push(CHANNEL_INFO_PREFIX);
                key.extend_from_slice(channel_id.as_slice());
                self.put(
                    key,
                    serde_json::to_vec(&channel).expect("serialize ChannelInfo should be OK"),
                );
            }
            KeyValue::NodeInfo(id, node) => {
                // Save node announcement timestamp to index, so that we can query nodes by timestamp
                self.put(
                    [
                        NODE_ANNOUNCEMENT_INDEX_PREFIX.to_be_bytes().as_slice(),
                        node.timestamp.to_be_bytes().as_slice(),
                    ]
                    .concat(),
                    id.serialize(),
                );

                let mut key = Vec::with_capacity(34);
                key.push(NODE_INFO_PREFIX);
                key.extend_from_slice(id.serialize().as_ref());
                self.put(
                    key,
                    serde_json::to_vec(&node).expect("serialize NodeInfo should be OK"),
                );
            }
            KeyValue::PeerIdMultiAddr(peer_id, multiaddr) => {
                let key = [&[PEER_ID_MULTIADDR_PREFIX], peer_id.as_bytes()].concat();
                self.put(
                    key,
                    serde_json::to_vec(&multiaddr).expect("serialize Multiaddr should be OK"),
                );
            }
        }
    }

    fn put<K: AsRef<[u8]>, V: AsRef<[u8]>>(&mut self, key: K, value: V) {
        self.wb.put(key, value).expect("put should be OK")
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        self.wb.delete(key.as_ref()).expect("delete should be OK")
    }

    fn commit(self) {
        self.db.write(&self.wb).expect("commit should be OK")
    }
}

///
/// +--------------+--------------------+--------------------------+
/// | KeyPrefix::  | Key::              | Value::                  |
/// +--------------+--------------------+--------------------------+
/// | 0            | Hash256            | ChannelActorState        |
/// | 32           | Hash256            | CkbInvoice               |
/// | 64           | PeerId | Hash256   | ChannelState             |
/// | 96           | ChannelId          | ChannelInfo              |
/// | 97           | Block | Index      | ChannelId                |
/// | 98           | Timestamp          | ChannelId                |
/// | 128          | NodeId             | NodeInfo                 |
/// | 129          | Timestamp          | NodeId                   |
/// | 160          | PeerId             | MultiAddr                |
/// | 192          | Hash256            | PaymentSession           |
/// +--------------+--------------------+--------------------------+
///

const CHANNEL_ACTOR_STATE_PREFIX: u8 = 0;
const CKB_INVOICE_PREFIX: u8 = 32;
const CKB_INVOICE_PREIMAGE_PREFIX: u8 = 33;
const PEER_ID_CHANNEL_ID_PREFIX: u8 = 64;
const CHANNEL_INFO_PREFIX: u8 = 96;
const CHANNEL_ANNOUNCEMENT_INDEX_PREFIX: u8 = 97;
const CHANNEL_UPDATE_INDEX_PREFIX: u8 = 98;
const NODE_INFO_PREFIX: u8 = 128;
const NODE_ANNOUNCEMENT_INDEX_PREFIX: u8 = 129;
const PEER_ID_MULTIADDR_PREFIX: u8 = 160;
const PAYMENT_SESSION_PREFIX: u8 = 192;

enum KeyValue {
    ChannelActorState(Hash256, ChannelActorState),
    CkbInvoice(Hash256, CkbInvoice),
    CkbInvoicePreimage(Hash256, Hash256),
    PeerIdChannelId((PeerId, Hash256), ChannelState),
    PeerIdMultiAddr(PeerId, Multiaddr),
    NodeInfo(Pubkey, NodeInfo),
    ChannelInfo(OutPoint, ChannelInfo),
}

impl ChannelActorStateStore for Store {
    fn get_channel_actor_state(&self, id: &Hash256) -> Option<ChannelActorState> {
        let mut key = Vec::with_capacity(33);
        key.extend_from_slice(&[CHANNEL_ACTOR_STATE_PREFIX]);
        key.extend_from_slice(id.as_ref());

        self.get(key).map(|v| {
            serde_json::from_slice(v.as_ref()).expect("deserialize ChannelActorState should be OK")
        })
    }

    fn insert_channel_actor_state(&self, state: ChannelActorState) {
        let mut batch = self.batch();
        batch.put_kv(KeyValue::ChannelActorState(state.id, state.clone()));
        batch.put_kv(KeyValue::PeerIdChannelId(
            (state.get_remote_peer_id(), state.id),
            state.state,
        ));
        batch.commit();
    }

    fn delete_channel_actor_state(&self, id: &Hash256) {
        if let Some(state) = self.get_channel_actor_state(id) {
            let mut batch = self.batch();
            batch.delete([&[CHANNEL_ACTOR_STATE_PREFIX], id.as_ref()].concat());
            batch.delete(
                [
                    &[PEER_ID_CHANNEL_ID_PREFIX],
                    state.get_remote_peer_id().as_bytes(),
                    id.as_ref(),
                ]
                .concat(),
            );
            batch.commit();
        }
    }

    fn get_channel_ids_by_peer(&self, peer_id: &tentacle::secio::PeerId) -> Vec<Hash256> {
        let prefix = [&[PEER_ID_CHANNEL_ID_PREFIX], peer_id.as_bytes()].concat();
        let iter = self
            .db
            .prefix_iterator(prefix.as_ref())
            .take_while(|(key, _)| key.starts_with(&prefix));
        iter.map(|(key, _)| {
            let channel_id: [u8; 32] = key[prefix.len()..]
                .try_into()
                .expect("channel id should be 32 bytes");
            channel_id.into()
        })
        .collect()
    }

    fn get_channel_states(&self, peer_id: Option<PeerId>) -> Vec<(PeerId, Hash256, ChannelState)> {
        let prefix = match peer_id {
            Some(peer_id) => [&[PEER_ID_CHANNEL_ID_PREFIX], peer_id.as_bytes()].concat(),
            None => vec![PEER_ID_CHANNEL_ID_PREFIX],
        };
        let iter = self
            .db
            .prefix_iterator(prefix.as_ref())
            .take_while(|(key, _)| key.starts_with(&prefix));
        iter.map(|(key, value)| {
            let key_len = key.len();
            let peer_id = PeerId::from_bytes(key[1..key_len - 32].into())
                .expect("deserialize peer id should be OK");
            let channel_id: [u8; 32] = key[key_len - 32..]
                .try_into()
                .expect("channel id should be 32 bytes");
            let state = serde_json::from_slice(value.as_ref())
                .expect("deserialize ChannelState should be OK");
            (peer_id, channel_id.into(), state)
        })
        .collect()
    }
}

impl InvoiceStore for Store {
    fn get_invoice(&self, id: &Hash256) -> Option<CkbInvoice> {
        let mut key = Vec::with_capacity(33);
        key.extend_from_slice(&[CKB_INVOICE_PREFIX]);
        key.extend_from_slice(id.as_ref());

        self.get(key).map(|v| {
            serde_json::from_slice(v.as_ref()).expect("deserialize CkbInvoice should be OK")
        })
    }

    fn insert_invoice(
        &self,
        invoice: CkbInvoice,
        preimage: Option<Hash256>,
    ) -> Result<(), InvoiceError> {
        let mut batch = self.batch();
        let hash = invoice.payment_hash();
        if self.get_invoice(hash).is_some() {
            return Err(InvoiceError::DuplicatedInvoice(hash.to_string()));
        }
        if let Some(preimage) = preimage {
            batch.put_kv(KeyValue::CkbInvoicePreimage(*hash, preimage));
        }
        batch.put_kv(KeyValue::CkbInvoice(*invoice.payment_hash(), invoice));
        batch.commit();
        return Ok(());
    }

    fn get_invoice_preimage(&self, id: &Hash256) -> Option<Hash256> {
        let mut key = Vec::with_capacity(33);
        key.extend_from_slice(&[CKB_INVOICE_PREIMAGE_PREFIX]);
        key.extend_from_slice(id.as_ref());

        self.get(key)
            .map(|v| serde_json::from_slice(v.as_ref()).expect("deserialize Hash256 should be OK"))
    }
}

impl NetworkGraphStateStore for Store {
    fn get_channels(&self, channel_id: Option<OutPoint>) -> Vec<ChannelInfo> {
        let key = match channel_id.clone() {
            Some(channel_id) => {
                let mut key = Vec::with_capacity(37);
                key.extend_from_slice(&[CHANNEL_INFO_PREFIX]);
                key.extend_from_slice(channel_id.as_slice());
                key
            }
            None => vec![CHANNEL_INFO_PREFIX],
        };

        let iter = self
            .db
            .prefix_iterator(key.as_ref())
            .take_while(|(col_key, _)| col_key.starts_with(&key));
        iter.map(|(_key, value)| {
            serde_json::from_slice(value.as_ref()).expect("deserialize ChannelInfo should be OK")
        })
        .collect()
    }

    fn get_nodes(&self, node_id: Option<Pubkey>) -> Vec<NodeInfo> {
        let key = match node_id {
            Some(node_id) => {
                let mut key = Vec::with_capacity(34);
                key.extend_from_slice(&[NODE_INFO_PREFIX]);
                key.extend_from_slice(node_id.serialize().as_ref());
                key
            }
            None => vec![NODE_INFO_PREFIX],
        };
        let iter = self
            .db
            .prefix_iterator(key.as_ref())
            .take_while(|(col_key, _)| col_key.starts_with(&key));
        iter.map(|(_col_key, value)| {
            serde_json::from_slice(value.as_ref()).expect("deserialize NodeInfo should be OK")
        })
        .collect()
    }

    fn get_connected_peer(&self, peer_id: Option<PeerId>) -> Vec<(PeerId, Multiaddr)> {
        let key = match peer_id {
            Some(peer_id) => {
                let mut key = Vec::with_capacity(33);
                key.push(PEER_ID_MULTIADDR_PREFIX);
                key.extend_from_slice(peer_id.as_bytes());
                key
            }
            None => vec![PEER_ID_MULTIADDR_PREFIX],
        };
        let iter = self
            .db
            .prefix_iterator(key.as_ref())
            .take_while(|(col_key, _)| col_key.starts_with(&key));
        iter.map(|(key, value)| {
            let peer_id =
                PeerId::from_bytes(key[1..].into()).expect("deserialize peer id should be OK");
            let addr =
                serde_json::from_slice(value.as_ref()).expect("deserialize Multiaddr should be OK");
            (peer_id, addr)
        })
        .collect()
    }

    fn insert_channel(&self, channel: ChannelInfo) {
        let mut batch = self.batch();
        batch.put_kv(KeyValue::ChannelInfo(channel.out_point(), channel.clone()));
        batch.commit();
    }

    fn insert_node(&self, node: NodeInfo) {
        let mut batch = self.batch();
        batch.put_kv(KeyValue::NodeInfo(node.node_id, node.clone()));
        batch.commit();
    }

    fn insert_connected_peer(&self, peer_id: PeerId, multiaddr: Multiaddr) {
        let mut batch = self.batch();
        batch.put_kv(KeyValue::PeerIdMultiAddr(peer_id, multiaddr));
        batch.commit();
    }

    fn remove_connected_peer(&self, peer_id: &PeerId) {
        let prefix = [&[PEER_ID_MULTIADDR_PREFIX], peer_id.as_bytes()].concat();
        let iter = self
            .db
            .prefix_iterator(prefix.as_ref())
            .take_while(|(key, _)| key.starts_with(&prefix));
        for (key, _) in iter {
            self.db.delete(key).expect("delete should be OK");
        }
    }

    fn get_payment_session(&self, payment_hash: Hash256) -> Option<PaymentSession> {
        let prefix = [&[PAYMENT_SESSION_PREFIX], payment_hash.as_ref()].concat();
        self.get(prefix).map(|v| {
            serde_json::from_slice(v.as_ref()).expect("deserialize PaymentSession should be OK")
        })
    }

    fn insert_payment_session(&self, session: PaymentSession) {
        let mut batch = self.batch();
        let key = [&[PAYMENT_SESSION_PREFIX], session.payment_hash().as_ref()].concat();
        batch.put(
            key,
            serde_json::to_vec(&session).expect("serialize PaymentSession should be OK"),
        );
        batch.commit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fiber::test_utils::gen_sha256_hash;
    use crate::invoice::*;
    use tempfile::tempdir;

    #[test]
    fn test_invoice_store() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("invoice_store");
        let store = Store::new(&path);

        let preimage = gen_sha256_hash();
        let invoice = InvoiceBuilder::new(Currency::Fibb)
            .amount(Some(1280))
            .payment_preimage(preimage)
            .fallback_address("address".to_string())
            .add_attr(Attribute::FinalHtlcTimeout(5))
            .build()
            .unwrap();
        let payment_hash = invoice.payment_hash();

        // let payment_hash = rand_sha256_hash();
        // let preimage = rand_sha256_hash();
        // let (public_key, private_key) = gen_rand_keypair();

        // let invoice = InvoiceBuilder::new(Currency::Fibb)
        //     .amount(Some(1280))
        //     .payment_hash(payment_hash)
        //     .payment_preimage(preimage)
        //     .fallback_address("address".to_string())
        //     .expiry_time(Duration::from_secs(1024))
        //     .payee_pub_key(public_key)
        //     .add_attr(Attribute::FinalHtlcTimeout(5))
        //     .add_attr(Attribute::FinalHtlcMinimumCltvExpiry(12))
        //     .add_attr(Attribute::Description("description".to_string()))
        //     .add_attr(Attribute::UdtScript(CkbScript(Script::default())))
        //     .build_with_sign(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, &private_key))
        //     .unwrap();

        // let address = invoice.to_string();

        let hash = invoice.payment_hash();
        store
            .insert_invoice(invoice.clone(), Some(preimage))
            .unwrap();
        assert_eq!(store.get_invoice(&hash), Some(invoice.clone()));
        assert_eq!(store.get_invoice_preimage(&hash), Some(preimage));

        let invalid_hash = gen_sha256_hash();
        assert_eq!(store.get_invoice_preimage(&invalid_hash), None);
    }
}
