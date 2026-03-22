use sha2::{Digest, Sha256};
use std::env;

fn main() {
    let input = env::args().nth(1).unwrap_or_else(|| "hello".to_string());
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!("{hex}");
}
