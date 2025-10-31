# Util to recover wallet w/ huge gap limits

This tool will connect to an electrum server, scan spks up to `<TARGET>` derivation
index & craft a sweep PSBT sweeping all coins to `<ADDRESS>` using `<FEE>` sats/vb fees.

## Usage:
```
Usage: spk_recovery [OPTIONS] -d <DESCRIPTOR> -i <IP> -p <PORT> -t <TARGET> -a <ADDRESS>

Options:
  -d, --descriptor <DESCRIPTOR>  Path to the file containing the descriptor,
   should be of form: wpkh([<fingerprint><deriv_path>]<xpub>/<0;1>/*)
  -i, --ip <IP>                  Ip of the electrum server
  -p, --port <PORT>              Port of the electrum server
  -t, --target <TARGET>          Target derivation index
  -a, --address <ADDRESS>        Address where the coins will be spent
  -m, --max <MAX>                Max subscription accepted by the server for
   each connections [default: 10000]
  -b, --batch <BATCH>            How many spk we ask for each requests Max
   subscription accepted by the server for each connections [default: 100]
  -f, --fee <FEE>                Fee rate in sats/vb [default: 1]
  -h, --help                     Print help
```

