//! Dev tool: sign a `compme://` deep link for the host's trusted key.
//!
//! Usage: cargo run -p webconfig --example sign_link -- <seed-hex-64> <url>
//! Prints the verifying (public) key hex — the host's `COMPME_TRUSTED_KEY` —
//! and the URL with the trailing `&sig=` appended. Deterministic from the
//! seed: keep real seeds out of the repo.

use ed25519_dalek::{Signer, SigningKey};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(seed_hex), Some(url)) = (args.next(), args.next()) else {
        eprintln!("usage: sign_link <seed-hex-64> <url>");
        std::process::exit(2);
    };
    if seed_hex.len() != 64 || !seed_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        eprintln!("seed must be exactly 64 hex chars");
        std::process::exit(2);
    }
    let seed: [u8; 32] = (0..32)
        .map(|i| u8::from_str_radix(&seed_hex[i * 2..i * 2 + 2], 16).expect("hex seed"))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let key = SigningKey::from_bytes(&seed);
    let sig = key.sign(url.as_bytes());
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    println!("trusted-key: {}", hex(key.verifying_key().as_bytes()));
    println!("signed-url:  {url}&sig={}", hex(&sig.to_bytes()));
}
