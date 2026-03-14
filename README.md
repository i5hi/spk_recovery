# SPK Recovery Tool

A desktop tool for recovering Bitcoin from wallets with a large address gap. Connect to an Electrum server, scan a range of addresses, and sweep all funds to a destination address in a single transaction.

**Built by developers at [Bull Bitcoin](https://bullbitcoin.com) & [Liana](https://wizardsardine.com/liana/).**

---

## Use Case

Standard wallets stop scanning for funds after a certain gap (usually 20–100 consecutive unused addresses). If your wallet skipped many addresses — due to bugs, multiple device restores, or unusual transaction patterns — funds can appear "lost" even though they're still on-chain.

This tool scans up to a configurable index (e.g. 10,000 or 100,000 addresses) and sweeps everything it finds into a single PSBT, which you then sign with your mnemonic and broadcast.

---

## Installation

Download the latest release for your platform from the [Releases](../../releases) page.

### macOS (Apple Silicon)

```bash
tar -xzf spk_recovery-macos-arm64.tar.gz
chmod +x spk_recovery
./spk_recovery
```

### macOS (Intel)

```bash
tar -xzf spk_recovery-macos-x86_64.tar.gz
chmod +x spk_recovery
./spk_recovery
```

### macOS: Gatekeeper Warning

Because this app is distributed via GitHub and not the Apple App Store, macOS Gatekeeper will block it on first launch. This is expected for any open-source app not signed with an Apple developer certificate. The app is safe to run — you can review the full source code in this repository.

**Step 1:** Try to open the app. macOS will block it. Open **System Settings → Privacy & Security**, scroll to the **Security** section, and click **Open Anyway**.

![macOS Privacy & Security — Open Anyway](docs/macos-gatekeeper.png)

**Step 2:** A confirmation dialog will appear. Click **Open Anyway** (not "Move to Bin").

![macOS Open Anyway dialog](docs/macos-gatekeeper-dialog.png)

After this, the app will open normally on all future launches.

### Linux (x86_64)

```bash
tar -xzf spk_recovery-linux-x86_64.tar.gz
chmod +x spk_recovery
./spk_recovery
```

### Windows (x86_64)

Extract `spk_recovery-windows-x86_64.zip` and run `spk_recovery.exe`.

> Windows Defender may flag the binary. Click **More info → Run anyway** if prompted.

---

## How to Use

### 1. Descriptor

Paste your wallet descriptor. It must be in the multipath format:

```
wpkh([fingerprint/84'/0'/0']xpub.../<0;1>/*)
```

The `/<0;1>/*` part tells the tool to scan both receive and change addresses.

**Not sure about the format?** Leave the **"Auto-format descriptor"** checkbox enabled. The tool will:
- Strip the checksum (the `#xxxxxxxx` suffix)
- Replace `/0/*` with `/<0;1>/*`

This covers the most common case (BIP84 native SegWit wallets exported from Sparrow, Electrum, or similar).

### 2. Target Index

Set this to the highest address index you think funds might be at. If you're unsure, use a large value like `10000` or `50000`. Higher values mean a longer scan.

| Scenario | Suggested Target |
|---|---|
| Normal gap, probably fine | 1,000 |
| Known large gap | 10,000 |
| Very unusual history | 50,000–100,000 |

### 3. Electrum Server

The default (`ssl://fulcrum.bullbitcoin.com:50002`) is a public server. You can point it to your own Electrum / Fulcrum node.

### 4. Destination Address

The Bitcoin address where **all** recovered funds will be sent. Make sure this is an address you control.

### 5. Fee Rate

Satoshis per virtual byte. Check [mempool.space](https://mempool.space) for current rates. `1` sat/vB is fine for non-urgent sweeps.

![SPK Recovery Tool — Sync Config](docs/app-screenshot.png)

The three fields you'll need to fill in are **Descriptor**, **Destination Address**, and **Target Index** if your gap is larger than the default. Everything else can stay as-is.

### 6. Sync & Create PSBT

Click **Sync & Create PSBT**. The tool will:
1. Scan all receive and change addresses up to the target index
2. Fetch transaction history for every funded address
3. Build a PSBT spending all unspent outputs to your destination address

The config panel folds automatically and progress appears in the log panel.

### 7. Sign & Broadcast

Once the PSBT is ready, the **Sign & Broadcast** panel appears at the bottom showing:
- Number of inputs
- Output address and amount
- Estimated fees

Enter your **BIP39 mnemonic** (12 or 24 words) and click **Sign PSBT**, then **Broadcast**.

> Your mnemonic never leaves your machine. Signing is done locally.

---

## Building from Source

Requires [Rust](https://rustup.rs/) stable.

```bash
git clone https://github.com/i5hi/spk_recovery
cd spk_recovery
cargo build --release
./target/release/spk_recovery
```

---

## License

MIT
