//! Proof size optimisation — run-length encoding compression for proof bytes.
//!
//! Proofs are typically ~300+ bytes of mostly-structured data with repeated
//! byte patterns. This module implements a simple RLE scheme suited to the
//! `no_std` / Soroban environment:
//!
//! ```text
//! Compressed format:
//!   [MAGIC: 0xC0]               — first byte, marks stream as compressed
//!   ([count: u8][byte: u8])*    — each run: "count" repetitions of "byte"
//!                                 count is always 1..=255 (never 0)
//! ```
//!
//! # Errors
//!
//! - [`CompressionError::EmptyInput`]     — input is empty.
//! - [`CompressionError::InvalidData`]    — magic missing, zero-count, or truncated.
//! - [`CompressionError::OutputTooLarge`] — output exceeds `max_output_size`.

use soroban_sdk::{contracterror, Bytes, Env};

/// First byte of every compressed stream.
const MAGIC: u8 = 0xC0;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum CompressionError {
    /// Input bytes were empty.
    EmptyInput = 100,
    /// Compressed data is malformed (bad magic, zero-count run, or truncated).
    InvalidData = 101,
    /// Decompressed output would exceed the allowed size limit.
    OutputTooLarge = 102,
}

/// RLE-compress `input` and return the compressed [`Bytes`].
///
/// The first byte of the output is always [`MAGIC`] (`0xC0`) so that
/// [`decompress_proof`] can verify it received a compressed stream.
pub fn compress_proof(env: &Env, input: &Bytes) -> Result<Bytes, CompressionError> {
    if input.is_empty() {
        return Err(CompressionError::EmptyInput);
    }

    let len = input.len();
    let mut out = Bytes::new(env);
    out.push_back(MAGIC);

    let mut i: u32 = 0;
    while i < len {
        let current = input.get(i).unwrap();
        let mut run: u32 = 1;
        while run < 255 && (i + run) < len && input.get(i + run).unwrap() == current {
            run += 1;
        }
        out.push_back(run as u8);
        out.push_back(current);
        i += run;
    }

    Ok(out)
}

/// RLE-decompress `input` (must start with [`MAGIC`]) and return raw bytes.
///
/// `max_output_size` guards against decompression bombs — pass `MAX_PROOF_SIZE`
/// (4096) from the calling contract.
pub fn decompress_proof(
    env: &Env,
    input: &Bytes,
    max_output_size: u32,
) -> Result<Bytes, CompressionError> {
    if input.is_empty() {
        return Err(CompressionError::EmptyInput);
    }
    if input.get(0).unwrap() != MAGIC {
        return Err(CompressionError::InvalidData);
    }

    let compressed_len = input.len();
    let mut out = Bytes::new(env);

    let mut i: u32 = 1;
    while i < compressed_len {
        // Need two bytes for a complete (count, byte) pair.
        if i + 1 >= compressed_len {
            return Err(CompressionError::InvalidData);
        }
        let count = input.get(i).unwrap();
        if count == 0 {
            return Err(CompressionError::InvalidData);
        }
        let byte = input.get(i + 1).unwrap();
        for _ in 0..count {
            if out.len() >= max_output_size {
                return Err(CompressionError::OutputTooLarge);
            }
            out.push_back(byte);
        }
        i += 2;
    }

    Ok(out)
}
