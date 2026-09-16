//! Dev tool: sign a `compme://` deep link for the host's trusted key.
//!
//! Usage: cargo run -p webconfig --example sign_link -- <seed-hex-64> <url>
//!        [ttl-seconds]
//! Prints the verifying (public) key hex — the host's `COMPME_TRUSTED_KEY` —
//! and the URL with `&exp=<unix seconds>` and the trailing `&sig=` appended.
//! The signer stamps the expiry because a signed link without one is rejected
//! (G14): `exp` must sit inside the signed prefix, so it is added BEFORE
//! signing. `ttl-seconds` defaults to 300; a URL that already carries `exp=`
//! is signed as given. Deterministic from the seed apart from `exp`: keep real
//! seeds out of the repo.

use ed25519_dalek::{Signer, SigningKey};

const DEFAULT_TTL_SECS: u64 = 300;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(seed_hex), Some(url)) = (args.next(), args.next()) else {
        eprintln!("usage: sign_link <seed-hex-64> <url> [ttl-seconds]");
        std::process::exit(2);
    };
    let ttl = match args.next() {
        None => DEFAULT_TTL_SECS,
        Some(raw) => match raw.parse::<u64>() {
            Ok(ttl) => ttl,
            Err(_) => {
                eprintln!("ttl-seconds must be a whole number of seconds");
                std::process::exit(2);
            }
        },
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
    // Stamp the deadline inside the payload, then sign the whole thing: an
    // `exp` appended after `&sig=` would be unsigned and rejected as misplaced.
    let payload = if url.contains("exp=") {
        url
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before the unix epoch")
            .as_secs();
        format!("{url}&exp={}", now.saturating_add(ttl))
    };
    let sig = key.sign(payload.as_bytes());
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    println!("trusted-key: {}", hex(key.verifying_key().as_bytes()));
    println!("signed-url:  {payload}&sig={}", hex(&sig.to_bytes()));
}
