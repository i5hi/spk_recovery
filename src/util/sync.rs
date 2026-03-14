use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::mpsc,
    time::SystemTime,
};

use tokio::sync::mpsc::UnboundedSender as LogSender;

use bwk_electrum::client::{CoinRequest, CoinResponse};
use miniscript::{
    bitcoin::{
        absolute,
        psbt::Input,
        transaction::Version,
        Address, Amount, Network, OutPoint, Psbt, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
        Txid, Witness,
    },
    psbt::PsbtExt,
    Descriptor, DescriptorPublicKey,
};

use super::{SyncResult, SpkEntry, Coin};

type TxMap = BTreeMap<Txid, Transaction>;
type CoinMap = BTreeMap<OutPoint, Coin>;

pub fn sync_wallet(
    descriptor_str: String,
    ip: String,
    port: String,
    target: String,
    address: String,
    batch: String,
    fee: String,
    log_tx: LogSender<String>,
) -> Result<SyncResult, String> {
    let target: u32 = target.parse().map_err(|e| format!("Invalid target: {}", e))?;
    let batch: u32 = batch.parse().map_err(|e| format!("Invalid batch: {}", e))?;
    let fee: u64 = fee.parse().map_err(|e| format!("Invalid fee: {}", e))?;
    let port: u16 = port.parse().map_err(|e| format!("Invalid port: {}", e))?;

    let max = batch * 2;

    let address = Address::from_str(&address)
        .map_err(|e| format!("Invalid address: {}", e))?;
    if !address.is_valid_for_network(Network::Bitcoin) {
        return Err("Address is for another network".to_string());
    }
    let addr = address.assume_checked();

    let descriptor = Descriptor::<DescriptorPublicKey>::from_str(descriptor_str.trim())
        .map_err(|e| format!("Invalid descriptor: {}", e))?;
    let descriptors = descriptor
        .into_single_descriptors()
        .map_err(|e| format!("Descriptor error: {}", e))?;
    let recv_descriptor = descriptors.first().ok_or("No receive descriptor")?.clone();
    let change_descriptor = descriptors.get(1).ok_or("No change descriptor")?.clone();

    let mut client = bwk_electrum::client::Client::new_local(&ip, port)
        .map_err(|e| format!("Failed to connect: {:?}", e))?;
    let (mut sender, mut receiver) = client.listen();

    let mut spks_index = BTreeMap::new();
    let mut funded_spks = vec![];
    let mut i = 0u32;
    let start = SystemTime::now();

    let _ = log_tx.send(format!("Starting sync up to index {}", target));
    let _ = log_tx.send(format!("Connected to {}:{}", ip, port));

    while i < target {
        let elapsed = SystemTime::now().duration_since(start).unwrap();

        if i > 0 && i % max == 0 {
            let _ = log_tx.send(format!("{:?} -- Closing old client and creating new client at index {} --", elapsed, i));

            client = bwk_electrum::client::Client::new_local(&ip, port)
                .map_err(|e| format!("Failed to reconnect: {:?}", e))?;
            (sender, receiver) = client.listen();

            let _ = log_tx.send(format!("{:?} -- New client ready --", elapsed));
        }

        if i % 1000 == 0 {
            let elapsed = SystemTime::now().duration_since(start).unwrap();
            let pct = i * 100 / target;
            let _ = log_tx.send(format!("{:?} -- scan height {} ({}%) --", elapsed, i, pct));
        }

        if i % 100 == 0 && i % 1000 != 0 {
            let elapsed = SystemTime::now().duration_since(start).unwrap();
            let pct = i * 100 / target;
            let _ = log_tx.send(format!("{:?} -- Processing index {} ({}%) --", elapsed, i, pct));
        }

        let recv_spks = spks_from(&recv_descriptor, i, batch);
        let change_spks = spks_from(&change_descriptor, i, batch);

        for (p, script) in recv_spks.iter().enumerate() {
            let index = i + (p as u32);
            spks_index.insert(script.clone(), (false, index));
        }

        for (p, script) in change_spks.iter().enumerate() {
            let index = i + (p as u32);
            spks_index.insert(script.clone(), (true, index));
        }

        scan(
            start,
            &mut sender,
            &mut receiver,
            recv_spks,
            &spks_index,
            false,
            &mut funded_spks,
            &log_tx,
        )?;

        scan(
            start,
            &mut sender,
            &mut receiver,
            change_spks,
            &spks_index,
            true,
            &mut funded_spks,
            &log_tx,
        )?;

        i += batch;
    }

    let _ = log_tx.send(format!("Scan complete. Found {} total outputs", funded_spks.len()));
    let _ = log_tx.send("STATUS:Building Transaction".to_string());
    let _ = log_tx.send("Fetching transactions and building PSBT...".to_string());

    client = bwk_electrum::client::Client::new_local(&ip, port)
        .map_err(|e| format!("Failed to reconnect: {:?}", e))?;
    (sender, receiver) = client.listen();

    let mut tx_map: TxMap = BTreeMap::new();
    for spk in funded_spks {
        get_txs_for_spk(&mut sender, &mut receiver, spk.spk, &mut tx_map);
    }

    let _ = log_tx.send(format!("Fetched {} unique transactions", tx_map.len()));

    let mut coins_map: CoinMap = BTreeMap::new();
    for (txid, tx) in &tx_map {
        for (vout, txout) in tx.output.iter().enumerate() {
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

    for tx in tx_map.values() {
        for txin in tx.input.iter() {
            let op = txin.previous_output;
            if let Some(coin) = coins_map.get_mut(&op) {
                coin.spent = true;
            }
        }
    }

    let unspent_coins: Vec<_> = coins_map
        .into_iter()
        .filter_map(|(_, c)| (!c.spent).then_some(c))
        .collect();

    let _ = log_tx.send(format!("Found {} unspent coins", unspent_coins.len()));

    if unspent_coins.is_empty() {
        return Err("No unspent coins found".to_string());
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

    let mut psbt = Psbt::from_unsigned_tx(tx).map_err(|e| format!("PSBT error: {}", e))?;

    let mut sum_inputs = Amount::ZERO;
    for (pos, coin) in unspent_coins.into_iter().enumerate() {
        sum_inputs += coin.value;
        let txin = TxIn {
            previous_output: coin.outpoint,
            script_sig: ScriptBuf::default(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };

        psbt.unsigned_tx.input.push(txin);

        let psbt_input = Input {
            witness_utxo: Some(coin.txout.clone()),
            ..Default::default()
        };
        psbt.inputs.push(psbt_input);

        let descriptor = if coin.is_change {
            change_descriptor.at_derivation_index(coin.index).unwrap()
        } else {
            recv_descriptor.at_derivation_index(coin.index).unwrap()
        };
        PsbtExt::update_input_with_descriptor(&mut psbt, pos, &descriptor)
            .map_err(|e| format!("Failed to update PSBT: {}", e))?;
    }

    let signatures_unit_weight = recv_descriptor.max_weight_to_satisfy().unwrap();
    let signatures_weight = signatures_unit_weight
        .checked_mul(psbt.unsigned_tx.input.len() as u64)
        .unwrap();
    let unsigned_tx_weight = psbt.unsigned_tx.weight();
    let weight_vb = (signatures_weight + unsigned_tx_weight).to_vbytes_ceil();
    let fees = Amount::from_sat(fee * weight_vb);

    let output_value = sum_inputs - fees;
    psbt.unsigned_tx.output[0].value = output_value;

    let _ = log_tx.send(format!("Created PSBT with {} inputs", psbt.inputs.len()));
    let _ = log_tx.send(format!("Total input: {} BTC", sum_inputs.to_btc()));
    let _ = log_tx.send(format!("Fees: {} sats", fees.to_sat()));
    let _ = log_tx.send(format!("Output: {} BTC", output_value.to_btc()));

    Ok(SyncResult {
        psbt: psbt.to_string(),
        num_inputs: psbt.inputs.len(),
        total_value: sum_inputs,
        fees,
        output_value,
    })
}

fn scan(
    start: SystemTime,
    sender: &mut mpsc::Sender<CoinRequest>,
    receiver: &mut mpsc::Receiver<CoinResponse>,
    spks: Vec<ScriptBuf>,
    spks_index: &BTreeMap<ScriptBuf, (bool, u32)>,
    is_change: bool,
    funded_spks: &mut Vec<SpkEntry>,
    log_tx: &LogSender<String>,
) -> Result<(), String> {
    let len = spks.len();
    let change_str = if is_change { "change" } else { "recv" };

    let req = CoinRequest::Subscribe(spks);
    sender.send(req).map_err(|e| format!("Send error: {}", e))?;

    let elapsed = SystemTime::now().duration_since(start).unwrap();
    let _ = log_tx.send(format!("{:?} -- Waiting for {} response ({} spks) --", elapsed, change_str, len));

    let resp: CoinResponse = receiver.recv().map_err(|e| format!("Recv error: {}", e))?;

    let elapsed = SystemTime::now().duration_since(start).unwrap();
    let _ = log_tx.send(format!("{:?} -- Received {} response --", elapsed, change_str));

    match resp {
        CoinResponse::Status(statuses) => {
            assert!(statuses.len() == len);
            for (script, status) in statuses {
                if status.is_some() {
                    let (_, index) = spks_index.get(&script).ok_or("Script not in index")?;
                    let elapsed = SystemTime::now().duration_since(start).unwrap();
                    let _ = log_tx.send(format!("{:?} {} coin found at index {}", elapsed, change_str, index));

                    funded_spks.push(SpkEntry {
                        spk: script,
                        change: is_change,
                        index: *index,
                    });
                }
            }
            Ok(())
        }
        CoinResponse::Error(e) => {
            Err(format!("Electrum error: {}", e))
        }
        _ => {
            Err("Unexpected response type".to_string())
        }
    }
}

fn get_txs_for_spk(
    sender: &mut mpsc::Sender<CoinRequest>,
    receiver: &mut mpsc::Receiver<CoinResponse>,
    spk: ScriptBuf,
    tx_map: &mut TxMap,
) {
    let req = CoinRequest::History(vec![spk]);
    sender.send(req).unwrap();

    let mut txids = vec![];
    let resp: CoinResponse = receiver.recv().unwrap();
    if let CoinResponse::History(hist) = resp {
        for (_, vec) in hist {
            for (txid, _) in vec {
                txids.push(txid);
            }
        }
    }

    let txids: Vec<_> = txids
        .into_iter()
        .filter(|txid| !tx_map.contains_key(txid))
        .collect();
    if txids.is_empty() {
        return;
    }

    let req = CoinRequest::Txs(txids.clone());
    sender.send(req).unwrap();

    let resp: CoinResponse = receiver.recv().unwrap();
    if let CoinResponse::Txs(txs) = resp {
        for tx in txs {
            tx_map.insert(tx.compute_txid(), tx);
        }
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
