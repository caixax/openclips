//! Regenerates the bundled games database from the public seed list.
//!
//! ```text
//! cargo run -p openclips-core --example build_games_db -- <gamedatabase.json> <crates/core/assets/games.json>
//! ```

use openclips_core::games::seed;

const SOURCE: &str = "https://gist.github.com/Gr3gorywolf/1757c79ce1152966bf77bf8c6d069161";

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("input path");
    let output = args.next().expect("output path");
    let raw = std::fs::read_to_string(&input).expect("read seed");
    let entries = seed::clean(&raw).expect("parse seed");
    println!("{} clean entries", entries.len());
    let json = seed::to_json(entries, SOURCE).expect("serialize");
    std::fs::write(&output, json + "\n").expect("write output");
    println!("wrote {output}");
}
