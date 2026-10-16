//! CI-only native metadata selection. Expected values never use RecordView.
use super::*;

const FIELDS: [&str; 7] = [
    "record_type",
    "client_previous_lsn",
    "client_undo_next_lsn",
    "seq_number",
    "client_index",
    "transaction_id",
    "flags",
];

fn metadata(r: &ntfslog::LogRecord) -> [u64; 7] {
    let kind = match &r.body {
        ntfslog::Body::Update(_) => 1,
        ntfslog::Body::Checkpoint(_) => 2,
        ntfslog::Body::Other { record_type, .. } => u64::from(*record_type),
    };
    [
        kind,
        r.previous_lsn,
        r.undo_next_lsn,
        u64::from(r.client.sequence),
        u64::from(r.client.index),
        u64::from(r.transaction_id),
        u64::from(r.flags),
    ]
}

fn varied(records: &[ntfslog::LogRecord]) -> Result<Vec<&ntfslog::LogRecord>, &'static str> {
    if records.len() > 1_000_000 {
        return Err("native-metadata precondition: inventory budget exceeded");
    }
    let mut identities = BTreeMap::new();
    let mut offsets = BTreeMap::new();
    for r in records {
        if r.lsn == 0
            || identities.insert(r.lsn, r).is_some()
            || offsets.insert(r.offset, r.lsn).is_some()
        {
            return Err("native-metadata precondition: ambiguous or zero independent identity");
        }
    }
    let checkpoint = identities
        .values()
        .find(|r| matches!(r.body, ntfslog::Body::Checkpoint(_)))
        .ok_or("native-metadata precondition: no independent checkpoint")?;
    let base = metadata(checkpoint);
    let mut selected = vec![*checkpoint];
    // One independently decoded witness per varying field; no production lookup
    // participates. Zero values are ordinary observations, never filtered out.
    for (field, value) in base.iter().enumerate() {
        if let Some(r) = identities.values().find(|r| metadata(r)[field] != *value) {
            if !selected.iter().any(|s| s.lsn == r.lsn) {
                selected.push(*r);
            }
        }
    }
    if !selected.iter().any(|r| metadata(r)[0] != base[0])
        || !selected.iter().any(|r| metadata(r)[1..] != base[1..])
    {
        return Err("native-metadata precondition: need record-type AND another metadata-field variation; run mounted concurrent workload capture");
    }
    Ok(selected)
}

fn checkpoint_length_identity(checkpoint: u64, current: u64) -> Result<(), &'static str> {
    if checkpoint == 0 || checkpoint != current {
        return Err("checkpoint-length precondition: independent NTFS restart_lsn must equal current_lsn; run quiescent volume-flush capture (no inferred checkpoint length)");
    }
    Ok(())
}

fn mismatched_fields(expected: [u64; 7], actual: [u64; 7]) -> Vec<&'static str> {
    FIELDS
        .iter()
        .enumerate()
        .filter_map(|(i, field)| (expected[i] != actual[i]).then_some(*field))
        .collect()
}

pub(super) fn run(mode: &str, bytes: &[u8], sha: &str, path: &str) -> Result<(), Box<dyn Error>> {
    let decoded = ntfslog::parse(bytes);
    let restart = decoded
        .restart()
        .ok_or("native-metadata precondition: missing restart")?;
    if restart.version.major != 2 {
        return Err("native-metadata precondition: requires LFS2".into());
    }
    let select = mode.starts_with("--select-");
    let length = mode.ends_with("checkpoint-length");
    let records = if length {
        let clients: Vec<_> = restart
            .area
            .clients
            .iter()
            .filter(|c| c.name == "NTFS")
            .collect();
        if clients.len() != 1 {
            return Err("checkpoint-length precondition: unique NTFS client required".into());
        }
        checkpoint_length_identity(clients[0].restart_lsn, restart.area.current_lsn)?;
        let records: Vec<_> = decoded
            .records
            .iter()
            .filter(|r| r.lsn == clients[0].restart_lsn)
            .collect();
        if records.len() != 1 || !matches!(records[0].body, ntfslog::Body::Checkpoint(_)) {
            return Err("checkpoint-length precondition: unique decoded checkpoint/current identity required".into());
        }
        records
    } else {
        varied(&decoded.records)?
    };
    println!("capture\tsha256\toracle_revision\tlsn\tphysical_offset\tfield\toracle\trecord_view");
    let mut failures = Vec::new();
    for record in records {
        if record
            .offset
            .checked_add(48)
            .is_none_or(|end| end > bytes.len() as u64)
        {
            return Err(
                "native-metadata precondition: independent position outside capture".into(),
            );
        }
        let expected = metadata(record);
        let view = if select {
            None
        } else {
            match record_at_lsn(bytes, record.lsn, bytes.len()) {
                Ok(view) => Some(view),
                Err(e) => {
                    failures.push(format!("{:#x}: {e}", record.lsn));
                    None
                }
            }
        };
        let actual = view.as_ref().map(|v| {
            [
                u64::from(v.record_type),
                v.client_previous_lsn,
                v.client_undo_next_lsn,
                u64::from(v.seq_number),
                u64::from(v.client_index),
                u64::from(v.transaction_id),
                u64::from(v.flags),
            ]
        });
        for (i, field) in FIELDS.iter().enumerate() {
            let observed = actual.map(|a| format!("{:#x}", a[i])).unwrap_or_else(|| {
                if select {
                    "selection"
                } else {
                    "lookup-refused"
                }
                .into()
            });
            println!(
                "{path}\t{sha}\t{ORACLE_REV}\t{:#x}\t{:#x}\t{field}\t{:#x}\t{observed}",
                record.lsn, record.offset, expected[i]
            );
        }
        if let Some(actual) = actual {
            for field in mismatched_fields(expected, actual) {
                failures.push(format!("{field} mismatch at {:#x}", record.lsn));
            }
        }
        if length {
            let expected_length = restart.area.last_lsn_data_length;
            let observed = view
                .as_ref()
                .map(|v| format!("{:#x}", v.client_data_length))
                .unwrap_or_else(|| {
                    if select {
                        "selection"
                    } else {
                        "lookup-refused"
                    }
                    .into()
                });
            println!("{path}\t{sha}\t{ORACLE_REV}\t{:#x}\t{:#x}\tcheckpoint_client_data_length\t{expected_length:#x}\t{observed}", record.lsn, record.offset);
            if view
                .as_ref()
                .is_some_and(|v| v.client_data_length != expected_length)
            {
                failures.push("checkpoint client-data length mismatch".into());
            }
        }
    }
    eprintln!("independent decoder problems: {:#?}", decoded.problems);
    if !failures.is_empty() {
        return Err(failures.join("; ").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_metadata_controls_grade_every_field_and_accept_zero_observations() {
        for expected in [[0; 7], [2, 0x10, 0x20, 3, 0, 16, 1]] {
            assert!(mismatched_fields(expected, expected).is_empty());
            for i in 0..7 {
                let mut wrong = expected;
                wrong[i] ^= 1;
                assert_eq!(mismatched_fields(expected, wrong), vec![FIELDS[i]]);
            }
        }
    }
    #[test]
    fn native_metadata_controls_reject_absent_variation_and_wrong_length_identity() {
        let seed = crate::selector_tests::decoded_seed();
        let checkpoint = seed
            .records
            .iter()
            .find(|r| matches!(r.body, ntfslog::Body::Checkpoint(_)))
            .unwrap()
            .clone();
        assert!(varied(&[]).is_err());
        assert!(varied(std::slice::from_ref(&checkpoint)).is_err());
        let update = seed
            .records
            .iter()
            .find(|r| matches!(r.body, ntfslog::Body::Update(_)))
            .unwrap()
            .clone();
        // Controlled metadata is selector-policy evidence only, not native proof.
        let mut controlled = update.clone();
        controlled.previous_lsn = checkpoint.previous_lsn;
        controlled.undo_next_lsn = checkpoint.undo_next_lsn;
        controlled.client = checkpoint.client;
        controlled.transaction_id = checkpoint.transaction_id;
        controlled.flags = checkpoint.flags;
        assert!(varied(&[checkpoint.clone(), controlled.clone()]).is_err());
        controlled.transaction_id ^= 1;
        assert_eq!(
            varied(&[checkpoint.clone(), controlled.clone()])
                .unwrap()
                .len(),
            2
        );
        assert!(varied(&[checkpoint, controlled.clone(), controlled]).is_err());
        assert!(checkpoint_length_identity(1, 2).is_err());
        assert!(checkpoint_length_identity(0, 0).is_err());
        assert!(checkpoint_length_identity(1, 1).is_ok());
    }
}
