mod storage;
mod tendermint;

use self::{
    storage::{
        Address, Balance, BasicAddress, BlockHash, BlockHeight, Storage,
        ValidatorAddress,
    },
    tendermint::{AbciMsg, AbciReceiver},
};
use anoma::{
    bytes::ByteBuf,
    config::Config,
    rpc_types::{Message, Tx},
};
use anoma_vm::{TxEnv, TxMsg, TxRunner, VpRunner};
use std::{path::PathBuf, sync::mpsc};

pub fn run(config: Config) {
    // run our shell via Tendermint ABCI
    let db_path = config.home_dir.join("db");
    // open a channel between ABCI (the sender) and the shell (the receiver)
    let (sender, receiver) = mpsc::channel();
    let shell = Shell::new(receiver, db_path);
    let addr = "127.0.0.1:26658".parse().unwrap();
    // Run Tendermint ABCI server in another thread
    std::thread::spawn(move || tendermint::run(sender, config, addr));
    shell.run().unwrap();
}

pub fn reset(config: Config) {
    // simply nuke the DB files
    let db_path = config.home_dir.join("db");
    match std::fs::remove_dir_all(db_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        res => res.unwrap(),
    };
    // reset Tendermint state
    tendermint::reset(config)
}

pub struct Shell {
    abci: AbciReceiver,
    storage: storage::Storage,
}

#[derive(Clone, Debug)]
pub enum MempoolTxType {
    /// A transaction that has not been validated by this node before
    NewTransaction,
    /// A transaction that has been validated at some previous level that may
    /// need to be validated again
    RecheckTransaction,
}
pub type MempoolValidationResult<'a> = Result<(), String>;
pub type ApplyResult<'a> = Result<(), String>;

pub struct MerkleRoot(pub Vec<u8>);

impl Shell {
    pub fn new(abci: AbciReceiver, db_path: PathBuf) -> Self {
        let mut storage = Storage::new(db_path);
        // TODO load initial accounts from genesis
        let va = ValidatorAddress::new_address("va".to_owned());
        storage.update_balance(&va, Balance::new(10000)).unwrap();
        let ba = BasicAddress::new_address("ba".to_owned());
        storage.update_balance(&ba, Balance::new(100)).unwrap();
        Self { abci, storage }
    }

    /// Run the shell in the current thread (blocking).
    pub fn run(mut self) -> Result<(), String> {
        loop {
            let msg = self.abci.recv().map_err(|e| e.to_string())?;
            match msg {
                AbciMsg::GetInfo { reply } => {
                    let result = self.last_state();
                    reply.send(result).map_err(|e| e.to_string())?
                }
                AbciMsg::InitChain { reply, chain_id } => {
                    self.init_chain(chain_id);
                    reply.send(()).map_err(|e| e.to_string())?
                }
                AbciMsg::MempoolValidate { reply, tx, r#type } => {
                    let result = self.mempool_validate(&tx, r#type);
                    reply.send(result).map_err(|e| e.to_string())?
                }
                AbciMsg::BeginBlock {
                    reply,
                    hash,
                    height,
                } => {
                    self.begin_block(hash, height);
                    reply.send(()).map_err(|e| e.to_string())?
                }
                AbciMsg::ApplyTx { reply, tx } => {
                    let result = self.apply_tx(&tx);
                    reply.send(result).map_err(|e| e.to_string())?
                }
                AbciMsg::EndBlock { reply, height } => {
                    self.end_block(height);
                    reply.send(()).map_err(|e| e.to_string())?
                }
                AbciMsg::CommitBlock { reply } => {
                    let result = self.commit();
                    reply.send(result).map_err(|e| e.to_string())?
                }
            }
        }
    }
}

fn transfer(
    env: &TxEnv,
    src_ptr: i32,
    src_len: i32,
    dest_ptr: i32,
    dest_len: i32,
    amount: u64,
) {
    let memory = unsafe { env.memory.get_unchecked() };

    let src_vec: Vec<_> = memory.view()
        [src_ptr as usize..(src_ptr + src_len) as usize]
        .iter()
        .map(|cell| cell.get())
        .collect();
    let src = std::str::from_utf8(&src_vec).unwrap().to_string();

    let dest_vec: Vec<_> = memory.view()
        [dest_ptr as usize..(dest_ptr + dest_len) as usize]
        .iter()
        .map(|cell| cell.get())
        .collect();
    let dest = std::str::from_utf8(&dest_vec).unwrap().to_string();

    log::debug!(
        "transfer called with src: {}, dest: {}, amount: {}",
        src,
        dest,
        amount
    );

    let sender = env.sender.lock().unwrap();
    (*sender).send(TxMsg { src, dest, amount }).unwrap();
}

impl Shell {
    pub fn init_chain(&mut self, chain_id: String) {
        self.storage.set_chain_id(&chain_id).unwrap();
    }

    /// Validate a transaction request. On success, the transaction will
    /// included in the mempool and propagated to peers, otherwise it will be
    /// rejected.
    pub fn mempool_validate(
        &self,
        tx_bytes: &[u8],
        r#_type: MempoolTxType,
    ) -> MempoolValidationResult {
        let _tx = Tx::decode(&tx_bytes[..]).map_err(|e| {
            format!(
                "Error decoding a transaction: {}, from bytes {:?}",
                e, tx_bytes
            )
        })?;
        Ok(())
    }

    /// Validate and apply a transaction.
    pub fn apply_tx(&mut self, tx_bytes: &[u8]) -> ApplyResult {
        let tx = Tx::decode(&tx_bytes[..]).map_err(|e| {
            format!(
                "Error decoding a transaction: {}, from bytes  from bytes
        {:?}",
                e, tx_bytes
            )
        })?;
        let tx_data = tx.data.unwrap_or(vec![]);

        // Execute the transaction code and wait for result
        let (tx_sender, tx_receiver) = mpsc::channel();
        let tx_runner = TxRunner::new();
        tx_runner
            .run(tx.code, tx_data, tx_sender, transfer)
            .unwrap();
        let tx_msg = tx_receiver.recv().unwrap();
        let src_addr = Address::new_address(tx_msg.src.clone());
        let dest_addr = Address::new_address(tx_msg.dest.clone());

        // Run a VP for every account with modified storage sub-space
        // TODO run in parallel for all accounts
        //   - all must return `true` to accept the tx
        //   - cancel all remaining workers and fail if any returns `false`
        let src_vp = self
            .storage
            .validity_predicate(&src_addr)
            .map_err(|e| format!("Encountered a storage error {:?}", e))?;
        let dest_vp = self
            .storage
            .validity_predicate(&dest_addr)
            .map_err(|e| format!("Encountered a storage error {:?}", e))?;
        let vp_runner = VpRunner::new();
        let (vp_sender, vp_receiver) = mpsc::channel();
        vp_runner.run(src_vp, &tx_msg, vp_sender.clone()).unwrap();
        let src_accept = vp_receiver.recv().unwrap();
        vp_runner.run(dest_vp, &tx_msg, vp_sender).unwrap();
        let dest_accept = vp_receiver.recv().unwrap();

        // Apply the transaction if accepted by all the VPs
        if src_accept && dest_accept {
            self.storage
                .transfer(&src_addr, &dest_addr, tx_msg.amount)
                .map_err(|e| format!("Encountered a storage error {:?}", e))?;
            log::debug!(
                "all accepted apply_tx storage modification {:#?}",
                self.storage
            );
        } else {
            log::debug!(
                "tx declined by {}",
                if src_accept {
                    "dest"
                } else {
                    if dest_accept {
                        "src"
                    } else {
                        "src and dest"
                    }
                }
            );
        }

        Ok(())
    }

    /// Begin a new block.
    pub fn begin_block(&mut self, hash: BlockHash, height: BlockHeight) {
        self.storage.begin_block(hash, height).unwrap();
    }

    /// End a block.
    pub fn end_block(&mut self, _height: BlockHeight) {}

    /// Commit a block. Persist the application state and return the Merkle root
    /// hash.
    pub fn commit(&mut self) -> MerkleRoot {
        log::debug!("storage to commit {:#?}", self.storage);
        // store the block's data in DB
        // TODO commit async?
        self.storage.commit().unwrap_or_else(|e| {
            log::error!(
                "Encountered a storage error while committing a block {:?}",
                e
            )
        });
        let root = self.storage.merkle_root();
        MerkleRoot(root.as_slice().to_vec())
    }

    /// Load the Merkle root hash and the height of the last committed block, if
    /// any.
    pub fn last_state(&mut self) -> Option<(MerkleRoot, u64)> {
        let result = self.storage.load_last_state().unwrap_or_else(|e| {
            log::error!(
                "Encountered an error while reading last state from
        storage {:?}",
                e
            );
            None
        });
        match &result {
            Some((root, height)) => {
                log::info!(
                    "Last state root hash: {}, height: {}",
                    ByteBuf(&root.0),
                    height
                )
            }
            None => {
                log::info!("No state could be found")
            }
        }
        result
    }
}