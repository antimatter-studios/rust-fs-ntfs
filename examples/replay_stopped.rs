//! Leaves IMAGE as a power cut right after its replay's first undo write
//! leaves it (#754): every write up to and including that one landed, none
//! after, and `$LogFile` still holds work. The Windows scenarios attach the
//! result, so Windows' own restart runs over the log a stopped replay left
//! (#755).
//!
//! Usage: `cargo run --release --example replay_stopped -- IMAGE`

#[path = "../tests/replay_stop/mod.rs"]
mod replay_stop;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [image] = args.as_slice() else {
        eprintln!("usage: replay_stopped IMAGE");
        std::process::exit(2);
    };
    let stopped = fs_ntfs::block_io::PathIo::open_rw(std::path::Path::new(image))
        .and_then(|mut io| replay_stop::stop_after_first_undo(&mut io));
    match stopped {
        Ok(n) => println!("replay_stopped: {image}: the power was cut after write {n}"),
        Err(e) => {
            eprintln!("replay_stopped: {image}: {e}");
            std::process::exit(1);
        }
    }
}
