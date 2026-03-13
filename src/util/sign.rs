use std::str::FromStr;

use bip39::Mnemonic;
use miniscript::{
    bitcoin::{
        bip32::{DerivationPath, Xpriv, Xpub},
        ecdsa,
        secp256k1::Secp256k1,
        sighash::{EcdsaSighashType, SighashCache},
        Network, Psbt,
    },
    psbt::PsbtExt,
    Descriptor, DescriptorPublicKey,
};

pub fn sign_psbt(
    mnemonic: String,
    psbt_str: String,
    _descriptor_str: String,
) -> Result<String, String> {
    let mut psbt = Psbt::from_str(psbt_str.trim())
        .map_err(|e| format!("Invalid PSBT: {}", e))?;

    let mnemonic = Mnemonic::from_str(mnemonic.trim())
        .map_err(|e| format!("Invalid mnemonic: {}", e))?;
    let seed = mnemonic.to_seed("");

    let secp = Secp256k1::new();
    let master_xpriv = Xpriv::new_master(Network::Bitcoin, &seed)
        .map_err(|e| format!("Failed to derive master key: {}", e))?;
    let master_fp = master_xpriv.fingerprint(&secp);

    let mut cache = SighashCache::new(psbt.unsigned_tx.clone());

    for i in 0..psbt.inputs.len() {
        let (hash, sighash_type) = psbt
            .sighash_ecdsa(i, &mut cache)
            .map_err(|e| format!("Sighash error on input {}: {}", i, e))?;

        if sighash_type != EcdsaSighashType::All {
            return Err(format!("Input {} uses unsupported sighash type", i));
        }

        let paths: Vec<_> = psbt.inputs[i]
            .bip32_derivation
            .iter()
            .filter_map(|(_, (fp, path))| {
                if *fp == master_fp {
                    Some(path.clone())
                } else {
                    None
                }
            })
            .collect();

        if paths.is_empty() {
            return Err(format!(
                "Input {}: no matching key found for master fingerprint {}. \
                 Make sure the mnemonic matches the descriptor used to create the PSBT.",
                i, master_fp
            ));
        }

        for path in paths {
            let child_xpriv = master_xpriv
                .derive_priv(&secp, &path)
                .map_err(|e| format!("Key derivation failed at path {:?}: {}", path, e))?;

            let secret_key = child_xpriv.private_key;
            let secp_pubkey = secret_key.public_key(&secp);

            if !psbt.inputs[i].bip32_derivation.contains_key(&secp_pubkey) {
                continue;
            }

            let sig = secp.sign_ecdsa_low_r(&hash, &secret_key);
            let signature = ecdsa::Signature {
                signature: sig,
                sighash_type: EcdsaSighashType::All,
            };
            let btc_pubkey = miniscript::bitcoin::PublicKey::new(secp_pubkey);
            psbt.inputs[i].partial_sigs.insert(btc_pubkey, signature);
        }
    }

    psbt.finalize_mut(&secp)
        .map_err(|e| format!("Failed to finalize PSBT: {:?}", e))?;

    Ok(psbt.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::{
        bitcoin::{
            absolute, psbt::Input, transaction::Version, Amount, OutPoint, ScriptBuf, Sequence,
            Transaction, TxIn, TxOut, Txid, Witness,
        },
        psbt::PsbtExt,
    };

    // BIP39 test vector mnemonic — well-known, safe for testing
    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn build_test_psbt(descriptor_str: &str, recv_index: u32) -> (Psbt, String) {
        let secp = Secp256k1::new();
        let descriptor =
            Descriptor::<DescriptorPublicKey>::from_str(descriptor_str).unwrap();
        let single = descriptor.into_single_descriptors().unwrap();
        let recv_desc = &single[0];
        let concrete = recv_desc.at_derivation_index(recv_index).unwrap();
        let spk = concrete.script_pubkey();

        let fake_txid = Txid::from_str(
            "a15d57094aa7a21a28cb20b59aab8fc7d1149a3bdbcddba9c622e4f5f6a99ece",
        )
        .unwrap();

        let txin = TxIn {
            previous_output: OutPoint { txid: fake_txid, vout: 0 },
            script_sig: ScriptBuf::default(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };
        let txout = TxOut {
            value: Amount::from_sat(99_000),
            script_pubkey: spk.clone(), // send back to same address for simplicity
        };
        let tx = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![txin],
            output: vec![txout],
        };

        let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
        psbt.inputs[0].witness_utxo = Some(TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: spk,
        });

        // populate bip32_derivation + miniscript descriptor metadata
        PsbtExt::update_input_with_descriptor(&mut psbt, 0, &concrete).unwrap();

        let psbt_str = psbt.to_string();
        (psbt, psbt_str)
    }

    fn test_descriptor() -> String {
        let mnemonic = Mnemonic::from_str(TEST_MNEMONIC).unwrap();
        let seed = mnemonic.to_seed("");
        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Bitcoin, &seed).unwrap();
        let fp = master.fingerprint(&secp);
        let path = DerivationPath::from_str("m/84'/0'/0'").unwrap();
        let account_xpriv = master.derive_priv(&secp, &path).unwrap();
        let account_xpub = Xpub::from_priv(&secp, &account_xpriv);
        format!("wpkh([{}/84'/0'/0']{}/<0;1>/*)", fp, account_xpub)
    }

    #[test]
    fn test_sign_psbt_receive_index_0() {
        let descriptor_str = test_descriptor();
        let (_, psbt_str) = build_test_psbt(&descriptor_str, 0);
        let result = sign_psbt(TEST_MNEMONIC.to_string(), psbt_str, descriptor_str);
        assert!(result.is_ok(), "Signing failed: {:?}", result.err());
        // result is a finalized PSBT string — non-empty
        assert!(!result.unwrap().is_empty());
    }

    #[test]
    fn test_sign_psbt_wrong_mnemonic() {
        let descriptor_str = test_descriptor();
        let (_, psbt_str) = build_test_psbt(&descriptor_str, 0);
        let wrong_mnemonic = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong";
        let result = sign_psbt(wrong_mnemonic.to_string(), psbt_str, descriptor_str);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("no matching key"),
            "Expected fingerprint mismatch error"
        );
    }
}
