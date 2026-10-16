//! Independent checkpoint headers and current-record client-data length.

use std::{collections::BTreeMap, env, error::Error, fs, io::Read};

use fs_ntfs::logfile_replay::record_at_lsn;
use sha2::{Digest, Sha256};

const ORACLE_REV: &str = "b88585646801ee2c1e2acf91a078da6ab57af778";
const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;

mod native_metadata;

#[cfg(test)]
mod selector_tests;

/// Select only decoded identities. Pointer words and production acceptance
/// cannot supply a missing target. Both links must be nonzero, with unique
/// targets, because the physical proof traverses both edges. Only the live
/// log is eligible: the source and both targets must be no older than the
/// NTFS client's oldest LSN, since an older record survives only as a stale
/// copy from an earlier pass of the circular log, away from where its LSN
/// says it is.
fn complete_pair(
    records: &[ntfslog::LogRecord],
    file_len: u64,
    oldest_lsn: u64,
) -> Result<&ntfslog::LogRecord, &'static str> {
    if records.len() > 1_000_000 {
        return Err("independent record inventory exceeds selector budget");
    }
    let mut by_lsn = BTreeMap::new();
    let mut by_offset = BTreeMap::new();
    for record in records {
        if record.lsn == 0
            || record
                .offset
                .checked_add(48)
                .is_none_or(|end| end > file_len)
        {
            return Err("independent record identity is zero or outside source bounds");
        }
        if by_lsn.insert(record.lsn, record).is_some()
            || by_offset.insert(record.offset, record.lsn).is_some()
        {
            return Err("independent record identity is ambiguous (LSN or physical offset)");
        }
    }
    // BTreeMap fixes selection order independent of decoder enumeration.
    for record in by_lsn.values() {
        let links = [record.previous_lsn, record.undo_next_lsn];
        if !matches!(record.body, ntfslog::Body::Update(_))
            || links.contains(&0)
            || record.lsn < oldest_lsn
        {
            continue;
        }
        if links.into_iter().all(|lsn| {
            lsn >= oldest_lsn
                && by_lsn.get(&lsn).is_some_and(|target| {
                    target.lsn != record.lsn
                        && target.client == record.client
                        && target.transaction_id == record.transaction_id
                        && matches!(target.body, ntfslog::Body::Update(_))
                })
        }) {
            return Ok(record);
        }
    }
    Err("independent decoder found no live update record with both links decoded")
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<String> = env::args().collect();
    let native_mode = args
        .get(1)
        .filter(|a| {
            matches!(
                a.as_str(),
                "--select-metadata"
                    | "--compare-metadata"
                    | "--select-checkpoint-length"
                    | "--compare-checkpoint-length"
            )
        })
        .cloned();
    if native_mode.is_some() {
        args.remove(1);
    }
    let select = args.get(1).is_some_and(|a| a == "--select-checkpoint");
    let checkpoint_only = args.get(1).is_some_and(|a| a == "--checkpoint-only");
    let select_pair = args.get(1).is_some_and(|a| a == "--select-complete-pair");
    if select || checkpoint_only || select_pair {
        args.remove(1);
    }
    if args.len() != 2 && args.len() != 3 {
        return Err("usage: logfile-record-oracle [--select-checkpoint|--checkpoint-only|--select-complete-pair] RAW_LOGFILE [EXPECTED_SHA256]".into());
    }
    let file = fs::File::open(&args[1])?;
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_LOG_BYTES {
        return Err("capture must contain 1..=64 MiB; regenerate the Windows capture".into());
    }
    let sha = format!("{:x}", Sha256::digest(&bytes));
    if let Some(expected) = args.get(2) {
        if *expected != sha {
            return Err(
                format!("capture SHA-256 mismatch: expected {expected}, found {sha}").into(),
            );
        }
    }

    if let Some(mode) = native_mode {
        return native_metadata::run(&mode, &bytes, &sha, &args[1]);
    }
    // All location and expected-field evidence comes from the independent
    // decoder. Never use RecordView to select a record or derive expected fields.
    let independent = ntfslog::parse(&bytes);
    let restart = independent
        .restart()
        .ok_or("oracle found no restart page")?;
    if restart.version.major != 2 {
        return Err(format!(
            "independent decoder requires LFS 2.x, found {}",
            restart.version
        )
        .into());
    }
    if restart
        .area
        .clients
        .iter()
        .filter(|c| c.name == "NTFS")
        .count()
        != 1
    {
        return Err("oracle returned ambiguous NTFS client identities".into());
    }
    let client = restart
        .area
        .clients
        .iter()
        .find(|client| client.name == "NTFS")
        .ok_or("oracle found no NTFS client")?;
    if select_pair {
        let source = complete_pair(&independent.records, bytes.len() as u64, client.oldest_lsn)?;
        println!("sha256\toracle_revision\tlfs_major\tlfs_minor\trole\tlsn\tphysical_offset\tprevious_lsn\tundo_next_lsn\tclient_sequence\tclient_index\ttransaction_id\tflags\tbody");
        for (role, lsn) in [
            ("source", source.lsn),
            ("previous_target", source.previous_lsn),
            ("undo_target", source.undo_next_lsn),
        ] {
            if lsn == 0 {
                continue;
            }
            let selected = independent.records.iter().find(|r| r.lsn == lsn).unwrap();
            println!(
                "{sha}\t{ORACLE_REV}\t{}\t{}\t{role}\t{:#x}\t{:#x}\t{:#x}\t{:#x}\t{:#x}\t{:#x}\t{:#x}\t{:#x}\tupdate",
                restart.version.major, restart.version.minor, selected.lsn,
                selected.offset, selected.previous_lsn, selected.undo_next_lsn,
                selected.client.sequence, selected.client.index,
                selected.transaction_id, selected.flags
            );
        }
        eprintln!("independent decoder problems: {:#?}", independent.problems);
        return Ok(());
    }
    let mut matches = independent
        .records
        .iter()
        .filter(|record| record.lsn == client.restart_lsn);
    let record = matches
        .next()
        .ok_or("oracle did not decode the checkpoint")?;
    if matches.next().is_some() {
        return Err("oracle returned ambiguous checkpoint identities".into());
    }
    // Published LFS_RECORD format: ClientRestart = 2. The independent
    // decoder must actually return a checkpoint, not a guessed raw word.
    // https://flatcap.github.io/linux-ntfs/ntfs/files/logfile.html
    let expected_type = match &record.body {
        ntfslog::Body::Checkpoint(_) => 2,
        _ => return Err("oracle did not classify the restart record as a checkpoint".into()),
    };
    // The public LogRecord API does not expose data_length. The independent
    // restart area does expose the last record's length, tied to current_lsn.
    // Require that exact record to be decoded; never substitute checkpoint
    // size, a raw-header read, or a value from RecordView.
    let mut current_matches = independent
        .records
        .iter()
        .filter(|record| record.lsn == restart.area.current_lsn);
    let current = current_matches
        .next()
        .ok_or("oracle did not decode the current record for client-data length")?;
    if current_matches.next().is_some() {
        return Err("oracle returned ambiguous current-record identities".into());
    }
    if select {
        // Selection uses only independent evidence, before RecordView is called.
        println!("sha256\toracle_revision\tlfs_major\tlfs_minor\tcheckpoint_lsn\tcurrent_lsn");
        println!(
            "{sha}\t{ORACLE_REV}\t{}\t{}\t{:#x}\t{:#x}",
            restart.version.major, restart.version.minor, record.lsn, current.lsn
        );
        eprintln!("independent decoder problems: {:#?}", independent.problems);
        return Ok(());
    }
    let view = record_at_lsn(&bytes, record.lsn, bytes.len())?;
    let current_view = record_at_lsn(&bytes, current.lsn, bytes.len())?;
    println!("capture\tsha256\toracle_revision\tlsn\tfield\toracle\trecord_view");
    println!(
        "{}\t{sha}\t{ORACLE_REV}\t{:#x}\trecord_type\t{expected_type}\t{}",
        args[1], record.lsn, view.record_type
    );
    // These are public LogRecord fields from the independent decoder, not
    // raw-header offsets or values inferred by following our own reader.
    for (field, expected, actual) in [
        (
            "client_previous_lsn",
            record.previous_lsn,
            view.client_previous_lsn,
        ),
        (
            "client_undo_next_lsn",
            record.undo_next_lsn,
            view.client_undo_next_lsn,
        ),
    ] {
        println!(
            "{}\t{sha}\t{ORACLE_REV}\t{:#x}\t{field}\t{expected:#x}\t{actual:#x}",
            args[1], record.lsn
        );
    }
    // Client identity is also decoded independently; compare both components
    // even when the index is zero. Keep every row before mismatch refusal.
    for (field, expected, actual) in [
        ("seq_number", record.client.sequence, view.seq_number),
        ("client_index", record.client.index, view.client_index),
    ] {
        println!(
            "{}\t{sha}\t{ORACLE_REV}\t{:#x}\t{field}\t{expected:#x}\t{actual:#x}",
            args[1], record.lsn
        );
    }
    for (field, expected, actual) in [
        ("transaction_id", record.transaction_id, view.transaction_id),
        ("flags", u32::from(record.flags), u32::from(view.flags)),
    ] {
        println!(
            "{}\t{sha}\t{ORACLE_REV}\t{:#x}\t{field}\t{expected:#x}\t{actual:#x}",
            args[1], record.lsn
        );
    }
    println!(
        "{}\t{sha}\t{ORACLE_REV}\t{:#x}\tclient_data_length\t{:#x}\t{:#x}",
        args[1], current.lsn, restart.area.last_lsn_data_length, current_view.client_data_length
    );
    if view.record_type != expected_type {
        return Err(format!("record_type differs at LSN {:#x}", record.lsn).into());
    }
    if view.client_previous_lsn != record.previous_lsn {
        return Err(format!("client_previous_lsn differs at LSN {:#x}", record.lsn).into());
    }
    if view.client_undo_next_lsn != record.undo_next_lsn {
        return Err(format!("client_undo_next_lsn differs at LSN {:#x}", record.lsn).into());
    }
    if view.seq_number != record.client.sequence {
        return Err(format!("seq_number differs at LSN {:#x}", record.lsn).into());
    }
    if view.client_index != record.client.index {
        return Err(format!("client_index differs at LSN {:#x}", record.lsn).into());
    }
    if view.transaction_id != record.transaction_id {
        return Err(format!("transaction_id differs at LSN {:#x}", record.lsn).into());
    }
    if view.flags != record.flags {
        return Err(format!("flags differ at LSN {:#x}", record.lsn).into());
    }
    if current_view.client_data_length != restart.area.last_lsn_data_length {
        return Err(format!("client_data_length differs at LSN {:#x}", current.lsn).into());
    }
    // Select solely from independent evidence. The immutable Windows CI
    // regression requires this path; inputs without a nonzero link retain
    // their original eight comparisons without claiming nonzero coverage.
    // Only the live log is eligible: the record and every record it links to
    // must be no older than the independent NTFS client's oldest LSN. An
    // older record survives only in a stale page copy from an earlier pass
    // of the circular log, so it is no longer part of the log at all.
    let live = |lsn: u64| lsn >= client.oldest_lsn;
    if let Some(linked) = independent.records.iter().find(|record| {
        let links = [record.previous_lsn, record.undo_next_lsn];
        !checkpoint_only
            && live(record.lsn)
            && links != [0, 0]
            && links.into_iter().all(|lsn| lsn == 0 || live(lsn))
    }) {
        if independent
            .records
            .iter()
            .filter(|record| record.lsn == linked.lsn)
            .count()
            != 1
        {
            return Err("oracle returned ambiguous nonzero-record identities".into());
        }
        let linked_view = record_at_lsn(&bytes, linked.lsn, bytes.len())?;
        for (field, expected, actual) in [
            (
                "client_previous_lsn",
                linked.previous_lsn,
                linked_view.client_previous_lsn,
            ),
            (
                "client_undo_next_lsn",
                linked.undo_next_lsn,
                linked_view.client_undo_next_lsn,
            ),
        ] {
            println!(
                "{}\t{sha}\t{ORACLE_REV}\t{:#x}\t{field}\t{expected:#x}\t{actual:#x}",
                args[1], linked.lsn
            );
        }
        if linked_view.client_previous_lsn != linked.previous_lsn
            || linked_view.client_undo_next_lsn != linked.undo_next_lsn
        {
            return Err(format!("nonzero-record links differ at LSN {:#x}", linked.lsn).into());
        }
        eprintln!("links: 2 independent nonzero-record comparisons passed");
    }
    eprintln!("headers: 7 checkpoint fields and 1 current-record length comparison passed");
    Ok(())
}
