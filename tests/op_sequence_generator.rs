//! Fixture-free contracts shared by deterministic replay and future fuzz targets.
#[path = "op_sequence_model/generator.rs"]
mod generator;
#[path = "op_sequence_model/model.rs"]
mod model;
use generator::{
    decode, decode_regression, decode_replay, encode, encode_regression, Generator, Geometry, Op,
    Regression, MAX_DATA, MAX_EA_VALUE, MAX_PATH, MAX_SIZE, MAX_STEPS,
};
use model::{Error, Model, NEED_EA};

fn tree() -> Model {
    let mut table: Vec<_> = (0..=u16::MAX).collect();
    for c in b'a'..=b'z' {
        table[c as usize] = c.to_ascii_uppercase() as u16;
    }
    Model::new(table).unwrap()
}
fn frame(verb: u8, target: u8, parent: u8, name: u8, len: u16, offset: u16) -> Vec<u8> {
    [
        vec![verb, target, parent, name],
        len.to_le_bytes().to_vec(),
        offset.to_le_bytes().to_vec(),
    ]
    .concat()
}

#[test]
fn named_reproducer_and_byte_decoding_have_a_host_independent_golden() {
    let input = [
        frame(0, 2, 2, 0, 0, 0),
        frame(2, 2, 2, 0, 3, 0),
        vec![0, 255, 65],
    ]
    .concat();
    let golden = include_bytes!("op_sequence_model/reproducers/basic-write-v1.json");
    let operations = decode_replay(golden).unwrap();
    assert_eq!(decode(&input, &tree()), operations);
    assert_eq!(encode(&operations).unwrap(), golden.trim_ascii_end());
    let mut m = tree();
    for op in operations {
        op.apply(&mut m).unwrap();
    }
    assert_eq!(m.read("/a").unwrap(), &[0, 255, 65]);
}

#[test]
fn empty_truncated_and_arbitrary_bytes_are_total_and_repeatable() {
    assert!(decode(&[], &tree()).is_empty());
    for byte in 0..=255 {
        let input = [byte; 33];
        for prefix in 0..=input.len() {
            let operations = decode(&input[..prefix], &tree());
            assert_eq!(decode(&input[..prefix], &tree()), operations);
            assert_eq!(
                decode_replay(&encode(&operations).unwrap()).unwrap(),
                operations
            );
        }
    }
}

#[test]
fn every_operation_is_reachable_without_a_disk() {
    let mut variants = std::collections::HashSet::new();
    for verb in 0..9 {
        let operations = decode(&frame(verb, 2, 2, 0, 0, 0), &tree());
        assert_eq!(operations.len(), 1);
        variants.insert(std::mem::discriminant(&operations[0]));
    }
    assert_eq!(variants.len(), 9);
}

#[test]
fn generation_obeys_operation_data_path_and_size_bounds() {
    let operations = decode(&vec![0; 100_000], &tree());
    assert_eq!(operations.len(), MAX_STEPS);
    for verb in [2, 3] {
        let input = [frame(verb, 2, 2, 0, 4096, u16::MAX), vec![7; MAX_DATA + 10]].concat();
        let op = decode(&input, &tree()).remove(0);
        match op {
            Op::Write { path, data } => {
                assert!(path.len() <= MAX_PATH);
                assert_eq!(data.len(), MAX_DATA);
            }
            Op::WriteAt { path, offset, data } => {
                assert!(path.len() <= MAX_PATH);
                assert_eq!(data.len(), MAX_DATA);
                assert!(offset + data.len() as u32 <= MAX_SIZE);
            }
            _ => panic!("write opcode"),
        }
    }
    assert!(encode(&vec![Op::Rmdir { path: "/".into() }; MAX_STEPS + 1]).is_err());
}

#[test]
fn live_model_paths_include_existing_missing_and_wrong_type_targets() {
    let mut m = tree();
    m.create("/", "dir", true).unwrap();
    m.create("/dir", "child", false).unwrap();
    m.rename("/dir", "moved", false).unwrap();
    for (selector, expected) in [(2, "/moved/child"), (0, "/missing"), (1, "/")] {
        let input = frame(3, selector, 2, 0, 0, 0);
        let mut generator = Generator::new(&input);
        let op = generator.next(&m).unwrap();
        assert!(matches!(op, Op::WriteAt { ref path, .. } if path == expected));
        assert!(generator.next(&m).is_none());
    }
    let input = [frame(0, 2, 2, 0, 0, 0), frame(3, 2, 2, 0, 1, 0), vec![42]].concat();
    let operations = decode(&input, &tree());
    assert!(
        matches!(&operations[1], Op::WriteAt { path, data, .. } if path == "/a" && data == &[42])
    );
}

#[test]
fn case_collisions_and_utf16_boundaries_remain_explicit_operations() {
    let mut m = tree();
    for (index, units, result) in [
        (0, 1, Ok(())),
        (1, 1, Err(Error::Exists)),
        (15, 255, Ok(())),
        (16, 254, Ok(())),
        (17, 255, Ok(())),
        (18, 256, Err(Error::InvalidName)),
    ] {
        let op = decode(&frame(0, 2, 2, index, 0, 0), &m).remove(0);
        assert!(matches!(&op, Op::Create { name, .. } if name.encode_utf16().count() == units));
        let before = m.clone();
        assert_eq!(op.apply(&mut m), result);
        if result.is_err() {
            assert_eq!(m, before);
        }
    }
}

#[test]
fn strict_replay_rejects_corruption_versions_unknown_fields_and_resource_excess() {
    let golden = include_bytes!("op_sequence_model/reproducers/basic-write-v1.json");
    for prefix in 0..golden.trim_ascii_end().len() {
        assert!(decode_replay(&golden[..prefix]).is_err());
    }
    for input in [
        r#"{"version":2,"operations":[]}"#,
        r#"{"version":1,"operations":[],"extra":0}"#,
        r#"{"version":1,"operations":[{"op":"unknown"}]}"#,
        r#"{"version":1,"operations":[]} trailing"#,
    ] {
        assert!(decode_replay(input.as_bytes()).is_err());
    }
    for value in [
        serde_json::json!({"op":"create", "parent":"/", "name":"n".repeat(257)}),
        serde_json::json!({"op":"unlink", "path":"p".repeat(MAX_PATH + 1)}),
        serde_json::json!({"op":"write", "path":"/a", "data":vec![0; MAX_DATA + 1]}),
        serde_json::json!({"op":"truncate", "path":"/a", "size":MAX_SIZE + 1}),
        serde_json::json!({"op":"write_at", "path":"/a", "offset":MAX_SIZE, "data":[1]}),
    ] {
        let op: Op = serde_json::from_value(value).unwrap();
        assert!(encode(std::slice::from_ref(&op)).is_err());
        let raw = serde_json::json!({"version": 1, "operations": [op]});
        assert!(decode_replay(&serde_json::to_vec(&raw).unwrap()).is_err());
    }
    assert!(decode_replay(&vec![b' '; 4 * 1024 * 1024 + 1]).is_err());
}

#[test]
fn operation_application_preserves_full_write_link_and_typed_removal_semantics() {
    let script = r#"{"version":1,"operations":[{"op":"create","parent":"/","name":"a"},{"op":"mkdir","parent":"/","name":"dir"},{"op":"write","path":"/a","data":[1,2,3]},{"op":"write_at","path":"/a","offset":5,"data":[9]},{"op":"truncate","path":"/a","size":4},{"op":"link","path":"/a","parent":"/dir","name":"b"},{"op":"rename","path":"/a","name":"A","replace":false},{"op":"unlink","path":"/A"},{"op":"rmdir","path":"/dir"}]}"#;
    let operations = decode_replay(script.as_bytes()).unwrap();
    let mut m = tree();
    for op in &operations[..8] {
        op.apply(&mut m).unwrap();
    }
    assert_eq!(m.read("/dir/b").unwrap(), &[1, 2, 3, 0]);
    assert_eq!(m.stat("/dir/b").unwrap().links, 1);
    let before = m.clone();
    assert_eq!(operations[8].apply(&mut m), Err(Error::NotEmpty));
    assert_eq!(
        Op::Unlink {
            path: "/dir".into()
        }
        .apply(&mut m),
        Err(Error::IsDirectory)
    );
    assert_eq!(
        Op::Rmdir {
            path: "/dir/b".into()
        }
        .apply(&mut m),
        Err(Error::NotDirectory)
    );
    assert_eq!(m, before);
    Op::Write {
        path: "/dir/b".into(),
        data: vec![7],
    }
    .apply(&mut m)
    .unwrap();
    assert_eq!(m.read("/dir/b").unwrap(), &[7]);
}

#[test]
fn captured_regressions_keep_their_pin_and_replay_ignores_it() {
    let operations = vec![
        Op::Create {
            parent: "/".into(),
            name: "a".into(),
        },
        Op::Truncate {
            path: "/a".into(),
            size: 0,
        },
    ];
    let regression = Regression {
        kind: "size of".into(),
        step: 2,
        geometry: Geometry {
            size: 64 * 1024 * 1024,
            cluster: 4096,
            record: 1024,
            label: "OPSEQ".into(),
            serial: 682,
        },
        original: "tmp/op-sequence-model/explorer/reproducer.json".into(),
        original_operations: 40,
    };
    let pinned = encode_regression(&operations, &regression).unwrap();
    assert_eq!(
        decode_regression(&pinned).unwrap(),
        (operations.clone(), Some(regression.clone()))
    );
    assert_eq!(decode_replay(&pinned).unwrap(), operations);
    let plain = encode(&operations).unwrap();
    assert!(!String::from_utf8(plain.clone())
        .unwrap()
        .contains("regression"));
    assert_eq!(
        decode_regression(&plain).unwrap(),
        (operations.clone(), None)
    );
    // The pin is as strict as the operations: unknown fields, a missing
    // kind and a step past the last operation are all refused.
    let mut value: serde_json::Value = serde_json::from_slice(&pinned).unwrap();
    value["regression"]["extra"] = 0.into();
    assert!(decode_replay(&serde_json::to_vec(&value).unwrap()).is_err());
    for (kind, step) in [("", 2), ("size of", 3)] {
        let bad = Regression {
            kind: kind.into(),
            step,
            ..regression.clone()
        };
        assert!(encode_regression(&operations, &bad).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&pinned).unwrap();
        value["regression"] = serde_json::to_value(&bad).unwrap();
        assert!(decode_regression(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let unbounded = vec![Op::Rmdir { path: "/".into() }; MAX_STEPS + 1];
    assert!(encode_regression(&unbounded, &regression).is_err());
}

#[test]
fn stream_operations_are_generated_on_files_bounded_and_replayable() {
    let mut variants = std::collections::HashSet::new();
    for verb in 0..generator::OPCODES as u8 {
        let operations = decode(&frame(verb, 2, 2, 0, 0, 0), &tree());
        assert_eq!(operations[0].opcode(), usize::from(verb));
        variants.insert(std::mem::discriminant(&operations[0]));
    }
    assert_eq!(variants.len(), generator::OPCODES);

    let mut m = tree();
    m.create("/", "f", false).unwrap();
    m.create("/", "d", true).unwrap();
    // A stream write consumes its payload, as a write does; names 0 and 1
    // are one stream under the table, so the delete removes the write's.
    let input = [
        frame(9, 2, 0, 0, 3, 0),
        vec![1, 2, 3],
        frame(10, 2, 0, 1, 0, 0),
    ]
    .concat();
    let operations = decode(&input, &m);
    assert_eq!(
        operations,
        vec![
            Op::WriteStream {
                path: "/f".into(),
                stream: "s".into(),
                data: vec![1, 2, 3],
            },
            Op::DeleteStream {
                path: "/f".into(),
                stream: "S".into(),
            },
        ]
    );
    let mut applied = m.clone();
    for op in &operations {
        op.apply(&mut applied).unwrap();
    }
    assert!(applied.streams("/f").unwrap().is_empty());
    let replay = encode(&operations).unwrap();
    assert_eq!(decode_replay(&replay).unwrap(), operations);
    let text = String::from_utf8(replay).unwrap();
    assert!(text.contains(r#""op":"write_stream""#), "{text}");
    assert!(text.contains(r#""op":"delete_stream""#), "{text}");

    // Selector 0 is missing and 1 is a file used as a directory; streams
    // are never generated on a directory in this slice (#731).
    for (selector, expected) in [(0, "/missing"), (1, "/f/x"), (2, "/f"), (3, "/f")] {
        let op = decode(&frame(10, selector, 0, 0, 0, 0), &m).remove(0);
        assert!(
            matches!(&op, Op::DeleteStream { path, .. } if path == expected),
            "{selector}: {op:?}"
        );
    }
    for selector in 0..=u8::MAX {
        let op = decode(&frame(9, selector, 0, 0, 0, 0), &m).remove(0);
        let Op::WriteStream { path, .. } = op else {
            panic!("stream opcode")
        };
        assert!(path != "/" && path != "/d", "{selector}: {path}");
    }

    // The name pool's UTF-16 boundaries, an existing name and a fresh one.
    let mut named = m.clone();
    named.write_stream("/f", "kept", b"").unwrap();
    for (index, units, result) in [
        (16, 255, Ok(())),
        (17, 255, Ok(())),
        (18, 256, Err(Error::InvalidName)),
        (11, 3, Err(Error::InvalidName)),
        (13, 2, Err(Error::InvalidName)),
    ] {
        let op = decode(&frame(9, 2, 0, index, 0, 0), &named).remove(0);
        assert!(
            matches!(&op, Op::WriteStream { stream, .. } if stream.encode_utf16().count() == units),
            "{index}: {op:?}"
        );
        let mut probe = named.clone();
        assert_eq!(op.apply(&mut probe), result, "{op:?}");
        if result.is_err() {
            assert_eq!(probe, named, "a refused stream write changed the model");
        }
    }
    let existing = decode(&frame(10, 2, 0, 19, 0, 0), &named).remove(0);
    assert!(matches!(&existing, Op::DeleteStream { stream, .. } if stream == "kept"));
    let fresh = decode(&frame(9, 2, 0, 20, 0, 0), &named).remove(0);
    assert!(matches!(&fresh, Op::WriteStream { stream, .. } if stream == "stream1"));

    for value in [
        serde_json::json!({"op":"write_stream", "path":"/f", "stream":"n".repeat(257), "data":[]}),
        serde_json::json!({"op":"write_stream", "path":"/f", "stream":"s", "data":vec![0; MAX_DATA + 1]}),
        serde_json::json!({"op":"delete_stream", "path":"p".repeat(MAX_PATH + 1), "stream":"s"}),
    ] {
        let op: Op = serde_json::from_value(value).unwrap();
        assert!(encode(std::slice::from_ref(&op)).is_err(), "{op:?}");
    }
}

#[test]
fn ea_operations_are_generated_on_files_bounded_and_replayable() {
    for verb in [11, 12] {
        let op = decode(&frame(verb, 2, 2, 0, 0, 0), &tree()).remove(0);
        assert_eq!(op.opcode(), usize::from(verb), "{op:?}");
    }
    let mut m = tree();
    m.create("/", "f", false).unwrap();
    m.create("/", "d", true).unwrap();
    // An EA write consumes its payload, as a write does, and the parent
    // byte's low bit sets NEED_EA. Name 13 is fresh; name 12 is the
    // target's first EA in lower case, which matches its stored upper-case
    // name only case-insensitively.
    let input = [
        frame(11, 2, 1, 13, 3, 0),
        vec![1, 2, 3],
        frame(12, 2, 0, 12, 0, 0),
    ]
    .concat();
    let operations = decode(&input, &m);
    assert_eq!(
        operations,
        vec![
            Op::WriteEa {
                path: "/f".into(),
                name: "ea1".into(),
                flags: NEED_EA,
                value: vec![1, 2, 3],
            },
            Op::RemoveEa {
                path: "/f".into(),
                name: "ea1".into(),
            },
        ]
    );
    let mut applied = m.clone();
    operations[0].apply(&mut applied).unwrap();
    assert_eq!(
        applied.eas("/f").unwrap(),
        vec![(b"EA1".to_vec(), NEED_EA, vec![1, 2, 3])]
    );
    operations[1].apply(&mut applied).unwrap();
    assert!(applied.eas("/f").unwrap().is_empty());
    let replay = encode(&operations).unwrap();
    assert_eq!(decode_replay(&replay).unwrap(), operations);
    let text = String::from_utf8(replay).unwrap();
    assert!(text.contains(r#""op":"write_ea""#), "{text}");
    assert!(text.contains(r#""op":"remove_ea""#), "{text}");

    // The offset's low byte picks a size class for the length: under 128,
    // 2048 and 16384 bytes, or the whole u16. Payload short of the size is
    // repeated; with none, a fixed pattern fills the value.
    for (class, size) in [(0, 56), (1, 952), (2, 3000), (3, 3000)] {
        let op = decode(&frame(11, 2, 0, 0, 3000, class), &m).remove(0);
        let Op::WriteEa { value, .. } = op else {
            panic!("EA opcode")
        };
        assert_eq!(value.len(), size, "class {class}");
        assert!(value
            .iter()
            .enumerate()
            .all(|(i, b)| *b == (i % 251) as u8 + 1));
    }
    let short = [frame(11, 2, 0, 0, 10, 0), vec![4, 5, 6]].concat();
    let op = decode(&short, &m).remove(0);
    assert!(
        matches!(&op, Op::WriteEa { value, .. } if value == &[4, 5, 6, 4, 5, 6, 4, 5, 6, 4]),
        "{op:?}"
    );
    // The whole u16 is a value no file's EAs can hold beside its name.
    let op = decode(&frame(11, 2, 0, 0, u16::MAX, 3), &m).remove(0);
    assert!(matches!(&op, Op::WriteEa { value, .. } if value.len() == MAX_EA_VALUE));
    let mut probe = m.clone();
    assert_eq!(op.apply(&mut probe), Err(Error::EaSetTooLarge));
    assert_eq!(probe, m, "a refused EA write changed the model");

    // Selector 0 is missing and 1 is a file used as a directory; EAs are
    // never generated on a directory in this slice (#732).
    for (selector, expected) in [(0, "/missing"), (1, "/f/x"), (2, "/f"), (3, "/f")] {
        let op = decode(&frame(12, selector, 0, 0, 0, 0), &m).remove(0);
        assert!(
            matches!(&op, Op::RemoveEa { path, .. } if path == expected),
            "{selector}: {op:?}"
        );
    }
    for selector in 0..=u8::MAX {
        let op = decode(&frame(11, selector, 0, 0, 0, 0), &m).remove(0);
        let Op::WriteEa { path, .. } = op else {
            panic!("EA opcode")
        };
        assert!(path != "/" && path != "/d", "{selector}: {path}");
    }

    // The name pool's byte-length boundaries and a case collision.
    let mut named = m.clone();
    named.write_ea("/f", b"kept", 0, b"").unwrap();
    for (index, bytes, result) in [
        (1, 1, Ok(())),
        (9, 254, Ok(())),
        (10, 254, Ok(())),
        (11, 255, Err(Error::InvalidName)),
        (8, 0, Err(Error::InvalidName)),
    ] {
        let op = decode(&frame(11, 2, 0, index, 0, 0), &named).remove(0);
        assert!(
            matches!(&op, Op::WriteEa { name, .. } if name.len() == bytes),
            "{index}: {op:?}"
        );
        let mut probe = named.clone();
        assert_eq!(op.apply(&mut probe), result, "{op:?}");
        if result.is_err() {
            assert_eq!(probe, named, "a refused EA write changed the model");
        }
    }
    let mut collided = named.clone();
    for index in [9, 10] {
        decode(&frame(11, 2, 0, index, 0, 0), &named)
            .remove(0)
            .apply(&mut collided)
            .unwrap();
    }
    assert_eq!(collided.eas("/f").unwrap().len(), 2, "n and N are one name");
    let existing = decode(&frame(12, 2, 0, 12, 0, 0), &named).remove(0);
    assert!(matches!(&existing, Op::RemoveEa { name, .. } if name == "kept"));
    let fresh = decode(&frame(11, 2, 0, 13, 0, 0), &named).remove(0);
    assert!(matches!(&fresh, Op::WriteEa { name, .. } if name == "ea1"));

    for value in [
        serde_json::json!({"op":"write_ea", "path":"/f", "name":"n".repeat(257), "flags":0, "value":[]}),
        serde_json::json!({"op":"write_ea", "path":"/f", "name":"a", "flags":0, "value":vec![0; MAX_EA_VALUE + 1]}),
        serde_json::json!({"op":"remove_ea", "path":"/f", "name":"n".repeat(257)}),
        serde_json::json!({"op":"remove_ea", "path":"p".repeat(MAX_PATH + 1), "name":"a"}),
    ] {
        let op: Op = serde_json::from_value(value).unwrap();
        assert!(encode(std::slice::from_ref(&op)).is_err(), "{op:?}");
    }
}
