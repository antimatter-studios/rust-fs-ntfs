//! Admission of a saved change-journal cursor against the live journal.
//!
//! The journal's identity and bounds come from Microsoft's documented
//! `USN_JOURNAL_DATA_V0` structure (winioctl.h), the reply to
//! `FSCTL_QUERY_USN_JOURNAL`: seven little-endian 64-bit fields,
//! `UsnJournalID`, `FirstUsn`, `NextUsn`, `LowestValidUsn`, `MaxUsn`,
//! `MaximumSize` and `AllocationDelta`. The later V1 and V2 forms extend
//! that prefix, so the first 56 bytes decode the same way for all three.
//!
//! A cursor is the full 64-bit journal identity plus the next USN the
//! caller wants. Admission never answers with an empty batch for a cursor
//! it cannot serve: a different journal, records already purged, a cursor
//! past `NextUsn` and self-contradictory bounds each have their own state.
//! Nothing here reads or writes a volume.

/// Bytes of the `USN_JOURNAL_DATA_V0` structure.
pub const USN_JOURNAL_DATA_V0_LEN: usize = 56;

/// The journal identity and bounds `FSCTL_QUERY_USN_JOURNAL` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsnJournalData {
    /// `UsnJournalID`: changes whenever the journal is deleted and recreated.
    pub journal_id: u64,
    /// `FirstUsn`: the lowest USN still readable from the journal.
    pub first_usn: i64,
    /// `NextUsn`: the USN the next record will receive.
    pub next_usn: i64,
    /// `LowestValidUsn`: the lowest USN that belongs to this journal instance.
    pub lowest_valid_usn: i64,
    /// `MaxUsn`: the largest USN this journal will assign.
    pub max_usn: i64,
    pub maximum_size: u64,
    pub allocation_delta: u64,
}

/// A buffer too short to hold `USN_JOURNAL_DATA_V0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsnJournalDataTruncated {
    pub available: usize,
}

impl UsnJournalData {
    /// Decodes the documented `USN_JOURNAL_DATA_V0` prefix of `buf`.
    pub fn decode_v0(buf: &[u8]) -> Result<Self, UsnJournalDataTruncated> {
        if buf.len() < USN_JOURNAL_DATA_V0_LEN {
            return Err(UsnJournalDataTruncated {
                available: buf.len(),
            });
        }
        let q = |i: usize| u64::from_le_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap());
        Ok(Self {
            journal_id: q(0),
            first_usn: q(1) as i64,
            next_usn: q(2) as i64,
            lowest_valid_usn: q(3) as i64,
            max_usn: q(4) as i64,
            maximum_size: q(5),
            allocation_delta: q(6),
        })
    }

    /// The first bound the journal data itself breaks, if any.
    pub fn bound_violation(&self) -> Option<InvalidBound> {
        if self.first_usn < 0 || self.lowest_valid_usn < 0 {
            return Some(InvalidBound::NegativeUsn);
        }
        if self.first_usn > self.next_usn {
            return Some(InvalidBound::FirstAfterNext);
        }
        if self.lowest_valid_usn > self.next_usn {
            return Some(InvalidBound::LowestValidAfterNext);
        }
        if self.next_usn > self.max_usn {
            return Some(InvalidBound::NextAfterMax);
        }
        None
    }
}

/// Which documented bound a `UsnJournalData` contradicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidBound {
    NegativeUsn,
    FirstAfterNext,
    LowestValidAfterNext,
    NextAfterMax,
}

/// A saved read position: the journal it belongs to and the next USN wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsnCursor {
    pub journal_id: u64,
    pub next_usn: i64,
}

/// What a cursor may do against the journal as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAdmission {
    /// Records `from..to` (exclusive of `to`) can be read.
    Admitted { from: i64, to: i64 },
    /// The cursor is at `NextUsn`: nothing new, and nothing lost.
    UpToDate { next_usn: i64 },
    /// The journal was recreated; every saved position is meaningless.
    JournalReset { cursor_id: u64, journal_id: u64 },
    /// The records from the cursor up to `first_usn` are gone.
    Wrapped { cursor_usn: i64, first_usn: i64 },
    /// The cursor predates this journal instance's `LowestValidUsn`.
    BeforeLowestValid {
        cursor_usn: i64,
        lowest_valid_usn: i64,
    },
    /// The cursor is beyond `NextUsn`: it was not issued by this journal.
    Future { cursor_usn: i64, next_usn: i64 },
    /// The journal data contradicts its own documented bounds.
    InvalidJournal(InvalidBound),
    /// The cursor holds a negative USN.
    InvalidCursor { cursor_usn: i64 },
}

/// Decides whether `cursor` can resume reading `journal`.
pub fn admit_cursor(cursor: &UsnCursor, journal: &UsnJournalData) -> CursorAdmission {
    if let Some(bound) = journal.bound_violation() {
        return CursorAdmission::InvalidJournal(bound);
    }
    if cursor.journal_id != journal.journal_id {
        return CursorAdmission::JournalReset {
            cursor_id: cursor.journal_id,
            journal_id: journal.journal_id,
        };
    }
    let usn = cursor.next_usn;
    if usn < 0 {
        return CursorAdmission::InvalidCursor { cursor_usn: usn };
    }
    if usn > journal.next_usn {
        return CursorAdmission::Future {
            cursor_usn: usn,
            next_usn: journal.next_usn,
        };
    }
    if usn < journal.lowest_valid_usn {
        return CursorAdmission::BeforeLowestValid {
            cursor_usn: usn,
            lowest_valid_usn: journal.lowest_valid_usn,
        };
    }
    if usn < journal.first_usn {
        return CursorAdmission::Wrapped {
            cursor_usn: usn,
            first_usn: journal.first_usn,
        };
    }
    if usn == journal.next_usn {
        return CursorAdmission::UpToDate { next_usn: usn };
    }
    CursorAdmission::Admitted {
        from: usn,
        to: journal.next_usn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: u64 = 0x01DB_1F2E_3D4C_5B6A;

    fn journal() -> UsnJournalData {
        UsnJournalData {
            journal_id: ID,
            first_usn: 0x1000,
            next_usn: 0x9000,
            lowest_valid_usn: 0x800,
            max_usn: 0x7FFF_FFFF_FFFF_0000,
            maximum_size: 0x0200_0000,
            allocation_delta: 0x0080_0000,
        }
    }

    fn at(usn: i64) -> UsnCursor {
        UsnCursor {
            journal_id: ID,
            next_usn: usn,
        }
    }

    #[test]
    fn v0_decodes_all_seven_fields_with_full_width() {
        let vals: [u64; 7] = [
            u64::MAX - 1,
            1,
            0x7FFF_FFFF_FFFF_FFF0,
            2,
            i64::MAX as u64,
            3,
            4,
        ];
        let mut buf: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        buf.extend_from_slice(&[2, 0, 3, 0]); // V1 tail is ignored.
        let d = UsnJournalData::decode_v0(&buf).unwrap();
        assert_eq!(d.journal_id, u64::MAX - 1);
        assert_eq!(d.first_usn, 1);
        assert_eq!(d.next_usn, 0x7FFF_FFFF_FFFF_FFF0);
        assert_eq!(d.lowest_valid_usn, 2);
        assert_eq!(d.max_usn, i64::MAX);
        assert_eq!((d.maximum_size, d.allocation_delta), (3, 4));
    }

    #[test]
    fn v0_refuses_a_short_buffer() {
        assert_eq!(
            UsnJournalData::decode_v0(&[0; 55]),
            Err(UsnJournalDataTruncated { available: 55 })
        );
    }

    #[test]
    fn cursor_inside_the_journal_is_admitted_to_next_usn() {
        assert_eq!(
            admit_cursor(&at(0x1000), &journal()),
            CursorAdmission::Admitted {
                from: 0x1000,
                to: 0x9000
            }
        );
        assert_eq!(
            admit_cursor(&at(0x8FF8), &journal()),
            CursorAdmission::Admitted {
                from: 0x8FF8,
                to: 0x9000
            }
        );
    }

    #[test]
    fn cursor_at_next_usn_is_up_to_date_not_an_empty_batch() {
        assert_eq!(
            admit_cursor(&at(0x9000), &journal()),
            CursorAdmission::UpToDate { next_usn: 0x9000 }
        );
    }

    #[test]
    fn identity_differing_only_in_the_high_bits_is_a_reset() {
        let cursor = UsnCursor {
            journal_id: ID ^ (1 << 63),
            next_usn: 0x1000,
        };
        assert_eq!(
            admit_cursor(&cursor, &journal()),
            CursorAdmission::JournalReset {
                cursor_id: ID ^ (1 << 63),
                journal_id: ID
            }
        );
    }

    #[test]
    fn cursor_below_first_usn_is_wrapped() {
        assert_eq!(
            admit_cursor(&at(0xFF8), &journal()),
            CursorAdmission::Wrapped {
                cursor_usn: 0xFF8,
                first_usn: 0x1000
            }
        );
    }

    #[test]
    fn cursor_below_lowest_valid_usn_is_reported_before_wrap() {
        assert_eq!(
            admit_cursor(&at(0x7F8), &journal()),
            CursorAdmission::BeforeLowestValid {
                cursor_usn: 0x7F8,
                lowest_valid_usn: 0x800
            }
        );
    }

    #[test]
    fn cursor_past_next_usn_is_future() {
        assert_eq!(
            admit_cursor(&at(0x9008), &journal()),
            CursorAdmission::Future {
                cursor_usn: 0x9008,
                next_usn: 0x9000
            }
        );
    }

    #[test]
    fn negative_cursor_is_invalid() {
        assert_eq!(
            admit_cursor(&at(-8), &journal()),
            CursorAdmission::InvalidCursor { cursor_usn: -8 }
        );
    }

    #[test]
    fn contradictory_journal_bounds_are_refused_before_the_cursor() {
        let mut j = journal();
        j.first_usn = 0x9008;
        assert_eq!(
            admit_cursor(&at(0x1000), &j),
            CursorAdmission::InvalidJournal(InvalidBound::FirstAfterNext)
        );
        let mut j = journal();
        j.lowest_valid_usn = 0x9008;
        assert_eq!(
            j.bound_violation(),
            Some(InvalidBound::LowestValidAfterNext)
        );
        let mut j = journal();
        j.max_usn = 0x8000;
        assert_eq!(j.bound_violation(), Some(InvalidBound::NextAfterMax));
        let mut j = journal();
        j.first_usn = -1;
        assert_eq!(j.bound_violation(), Some(InvalidBound::NegativeUsn));
    }

    #[test]
    fn an_empty_journal_admits_only_its_start() {
        let mut j = journal();
        j.first_usn = 0;
        j.lowest_valid_usn = 0;
        j.next_usn = 0;
        assert_eq!(
            admit_cursor(&at(0), &j),
            CursorAdmission::UpToDate { next_usn: 0 }
        );
        assert_eq!(
            admit_cursor(&at(8), &j),
            CursorAdmission::Future {
                cursor_usn: 8,
                next_usn: 0
            }
        );
    }
}
