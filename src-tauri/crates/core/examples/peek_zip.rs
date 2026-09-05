//! List the entries of an archive, for checking what a component ships.

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        println!("usage: peek_zip <archive>");
        return;
    };
    let Ok(mut file) = std::fs::File::open(&path) else {
        println!("cannot open {path}");
        return;
    };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else {
        return;
    };
    match neuralswap_core::zip::read::read_entries(&mut file, size) {
        Ok(entries) => {
            println!("{} entries in {path}", entries.len());
            for entry in &entries {
                println!("  {:>10}  {}", entry.uncompressed_size, entry.name);
            }
        }
        Err(error) => println!("could not read: {error}"),
    }
}
