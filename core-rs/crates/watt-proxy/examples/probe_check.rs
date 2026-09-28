//! See what a candidate address replies, and what the parser makes of it.
//!
//! Run: cargo run -p watt-proxy --example probe_check
//!
//! The address is a variable on purpose. Pointing it at a real GitHub address
//! shows a `Covers` verdict; pointing it at any unrelated HTTPS server shows
//! `DoesNotCover` with that server's actual names, which is how the parser was
//! confirmed to be reading the certificate rather than guessing. The default is
//! the GitHub address `github.com` resolves to on this network.

fn main() {
    let host = "github.com";
    let address = "20.205.243.166:443".parse().unwrap();

    let hello = watt_net::probe::debug_hello(host);
    println!("ClientHello: {} bytes", hello.len());

    match watt_net::probe::debug_flight(address, host) {
        Ok(reply) => {
            println!("reply: {} bytes", reply.len());
            // Walk the records so the handshake messages are visible.
            let mut offset = 0usize;
            while offset + 5 <= reply.len() {
                let kind = reply[offset];
                let len = ((reply[offset + 3] as usize) << 8) | reply[offset + 4] as usize;
                if kind == 0x16 && len >= 4 {
                    let inner = reply[offset + 5];
                    let inner_len = ((reply[offset + 6] as usize) << 16)
                        | ((reply[offset + 7] as usize) << 8)
                        | reply[offset + 8] as usize;
                    println!(
                        "  record 0x{kind:02x} len={len} -> handshake 0x{inner:02x} len={inner_len} (0x02 ServerHello, 0x0b Certificate)"
                    );
                } else {
                    println!("  record 0x{kind:02x} len={len}");
                }
                offset += 5 + len;
            }
            println!("parsed names: {:?}", watt_net::probe::debug_names(&reply));
        }
        Err(err) => println!("no reply: {err}"),
    }

    println!("verdict: {:?}", watt_net::probe::check(address, host));
}
