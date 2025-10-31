use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::PathBuf,
    str::FromStr,
    sync::mpsc,
    time::SystemTime,
};

use bwk_electrum::client::{CoinRequest, CoinResponse};
use clap::Parser;
use miniscript::{
    Descriptor, DescriptorPublicKey,
    bitcoin::{
        Address, Amount, Network, OutPoint, Psbt, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
        Txid, Witness, absolute,
        psbt::{Input, Output},
        transaction::Version,
    },
    psbt::PsbtExt,
};
use serde::{Deserialize, Serialize};

type TxMap = BTreeMap<Txid, Transaction>;
type CoinMap = BTreeMap<OutPoint, Coin>;

#[derive(Parser, Debug)]
pub struct Args {
    #[arg(short, long)]
    /// Path to the file containing the descriptor, should be of form:
    /// wpkh([<fingerprint><deriv_path>]<xpub>/<0;1>/*)
    pub descriptor: PathBuf,
    #[arg(short, long)]
    /// Ip of the electrum server
    pub ip: String,
    #[arg(short, long)]
    /// Port of the electrum server
    pub port: u16,
    #[arg(short, long)]
    /// Target derivation index
    pub target: u32,
    #[arg(short, long)]
    /// Address where the coins will be spent
    pub address: String,
    #[arg(short, long, default_value = "10000")]
    /// Max subscription accepted by the server for each connections
    pub max: u32,
    /// How many spk we ask for each requests
    #[arg(short, long, default_value = "100")]
    /// Max subscription accepted by the server for each connections
    pub batch: u32,
    #[arg(short, long, default_value = "1")]
    /// Fee rate in sats/vb
    pub fee: u64,
}

fn main() {
    let mut args = Args::parse();
    args.max /= 2;
    let start = SystemTime::now();

    print!(
        "Open descriptor file at {}",
        args.descriptor.to_str().unwrap()
    );
    let path = args.descriptor.canonicalize().unwrap();
    let descriptor_str = fs::read_to_string(path).unwrap();

    let address = Address::from_str(&args.address).unwrap();
    if !address.is_valid_for_network(Network::Bitcoin) {
        panic!("Address is for another network");
    }
    let addr = address.assume_checked();

    let descriptor = Descriptor::<DescriptorPublicKey>::from_str(descriptor_str.trim())
        .expect("Invalid descriptor");
    let descriptors = descriptor
        .into_single_descriptors()
        .expect("The descriptor needs a multipath");
    let recv_descriptor = descriptors
        .first()
        .expect("The descriptor needs a multipath")
        .clone();
    let change_descriptor = descriptors
        .get(1)
        .expect("The descriptor needs a multipath")
        .clone();

    let mut client = bwk_electrum::client::Client::new(&args.ip, args.port).unwrap();
    let (mut sender, mut receiver) = client.listen();

    // List funded spks
    let mut spks_index = BTreeMap::new();
    let mut funded_spks = vec![];
    let mut i = 0u32;
    while i < args.target {
        if i % 1000 == 0 {
            let now = SystemTime::now();
            let time = now.duration_since(start).unwrap();
            println!("{time:?} -- scan height {} --", i);
        }
        if i % args.max == 0 {
            println!("New client");

            client = bwk_electrum::client::Client::new(&args.ip, args.port).unwrap();
            (sender, receiver) = client.listen();
        }

        // process spks
        let recv_spks = spks_from(&recv_descriptor, i, args.batch);
        let change_spks = spks_from(&change_descriptor, i, args.batch);

        // register receive index
        for (p, script) in recv_spks.iter().enumerate() {
            let index = i + (p as u32);
            spks_index.insert(script.clone(), (false, index));
        }

        // register change index
        for (p, script) in change_spks.iter().enumerate() {
            let index = i + (p as u32);
            spks_index.insert(script.clone(), (true, index));
        }

        // scan recv
        scan(
            start,
            &mut sender,
            &mut receiver,
            recv_spks,
            &mut spks_index,
            false,
            &mut funded_spks,
        );

        // scan change
        scan(
            start,
            &mut sender,
            &mut receiver,
            change_spks,
            &mut spks_index,
            true,
            &mut funded_spks,
        );
        i += args.batch;
    }

    // persist
    let mut file = File::create("funded_spks.json").unwrap();
    for entry in &funded_spks {
        let mut buf = serde_json::to_string(entry).unwrap();
        buf.push('\n');
        file.write_all(buf.as_bytes()).unwrap();
    }

    // create a new client so we dont get random notif from subscriptions
    client = bwk_electrum::client::Client::new(&args.ip, args.port).unwrap();
    (sender, receiver) = client.listen();

    // Fetch all txs
    let mut tx_map: TxMap = BTreeMap::new();
    for spk in funded_spks {
        get_txs_for_spk(&mut sender, &mut receiver, spk.spk, &mut tx_map);
    }

    let mut coins_map: CoinMap = BTreeMap::new();
    // First fetch all owned Coins & mark them unspent
    for (txid, tx) in &tx_map {
        for (vout, txout) in tx.output.iter().enumerate() {
            // if the coin is owned
            if let Some((is_change, index)) = spks_index.get(&txout.script_pubkey).cloned() {
                let outpoint = OutPoint {
                    txid: *txid,
                    vout: vout as u32,
                };
                let coin = Coin {
                    outpoint,
                    value: txout.value,
                    spent: false,
                    is_change,
                    index,
                    txout: txout.clone(),
                };
                coins_map.insert(outpoint, coin);
            }
        }
    }

    // Then mark coins that are spent
    for tx in tx_map.values() {
        for txin in tx.input.iter() {
            let op = txin.previous_output;
            // If the coin is owned
            if let Some(coin) = coins_map.get_mut(&op) {
                coin.spent = true;
            }
        }
    }

    // Collect unspent coins
    let unspent_coins: Vec<_> = coins_map
        .into_iter()
        .filter_map(|(_, c)| (!c.spent).then_some(c))
        .collect();

    // persist coins
    let mut file = File::create("unspent_coins.json").unwrap();
    for coin in &unspent_coins {
        let mut buf = serde_json::to_string(coin).unwrap();
        buf.push('\n');
        file.write_all(buf.as_bytes()).unwrap();
    }

    let txout = TxOut {
        value: Amount::from_btc(21_000_000.0).unwrap(),
        script_pubkey: addr.script_pubkey(),
    };
    let tx = Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![],
        output: vec![txout],
    };

    let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
    // It's considered an external spend
    psbt.outputs.push(Output::default());

    let mut sum_inputs = Amount::ZERO;
    for (pos, coin) in unspent_coins.into_iter().enumerate() {
        sum_inputs += coin.value;
        let txin = TxIn {
            previous_output: coin.outpoint,
            script_sig: ScriptBuf::default(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };

        // Populate tx
        psbt.unsigned_tx.input.push(txin);

        // Populate psbt
        let psbt_input = Input {
            witness_utxo: Some(coin.txout.clone()),
            ..Default::default()
        };
        psbt.inputs.push(psbt_input);

        // Update signing informations
        let descriptor = if coin.is_change {
            change_descriptor.at_derivation_index(coin.index).unwrap()
        } else {
            recv_descriptor.at_derivation_index(coin.index).unwrap()
        };
        PsbtExt::update_input_with_descriptor(&mut psbt, pos, &descriptor).unwrap();
    }

    let signatures_unit_weight = recv_descriptor.max_weight_to_satisfy().unwrap();
    let signatures_weight = signatures_unit_weight
        .checked_mul(psbt.unsigned_tx.input.len() as u64)
        .unwrap();
    let unsigned_tx_weight = psbt.unsigned_tx.weight();
    let weight_vb = (signatures_weight + unsigned_tx_weight).to_vbytes_ceil();
    let fees = Amount::from_sat(args.fee * weight_vb);

    let output_value = sum_inputs - fees;
    psbt.unsigned_tx.output[0].value = output_value;

    println!("{} inputs: {sum_inputs}", psbt.inputs.len());
    println!("Fees: {fees}");
    println!("Output: {output_value}");

    println!("Sweep psbt:\n{}", psbt);

    let now = SystemTime::now();
    let time = now.duration_since(start).unwrap();
    let spk_len = spks_index.len();
    println!("{spk_len} spks scanned in {time:?}");
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpkEntry {
    spk: ScriptBuf,
    change: bool,
    index: u32,
}

fn scan(
    start: SystemTime,
    sender: &mut mpsc::Sender<CoinRequest>,
    receiver: &mut mpsc::Receiver<CoinResponse>,
    spks: Vec<ScriptBuf>,
    spks_index: &mut BTreeMap<ScriptBuf, (bool /* is_change*/, u32 /* index */)>,
    is_change: bool,
    funded_spks: &mut Vec<SpkEntry>,
) {
    let len = spks.len();
    // send request
    let req = CoinRequest::Subscribe(spks);
    sender.send(req).unwrap();

    // wait for response
    let resp: CoinResponse = receiver.recv().unwrap();
    if let CoinResponse::Status(statuses) = resp {
        assert!(statuses.len() == len);
        for (script, status) in statuses {
            if status.is_some() {
                let (_, index) = spks_index.get(&script).unwrap();
                let now = SystemTime::now();
                let time = now.duration_since(start).unwrap();
                let change = if is_change { "change" } else { "recv" };
                funded_spks.push(SpkEntry {
                    spk: script,
                    change: is_change,
                    index: *index,
                });
                println!("{time:?} {change} coin found at index {index}");
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coin {
    pub outpoint: OutPoint,
    pub txout: TxOut,
    pub value: Amount,
    pub spent: bool,
    pub is_change: bool,
    pub index: u32,
}

fn get_txs_for_spk(
    sender: &mut mpsc::Sender<CoinRequest>,
    receiver: &mut mpsc::Receiver<CoinResponse>,
    spk: ScriptBuf,
    tx_map: &mut TxMap,
) {
    // First list txids
    let req = CoinRequest::History(vec![spk]);
    sender.send(req).unwrap();

    let mut txids = vec![];
    // wait for response
    let resp: CoinResponse = receiver.recv().unwrap();
    if let CoinResponse::History(hist) = resp {
        for (_, vec) in hist {
            for (txid, _) in vec {
                txids.push(txid);
            }
        }
    } else {
        panic!("Unexpected, a CoinResponse::History was expected");
    }

    // only request txs we dont already have
    let txids: Vec<_> = txids
        .into_iter()
        .filter(|txid| !tx_map.contains_key(txid))
        .collect();
    if txids.is_empty() {
        return;
    }

    // Then list txs
    let req = CoinRequest::Txs(txids.clone());
    sender.send(req).unwrap();

    // wait for response
    let resp: CoinResponse = receiver.recv().unwrap();
    if let CoinResponse::Txs(txs) = resp {
        // check no tx are missing
        assert_eq!(txids.len(), txs.len());
        let map: BTreeSet<_> = txs.iter().map(|tx| tx.compute_txid()).collect();
        for txid in txids {
            assert!(map.contains(&txid));
        }

        // insert txs
        for tx in txs {
            tx_map.insert(tx.compute_txid(), tx);
        }
    } else {
        panic!("Unexpected, a CoinResponse::Txs was expected");
    }
}

fn spks_from(
    descriptor: &Descriptor<DescriptorPublicKey>,
    start_index: u32,
    batch: u32,
) -> Vec<ScriptBuf> {
    let mut out = vec![];
    for index in start_index..start_index + batch {
        let spk = descriptor
            .at_derivation_index(index)
            .unwrap()
            .address(Network::Bitcoin)
            .unwrap()
            .script_pubkey();
        out.push(spk);
    }
    out
}
