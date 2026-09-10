//! Credential metadata compression using a MessagePack extension envelope.
//!
//! The extension payload uses a small PackBits-style encoding and selects
//! between direct bytes and delta bytes. Inputs that would not become smaller
//! are returned unchanged, preserving compatibility with existing metadata.

use soroban_sdk::{contracterror, Bytes, Env};

/// Maximum supported uncompressed credential metadata size.
pub const MAX_METADATA_SIZE: u32 = 4096;

const MESSAGEPACK_EXT8: u8 = 0xC7;
const MESSAGEPACK_EXT16: u8 = 0xC8;
const HEIRLOOM_METADATA_TYPE: u8 = 0x45;
const FORMAT_VERSION: u8 = 1;
const MODE_DIRECT: u8 = 0;
const MODE_DELTA: u8 = 1;
const LEGACY_MAGIC: u8 = 0xC1;
const MAX_LITERAL_LENGTH: u32 = 128;
const MAX_REPEAT_LENGTH: u32 = 130;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum CompressionError {
    /// The MessagePack extension or its block stream is malformed.
    InvalidCompressedData = 200,
    /// Decompressed output would exceed the metadata size limit.
    OutputTooLarge = 201,
}

/// Compress metadata into the Heirloom MessagePack extension format.
///
/// The original bytes are returned when the input is empty, exceeds the
/// metadata limit, or would not become smaller after framing.
pub fn compress_metadata(env: &Env, metadata: &Bytes) -> Bytes {
    if metadata.is_empty() || metadata.len() > MAX_METADATA_SIZE {
        return metadata.clone();
    }

    let direct = encode_blocks(env, metadata, MODE_DIRECT);
    let delta = encode_blocks(env, metadata, MODE_DELTA);
    let payload = if delta.len() < direct.len() {
        delta
    } else {
        direct
    };
    let framed = frame_messagepack_extension(env, &payload);

    if framed.len() < metadata.len() {
        framed
    } else {
        metadata.clone()
    }
}

/// Decompress MessagePack-framed, legacy, or uncompressed metadata.
///
/// Ordinary uncompressed bytes are returned unchanged. The legacy `0xC1`
/// delta/RLE format remains readable for backwards compatibility.
pub fn decompress_metadata(env: &Env, metadata: &Bytes) -> Result<Bytes, CompressionError> {
    if let Some((payload_start, payload_end, mode)) = parse_messagepack_header(metadata)? {
        return decode_blocks(env, metadata, payload_start, payload_end, mode);
    }

    if is_legacy_compressed(metadata) {
        return decompress_legacy(env, metadata);
    }

    Ok(metadata.clone())
}

/// Return whether metadata uses the current or legacy compressed format.
pub fn is_compressed(metadata: &Bytes) -> bool {
    matches!(parse_messagepack_header(metadata), Ok(Some(_))) || is_legacy_compressed(metadata)
}

fn encode_blocks(env: &Env, metadata: &Bytes, mode: u8) -> Bytes {
    let mut payload = Bytes::new(env);
    payload.push_back(FORMAT_VERSION);
    payload.push_back(mode);

    let mut index = 0u32;
    while index < metadata.len() {
        let repeated = repeat_length(metadata, index, mode);
        if repeated >= 3 {
            payload.push_back(0x80 | ((repeated - 3) as u8));
            payload.push_back(transformed_byte(metadata, index, mode));
            index += repeated;
            continue;
        }

        let literal_start = index;
        let mut literal_length = 0u32;
        while index < metadata.len() && literal_length < MAX_LITERAL_LENGTH {
            if literal_length > 0 && repeat_length(metadata, index, mode) >= 3 {
                break;
            }
            index += 1;
            literal_length += 1;
        }

        payload.push_back((literal_length - 1) as u8);
        for literal_index in literal_start..(literal_start + literal_length) {
            payload.push_back(transformed_byte(metadata, literal_index, mode));
        }
    }

    payload
}

fn repeat_length(metadata: &Bytes, start: u32, mode: u8) -> u32 {
    let expected = transformed_byte(metadata, start, mode);
    let mut length = 1u32;
    while length < MAX_REPEAT_LENGTH
        && start + length < metadata.len()
        && transformed_byte(metadata, start + length, mode) == expected
    {
        length += 1;
    }
    length
}

fn transformed_byte(metadata: &Bytes, index: u32, mode: u8) -> u8 {
    let current = metadata.get(index).unwrap();
    if mode == MODE_DELTA {
        let previous = if index == 0 {
            0
        } else {
            metadata.get(index - 1).unwrap()
        };
        current.wrapping_sub(previous)
    } else {
        current
    }
}

fn frame_messagepack_extension(env: &Env, payload: &Bytes) -> Bytes {
    let mut framed = Bytes::new(env);
    if payload.len() <= u8::MAX as u32 {
        framed.push_back(MESSAGEPACK_EXT8);
        framed.push_back(payload.len() as u8);
    } else {
        framed.push_back(MESSAGEPACK_EXT16);
        framed.push_back((payload.len() >> 8) as u8);
        framed.push_back(payload.len() as u8);
    }
    framed.push_back(HEIRLOOM_METADATA_TYPE);
    framed.append(payload);
    framed
}

fn parse_messagepack_header(metadata: &Bytes) -> Result<Option<(u32, u32, u8)>, CompressionError> {
    if metadata.is_empty() {
        return Ok(None);
    }

    let (payload_start, payload_length) = match metadata.get(0).unwrap() {
        MESSAGEPACK_EXT8
            if metadata.len() >= 3 && metadata.get(2).unwrap() == HEIRLOOM_METADATA_TYPE =>
        {
            (3u32, metadata.get(1).unwrap() as u32)
        }
        MESSAGEPACK_EXT16
            if metadata.len() >= 4 && metadata.get(3).unwrap() == HEIRLOOM_METADATA_TYPE =>
        {
            let length = ((metadata.get(1).unwrap() as u32) << 8) | metadata.get(2).unwrap() as u32;
            (4u32, length)
        }
        _ => return Ok(None),
    };

    let payload_end = payload_start
        .checked_add(payload_length)
        .ok_or(CompressionError::InvalidCompressedData)?;
    if payload_length < 2 || payload_end != metadata.len() {
        return Err(CompressionError::InvalidCompressedData);
    }

    let version = metadata.get(payload_start).unwrap();
    let mode = metadata.get(payload_start + 1).unwrap();
    if version != FORMAT_VERSION || (mode != MODE_DIRECT && mode != MODE_DELTA) {
        return Err(CompressionError::InvalidCompressedData);
    }

    Ok(Some((payload_start + 2, payload_end, mode)))
}

fn decode_blocks(
    env: &Env,
    encoded: &Bytes,
    mut index: u32,
    end: u32,
    mode: u8,
) -> Result<Bytes, CompressionError> {
    let mut output = Bytes::new(env);
    let mut previous = 0u8;

    while index < end {
        let token = encoded.get(index).unwrap();
        index += 1;

        if token & 0x80 != 0 {
            if index >= end {
                return Err(CompressionError::InvalidCompressedData);
            }
            let count = (token as u32 & 0x7F) + 3;
            let value = encoded.get(index).unwrap();
            index += 1;
            for _ in 0..count {
                push_decoded(&mut output, value, mode, &mut previous)?;
            }
        } else {
            let count = (token as u32 & 0x7F) + 1;
            if index + count > end {
                return Err(CompressionError::InvalidCompressedData);
            }
            for _ in 0..count {
                let value = encoded.get(index).unwrap();
                index += 1;
                push_decoded(&mut output, value, mode, &mut previous)?;
            }
        }
    }

    Ok(output)
}

fn push_decoded(
    output: &mut Bytes,
    value: u8,
    mode: u8,
    previous: &mut u8,
) -> Result<(), CompressionError> {
    if output.len() >= MAX_METADATA_SIZE {
        return Err(CompressionError::OutputTooLarge);
    }

    let decoded = if mode == MODE_DELTA {
        previous.wrapping_add(value)
    } else {
        value
    };
    output.push_back(decoded);
    *previous = decoded;
    Ok(())
}

fn is_legacy_compressed(metadata: &Bytes) -> bool {
    metadata.len() >= 2
        && metadata.get(0).unwrap() == LEGACY_MAGIC
        && matches!(metadata.get(1).unwrap(), MODE_DIRECT | MODE_DELTA)
}

fn decompress_legacy(env: &Env, metadata: &Bytes) -> Result<Bytes, CompressionError> {
    let mode = metadata.get(1).unwrap();
    if mode == MODE_DELTA {
        let mut output = Bytes::new(env);
        let mut previous = 0u8;
        for index in 2..metadata.len() {
            push_decoded(
                &mut output,
                metadata.get(index).unwrap(),
                MODE_DELTA,
                &mut previous,
            )?;
        }
        return Ok(output);
    }

    let mut output = Bytes::new(env);
    let mut index = 2u32;
    let mut previous = 0u8;
    while index < metadata.len() {
        if index + 1 >= metadata.len() {
            return Err(CompressionError::InvalidCompressedData);
        }
        let count = metadata.get(index).unwrap();
        let value = metadata.get(index + 1).unwrap();
        if count == 0 {
            return Err(CompressionError::InvalidCompressedData);
        }
        for _ in 0..count {
            push_decoded(&mut output, value, MODE_DIRECT, &mut previous)?;
        }
        index += 2;
    }

    Ok(output)
}
