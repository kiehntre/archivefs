//! Shared, platform-agnostic decoding helpers for Dolphin-targeted
//! Action Replay / Gecko hex-pair cheat bodies.
//!
//! Both the BSFree GameCube and BSFree Wii classifiers parse the same
//! underlying `XXXXXXXX YYYY` Action Replay hex-pair grammar and the `04XXXXXX`
//! Gecko write form, and both write into the same Dolphin `GameSettings`
//! structure (`[Gecko]` / `[ActionReplay]`). Keeping the per-line decoding
//! here - rather than duplicated per platform module - is what lets a
//! platform-parameterized duplicate/conflict analyser fingerprint code
//! bodies the same way regardless of which provider produced them.
//!
//! Nothing in this module touches a filesystem or an emulator configuration.

use std::fmt;

/// Per-line Action Replay command family, decoded from the first word's bit
/// fields exactly as Dolphin's `ActionReplay.cpp` does.
///
/// This is deliberately shared verbatim with the GameCube classifier's own
/// decoding: GameCube and Wii share Dolphin's Action Replay engine, so the
/// byte-identity reasoning that is safe for GameCube is the same reasoning
/// that is safe for Wii.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArLineFamily {
    Write8,
    Write16,
    Write32,
    WriteFloat,
    WritePointer,
    AddCode,
    MasterCode,
    Conditional,
    ZeroCode,
    SelfModifying,
    Malformed,
}

impl ArLineFamily {
    /// A simple direct RAM write whose address, size and value are all
    /// provable from the line alone. Pointer writes, add codes, conditionals,
    /// master/zero/self-modifying codes and malformed lines are never here -
    /// for those the target address cannot be proven, so they can never
    /// participate in a `ConflictingMemoryWrite` finding.
    pub const fn is_direct_write(self) -> bool {
        matches!(
            self,
            Self::Write8 | Self::Write16 | Self::Write32 | Self::WriteFloat
        )
    }
}

impl fmt::Display for ArLineFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Write8 => "8-bit write",
            Self::Write16 => "16-bit write",
            Self::Write32 => "32-bit write",
            Self::WriteFloat => "float write",
            Self::WritePointer => "pointer write",
            Self::AddCode => "add code",
            Self::MasterCode => "master code",
            Self::Conditional => "conditional",
            Self::ZeroCode => "zero code",
            Self::SelfModifying => "self-modifying",
            Self::Malformed => "malformed",
        })
    }
}

/// Decodes one `XXXXXXXX YYYY` hex-pair line under the GameCube/Wii Action
/// Replay bit layout (`subtype:2 | type:3 | size:2 | gcaddr:25`).
pub fn ar_line_family(line: &str) -> ArLineFamily {
    let mut pieces = line.split_whitespace();
    let (Some(first), Some(second)) = (pieces.next(), pieces.next()) else {
        return ArLineFamily::Malformed;
    };
    if pieces.next().is_some() {
        return ArLineFamily::Malformed;
    }
    if first.len() != 8
        || second.len() != 8
        || !first.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !second.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return ArLineFamily::Malformed;
    }
    let word = u32::from_str_radix(first, 16).unwrap_or(0);
    if word == 0 {
        return ArLineFamily::ZeroCode;
    }
    if (0x2000..0x3000).contains(&word) {
        return ArLineFamily::SelfModifying;
    }
    let subtype = (word >> 30) & 0b11;
    let code_type = (word >> 27) & 0b111;
    let size = (word >> 25) & 0b11;
    match code_type {
        0 => match subtype {
            0 => match size {
                0 => ArLineFamily::Write8,
                1 => ArLineFamily::Write16,
                2 => ArLineFamily::Write32,
                _ => ArLineFamily::WriteFloat,
            },
            1 => ArLineFamily::WritePointer,
            2 => ArLineFamily::AddCode,
            _ => ArLineFamily::MasterCode,
        },
        1..=7 => ArLineFamily::Conditional,
        _ => ArLineFamily::Malformed,
    }
}

/// Whether a `04XXXXXX` 32-bit RAM write's address fits Gecko's 24-bit
/// address field (i.e. the write lands below `0x81000000`).
pub fn is_gecko_addressable_write(word: u32) -> bool {
    let gcaddr = word & 0x01FF_FFFF;
    gcaddr < 0x0100_0000
}

/// Whether a line is a strict `XXXXXXXX YYYY` hex-pair line (the only shape
/// Dolphin's `[Gecko]`/`[ActionReplay]` bodies accept).
pub fn strict_code_line(line: &str) -> bool {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    tokens.len() == 2
        && tokens
            .iter()
            .all(|token| token.len() == 8 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Whether a line carries replaceable/placeholder tokens (`?`, `X`, `Y`, `Z`).
pub fn contains_placeholder(line: &str) -> bool {
    let compact = line
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    compact
        .chars()
        .any(|character| matches!(character.to_ascii_uppercase(), '?' | 'X' | 'Y' | 'Z'))
}

/// Lowercase hex encoding of `bytes`, used for canonical digests across the
/// Dolphin-targeted cheat modules. Shared so all providers fingerprint the
/// same way.
pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One provable direct memory write derived from a canonical hex-pair line.
///
/// Addresses are normalized to Dolphin's physical view (`0x80000000 | gcaddr`
/// for Action Replay direct writes), which for the `GeckoEquivalent` subset is
/// byte-identical to Gecko's own `0x80000000 + offset` form - so an AR write
/// and a Gecko write of the same value to the same address fingerprint the
/// same. The value is the raw second word (for float writes, the IEEE-754
/// bit pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryOperation {
    pub address: u32,
    /// Size in bytes: 1, 2 or 4.
    pub size: u8,
    pub value: u32,
}

/// Derives the provable direct-write operations of one canonical code body.
///
/// Only `Write8`/`Write16`/`Write32`/`WriteFloat` lines contribute; every
/// other line family either cannot prove its target address (pointer, add,
/// conditional) or is not a write at all (master/zero/self-modifying), so it
/// is excluded rather than guessed at. An empty result means "no provable
/// direct writes" - such a code can never be the subject of a conflicting
/// write finding.
///
/// `Write8`/`Write16` are Dolphin's "RAM write and fill" subtype
/// (`Subtype_RamWriteAndFill` in `ActionReplay.cpp`): the low 8/16 bits of
/// the second word are the value actually written, and the *upper* bits are
/// a repeat count - the same value is written to `repeat + 1` consecutive
/// addresses (stepping by 1 byte for `Write8`, 2 bytes for `Write16`). Each
/// repetition becomes its own [`MemoryOperation`] with the masked, effective
/// value - never the raw, un-masked second word, which for a nonzero repeat
/// count is not itself a valid 8/16-bit value. Address arithmetic uses
/// wrapping addition to mirror Dolphin's own unsigned 32-bit address math
/// rather than panicking on overflow. `Write32`/`WriteFloat` have no fill
/// behaviour in Dolphin - always exactly one write of the full 32-bit word -
/// and are unaffected by this.
pub fn derive_memory_operations(lines: &[String]) -> Vec<MemoryOperation> {
    let mut operations = Vec::new();
    for line in lines {
        let family = ar_line_family(line);
        if !family.is_direct_write() {
            continue;
        }
        let tokens = line.split_whitespace().collect::<Vec<_>>();
        let (Some(first), Some(second)) = (tokens.first(), tokens.get(1)) else {
            continue;
        };
        let (Ok(word), Ok(data)) = (
            u32::from_str_radix(first, 16),
            u32::from_str_radix(second, 16),
        ) else {
            continue;
        };
        let gcaddr = word & 0x01FF_FFFF;
        let base_address = gcaddr | 0x8000_0000;
        match family {
            ArLineFamily::Write8 => {
                let value = data & 0xFF;
                let repeat = data >> 8;
                for offset in 0..=repeat {
                    operations.push(MemoryOperation {
                        address: base_address.wrapping_add(offset),
                        size: 1,
                        value,
                    });
                }
            }
            ArLineFamily::Write16 => {
                let value = data & 0xFFFF;
                let repeat = data >> 16;
                for index in 0..=repeat {
                    operations.push(MemoryOperation {
                        address: base_address.wrapping_add(index.wrapping_mul(2)),
                        size: 2,
                        value,
                    });
                }
            }
            ArLineFamily::Write32 | ArLineFamily::WriteFloat => {
                operations.push(MemoryOperation {
                    address: base_address,
                    size: 4,
                    value: data,
                });
            }
            _ => {}
        }
    }
    operations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_write_families_decode_to_provable_memory_operations() {
        // 04XXXXXX 32-bit write (GeckoEquivalent shape): gcaddr = 0x002318AC,
        // address = 0x80000000 | gcaddr, matching Gecko's 0x80000000+offset.
        let ops = derive_memory_operations(&["042318AC 3B8003E7".to_string()]);
        assert_eq!(
            ops,
            vec![MemoryOperation {
                address: 0x8023_18AC,
                size: 4,
                value: 0x3B80_03E7,
            }]
        );
        // 16-bit write.
        let ops = derive_memory_operations(&["0224CD50 00003E7F".to_string()]);
        assert_eq!(
            ops,
            vec![MemoryOperation {
                address: 0x8024_CD50,
                size: 2,
                value: 0x3E7F,
            }]
        );
        // 8-bit write.
        let ops = derive_memory_operations(&["0024CD50 0000003F".to_string()]);
        assert_eq!(
            ops,
            vec![MemoryOperation {
                address: 0x8024_CD50,
                size: 1,
                value: 0x3F,
            }]
        );
    }

    #[test]
    fn a_write8_repeat_count_expands_into_one_operation_per_byte_with_the_masked_value() {
        // Dolphin's Subtype_RamWriteAndFill: low byte 0x02 is the value,
        // upper 24 bits (0x000003) are "3 additional repeats", so the byte
        // is written to 4 consecutive addresses starting at 0x8024CD50. The
        // raw second word (0x00000302) must never appear as `value` - only
        // the masked, effective byte.
        let ops = derive_memory_operations(&["0024CD50 00000302".to_string()]);
        assert_eq!(
            ops,
            vec![
                MemoryOperation {
                    address: 0x8024_CD50,
                    size: 1,
                    value: 0x02,
                },
                MemoryOperation {
                    address: 0x8024_CD51,
                    size: 1,
                    value: 0x02,
                },
                MemoryOperation {
                    address: 0x8024_CD52,
                    size: 1,
                    value: 0x02,
                },
                MemoryOperation {
                    address: 0x8024_CD53,
                    size: 1,
                    value: 0x02,
                },
            ]
        );
    }

    #[test]
    fn a_write16_repeat_count_expands_into_one_operation_per_halfword_with_the_masked_value() {
        // Low 16 bits 0x0055 is the value, upper 16 bits (0x0002) is "2
        // additional repeats" at 2-byte strides, so 3 halfword writes at
        // 0x8024CD50, 0x8024CD52, 0x8024CD54.
        let ops = derive_memory_operations(&["0224CD50 00020055".to_string()]);
        assert_eq!(
            ops,
            vec![
                MemoryOperation {
                    address: 0x8024_CD50,
                    size: 2,
                    value: 0x0055,
                },
                MemoryOperation {
                    address: 0x8024_CD52,
                    size: 2,
                    value: 0x0055,
                },
                MemoryOperation {
                    address: 0x8024_CD54,
                    size: 2,
                    value: 0x0055,
                },
            ]
        );
    }

    #[test]
    fn write8_repeat_steps_correctly_across_the_top_of_the_25_bit_address_field() {
        // gcaddr is masked to 25 bits before the physical `| 0x8000_0000` is
        // applied, so the largest representable base address is
        // 0x81FFFFFF; stepping through a repeat count here must land on the
        // exact expected addresses without panicking (address arithmetic
        // uses wrapping addition throughout, matching Dolphin's own
        // unsigned 32-bit math, even though this specific input cannot
        // reach a genuine u32 wraparound given the 25-bit address mask and
        // the 24-bit maximum repeat count for Write8).
        let ops = derive_memory_operations(&["01FFFFFE 00000201".to_string()]);
        assert_eq!(
            ops,
            vec![
                MemoryOperation {
                    address: 0x81FF_FFFE,
                    size: 1,
                    value: 0x01,
                },
                MemoryOperation {
                    address: 0x81FF_FFFF,
                    size: 1,
                    value: 0x01,
                },
                MemoryOperation {
                    address: 0x8200_0000,
                    size: 1,
                    value: 0x01,
                },
            ]
        );
    }

    #[test]
    fn pointer_add_conditional_and_master_lines_contribute_no_operations() {
        for line in [
            // Pointer write (type 0 / subtype 1).
            "1024CD50 00000008",
            // Add code (type 0 / subtype 2).
            "2024CD50 00000001",
            // Master code (type 0 / subtype 3).
            "3024CD50 00000001",
            // Conditional (type 1..=7).
            "48000002 00000001",
            // Zero code.
            "00000000 00000001",
            // Self-modifying.
            "20000002 00000000",
        ] {
            assert!(
                derive_memory_operations(&[line.to_string()]).is_empty(),
                "{line} must not produce a provable write"
            );
        }
    }

    #[test]
    fn malformed_and_placeholder_lines_contribute_no_operations() {
        assert!(derive_memory_operations(&["not a code".to_string()]).is_empty());
        assert!(derive_memory_operations(&["042318AC".to_string()]).is_empty());
        assert!(derive_memory_operations(&["XR7M-X292-DZ418".to_string()]).is_empty());
    }
}
