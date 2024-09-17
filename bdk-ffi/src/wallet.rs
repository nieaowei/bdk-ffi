use crate::bitcoin::{Psbt, Transaction, TxOut};
use crate::descriptor::Descriptor;
use crate::error::{
    CalculateFeeError, CannotConnectError, CreateTxError, CreateWithPersistError,
    LoadWithPersistError, SignerError, SqliteError, TxidParseError,
};
use crate::store::Connection;
use crate::types::{AddressInfo, Balance, CanonicalTx, LocalOutput, ScriptAmount};
use crate::types::{FullScanRequestBuilder, SyncRequestBuilder, Update};

use bitcoin_ffi::Amount;
use bitcoin_ffi::FeeRate;
use bitcoin_ffi::OutPoint;
use bitcoin_ffi::Script;

use bdk_wallet::bitcoin::amount::Amount as BdkAmount;
use bdk_wallet::bitcoin::Psbt as BdkPsbt;
use bdk_wallet::bitcoin::ScriptBuf as BdkScriptBuf;
use bdk_wallet::bitcoin::{Sequence, Txid};
use bdk_wallet::rusqlite::Connection as BdkConnection;
use bdk_wallet::tx_builder::ChangeSpendPolicy;
use bdk_wallet::{PersistedWallet};
use bdk_wallet::Wallet as BdkWallet;
use bdk_wallet::bitcoin::Transaction as BdkTransaction;
use bdk_wallet::{KeychainKind, SignOptions};
use bdk_wallet::bitcoin::network::Network;

use std::borrow::BorrowMut;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use crate::testnet4::{testnet4_genesis_block, CustomNetwork};

pub struct Wallet {
    inner_mutex: Mutex<PersistedWallet<BdkConnection>>,
}

impl Wallet {
    pub fn new(
        descriptor: Arc<Descriptor>,
        change_descriptor: Arc<Descriptor>,
        network: CustomNetwork,
        connection: Arc<Connection>,
    ) -> Result<Self, CreateWithPersistError> {
        let descriptor = descriptor.to_string_with_secret();
        let change_descriptor = change_descriptor.to_string_with_secret();
        let mut binding = connection.get_store();
        let db: &mut BdkConnection = binding.borrow_mut();

        let mut create_params =
            BdkWallet::create(descriptor, change_descriptor).network(network.to_bitcoin_network());

        if network == CustomNetwork::Testnet4 {
            create_params = create_params.genesis_hash(testnet4_genesis_block().block_hash())
        }
        let wallet: PersistedWallet<BdkConnection> = create_params.create_wallet(db)?;

        Ok(Wallet {
            inner_mutex: Mutex::new(wallet),
        })
    }

    pub fn create_single(
        descriptor: Arc<Descriptor>,
        network: CustomNetwork,
        connection: Arc<Connection>,
    ) -> Result<Self, CreateWithPersistError> {
        let descriptor = descriptor.to_string_with_secret();
        let mut binding = connection.get_store();
        let db: &mut BdkConnection = binding.borrow_mut();

        let mut create_params =
            BdkWallet::create_single(descriptor).network(network.to_bitcoin_network());

        if network == CustomNetwork::Testnet4 {
            create_params = create_params.genesis_hash(testnet4_genesis_block().block_hash())
        }
        let wallet: PersistedWallet<BdkConnection> = create_params.create_wallet(db)?;

        Ok(Wallet {
            inner_mutex: Mutex::new(wallet),
        })
    }

    pub fn load(
        descriptor: Arc<Descriptor>,
        change_descriptor: Option<Arc<Descriptor>>,
        connection: Arc<Connection>,
    ) -> Result<Wallet, LoadWithPersistError> {
        let descriptor = descriptor.to_string_with_secret();
        let change_descriptor = change_descriptor.map_or(None,|e|Some(e.to_string_with_secret()));
        let mut binding = connection.get_store();
        let db: &mut BdkConnection = binding.borrow_mut();

        let wallet: PersistedWallet<BdkConnection> = BdkWallet::load()
            .descriptor(KeychainKind::External, Some(descriptor))
            .descriptor(KeychainKind::Internal, change_descriptor)
            .extract_keys()
            .load_wallet(db)?
            .ok_or(LoadWithPersistError::CouldNotLoad)?;

        Ok(Wallet {
            inner_mutex: Mutex::new(wallet),
        })
    }

    pub(crate) fn get_wallet(&self) -> MutexGuard<PersistedWallet<BdkConnection>> {
        self.inner_mutex.lock().expect("wallet")
    }

    pub fn reveal_next_address(&self, keychain_kind: KeychainKind) -> AddressInfo {
        self.get_wallet().reveal_next_address(keychain_kind).into()
    }

    pub fn reveal_addresses_to(&self, keychain_kind: KeychainKind, index: u32) -> Vec<AddressInfo> {
        self.get_wallet().reveal_addresses_to(keychain_kind, index).map(|e| e.into()).collect::<Vec<AddressInfo>>()
    }

    pub fn peek_address(&self, keychain_kind: KeychainKind, index: u32) -> AddressInfo {
        self.get_wallet().peek_address(keychain_kind, index).into()
    }


    pub fn apply_update(&self, update: Arc<Update>) -> Result<(), CannotConnectError> {
        self.get_wallet()
            .apply_update(update.0.clone())
            .map_err(CannotConnectError::from)
    }

    pub(crate) fn derivation_index(&self, keychain: KeychainKind) -> Option<u32> {
        self.get_wallet().derivation_index(keychain)
    }

    pub fn network(&self) -> Network {
        self.get_wallet().network()
    }

    pub fn balance(&self) -> Balance {
        let bdk_balance = self.get_wallet().balance();
        Balance::from(bdk_balance)
    }

    pub fn is_mine(&self, script: Arc<Script>) -> bool {
        self.get_wallet().is_mine(script.0.clone())
    }

    pub(crate) fn sign(
        &self,
        psbt: Arc<Psbt>,
        // sign_options: Option<SignOptions>,
    ) -> Result<bool, SignerError> {
        let mut psbt = psbt.0.lock().unwrap();
        self.get_wallet()
            .sign(&mut psbt, SignOptions::default())
            .map_err(SignerError::from)
    }

    pub fn sent_and_received(&self, tx: &Transaction) -> SentAndReceivedValues {
        let (sent, received) = self.get_wallet().sent_and_received(&tx.into());
        SentAndReceivedValues {
            sent: Arc::new(sent.into()),
            received: Arc::new(received.into()),
        }
    }

    pub fn transactions(&self) -> Vec<CanonicalTx> {
        self.get_wallet()
            .transactions()
            .map(|tx| tx.into())
            .collect()
    }

    pub fn get_tx(&self, txid: String) -> Result<Option<CanonicalTx>, TxidParseError> {
        let txid =
            Txid::from_str(txid.as_str()).map_err(|_| TxidParseError::InvalidTxid { txid })?;
        Ok(self.get_wallet().get_tx(txid).map(|tx| tx.into()))
    }

    pub fn get_txout(&self, outpoint: OutPoint) -> Option<TxOut> {
        self.get_wallet()
            .tx_graph()
            .get_txout(outpoint)
            .map(|txout| txout.into())
    }

    pub fn insert_tx(&self, tx: &Transaction) -> bool {
        self.get_wallet()
            .insert_tx(tx.into())
    }

    pub fn apply_unconfirmed_txs(&self, tx_and_last_seens: Vec<TransactionAndLastSeen>) {
        let txs = tx_and_last_seens.into_iter().map(|e| ((&*e.tx).into(), e.last_seen)).collect::<Vec<(BdkTransaction, u64)>>();
        self.get_wallet()
            .apply_unconfirmed_txs(txs.iter().map(|e| (&e.0, e.1)));
    }

    pub fn insert_txout(&self, outpoint: OutPoint, txout: TxOut) {
        self.get_wallet()
            .insert_txout(outpoint, txout.into())
    }

    pub fn calculate_fee(&self, tx: &Transaction) -> Result<Arc<Amount>, CalculateFeeError> {
        self.get_wallet()
            .calculate_fee(&tx.into())
            .map(Amount::from)
            .map(Arc::new)
            .map_err(|e| e.into())
    }

    pub fn calculate_fee_rate(&self, tx: &Transaction) -> Result<Arc<FeeRate>, CalculateFeeError> {
        self.get_wallet()
            .calculate_fee_rate(&tx.into())
            .map(|bdk_fee_rate| Arc::new(FeeRate(bdk_fee_rate)))
            .map_err(|e| e.into())
    }

    pub fn list_unspent(&self) -> Vec<LocalOutput> {
        self.get_wallet().list_unspent().map(|o| o.into()).collect()
    }

    pub fn list_output(&self) -> Vec<LocalOutput> {
        self.get_wallet().list_output().map(|o| o.into()).collect()
    }

    pub fn start_full_scan(&self) -> Arc<FullScanRequestBuilder> {
        let builder = self.get_wallet().start_full_scan();
        Arc::new(FullScanRequestBuilder(Mutex::new(Some(builder))))
    }

    pub fn start_sync_with_revealed_spks(&self) -> Arc<SyncRequestBuilder> {
        let builder = self.get_wallet().start_sync_with_revealed_spks();
        Arc::new(SyncRequestBuilder(Mutex::new(Some(builder))))
    }

    // pub fn persist(&self, connection: Connection) -> Result<bool, FfiGenericError> {
    pub fn persist(&self, connection: Arc<Connection>) -> Result<bool, SqliteError> {
        let mut binding = connection.get_store();
        let db: &mut BdkConnection = binding.borrow_mut();
        self.get_wallet()
            .persist(db)
            .map_err(|e| SqliteError::Sqlite {
                rusqlite_error: e.to_string(),
            })
    }
}

pub struct SentAndReceivedValues {
    pub sent: Arc<Amount>,
    pub received: Arc<Amount>,
}

pub struct TransactionAndLastSeen {
    pub tx: Arc<Transaction>,
    pub last_seen: u64,
}

#[derive(Clone)]
pub struct TxBuilder {
    pub(crate) add_global_xpubs: bool,
    pub(crate) recipients: Vec<(BdkScriptBuf, BdkAmount)>,
    pub(crate) utxos: Vec<OutPoint>,
    pub(crate) unspendable: HashSet<OutPoint>,
    pub(crate) change_policy: ChangeSpendPolicy,
    pub(crate) manually_selected_only: bool,
    pub(crate) fee_rate: Option<FeeRate>,
    pub(crate) fee_absolute: Option<Arc<Amount>>,
    pub(crate) drain_wallet: bool,
    pub(crate) drain_to: Option<BdkScriptBuf>,
    pub(crate) rbf: Option<RbfValue>,
    // pub(crate) data: Vec<u8>,
}

impl TxBuilder {
    pub(crate) fn new() -> Self {
        TxBuilder {
            add_global_xpubs: false,
            recipients: Vec::new(),
            utxos: Vec::new(),
            unspendable: HashSet::new(),
            change_policy: ChangeSpendPolicy::ChangeAllowed,
            manually_selected_only: false,
            fee_rate: None,
            fee_absolute: None,
            drain_wallet: false,
            drain_to: None,
            rbf: None,
            // data: Vec::new(),
        }
    }

    pub(crate) fn add_global_xpubs(&self) -> Arc<Self> {
        Arc::new(TxBuilder {
            add_global_xpubs: true,
            ..self.clone()
        })
    }

    pub(crate) fn add_recipient(&self, script: &Script, amount: Arc<Amount>) -> Arc<Self> {
        let mut recipients: Vec<(BdkScriptBuf, BdkAmount)> = self.recipients.clone();
        recipients.append(&mut vec![(script.0.clone(), amount.0)]);

        Arc::new(TxBuilder {
            recipients,
            ..self.clone()
        })
    }

    pub(crate) fn set_recipients(&self, recipients: Vec<ScriptAmount>) -> Arc<Self> {
        let recipients = recipients
            .iter()
            .map(|script_amount| (script_amount.script.0.clone(), script_amount.amount.0)) //;
            .collect();
        Arc::new(TxBuilder {
            recipients,
            ..self.clone()
        })
    }

    pub(crate) fn add_unspendable(&self, unspendable: OutPoint) -> Arc<Self> {
        let mut unspendable_hash_set = self.unspendable.clone();
        unspendable_hash_set.insert(unspendable);
        Arc::new(TxBuilder {
            unspendable: unspendable_hash_set,
            ..self.clone()
        })
    }

    pub(crate) fn unspendable(&self, unspendable: Vec<OutPoint>) -> Arc<Self> {
        Arc::new(TxBuilder {
            unspendable: unspendable.into_iter().collect(),
            ..self.clone()
        })
    }

    pub(crate) fn add_utxo(&self, outpoint: OutPoint) -> Arc<Self> {
        self.add_utxos(vec![outpoint])
    }

    pub(crate) fn add_utxos(&self, mut outpoints: Vec<OutPoint>) -> Arc<Self> {
        let mut utxos = self.utxos.to_vec();
        utxos.append(&mut outpoints);
        Arc::new(TxBuilder {
            utxos,
            ..self.clone()
        })
    }

    pub(crate) fn change_policy(&self, change_policy: ChangeSpendPolicy) -> Arc<Self> {
        Arc::new(TxBuilder {
            change_policy,
            ..self.clone()
        })
    }

    pub(crate) fn do_not_spend_change(&self) -> Arc<Self> {
        Arc::new(TxBuilder {
            change_policy: ChangeSpendPolicy::ChangeForbidden,
            ..self.clone()
        })
    }

    pub(crate) fn only_spend_change(&self) -> Arc<Self> {
        Arc::new(TxBuilder {
            change_policy: ChangeSpendPolicy::OnlyChange,
            ..self.clone()
        })
    }

    pub(crate) fn manually_selected_only(&self) -> Arc<Self> {
        Arc::new(TxBuilder {
            manually_selected_only: true,
            ..self.clone()
        })
    }

    pub(crate) fn fee_rate(&self, fee_rate: &FeeRate) -> Arc<Self> {
        Arc::new(TxBuilder {
            fee_rate: Some(fee_rate.clone()),
            ..self.clone()
        })
    }

    pub(crate) fn fee_absolute(&self, fee_amount: Arc<Amount>) -> Arc<Self> {
        Arc::new(TxBuilder {
            fee_absolute: Some(fee_amount),
            ..self.clone()
        })
    }

    pub(crate) fn drain_wallet(&self) -> Arc<Self> {
        Arc::new(TxBuilder {
            drain_wallet: true,
            ..self.clone()
        })
    }

    pub(crate) fn drain_to(&self, script: &Script) -> Arc<Self> {
        Arc::new(TxBuilder {
            drain_to: Some(script.0.clone()),
            ..self.clone()
        })
    }

    pub(crate) fn enable_rbf(&self) -> Arc<Self> {
        Arc::new(TxBuilder {
            rbf: Some(RbfValue::Default),
            ..self.clone()
        })
    }

    pub(crate) fn enable_rbf_with_sequence(&self, nsequence: u32) -> Arc<Self> {
        Arc::new(TxBuilder {
            rbf: Some(RbfValue::Value(nsequence)),
            ..self.clone()
        })
    }

    pub(crate) fn finish(&self, wallet: &Arc<Wallet>) -> Result<Arc<Psbt>, CreateTxError> {
        // TODO: I had to change the wallet here to be mutable. Why is that now required with the 1.0 API?
        let mut wallet = wallet.get_wallet();
        let mut tx_builder = wallet.build_tx();
        if self.add_global_xpubs {
            tx_builder.add_global_xpubs();
        }
        for (script, amount) in &self.recipients {
            tx_builder.add_recipient(script.clone(), *amount);
        }
        tx_builder.change_policy(self.change_policy);
        if !self.utxos.is_empty() {
            tx_builder
                .add_utxos(&self.utxos)
                .map_err(CreateTxError::from)?;
        }
        if !self.unspendable.is_empty() {
            let bdk_unspendable: Vec<OutPoint> = self.unspendable.clone().into_iter().collect();
            tx_builder.unspendable(bdk_unspendable);
        }
        if self.manually_selected_only {
            tx_builder.manually_selected_only();
        }
        if let Some(fee_rate) = &self.fee_rate {
            tx_builder.fee_rate(fee_rate.0);
        }
        if let Some(fee_amount) = &self.fee_absolute {
            tx_builder.fee_absolute(fee_amount.0);
        }
        if self.drain_wallet {
            tx_builder.drain_wallet();
        }
        if let Some(script) = &self.drain_to {
            tx_builder.drain_to(script.clone());
        }
        if let Some(rbf) = &self.rbf {
            match *rbf {
                RbfValue::Default => {
                    tx_builder.enable_rbf();
                }
                RbfValue::Value(nsequence) => {
                    tx_builder.enable_rbf_with_sequence(Sequence(nsequence));
                }
            }
        }

        let psbt = tx_builder.finish().map_err(CreateTxError::from)?;

        Ok(Arc::new(psbt.into()))
    }
}

#[derive(Clone)]
pub(crate) struct BumpFeeTxBuilder {
    pub(crate) txid: String,
    pub(crate) fee_rate: Arc<FeeRate>,
    pub(crate) rbf: Option<RbfValue>,
}

impl BumpFeeTxBuilder {
    pub(crate) fn new(txid: String, fee_rate: Arc<FeeRate>) -> Self {
        Self {
            txid,
            fee_rate,
            rbf: None,
        }
    }

    pub(crate) fn enable_rbf(&self) -> Arc<Self> {
        Arc::new(Self {
            rbf: Some(RbfValue::Default),
            ..self.clone()
        })
    }

    pub(crate) fn enable_rbf_with_sequence(&self, nsequence: u32) -> Arc<Self> {
        Arc::new(Self {
            rbf: Some(RbfValue::Value(nsequence)),
            ..self.clone()
        })
    }

    pub(crate) fn finish(&self, wallet: &Arc<Wallet>) -> Result<Arc<Psbt>, CreateTxError> {
        let txid = Txid::from_str(self.txid.as_str()).map_err(|_| CreateTxError::UnknownUtxo {
            outpoint: self.txid.clone(),
        })?;
        let mut wallet = wallet.get_wallet();
        let mut tx_builder = wallet.build_fee_bump(txid).map_err(CreateTxError::from)?;
        tx_builder.fee_rate(self.fee_rate.0);
        if let Some(rbf) = &self.rbf {
            match *rbf {
                RbfValue::Default => {
                    tx_builder.enable_rbf();
                }
                RbfValue::Value(nsequence) => {
                    tx_builder.enable_rbf_with_sequence(Sequence(nsequence));
                }
            }
        }
        let psbt: BdkPsbt = tx_builder.finish()?;

        Ok(Arc::new(psbt.into()))
    }
}
#[derive(Clone, Debug)]
pub enum RbfValue {
    Default,
    Value(u32),
}
